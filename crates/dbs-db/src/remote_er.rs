//! 远程方言的 ER 目录装配。
//!
//! 各驱动只负责从自己的数据字典读出“候选键位、完整类型签名和状态缺口”，
//! 这里统一做三件事：拒绝不能证明全表唯一的键、把名称对回真实表结构、
//! 丢弃指向目录外对象的证据并留下具体警告。

use crate::ConnectionOptions;
use anyhow::Result;
use dbs_core::er::{Endpoint, ErCatalog, ErColumnType, ScopedWarning, TableId, ViewComparison};
use dbs_core::{DbType, TableInfo};
use std::collections::{BTreeMap, BTreeSet};

/// 驱动交给公共装配器的 ER 证据。
///
/// 键与类型在这里可以暂时使用驱动返回的原始名称；[`assemble`] 会按真实表结构
/// 重新核对并改写为目录里的实际名称。
#[derive(Debug, Default, Clone)]
pub(crate) struct ErEvidence {
    pub keys: Vec<Endpoint>,
    /// MySQL 普通唯一索引带可空列时，只在“普通 `=` 且值非空”条件下唯一。
    pub non_null_keys: Vec<Endpoint>,
    pub views: Vec<ViewComparison>,
    /// 含 `<=>` 的视图比较；仅用于阻断普通等值，不作为正证。
    pub null_safe_views: Vec<ViewComparison>,
    pub types: Vec<ErColumnType>,
    /// 无法绑定到具体表的全局/内部提示。
    pub warnings: Vec<String>,
    /// 生成时已绑定到具体表的提示。
    pub scoped_warnings: Vec<ScopedWarning>,
}

impl ErEvidence {
    pub(crate) fn warn(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    pub(crate) fn warn_table(&mut self, table: &TableInfo, warning: impl Into<String>) {
        self.scoped_warnings.push(ScopedWarning {
            table: TableId::from(table),
            message: warning.into(),
        });
    }
}

/// 索引中的一个键位。
///
/// `column` 为空表示表达式、函数索引或驱动无法还原的键位；`prefix` 表示前缀索引。
/// `nullable` 为 `None` 表示目录没有给出可空性，装配时按“未知”保守处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyPart {
    pub column: Option<String>,
    pub prefix: bool,
    pub nullable: Option<bool>,
    pub gap: String,
}

impl KeyPart {
    pub(crate) fn column(column: impl Into<String>, nullable: bool) -> Self {
        Self {
            column: Some(column.into()),
            prefix: false,
            nullable: Some(nullable),
            gap: String::new(),
        }
    }

    pub(crate) fn column_unknown_nullability(column: impl Into<String>) -> Self {
        Self {
            column: Some(column.into()),
            prefix: false,
            nullable: None,
            gap: String::new(),
        }
    }

    pub(crate) fn prefix(column: impl Into<String>, nullable: bool) -> Self {
        Self {
            column: Some(column.into()),
            prefix: true,
            nullable: Some(nullable),
            gap: String::new(),
        }
    }

    pub(crate) fn prefix_unknown_nullability(column: impl Into<String>) -> Self {
        Self {
            column: Some(column.into()),
            prefix: true,
            nullable: None,
            gap: String::new(),
        }
    }

    pub(crate) fn gap(gap: impl Into<String>) -> Self {
        Self {
            column: None,
            prefix: false,
            nullable: None,
            gap: gap.into(),
        }
    }
}

/// 驱动从唯一索引/唯一约束读出的候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexCandidate {
    pub name: String,
    pub primary: bool,
    pub unique: bool,
    /// 索引/约束是否处于可用、全表生效的状态。不可用时在 `gap` 说明原因。
    pub usable: bool,
    pub gap: String,
    pub parts: Vec<KeyPart>,
}

impl IndexCandidate {
    pub(crate) fn new(name: impl Into<String>, primary: bool, parts: Vec<KeyPart>) -> Self {
        Self {
            name: name.into(),
            primary,
            unique: true,
            usable: true,
            gap: String::new(),
            parts,
        }
    }

    pub(crate) fn unusable(mut self, gap: impl Into<String>) -> Self {
        self.usable = false;
        self.gap = gap.into();
        self
    }
}

/// 按 `DbType` 分派到各驱动的 ER 读取。
pub(crate) async fn load(opts: &ConnectionOptions) -> Result<ErCatalog> {
    match opts.db_type {
        DbType::MySql => crate::mysql::load_er_catalog(opts).await,
        DbType::PostgreSql => crate::postgres::load_er_catalog(opts).await,
        DbType::SqlServer => crate::sqlserver::load_er_catalog(opts).await,
        DbType::Oracle => crate::oracle::load_er_catalog(opts).await,
        DbType::Dameng => crate::dameng::load_er_catalog(opts).await,
        DbType::Sqlite => anyhow::bail!("SQLite 的 ER 目录由 sqlite::er 负责"),
        DbType::MongoDb => crate::mongodb::er::load(opts).await,
    }
}

/// 从候选索引中挑出可证明全表有效、键位顺序正确的唯一键。
///
/// `null_blocks_unique` 表示该方言的非主键唯一键是否允许同一列出现多个 NULL：
/// MySQL / PostgreSQL / Oracle / 达梦为 `true`，SQL Server 的唯一索引把 NULL
/// 也视为可比较值，因此为 `false`。
pub(crate) fn collect_keys(
    dialect: &str,
    table: &TableInfo,
    candidates: &[IndexCandidate],
    null_blocks_unique: bool,
    evidence: &mut ErEvidence,
) -> Vec<Endpoint> {
    collect_keys_inner(
        dialect,
        table,
        candidates,
        null_blocks_unique,
        NullableKeys::Reject,
        evidence,
    )
    .0
}

/// MySQL 专用：可空唯一索引不能进入全表键，但可作为“非空等值”键。
pub(crate) fn collect_keys_and_nullable(
    dialect: &str,
    table: &TableInfo,
    candidates: &[IndexCandidate],
    null_blocks_unique: bool,
    evidence: &mut ErEvidence,
) -> (Vec<Endpoint>, Vec<Endpoint>) {
    collect_keys_inner(
        dialect,
        table,
        candidates,
        null_blocks_unique,
        NullableKeys::Record,
        evidence,
    )
}

#[derive(Clone, Copy)]
enum NullableKeys {
    Reject,
    Record,
}

fn collect_keys_inner(
    dialect: &str,
    table: &TableInfo,
    candidates: &[IndexCandidate],
    null_blocks_unique: bool,
    nullable_keys: NullableKeys,
    evidence: &mut ErEvidence,
) -> (Vec<Endpoint>, Vec<Endpoint>) {
    let table_label = table.qualified_name();
    let mut keys = Vec::new();
    let mut non_null_keys = Vec::new();

    for candidate in candidates {
        let kind = if candidate.primary {
            "主键"
        } else {
            "唯一索引"
        };
        let label = format!(
            "{dialect} 表 {table_label} 的{kind} {}",
            candidate.name.trim()
        );
        if !candidate.unique {
            continue;
        }
        if !candidate.usable {
            evidence.warn_table(
                table,
                format!(
                    "{label} 不能作为全表唯一键：{}",
                    candidate.gap.trim().trim_end_matches('；')
                ),
            );
            continue;
        }
        if candidate.parts.is_empty() {
            evidence.warn_table(table, format!("{label} 不能作为全表唯一键：没有键位列"));
            continue;
        }

        // 先完整检查键位结构，不因中途发现可空列就提前退出，
        // 否则后面的前缀/表达式/缺位会被漏掉并误当成“非空等值键”。
        let mut columns = Vec::with_capacity(candidate.parts.len());
        let mut seen = BTreeSet::new();
        let mut rejected = None;
        for part in &candidate.parts {
            if !part.gap.trim().is_empty() {
                rejected = Some(part.gap.trim().to_string());
                break;
            }
            let Some(column) = part.column.as_deref() else {
                rejected = Some("键位列不是普通列".to_string());
                break;
            };
            if part.prefix {
                rejected = Some(format!("键位列 {column} 使用前缀索引，只保证前缀唯一"));
                break;
            }
            if !seen.insert(column.to_ascii_lowercase()) {
                rejected = Some(format!("键位列 {column} 重复出现"));
                break;
            }
            columns.push(column.to_string());
        }
        if let Some(gap) = rejected {
            evidence.warn_table(table, format!("{label} 不能作为全表唯一键：{gap}"));
            continue;
        }

        let mut actual = Vec::with_capacity(columns.len());
        let mut missing = None;
        for (part, column) in candidate.parts.iter().zip(columns) {
            match unique_column(table, &column) {
                Ok(name) => {
                    // 只有 MySQL 需要把可空唯一键降级为“非空等值键”，
                    // 其他方言保持原有全表键语义，不引入新的拒绝条件。
                    if matches!(nullable_keys, NullableKeys::Record)
                        && !candidate.primary
                        && null_blocks_unique
                    {
                        if let Some(nullable) = part.nullable {
                            let declared = match table.columns.iter().find(|meta| meta.name == name)
                            {
                                Some(meta) => meta.nullable == nullable,
                                None => true,
                            };
                            if !declared {
                                missing = Some(format!(
                                    "键位列 {name} 的可空性与表结构冲突，不能证明唯一"
                                ));
                                break;
                            }
                        }
                    }
                    actual.push(name.to_string());
                }
                Err(gap) => {
                    missing = Some(gap);
                    break;
                }
            }
        }
        if let Some(gap) = missing {
            evidence.warn_table(table, format!("{label} 不能作为全表唯一键：{gap}"));
            continue;
        }

        let endpoint = Endpoint {
            table: TableId::from(table),
            columns: actual,
        };
        if candidate.primary || !null_blocks_unique {
            keys.push(endpoint);
            continue;
        }

        if candidate.parts.iter().any(|part| part.nullable.is_none()) {
            evidence.warn_table(
                table,
                format!("{label} 不能作为全表唯一键：键位列的可空性未知，不能证明全表唯一"),
            );
            continue;
        }
        if candidate
            .parts
            .iter()
            .any(|part| part.nullable == Some(true))
        {
            match nullable_keys {
                NullableKeys::Record => non_null_keys.push(endpoint),
                NullableKeys::Reject => evidence.warn_table(
                    table,
                    format!("{label} 不能作为全表唯一键：键位列可为 NULL，唯一键允许多个 NULL"),
                ),
            }
            continue;
        }
        keys.push(endpoint);
    }

    keys.sort();
    keys.dedup();
    non_null_keys.sort();
    non_null_keys.dedup();
    (keys, non_null_keys)
}

/// Oracle / 达梦共享的目录行。两种方言的数据字典形状接近，差异只在取值方式。
#[derive(Debug, Clone)]
pub(crate) struct OracleLikeTypeRow {
    pub table: String,
    pub column: String,
    pub data_type: String,
    pub data_length: Option<i64>,
    pub precision: Option<i64>,
    pub scale: Option<i64>,
    pub char_length: Option<i64>,
    pub char_used: Option<String>,
    pub charset: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleLikeConstraintRow {
    pub table: String,
    pub constraint: String,
    pub kind: String,
    pub column: String,
    pub position: i64,
    pub status: String,
    pub validated: String,
    pub index_status: Option<String>,
    pub index_type: Option<String>,
    pub nullable: Option<bool>,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleLikeIndexRow {
    pub table: String,
    pub index: String,
    pub status: String,
    pub index_type: String,
    pub partitioned: String,
    pub column: String,
    pub position: i64,
    pub nullable: Option<bool>,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleLikeFkRow {
    pub table: String,
    pub constraint: String,
    pub status: String,
    pub validated: String,
}

/// 装配 Oracle / 达梦共享的 ER 证据。
///
/// 键可以从约束或独立唯一索引来；约束路径要求约束状态、验证状态和背后索引
/// 都可用；独立索引路径拒绝函数索引、不可用索引和分区唯一索引。外键状态只
/// 产生警告，不把声明外键伪装成已证明的引用完整性。
#[allow(clippy::too_many_arguments)]
pub(crate) fn oracle_like_evidence(
    dialect: &str,
    owner: &str,
    tables: &[TableInfo],
    type_rows: &[OracleLikeTypeRow],
    constraint_rows: &[OracleLikeConstraintRow],
    index_rows: &[OracleLikeIndexRow],
    fk_rows: &[OracleLikeFkRow],
) -> ErEvidence {
    let mut evidence = ErEvidence::default();

    for row in type_rows {
        let Some(table) = tables
            .iter()
            .find(|table| table.name.eq_ignore_ascii_case(&row.table))
        else {
            // all_tab_cols / all_tab_columns 也包含视图列；视图不在基础表清单里。
            continue;
        };
        let signature = oracle_type_signature(
            &row.data_type,
            row.data_length,
            row.precision,
            row.scale,
            row.char_length,
            row.char_used.as_deref(),
            row.charset.as_deref(),
        );
        if signature.is_empty() {
            evidence.warn_table(
                table,
                format!(
                    "{dialect} 表 {owner}.{} 的列 {} 没有可比较的类型签名",
                    row.table, row.column
                ),
            );
            continue;
        }
        evidence.types.push(ErColumnType {
            table: TableId {
                schema: owner.to_string(),
                name: row.table.clone(),
            },
            column: row.column.clone(),
            signature,
        });
    }

    let mut candidates_by_table: BTreeMap<String, Vec<IndexCandidate>> = BTreeMap::new();

    #[derive(Default)]
    struct ConstraintGroup {
        primary: bool,
        status: String,
        validated: String,
        index_status: Option<String>,
        index_type: Option<String>,
        parts: Vec<(i64, String, Option<bool>)>,
    }
    let mut groups: BTreeMap<(String, String), ConstraintGroup> = BTreeMap::new();
    for row in constraint_rows {
        let group = groups
            .entry((row.table.clone(), row.constraint.clone()))
            .or_default();
        group.primary = row.kind.eq_ignore_ascii_case("P");
        group.status = row.status.clone();
        group.validated = row.validated.clone();
        group.index_status = row.index_status.clone();
        group.index_type = row.index_type.clone();
        group
            .parts
            .push((row.position, row.column.clone(), row.nullable));
    }
    for ((table, name), mut group) in groups {
        group.parts.sort_by_key(|(position, ..)| *position);
        let mut gap = Vec::new();
        if !group.status.eq_ignore_ascii_case("ENABLED") {
            gap.push(format!("约束状态为 {}", group.status));
        }
        if !group.validated.eq_ignore_ascii_case("VALIDATED") {
            gap.push(format!("约束未验证（{}）", group.validated));
        }
        match group.index_status.as_deref() {
            Some(status) if status.eq_ignore_ascii_case("VALID") => {}
            Some(status) => gap.push(format!("约束背后的索引状态为 {status}")),
            None => gap.push("约束背后的索引状态未知".to_string()),
        }
        match group.index_type.as_deref() {
            Some(index_type) if oracle_plain_index(index_type) => {}
            Some(index_type) => gap.push(format!("约束背后的索引类型 {index_type} 不是普通列索引")),
            None => gap.push("约束背后的索引类型未知".to_string()),
        }
        let parts = group
            .parts
            .iter()
            .map(|(_, column, nullable)| {
                if group.primary {
                    KeyPart::column(column.clone(), false)
                } else {
                    match nullable {
                        Some(nullable) => KeyPart::column(column.clone(), *nullable),
                        None => KeyPart::column_unknown_nullability(column.clone()),
                    }
                }
            })
            .collect();
        let mut candidate = IndexCandidate::new(name, group.primary, parts);
        if !gap.is_empty() {
            gap.sort();
            gap.dedup();
            candidate = candidate.unusable(gap.join("；"));
        }
        candidates_by_table
            .entry(table.to_ascii_lowercase())
            .or_default()
            .push(candidate);
    }

    struct IndexGroup {
        status: String,
        index_type: String,
        partitioned: String,
        parts: Vec<(i64, String, Option<bool>)>,
    }
    let mut index_groups = BTreeMap::new();
    for row in index_rows {
        let entry = index_groups
            .entry((row.table.clone(), row.index.clone()))
            .or_insert_with(|| IndexGroup {
                status: row.status.clone(),
                index_type: row.index_type.clone(),
                partitioned: row.partitioned.clone(),
                parts: Vec::new(),
            });
        entry
            .parts
            .push((row.position, row.column.clone(), row.nullable));
    }
    for (
        (table, name),
        IndexGroup {
            status,
            index_type,
            partitioned,
            mut parts,
        },
    ) in index_groups
    {
        parts.sort_by_key(|(position, ..)| *position);
        let mut gap = Vec::new();
        if !status.eq_ignore_ascii_case("VALID") {
            gap.push(format!("索引状态为 {status}，未生效"));
        }
        if !oracle_plain_index(&index_type) {
            gap.push(format!("索引类型 {index_type} 不是普通列索引"));
        }
        if partitioned.eq_ignore_ascii_case("YES") {
            gap.push("分区唯一索引不证明全表唯一".to_string());
        }
        let parts = parts
            .into_iter()
            .map(|(_, column, nullable)| match nullable {
                Some(nullable) => KeyPart::column(column, nullable),
                None => KeyPart::column_unknown_nullability(column),
            })
            .collect();
        let mut candidate = IndexCandidate::new(name, false, parts);
        if !gap.is_empty() {
            gap.sort();
            gap.dedup();
            candidate = candidate.unusable(gap.join("；"));
        }
        candidates_by_table
            .entry(table.to_ascii_lowercase())
            .or_default()
            .push(candidate);
    }

    for table in tables {
        let Some(candidates) = candidates_by_table.get(&table.name.to_ascii_lowercase()) else {
            continue;
        };
        let keys = collect_keys(dialect, table, candidates, true, &mut evidence);
        evidence.keys.extend(keys);
    }

    for fk in fk_rows {
        if !fk.status.eq_ignore_ascii_case("ENABLED")
            || !fk.validated.eq_ignore_ascii_case("VALIDATED")
        {
            let message = format!(
                "{dialect} 外键 {}.{} 状态为 {}/{}，仅作为声明关系，不证明引用完整性",
                fk.table, fk.constraint, fk.status, fk.validated
            );
            match tables
                .iter()
                .find(|table| table.name.eq_ignore_ascii_case(&fk.table))
            {
                Some(table) => evidence.warn_table(table, message),
                None => evidence.warn(message),
            }
        }
    }

    evidence
}

/// 把驱动证据对回真实表结构，得到最终 `ErCatalog`。
#[allow(clippy::needless_update)] // 保留 ..Default::default()，便于目录结构新增字段时协作合并
pub(crate) fn assemble(tables: Vec<TableInfo>, evidence: ErEvidence) -> ErCatalog {
    let ErEvidence {
        keys: evidence_keys,
        non_null_keys: evidence_non_null_keys,
        views: evidence_views,
        null_safe_views: evidence_null_safe_views,
        types: evidence_types,
        mut warnings,
        mut scoped_warnings,
    } = evidence;
    let lookup = Lookup::new(&tables);

    let mut keys = resolve_endpoints(
        &lookup,
        evidence_keys,
        "键",
        &mut warnings,
        &mut scoped_warnings,
    );
    keys.sort();
    keys.dedup();

    let mut non_null_keys = resolve_endpoints(
        &lookup,
        evidence_non_null_keys,
        "非空等值键",
        &mut warnings,
        &mut scoped_warnings,
    );
    non_null_keys.sort();
    non_null_keys.dedup();

    let mut all_keys = keys.clone();
    all_keys.extend(non_null_keys.iter().cloned());
    let views = resolve_views(
        &lookup,
        evidence_views,
        "普通等值视图",
        &all_keys,
        &mut warnings,
        &mut scoped_warnings,
    );
    let null_safe_views = resolve_views(
        &lookup,
        evidence_null_safe_views,
        "NULL 安全比较视图",
        &all_keys,
        &mut warnings,
        &mut scoped_warnings,
    );

    let mut types = Vec::new();
    for ty in evidence_types {
        let Some(table) = lookup.table(&ty.table) else {
            warnings.push(format!(
                "已忽略无法唯一定位的表 {} 的类型签名：{}",
                ty.table.label(),
                ty.column
            ));
            continue;
        };
        let column = match unique_column(table, &ty.column) {
            Ok(name) => name.to_string(),
            Err(gap) => {
                scoped_warnings.push(ScopedWarning {
                    table: TableId::from(table),
                    message: format!(
                        "已忽略表 {} 的类型签名（{}）：{gap}",
                        table.qualified_name(),
                        ty.column
                    ),
                });
                continue;
            }
        };
        let signature = ty.signature.trim().to_string();
        if signature.is_empty() {
            scoped_warnings.push(ScopedWarning {
                table: TableId::from(table),
                message: format!(
                    "表 {} 的列 {} 没有可比较的类型签名",
                    table.qualified_name(),
                    column
                ),
            });
            continue;
        }
        types.push(ErColumnType {
            table: TableId::from(table),
            column,
            signature,
        });
    }
    types.sort_by(|a, b| {
        (&a.table, &a.column, &a.signature).cmp(&(&b.table, &b.column, &b.signature))
    });
    let mut signatures: BTreeMap<(TableId, String), BTreeSet<String>> = BTreeMap::new();
    for ty in &types {
        signatures
            .entry((ty.table.clone(), ty.column.clone()))
            .or_default()
            .insert(ty.signature.clone());
    }
    for ((table, column), signatures) in signatures {
        if signatures.len() > 1 {
            scoped_warnings.push(ScopedWarning {
                table: table.clone(),
                message: format!(
                    "表 {} 的列 {} 有多个不一致的类型签名（{}），推断时按未知处理",
                    table.label(),
                    column,
                    signatures.into_iter().collect::<Vec<_>>().join(" / ")
                ),
            });
        }
    }
    types.dedup_by(|a, b| a.table == b.table && a.column == b.column && a.signature == b.signature);

    dedup_warnings(&mut warnings);
    dedup_scoped_warnings(&mut scoped_warnings);
    if !tables.is_empty() && keys.is_empty() {
        if non_null_keys.is_empty() {
            warnings.push(
                "没有可证明全表有效的唯一键：关系推断不会自动连线到未证明的目标键".to_string(),
            );
        } else {
            warnings.push(
                "此目录仅识别到可空唯一键，相关推断只在完整键位非空且按普通等号比较时适用".into(),
            );
        }
    }

    ErCatalog {
        tables,
        keys,
        non_null_keys,
        types,
        views,
        null_safe_views,
        document_links: Default::default(),
        guarded_keys: Vec::new(),
        db_type: None,
        warnings,
        scoped_warnings,
        ..Default::default()
    }
}

/// 把驱动返回的原始表/列名对回目录里的实际名称；歧义或缺失一律丢弃并留提示。
fn resolve_endpoints(
    lookup: &Lookup,
    endpoints: Vec<Endpoint>,
    kind: &str,
    warnings: &mut Vec<String>,
    scoped_warnings: &mut Vec<ScopedWarning>,
) -> Vec<Endpoint> {
    let mut resolved = Vec::new();
    for key in endpoints {
        let Some(table) = lookup.table(&key.table) else {
            warnings.push(format!(
                "已忽略无法唯一定位的表 {} 上的{kind}（表不存在或存在大小写不同的同名表）",
                key.table.label()
            ));
            continue;
        };
        let mut columns = Vec::with_capacity(key.columns.len());
        let mut missing = None;
        for column in &key.columns {
            match unique_column(table, column) {
                Ok(name) => columns.push(name.to_string()),
                Err(gap) => {
                    missing = Some(gap);
                    break;
                }
            }
        }
        if let Some(gap) = missing {
            scoped_warnings.push(ScopedWarning {
                table: TableId::from(table),
                message: format!(
                    "已忽略表 {} 上的{kind}（{}）：{gap}",
                    table.qualified_name(),
                    key.columns.join(", ")
                ),
            });
            continue;
        }
        resolved.push(Endpoint {
            table: TableId::from(table),
            columns,
        });
    }
    resolved
}

/// 视图端点必须能唯一定位并还原为真实列；列对按最终键位序重排。
fn resolve_views(
    lookup: &Lookup,
    comparisons: Vec<ViewComparison>,
    kind: &str,
    keys: &[Endpoint],
    warnings: &mut Vec<String>,
    scoped_warnings: &mut Vec<ScopedWarning>,
) -> Vec<ViewComparison> {
    let mut resolved = Vec::new();
    for comparison in comparisons {
        if comparison.view.name.trim().is_empty() {
            continue;
        }
        let Some(left) = resolve_endpoints(
            lookup,
            vec![comparison.left],
            kind,
            warnings,
            scoped_warnings,
        )
        .pop() else {
            continue;
        };
        let Some(right) = resolve_endpoints(
            lookup,
            vec![comparison.right],
            kind,
            warnings,
            scoped_warnings,
        )
        .pop() else {
            continue;
        };
        if left.table == right.table {
            warnings.push(format!(
                "已忽略视图 {} 的{kind}：比较两端指向同一张表",
                comparison.view.label()
            ));
            continue;
        }
        let (left, right) = align_view_columns(left, right, keys);
        resolved.push(ViewComparison {
            view: comparison.view,
            left,
            right,
        });
    }
    resolved
}

/// SQL 的 AND 项可交换，但视图证据仍须与目标键的列顺序一致。
fn align_view_columns(
    mut left: Endpoint,
    mut right: Endpoint,
    keys: &[Endpoint],
) -> (Endpoint, Endpoint) {
    for key in keys {
        let names = if key.table == left.table {
            &left.columns
        } else if key.table == right.table {
            &right.columns
        } else {
            continue;
        };
        if names.len() != key.columns.len() {
            continue;
        }
        let order: Option<Vec<_>> = key
            .columns
            .iter()
            .map(|column| names.iter().position(|name| name == column))
            .collect();
        if let Some(order) = order {
            left.columns = order.iter().map(|&i| left.columns[i].clone()).collect();
            right.columns = order.iter().map(|&i| right.columns[i].clone()).collect();
            break;
        }
    }
    (left, right)
}

/// 补充目录读取失败时的降级装配：保留已经读到的表结构，键和类型留空，
/// 只留下具体失败原因，让调用方仍能展示表并保守待判断。
pub(crate) fn assemble_read(tables: Vec<TableInfo>, evidence: Result<ErEvidence>) -> ErCatalog {
    match evidence {
        Ok(evidence) => assemble(tables, evidence),
        Err(err) => ErCatalog {
            tables,
            warnings: vec![format!("补充目录读取失败，仅保留表结构：{err}")],
            ..Default::default()
        },
    }
}

/// Oracle / 达梦的普通列索引类型。`IOT - TOP` 是索引组织表的顶层索引，
/// 仍是普通列上的 B 树键，不是函数或表达式索引。
fn oracle_plain_index(index_type: &str) -> bool {
    matches!(
        index_type.trim().to_ascii_uppercase().as_str(),
        "NORMAL" | "IOT - TOP"
    )
}

/// Oracle / 达梦这类共享数据字典方言的类型签名。
///
/// 展示用的 `data_type` 和长度不足以说明可比性：字符语义（CHAR/BYTE）、
/// 字符集、`NUMBER` 精度和 `TIMESTAMP` 小数秒都必须进入签名。
#[allow(clippy::too_many_arguments)]
pub(crate) fn oracle_type_signature(
    data_type: &str,
    data_length: Option<i64>,
    precision: Option<i64>,
    scale: Option<i64>,
    char_length: Option<i64>,
    char_used: Option<&str>,
    charset: Option<&str>,
) -> String {
    let declared = data_type.trim().to_uppercase();
    if declared.is_empty() {
        return String::new();
    }
    let base = declared
        .split('(')
        .next()
        .unwrap_or(&declared)
        .trim()
        .to_string();

    let mut signature = match base.as_str() {
        "VARCHAR2" | "VARCHAR" | "CHAR" => {
            let used = char_used.map(|v| v.trim().to_uppercase());
            let (length, unit) = match used.as_deref() {
                Some("C") => (char_length.or(data_length), " CHAR"),
                Some("B") => (data_length, " BYTE"),
                _ => (data_length.or(char_length), ""),
            };
            match length {
                Some(length) => format!("{base}({length}{unit})"),
                None => base.clone(),
            }
        }
        "NVARCHAR2" | "NCHAR" => match char_length.or(data_length) {
            Some(length) => format!("{base}({length})"),
            None => base.clone(),
        },
        "RAW" | "BINARY" | "VARBINARY" | "BIT" => match data_length {
            Some(length) => format!("{base}({length})"),
            None => base.clone(),
        },
        "NUMBER" | "DECIMAL" | "NUMERIC" => match (precision, scale) {
            (Some(precision), Some(scale)) => format!("{base}({precision},{scale})"),
            (Some(precision), None) => format!("{base}({precision})"),
            _ => declared.clone(),
        },
        "FLOAT" => match precision {
            Some(precision) => format!("FLOAT({precision})"),
            None => declared.clone(),
        },
        _ if declared.starts_with("TIMESTAMP") => {
            if declared.contains('(') {
                declared.clone()
            } else {
                match scale {
                    Some(scale) => format!("{declared}({scale})"),
                    None => declared.clone(),
                }
            }
        }
        _ => declared.clone(),
    };

    let character_type = matches!(
        base.as_str(),
        "VARCHAR2" | "VARCHAR" | "CHAR" | "NVARCHAR2" | "NCHAR" | "CLOB" | "NCLOB" | "TEXT"
    );
    if character_type {
        if let Some(charset) = charset.map(str::trim).filter(|v| !v.is_empty()) {
            signature.push_str(" CHARACTER SET ");
            signature.push_str(charset);
        }
    }
    signature
}

struct Lookup<'a> {
    by_name: BTreeMap<(String, String), Option<&'a TableInfo>>,
}

impl<'a> Lookup<'a> {
    fn new(tables: &'a [TableInfo]) -> Self {
        let mut by_name: BTreeMap<(String, String), Option<&'a TableInfo>> = BTreeMap::new();
        for table in tables {
            let key = (
                table.schema.to_ascii_lowercase(),
                table.name.to_ascii_lowercase(),
            );
            by_name
                .entry(key)
                .and_modify(|slot| *slot = None)
                .or_insert(Some(table));
        }
        Self { by_name }
    }

    fn table(&self, id: &TableId) -> Option<&'a TableInfo> {
        self.by_name
            .get(&(id.schema.to_ascii_lowercase(), id.name.to_ascii_lowercase()))
            .copied()
            .flatten()
    }
}

/// 在表结构里找到唯一的真实列名；大小写不同的重名列视为歧义。
fn unique_column<'a>(table: &'a TableInfo, column: &str) -> Result<&'a str, String> {
    let matches: Vec<_> = table
        .columns
        .iter()
        .filter(|c| c.name.eq_ignore_ascii_case(column))
        .map(|c| c.name.as_str())
        .collect();
    match matches.as_slice() {
        [] => Err(format!(
            "列 {column} 不在表 {} 的结构中",
            table.qualified_name()
        )),
        [name] => Ok(name),
        _ => Err(format!(
            "列 {column} 在表 {} 中有多个大小写不同的同名对象",
            table.qualified_name()
        )),
    }
}

fn dedup_warnings(warnings: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    warnings.retain(|warning| seen.insert(warning.clone()));
}

fn dedup_scoped_warnings(warnings: &mut Vec<ScopedWarning>) {
    let mut seen = BTreeSet::new();
    warnings.retain(|warning| seen.insert((warning.table.clone(), warning.message.clone())));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_supplementary_catalog_keeps_tables_and_marks_evidence_unavailable() {
        let tables = vec![TableInfo {
            name: "t".into(),
            ..Default::default()
        }];
        let catalog = assemble_read(tables, Err(anyhow::anyhow!("目录访问受限")));
        assert_eq!(catalog.tables[0].name, "t");
        assert!(catalog.keys.is_empty() && catalog.types.is_empty());
        assert_eq!(catalog.warnings.len(), 1);
    }
    use dbs_core::ColumnInfo;

    fn column(name: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            order: 1,
            name: name.into(),
            data_type: "x".into(),
            length: String::new(),
            is_primary_key: false,
            is_auto_increment: false,
            nullable,
            default_value: String::new(),
            comment: String::new(),
        }
    }

    fn table(name: &str, columns: Vec<ColumnInfo>) -> TableInfo {
        TableInfo {
            order: 1,
            schema: "app".into(),
            name: name.into(),
            columns,
            ..Default::default()
        }
    }

    fn scoped_messages(evidence: &ErEvidence) -> Vec<&str> {
        evidence
            .scoped_warnings
            .iter()
            .map(|warning| warning.message.as_str())
            .collect()
    }

    #[test]
    fn keeps_composite_key_in_index_order_and_rejects_every_unproven_index() {
        let items = table(
            "items",
            vec![
                column("tenant_id", false),
                column("code", false),
                column("optional", true),
            ],
        );
        let candidates = vec![
            IndexCandidate::new(
                "uk_items",
                false,
                vec![
                    KeyPart::column("tenant_id", false),
                    KeyPart::column("code", false),
                ],
            ),
            IndexCandidate::new("uk_partial", false, vec![KeyPart::column("code", false)])
                .unusable("部分索引只覆盖满足条件的行"),
            IndexCandidate::new("uk_prefix", false, vec![KeyPart::prefix("code", false)]),
            IndexCandidate::new(
                "uk_nullable",
                false,
                vec![KeyPart::column("optional", true)],
            ),
            IndexCandidate::new(
                "uk_expression",
                false,
                vec![KeyPart::gap("键位是表达式，不是普通列")],
            ),
        ];
        let mut evidence = ErEvidence::default();
        let keys = collect_keys("MySQL", &items, &candidates, true, &mut evidence);

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].columns, ["tenant_id", "code"]);
        assert_eq!(keys[0].table, TableId::from(&items));
        let warnings = scoped_messages(&evidence);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(evidence.warnings.is_empty(), "{:?}", evidence.warnings);
        assert!(
            evidence
                .scoped_warnings
                .iter()
                .all(|warning| warning.table == TableId::from(&items)),
            "{:?}",
            evidence.scoped_warnings
        );
        for expected in [
            "部分索引只覆盖满足条件的行",
            "前缀索引",
            "可为 NULL",
            "表达式",
        ] {
            assert!(
                warnings.iter().any(|warning| warning.contains(expected)),
                "缺少警告 {expected}：{warnings:?}"
            );
        }
    }

    #[test]
    fn primary_key_does_not_need_nullability_and_sqlserver_like_unique_does_not_block_on_null() {
        let items = table("items", vec![column("id", false), column("legacy", true)]);
        let primary = IndexCandidate::new("pk_items", true, vec![KeyPart::column("id", false)]);
        let unique = IndexCandidate::new(
            "uk_legacy",
            false,
            vec![KeyPart::column_unknown_nullability("legacy")],
        );
        let mut evidence = ErEvidence::default();
        let keys = collect_keys(
            "SQL Server",
            &items,
            &[primary, unique],
            false,
            &mut evidence,
        );
        assert_eq!(keys.len(), 2, "{:?}", evidence.scoped_warnings);
        assert!(evidence.warnings.is_empty(), "{:?}", evidence.warnings);
        assert!(
            evidence.scoped_warnings.is_empty(),
            "{:?}",
            evidence.scoped_warnings
        );
    }

    #[test]
    fn unknown_nullability_is_a_gap_for_dialects_that_allow_multiple_nulls() {
        let items = table("items", vec![column("legacy", true)]);
        let unique = IndexCandidate::new(
            "uk_legacy",
            false,
            vec![KeyPart::column_unknown_nullability("legacy")],
        );
        let mut evidence = ErEvidence::default();
        let keys = collect_keys("PostgreSQL", &items, &[unique], true, &mut evidence);
        assert!(keys.is_empty());
        assert!(
            scoped_messages(&evidence)
                .iter()
                .any(|warning| warning.contains("可空性未知")),
            "{:?}",
            evidence.scoped_warnings
        );
    }

    #[test]
    fn duplicate_key_column_is_rejected() {
        let items = table("items", vec![column("id", false)]);
        let candidate = IndexCandidate::new(
            "uk_dup",
            false,
            vec![KeyPart::column("id", false), KeyPart::column("ID", false)],
        );
        let mut evidence = ErEvidence::default();
        let keys = collect_keys("MySQL", &items, &[candidate], true, &mut evidence);
        assert!(keys.is_empty());
        assert!(
            scoped_messages(&evidence)
                .iter()
                .any(|warning| warning.contains("重复")),
            "{:?}",
            evidence.scoped_warnings
        );
    }

    #[test]
    fn key_column_missing_from_table_structure_is_rejected() {
        let items = table("items", vec![column("id", false)]);
        let candidate =
            IndexCandidate::new("uk_missing", false, vec![KeyPart::column("missing", false)]);
        let mut evidence = ErEvidence::default();
        let keys = collect_keys("MySQL", &items, &[candidate], true, &mut evidence);
        assert!(keys.is_empty());
        assert!(
            scoped_messages(&evidence)
                .iter()
                .any(|warning| warning.contains("不在表")),
            "{:?}",
            evidence.scoped_warnings
        );
    }

    #[test]
    fn assembly_rewrites_actual_names_and_drops_unknown_objects_with_specific_warnings() {
        let items = table("items", vec![column("id", false), column("code", false)]);
        let evidence = ErEvidence {
            keys: vec![
                Endpoint {
                    table: TableId {
                        schema: "APP".into(),
                        name: "ITEMS".into(),
                    },
                    columns: vec!["ID".into()],
                },
                Endpoint {
                    table: TableId {
                        schema: "app".into(),
                        name: "missing".into(),
                    },
                    columns: vec!["id".into()],
                },
            ],
            types: vec![
                ErColumnType {
                    table: TableId {
                        schema: "APP".into(),
                        name: "ITEMS".into(),
                    },
                    column: "CODE".into(),
                    signature: "varchar(8) collate utf8mb4_bin".into(),
                },
                ErColumnType {
                    table: TableId {
                        schema: "app".into(),
                        name: "items".into(),
                    },
                    column: "missing".into(),
                    signature: "int".into(),
                },
            ],
            warnings: vec!["驱动警告".into()],
            scoped_warnings: Vec::new(),
            ..Default::default()
        };
        let catalog = assemble(vec![items], evidence);
        assert_eq!(catalog.keys.len(), 1);
        assert_eq!(catalog.keys[0].columns, ["id"]);
        assert_eq!(catalog.types.len(), 1);
        assert_eq!(catalog.types[0].column, "code");
        assert_eq!(catalog.types[0].signature, "varchar(8) collate utf8mb4_bin");
        assert!(catalog.warnings.iter().any(|w| w.contains("无法唯一定位")));
        assert!(catalog.scoped_warnings.iter().any(|w| {
            w.table.schema == "app" && w.table.name == "items" && w.message.contains("missing")
        }));
    }

    #[test]
    fn case_ambiguous_tables_and_conflicting_signatures_are_not_trusted() {
        let lower = table("items", vec![column("id", false)]);
        let upper = table("ITEMS", vec![column("id", false)]);
        let codes = table("codes", vec![column("code", false)]);
        let evidence = ErEvidence {
            keys: vec![Endpoint {
                table: TableId {
                    schema: "app".into(),
                    name: "Items".into(),
                },
                columns: vec!["id".into()],
            }],
            types: vec![
                ErColumnType {
                    table: TableId {
                        schema: "app".into(),
                        name: "codes".into(),
                    },
                    column: "code".into(),
                    signature: "integer".into(),
                },
                ErColumnType {
                    table: TableId {
                        schema: "app".into(),
                        name: "codes".into(),
                    },
                    column: "code".into(),
                    signature: "bigint".into(),
                },
            ],
            warnings: Vec::new(),
            scoped_warnings: Vec::new(),
            ..Default::default()
        };
        let catalog = assemble(vec![lower, upper, codes], evidence);
        assert!(catalog.keys.is_empty());
        assert!(catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("大小写不同的同名表")));
        assert!(catalog
            .scoped_warnings
            .iter()
            .any(|warning| warning.table.name == "codes"
                && warning.message.contains("多个不一致的类型签名")));
    }

    #[test]
    fn oracle_signature_distinguishes_char_semantics_charset_and_number_precision() {
        assert_eq!(
            oracle_type_signature(
                "VARCHAR2",
                Some(200),
                None,
                None,
                Some(50),
                Some("C"),
                Some("AL32UTF8")
            ),
            "VARCHAR2(50 CHAR) CHARACTER SET AL32UTF8"
        );
        assert_eq!(
            oracle_type_signature(
                "VARCHAR2",
                Some(50),
                None,
                None,
                Some(50),
                Some("B"),
                Some("AL32UTF8")
            ),
            "VARCHAR2(50 BYTE) CHARACTER SET AL32UTF8"
        );
        assert_eq!(
            oracle_type_signature("NUMBER", None, Some(10), Some(2), None, None, None),
            "NUMBER(10,2)"
        );
        assert_eq!(
            oracle_type_signature("TIMESTAMP", None, None, Some(6), None, None, None),
            "TIMESTAMP(6)"
        );
        assert_eq!(
            oracle_type_signature("NVARCHAR2", Some(200), None, None, Some(100), None, None),
            "NVARCHAR2(100)"
        );
    }
}
