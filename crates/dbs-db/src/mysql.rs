//! MySQL / MariaDB 元数据读取。
//!
//! 直接查 `information_schema`，一次拿全库表清单与列信息，避免 N+1 查询。

use anyhow::{Context, Result};
use dbs_core::er::{ErCatalog, ErColumnType, TableId};
use dbs_core::{ColumnInfo, ForeignKey, IndexInfo, TableInfo};
use mysql_async::prelude::*;
use mysql_async::{OptsBuilder, Pool, Row};

use crate::mysql_views;
use crate::options::ConnectionOptions;
use crate::remote_er::{self, ErEvidence, IndexCandidate, KeyPart};

const TABLE_SQL: &str = r#"
SELECT TABLE_NAME, TABLE_COMMENT
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE'
ORDER BY TABLE_NAME
"#;

/// 一次取回全部列信息，按 TABLE_NAME、ORDINAL_POSITION 排序。
const COLUMN_SQL: &str = r#"
SELECT TABLE_NAME, ORDINAL_POSITION, COLUMN_NAME, DATA_TYPE, COLUMN_TYPE,
       COLUMN_KEY, EXTRA, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_COMMENT
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = ?
ORDER BY TABLE_NAME, ORDINAL_POSITION
"#;

/// 系统库，列出可导出库时排除。
const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

const SCHEMA_SQL: &str = r#"
SELECT SCHEMA_NAME
FROM information_schema.SCHEMATA
ORDER BY SCHEMA_NAME
"#;

/// 构造连接池；`database` 为空时只连服务器，用于列出库清单。
fn build_pool(opts: &ConnectionOptions, database: Option<&str>) -> Pool {
    let mut builder = OptsBuilder::default()
        .ip_or_hostname(opts.host.clone())
        .tcp_port(opts.port)
        .user(Some(opts.user.clone()))
        .pass(Some(opts.password.clone()))
        .init(vec!["SET NAMES utf8mb4".to_string()]);
    if let Some(database) = database {
        builder = builder.db_name(Some(database.to_string()));
    }
    Pool::new(builder)
}

/// 列出可导出的数据库（排除系统库）。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    let pool = build_pool(opts, None);
    let result = list_databases_with_pool(&pool).await;
    pool.disconnect().await.ok();
    result
}

async fn list_databases_with_pool(pool: &Pool) -> Result<Vec<String>> {
    let mut conn = pool
        .get_conn()
        .await
        .context("连接 MySQL 失败（请检查地址、端口、账号密码）")?;
    let rows: Vec<Row> = conn.exec(SCHEMA_SQL, ()).await.context("查询库清单失败")?;
    let names: Vec<String> = rows
        .iter()
        .map(|row| get_string(row, 0))
        .filter(|name| !name.is_empty() && !SYSTEM_SCHEMAS.contains(&name.as_str()))
        .collect();
    Ok(names)
}

/// 单表列查询（用于界面预览）。
const TABLE_COLUMN_SQL: &str = r#"
SELECT ORDINAL_POSITION, COLUMN_NAME, DATA_TYPE, COLUMN_TYPE,
       COLUMN_KEY, EXTRA, IS_NULLABLE, COLUMN_DEFAULT, COLUMN_COMMENT
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
ORDER BY ORDINAL_POSITION
"#;

/// 读取单张表的列信息（界面预览用，避免整库重查）。
pub async fn load_table_columns(
    opts: &ConnectionOptions,
    table: &str,
) -> Result<Vec<dbs_core::ColumnInfo>> {
    let pool = build_pool(opts, Some(opts.database.as_str()));
    let result = async {
        let mut conn = pool
            .get_conn()
            .await
            .context("连接 MySQL 失败（请检查地址、端口、账号密码）")?;
        let rows: Vec<Row> = conn
            .exec(TABLE_COLUMN_SQL, (opts.database.as_str(), table))
            .await
            .context("查询列信息失败")?;
        Ok(rows.iter().map(|row| map_column(row, 0)).collect())
    }
    .await;
    pool.disconnect().await.ok();
    result
}

/// 从一行查询结果映射列信息；`offset` 为第一个列字段的下标。
fn map_column(row: &Row, offset: usize) -> dbs_core::ColumnInfo {
    dbs_core::ColumnInfo {
        order: get_u32(row, offset),
        name: get_string(row, offset + 1),
        data_type: get_string(row, offset + 2).to_lowercase(),
        length: extract_length(&get_string(row, offset + 3)),
        is_primary_key: get_string(row, offset + 4).eq_ignore_ascii_case("PRI"),
        is_auto_increment: get_string(row, offset + 5)
            .to_lowercase()
            .contains("auto_increment"),
        nullable: get_string(row, offset + 6).eq_ignore_ascii_case("YES"),
        default_value: get_opt_string(row, offset + 7).unwrap_or_default(),
        comment: get_string(row, offset + 8),
    }
}

/// 索引信息：一个索引可能有多列，按 SEQ_IN_INDEX 排序后在 Rust 侧分组。
const INDEX_SQL: &str = r#"
SELECT TABLE_NAME, INDEX_NAME, NON_UNIQUE, COLUMN_NAME
FROM information_schema.STATISTICS
WHERE TABLE_SCHEMA = ?
ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX
"#;

/// 外键信息：组合同一个约束的多列。
const FK_SQL: &str = r#"
SELECT TABLE_NAME, CONSTRAINT_NAME, COLUMN_NAME,
       REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME
FROM information_schema.KEY_COLUMN_USAGE
WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL
ORDER BY TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION
"#;

/// 分区表清单。
const PARTITION_SQL: &str = r#"
SELECT DISTINCT TABLE_NAME
FROM information_schema.PARTITIONS
WHERE TABLE_SCHEMA = ? AND PARTITION_NAME IS NOT NULL
"#;

/// 读取指定库的全部基础表（不含视图）及其列信息。
pub async fn load_tables(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    let pool = build_pool(opts, Some(opts.database.as_str()));
    let result = load_with_pool(&pool, opts.database.as_str()).await;
    pool.disconnect().await.ok();
    result
}

async fn load_with_pool(pool: &Pool, schema: &str) -> Result<Vec<TableInfo>> {
    let mut conn = pool
        .get_conn()
        .await
        .context("连接 MySQL 失败（请检查地址、端口、账号密码）")?;

    let table_rows: Vec<Row> = conn
        .exec(TABLE_SQL, (schema,))
        .await
        .context("查询表清单失败")?;

    let column_rows: Vec<Row> = conn
        .exec(COLUMN_SQL, (schema,))
        .await
        .context("查询列信息失败")?;

    // 表名 -> 列集合
    let mut columns_by_table: std::collections::HashMap<String, Vec<ColumnInfo>> =
        std::collections::HashMap::new();

    for row in column_rows {
        let table_name = get_string(&row, 0);
        let column = map_column(&row, 1);
        columns_by_table.entry(table_name).or_default().push(column);
    }

    // 索引：表名 -> 索引列表（同名索引的多列合并）
    let index_rows: Vec<Row> = conn
        .exec(INDEX_SQL, (schema,))
        .await
        .context("查询索引信息失败")?;
    let mut indexes_by_table: std::collections::HashMap<String, Vec<IndexInfo>> =
        std::collections::HashMap::new();
    for row in index_rows {
        let table_name = get_string(&row, 0);
        let index_name = get_string(&row, 1);
        let non_unique = get_u32(&row, 2);
        let column_name = get_string(&row, 3);

        let list = indexes_by_table.entry(table_name).or_default();
        match list.iter_mut().find(|i| i.name == index_name) {
            Some(existing) => existing.columns.push(column_name),
            None => list.push(IndexInfo {
                is_primary: index_name == "PRIMARY",
                name: index_name,
                columns: vec![column_name],
                is_unique: non_unique == 0,
            }),
        }
    }

    // 外键：表名 -> 外键列表（同一约束的多列合并）
    let fk_rows: Vec<Row> = conn
        .exec(FK_SQL, (schema,))
        .await
        .context("查询外键信息失败")?;
    let mut fks_by_table: std::collections::HashMap<String, Vec<ForeignKey>> =
        std::collections::HashMap::new();
    for row in fk_rows {
        let table_name = get_string(&row, 0);
        let constraint = get_string(&row, 1);
        let column_name = get_string(&row, 2);
        let ref_schema = get_string(&row, 3);
        let ref_table = get_string(&row, 4);
        let ref_column = get_string(&row, 5);

        let list = fks_by_table.entry(table_name).or_default();
        match list.iter_mut().find(|f| f.name == constraint) {
            Some(existing) => {
                existing.columns.push(column_name);
                existing.ref_columns.push(ref_column);
            }
            None => list.push(ForeignKey {
                name: constraint,
                columns: vec![column_name],
                ref_schema,
                ref_table,
                ref_columns: vec![ref_column],
            }),
        }
    }

    // 分区表
    let partition_rows: Vec<Row> = conn
        .exec(PARTITION_SQL, (schema,))
        .await
        .context("查询分区信息失败")?;
    let partitioned: std::collections::HashSet<String> = partition_rows
        .iter()
        .map(|row| get_string(row, 0))
        .collect();

    let mut tables = Vec::with_capacity(table_rows.len());
    for (idx, row) in table_rows.into_iter().enumerate() {
        let name = get_string(&row, 0);
        let comment = get_string(&row, 1);
        let columns = columns_by_table.remove(&name).unwrap_or_default();
        tables.push(TableInfo {
            order: (idx + 1) as u32,
            schema: schema.to_string(),
            indexes: indexes_by_table.remove(&name).unwrap_or_default(),
            foreign_keys: fks_by_table.remove(&name).unwrap_or_default(),
            is_partitioned: partitioned.contains(&name),
            note: String::new(),
            name,
            comment,
            columns,
        });
    }

    tracing::info!(
        "已读取 {} 张表、{} 个列定义",
        tables.len(),
        tables.iter().map(|t| t.columns.len()).sum::<usize>()
    );
    Ok(tables)
}

/// ER 类型签名：`COLUMN_TYPE` 保留长度、精度、ENUM/SET 值和 unsigned，
/// 字符列再追加排序规则。展示用的 `DATA_TYPE` 不含这些属性，不能直接当签名。
const ER_TYPE_SQL: &str = r#"
SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, COALESCE(COLLATION_NAME, '')
FROM information_schema.COLUMNS
WHERE TABLE_SCHEMA = ?
ORDER BY TABLE_NAME, ORDINAL_POSITION
"#;

/// ER 唯一索引：只取全库唯一索引的键位。`COLUMN_NAME` 为空表示表达式键位；
/// `SUB_PART` 非空表示前缀索引；`NULLABLE` 用于判断 NULL 是否可重复。
const ER_INDEX_SQL: &str = r#"
SELECT TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX, COLUMN_NAME, SUB_PART,
       NULLABLE, INDEX_TYPE
FROM information_schema.STATISTICS
WHERE TABLE_SCHEMA = ? AND NON_UNIQUE = 0
ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX
"#;

/// ER 视图定义：只读 `information_schema.VIEWS`，不执行视图查询。
/// `VIEW_DEFINITION` 可能因权限为空；读取失败时在驱动层降级为警告。
const ER_VIEW_SQL: &str = r#"
SELECT TABLE_NAME, COALESCE(VIEW_DEFINITION, '')
FROM information_schema.VIEWS
WHERE TABLE_SCHEMA = ?
ORDER BY TABLE_NAME
"#;

/// 驱动原始行，与 SQL 查询解耦后便于在无实库时验证键位规则。
#[derive(Debug, Clone)]
struct MysqlTypeRow {
    table: String,
    column: String,
    column_type: String,
    collation: String,
}

#[derive(Debug, Clone)]
struct MysqlIndexRow {
    table: String,
    index: String,
    seq: u32,
    column: Option<String>,
    sub_part: Option<u32>,
    /// `None` 表示 `information_schema.STATISTICS.NULLABLE` 没有给出可空性。
    nullable: Option<bool>,
    index_type: String,
}

#[derive(Debug, Clone)]
struct MysqlViewRow {
    name: String,
    definition: String,
}

/// 读取 ER 专用证据：完整类型签名与可证明全表唯一的键。
pub(crate) async fn load_er_catalog(opts: &ConnectionOptions) -> Result<ErCatalog> {
    let tables = load_tables(opts).await?;
    let evidence = read_er_evidence(opts, &tables).await;
    Ok(remote_er::assemble_read(tables, evidence))
}

async fn read_er_evidence(opts: &ConnectionOptions, tables: &[TableInfo]) -> Result<ErEvidence> {
    let pool = build_pool(opts, Some(opts.database.as_str()));
    let result = async {
        let mut conn = pool
            .get_conn()
            .await
            .context("连接 MySQL 失败（请检查地址、端口、账号密码）")?;
        let schema = opts.database.as_str();

        let type_rows: Vec<Row> = conn
            .exec(ER_TYPE_SQL, (schema,))
            .await
            .context("查询 ER 类型信息失败")?;
        let type_rows: Vec<MysqlTypeRow> = type_rows
            .iter()
            .map(|row| MysqlTypeRow {
                table: get_string(row, 0),
                column: get_string(row, 1),
                column_type: get_string(row, 2),
                collation: get_string(row, 3),
            })
            .collect();

        let index_rows: Vec<Row> = conn
            .exec(ER_INDEX_SQL, (schema,))
            .await
            .context("查询 ER 唯一索引失败")?;
        let index_rows: Vec<MysqlIndexRow> = index_rows
            .iter()
            .map(|row| MysqlIndexRow {
                table: get_string(row, 0),
                index: get_string(row, 1),
                seq: get_u32(row, 2),
                column: get_opt_string(row, 3).filter(|name| !name.is_empty()),
                sub_part: row.get_opt::<u32, _>(4).and_then(|value| value.ok()),
                // STATISTICS.NULLABLE 将 NOT NULL 记作空串。SQL NULL 或未知标记仍表示未知。
                nullable: match get_opt_string(row, 5).as_deref() {
                    Some("YES") => Some(true),
                    Some("") | Some("NO") => Some(false),
                    _ => None,
                },
                index_type: get_string(row, 6),
            })
            .collect();

        let view_rows: Result<Vec<MysqlViewRow>> = match conn.exec(ER_VIEW_SQL, (schema,)).await {
            Ok(rows) => Ok(rows
                .iter()
                .map(|row| MysqlViewRow {
                    name: get_string(row, 0),
                    definition: get_string(row, 1),
                })
                .collect()),
            Err(err) => Err(anyhow::Error::new(err).context("查询 ER 视图定义失败")),
        };

        let mut evidence = evidence_from_rows(schema, tables, &type_rows, &index_rows);
        // 视图权限缺失或读取失败只跳过视图证据，不影响已经读到的索引与类型。
        attach_views(schema, tables, view_rows, &mut evidence);
        Ok(evidence)
    }
    .await;
    pool.disconnect().await.ok();
    result
}

/// 把 MySQL 目录行装配成 ER 证据。此函数不连接数据库，便于直接验证规则。
fn evidence_from_rows(
    schema: &str,
    tables: &[TableInfo],
    type_rows: &[MysqlTypeRow],
    index_rows: &[MysqlIndexRow],
) -> ErEvidence {
    let mut evidence = ErEvidence::default();
    let table_names: std::collections::BTreeSet<String> = tables
        .iter()
        .map(|table| table.name.to_ascii_lowercase())
        .collect();

    for row in type_rows {
        if !table_names.contains(&row.table.to_ascii_lowercase()) {
            // information_schema.COLUMNS 也包含视图列；视图不在基础表清单里。
            continue;
        }
        let signature = mysql_type_signature(&row.column_type, &row.collation);
        if signature.is_empty() {
            if let Some(table) = tables
                .iter()
                .find(|table| table.name.eq_ignore_ascii_case(&row.table))
            {
                evidence.warn_table(
                    table,
                    format!(
                        "MySQL 表 {schema}.{} 的列 {} 没有可比较的类型签名",
                        row.table, row.column
                    ),
                );
            }
            continue;
        }
        evidence.types.push(ErColumnType {
            table: TableId {
                schema: schema.to_string(),
                name: row.table.clone(),
            },
            column: row.column.clone(),
            signature,
        });
    }

    // table -> index -> rows，最后按 SEQ_IN_INDEX 还原键位顺序。
    let mut grouped: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, Vec<&MysqlIndexRow>>,
    > = std::collections::BTreeMap::new();
    for row in index_rows {
        grouped
            .entry(row.table.clone())
            .or_default()
            .entry(row.index.clone())
            .or_default()
            .push(row);
    }

    for table in tables {
        let Some(indexes) = grouped.get(&table.name) else {
            continue;
        };
        let mut candidates = Vec::with_capacity(indexes.len());
        for (name, rows) in indexes {
            let mut rows = rows.to_vec();
            rows.sort_by_key(|row| row.seq);
            let primary = name.eq_ignore_ascii_case("PRIMARY");
            let mut candidate = IndexCandidate::new(name.clone(), primary, Vec::new());
            let mut gap = Vec::new();
            if rows
                .iter()
                .enumerate()
                .any(|(index, row)| row.seq as usize != index + 1)
            {
                candidate
                    .parts
                    .push(KeyPart::gap("键位序号不连续，无法确认完整索引"));
            }
            for row in rows {
                match row.column.as_deref() {
                    Some(column) if !column.is_empty() => {
                        let part = match (row.sub_part.is_some(), row.nullable) {
                            (false, Some(nullable)) => KeyPart::column(column, nullable),
                            (false, None) => KeyPart::column_unknown_nullability(column),
                            (true, Some(nullable)) => KeyPart::prefix(column, nullable),
                            (true, None) => KeyPart::prefix_unknown_nullability(column),
                        };
                        candidate.parts.push(part);
                    }
                    _ => {
                        candidate
                            .parts
                            .push(KeyPart::gap("键位是表达式或函数索引，无法还原普通列"));
                    }
                }
                if !matches!(
                    row.index_type.to_ascii_uppercase().as_str(),
                    "BTREE" | "HASH"
                ) {
                    gap.push(format!(
                        "索引类型 {} 不保证普通列唯一",
                        row.index_type.trim()
                    ));
                }
            }
            if !gap.is_empty() {
                gap.sort();
                gap.dedup();
                candidate = candidate.unusable(gap.join("；"));
            }
            candidates.push(candidate);
        }
        let (keys, non_null_keys) =
            remote_er::collect_keys_and_nullable("MySQL", table, &candidates, true, &mut evidence);
        evidence.keys.extend(keys);
        evidence.non_null_keys.extend(non_null_keys);
    }

    evidence
}

/// 把视图目录行转为证据；读取失败只降级为警告，不影响已读到的索引。
fn attach_views(
    schema: &str,
    tables: &[TableInfo],
    rows: Result<Vec<MysqlViewRow>>,
    evidence: &mut ErEvidence,
) {
    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            evidence.warn(format!("读取 MySQL 视图定义失败，已跳过视图证据：{err:#}"));
            return;
        }
    };
    for row in rows {
        let Some(parsed) = mysql_views::comparison(schema, &row.definition, tables) else {
            continue;
        };
        let comparison = dbs_core::er::ViewComparison {
            view: TableId {
                schema: schema.to_string(),
                name: row.name,
            },
            left: parsed.left,
            right: parsed.right,
        };
        if parsed.null_safe {
            evidence.null_safe_views.push(comparison);
        } else {
            evidence.views.push(comparison);
        }
    }
}

/// MySQL 完整类型签名。
///
/// 整数显示宽度（如 `int(11)`）不参与比较，因此去掉；`unsigned`、长度、
/// 精度、ENUM/SET 取值和字符排序规则都保留。
pub(crate) fn mysql_type_signature(column_type: &str, collation: &str) -> String {
    let raw = column_type.trim();
    if raw.is_empty() {
        return String::new();
    }
    let lower = raw.to_ascii_lowercase();
    let base = lower
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("");
    let integer = matches!(
        base,
        "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint"
    );
    let mut signature = if integer {
        match (raw.find('('), raw.find(')')) {
            (Some(start), Some(end)) if end > start => {
                let stripped = format!("{}{}", &raw[..start], &raw[end + 1..]);
                stripped.split_whitespace().collect::<Vec<_>>().join(" ")
            }
            _ => raw.split_whitespace().collect::<Vec<_>>().join(" "),
        }
    } else {
        // ENUM/SET 的字面量里可能有连续空格，不能整体折叠空白。
        raw.to_string()
    };
    if !collation.trim().is_empty() {
        signature.push_str(" collate ");
        signature.push_str(collation.trim());
    }
    signature
}

/// 安全取字符串。
///
/// `mysql_async::Row::get::<String>` 在字段为 SQL `NULL` 时**会 panic**
/// （`Could not retrieve String: Couldn't convert the value Null`），
/// 而 `information_schema` 里 `COLUMN_DEFAULT`、`TABLE_COMMENT` 等大量为 NULL。
/// 统一走 `get_opt` 拿 `Result`，转换失败回退默认值。
fn get_string(row: &Row, idx: usize) -> String {
    get_opt_string(row, idx).unwrap_or_default()
}

fn get_opt_string(row: &Row, idx: usize) -> Option<String> {
    row.get_opt::<String, _>(idx).and_then(|r| r.ok())
}

fn get_u32(row: &Row, idx: usize) -> u32 {
    row.get_opt::<u32, _>(idx).and_then(|r| r.ok()).unwrap_or(0)
}

/// 从 `COLUMN_TYPE` 中提取长度文本。
///
/// * `varchar(64)`            -> `64`
/// * `decimal(10,2)`          -> `10,2`
/// * `bigint(20) unsigned`    -> `20`
/// * `datetime`               -> ``
fn extract_length(column_type: &str) -> String {
    let Some(start) = column_type.find('(') else {
        return String::new();
    };
    let rest = &column_type[start + 1..];
    let Some(end) = rest.find(')') else {
        return String::new();
    };
    rest[..end].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbs_core::er::infer_relations;
    use dbs_core::{ColumnInfo, TableInfo};

    #[test]
    fn extract_length_variants() {
        assert_eq!(extract_length("varchar(64)"), "64");
        assert_eq!(extract_length("decimal(10,2)"), "10,2");
        assert_eq!(extract_length("bigint(20) unsigned"), "20");
        assert_eq!(extract_length("datetime"), "");
        assert_eq!(extract_length("enum('a','b')"), "'a','b'");
        assert_eq!(extract_length("bad("), "");
    }

    #[test]
    fn type_signature_keeps_signedness_collation_and_declared_shape() {
        assert_eq!(
            mysql_type_signature("bigint(20) unsigned", ""),
            "bigint unsigned"
        );
        assert_eq!(mysql_type_signature("bigint(20)", ""), "bigint");
        assert_eq!(
            mysql_type_signature("int(11)", ""),
            mysql_type_signature("int", "")
        );
        assert_eq!(
            mysql_type_signature("varchar(64)", "utf8mb4_bin"),
            "varchar(64) collate utf8mb4_bin"
        );
        assert_ne!(
            mysql_type_signature("varchar(64)", "utf8mb4_bin"),
            mysql_type_signature("varchar(64)", "utf8mb4_general_ci")
        );
        assert_eq!(mysql_type_signature("decimal(10,2)", ""), "decimal(10,2)");
        assert_eq!(
            mysql_type_signature("enum('A','b')", "utf8mb4_bin"),
            "enum('A','b') collate utf8mb4_bin"
        );
        // ENUM 字面量里的连续空格属于类型定义，不能被折叠。
        assert_eq!(
            mysql_type_signature("enum('a  b','c')", ""),
            "enum('a  b','c')"
        );
        assert_ne!(
            mysql_type_signature("enum('a  b','c')", ""),
            mysql_type_signature("enum('a b','c')", "")
        );
        assert_eq!(mysql_type_signature("bit(1)", ""), "bit(1)");
    }

    fn column(name: &str, data_type: &str, pk: bool, comment: &str) -> ColumnInfo {
        ColumnInfo {
            order: 1,
            name: name.into(),
            data_type: data_type.into(),
            length: String::new(),
            is_primary_key: pk,
            is_auto_increment: false,
            // 测试 DDL 里这些列都是 NOT NULL。
            nullable: false,
            default_value: String::new(),
            comment: comment.into(),
        }
    }

    fn table(name: &str, comment: &str, columns: Vec<ColumnInfo>) -> TableInfo {
        TableInfo {
            order: 1,
            schema: "app".into(),
            name: name.into(),
            comment: comment.into(),
            columns,
            ..Default::default()
        }
    }

    fn type_row(table: &str, column: &str, column_type: &str) -> MysqlTypeRow {
        MysqlTypeRow {
            table: table.into(),
            column: column.into(),
            column_type: column_type.into(),
            collation: "utf8mb4_general_ci".into(),
        }
    }

    fn index_row(table: &str, index: &str, seq: u32, column: Option<&str>) -> MysqlIndexRow {
        MysqlIndexRow {
            table: table.into(),
            index: index.into(),
            seq,
            column: column.map(str::to_string),
            sub_part: None,
            nullable: Some(false),
            index_type: "BTREE".into(),
        }
    }

    #[test]
    fn business_zero_foreign_key_schema_infers_a_real_comment_target() {
        let users = table(
            "users",
            "客户信息表",
            vec![
                column("id", "bigint", true, ""),
                column("customer_no", "varchar", false, "客户业务编号"),
            ],
        );
        let invoices = table(
            "invoices",
            "发票表",
            vec![
                column("id", "bigint", true, ""),
                column("payer_ref", "varchar", false, "关联付款客户的业务编号"),
            ],
        );
        let types = vec![
            type_row("users", "id", "bigint(20)"),
            type_row("users", "customer_no", "varchar(32)"),
            type_row("invoices", "id", "bigint(20)"),
            type_row("invoices", "payer_ref", "varchar(32)"),
        ];
        let indexes = vec![
            index_row("users", "PRIMARY", 1, Some("id")),
            index_row("users", "uk_users_customer_no", 1, Some("customer_no")),
            index_row("invoices", "PRIMARY", 1, Some("id")),
        ];

        let evidence =
            evidence_from_rows("app", &[users.clone(), invoices.clone()], &types, &indexes);
        let catalog = remote_er::assemble(vec![users, invoices], evidence);
        assert!(
            catalog
                .keys
                .iter()
                .any(|key| key.table.name == "users" && key.columns == ["customer_no"]),
            "{:?}",
            catalog.keys
        );

        let analysis = infer_relations(&catalog);
        assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
        let relation = &analysis.relations[0];
        assert_eq!(relation.source.table.name, "invoices");
        assert_eq!(relation.source.columns, ["payer_ref"]);
        assert_eq!(relation.target.table.name, "users");
        assert_eq!(relation.target.columns, ["customer_no"]);
    }

    fn nullable_column(name: &str, data_type: &str, comment: &str) -> ColumnInfo {
        let mut column = column(name, data_type, false, comment);
        column.nullable = true;
        column
    }

    fn nullable_index_row(
        table: &str,
        index: &str,
        seq: u32,
        column: Option<&str>,
    ) -> MysqlIndexRow {
        let mut row = index_row(table, index, seq, column);
        row.nullable = Some(true);
        row
    }

    fn view_row(name: &str, definition: &str) -> MysqlViewRow {
        MysqlViewRow {
            name: name.into(),
            definition: definition.into(),
        }
    }

    #[test]
    fn mysql_index_shapes_are_split_into_keys_and_non_null_keys() {
        let items = table(
            "items",
            "明细",
            vec![
                column("tenant_id", "bigint", false, ""),
                column("code", "varchar", false, ""),
                nullable_column("optional", "varchar", ""),
            ],
        );
        let types = vec![
            type_row("items", "tenant_id", "bigint(20)"),
            type_row("items", "code", "varchar(32)"),
            type_row("items", "optional", "varchar(32)"),
        ];
        let mut prefix = index_row("items", "uk_prefix", 1, Some("code"));
        prefix.sub_part = Some(8);
        let nullable = nullable_index_row("items", "uk_nullable", 1, Some("optional"));
        let indexes = vec![
            index_row("items", "uk_full", 1, Some("tenant_id")),
            index_row("items", "uk_full", 2, Some("code")),
            prefix,
            nullable,
        ];

        let evidence = evidence_from_rows("app", &[items], &types, &indexes);
        assert_eq!(evidence.keys.len(), 1, "{:?}", evidence.keys);
        assert_eq!(evidence.keys[0].columns, ["tenant_id", "code"]);
        assert_eq!(
            evidence.non_null_keys.len(),
            1,
            "{:?}",
            evidence.non_null_keys
        );
        assert_eq!(evidence.non_null_keys[0].columns, ["optional"]);
        assert!(
            evidence
                .scoped_warnings
                .iter()
                .any(|warning| warning.message.contains("前缀索引")),
            "{:?}",
            evidence.scoped_warnings
        );
    }

    #[test]
    fn nullable_mysql_unique_indexes_are_non_null_keys_but_never_full_keys() {
        let items = table(
            "items",
            "明细",
            vec![
                column("tenant_id", "bigint", false, ""),
                column("fixed", "varchar", false, ""),
                nullable_column("code", "varchar", ""),
                nullable_column("token", "varchar", ""),
            ],
        );
        let indexes = vec![
            nullable_index_row("items", "uk_code", 1, Some("code")),
            index_row("items", "uk_pair", 1, Some("tenant_id")),
            nullable_index_row("items", "uk_pair", 2, Some("code")),
            nullable_index_row("items", "uk_two_nullable", 1, Some("token")),
            nullable_index_row("items", "uk_two_nullable", 2, Some("code")),
            index_row("items", "uk_full", 1, Some("tenant_id")),
            index_row("items", "uk_full", 2, Some("fixed")),
        ];

        let evidence = evidence_from_rows("app", std::slice::from_ref(&items), &[], &indexes);
        assert_eq!(evidence.keys.len(), 1, "{:?}", evidence.keys);
        assert_eq!(evidence.keys[0].columns, ["tenant_id", "fixed"]);
        assert_eq!(
            evidence.non_null_keys.len(),
            3,
            "{:?}",
            evidence.non_null_keys
        );

        let catalog = remote_er::assemble(vec![items], evidence);
        assert!(catalog
            .keys
            .iter()
            .all(|key| key.columns == ["tenant_id", "fixed"]));
        let mut nullable_keys: Vec<_> = catalog
            .non_null_keys
            .iter()
            .map(|key| key.columns.clone())
            .collect();
        nullable_keys.sort();
        assert_eq!(
            nullable_keys,
            vec![
                vec!["code".to_string()],
                vec!["tenant_id".to_string(), "code".to_string()],
                vec!["token".to_string(), "code".to_string()],
            ]
        );
    }

    #[test]
    fn nullable_mysql_indexes_still_abstain_on_gap_prefix_expression_and_conflict() {
        let items = table(
            "items",
            "明细",
            vec![
                column("tenant_id", "bigint", false, ""),
                nullable_column("code", "varchar", ""),
                nullable_column("token", "varchar", ""),
                column("fixed", "varchar", false, ""),
            ],
        );
        let mut expression = index_row("items", "uk_expression", 1, None);
        expression.nullable = Some(true);
        let mut unknown = index_row("items", "uk_unknown", 1, Some("code"));
        unknown.nullable = None;
        let indexes = vec![
            index_row("items", "uk_gap", 1, Some("tenant_id")),
            nullable_index_row("items", "uk_gap", 3, Some("code")),
            {
                let mut prefix = nullable_index_row("items", "uk_prefix", 1, Some("token"));
                prefix.sub_part = Some(8);
                prefix
            },
            expression,
            nullable_index_row("items", "uk_missing", 1, Some("absent")),
            nullable_index_row("items", "uk_conflict", 1, Some("fixed")),
            unknown,
        ];

        let evidence = evidence_from_rows("app", &[items], &[], &indexes);
        assert!(evidence.keys.is_empty(), "{:?}", evidence.keys);
        assert!(
            evidence.non_null_keys.is_empty(),
            "{:?}",
            evidence.non_null_keys
        );
        for expected in [
            "不连续",
            "前缀索引",
            "表达式",
            "不在表",
            "可空性与表结构冲突",
            "可空性未知",
        ] {
            assert!(
                evidence
                    .scoped_warnings
                    .iter()
                    .any(|warning| warning.message.contains(expected)),
                "缺少警告 {expected}：{:?}",
                evidence.scoped_warnings
            );
        }
    }

    #[test]
    fn normalized_mysql_views_record_plain_equality_with_actual_identifiers() {
        let orders = table(
            "orders",
            "订单",
            vec![
                column("id", "bigint", true, ""),
                column("tenant_id", "bigint", false, ""),
                column("customer_id", "bigint", false, ""),
            ],
        );
        let customers = table(
            "customers",
            "客户",
            vec![
                column("id", "bigint", true, ""),
                column("tenant_id", "bigint", false, ""),
            ],
        );
        let definitions = vec![
            // MySQL 8 常见形态：全反引号、三段列名、无别名。
            view_row(
                "v_customer",
                "select `app`.`orders`.`id` AS `id`,`app`.`customers`.`id` AS `customer_id` \
                 from `app`.`orders` join `app`.`customers` \
                 on((`app`.`orders`.`customer_id` = `app`.`customers`.`id`))",
            ),
            // 别名、库名表限定、复合 AND 与多层括号。
            view_row(
                "v_tenant_customer",
                "select `o`.`tenant_id` AS `tenant_id`,`c`.`id` AS `customer_id` \
                 from (`app`.`orders` `o` join `app`.`customers` `c` \
                 on(((`o`.`tenant_id` = `c`.`tenant_id`) and (`o`.`customer_id` = `c`.`id`))))",
            ),
        ];

        let mut evidence =
            evidence_from_rows("app", &[orders.clone(), customers.clone()], &[], &[]);
        attach_views(
            "app",
            &[orders.clone(), customers.clone()],
            Ok(definitions),
            &mut evidence,
        );
        let catalog = remote_er::assemble(vec![orders, customers], evidence);
        assert_eq!(catalog.views.len(), 2, "{:?}", catalog.views);
        let plain = catalog
            .views
            .iter()
            .find(|view| view.view.name == "v_customer")
            .expect("普通等值视图");
        assert_eq!(plain.left.table.name, "orders");
        assert_eq!(plain.left.columns, ["customer_id"]);
        assert_eq!(plain.right.table.name, "customers");
        assert_eq!(plain.right.columns, ["id"]);
        let composite = catalog
            .views
            .iter()
            .find(|view| view.view.name == "v_tenant_customer")
            .expect("复合等值视图");
        assert_eq!(
            composite.left.columns,
            ["tenant_id".to_string(), "customer_id".to_string()]
        );
        assert_eq!(
            composite.right.columns,
            ["tenant_id".to_string(), "id".to_string()]
        );
        assert!(catalog.null_safe_views.is_empty());
    }

    #[test]
    fn mysql_plain_and_mixed_null_safe_views_are_kept_apart() {
        let orders = table(
            "orders",
            "订单",
            vec![
                column("id", "bigint", true, ""),
                column("tenant_id", "bigint", false, ""),
                column("customer_id", "bigint", false, ""),
            ],
        );
        let customers = table(
            "customers",
            "客户",
            vec![
                column("id", "bigint", true, ""),
                column("tenant_id", "bigint", false, ""),
            ],
        );
        let definitions = vec![
            view_row(
                "v_plain",
                "select `app`.`orders`.`id` AS `id` from `app`.`orders` join `app`.`customers` \
                 on((`app`.`orders`.`customer_id` = `app`.`customers`.`id`))",
            ),
            view_row(
                "v_null_safe",
                "select `app`.`orders`.`id` AS `id` from `app`.`orders` join `app`.`customers` \
                 on((`app`.`orders`.`customer_id` <=> `app`.`customers`.`id`))",
            ),
            view_row(
                "v_mixed",
                "select `app`.`orders`.`id` AS `id` from `app`.`orders` join `app`.`customers` \
                 on(((`app`.`orders`.`tenant_id` = `app`.`customers`.`tenant_id`) \
                 and (`app`.`orders`.`customer_id` <=> `app`.`customers`.`id`)))",
            ),
        ];

        let mut evidence =
            evidence_from_rows("app", &[orders.clone(), customers.clone()], &[], &[]);
        attach_views(
            "app",
            &[orders.clone(), customers.clone()],
            Ok(definitions),
            &mut evidence,
        );
        let catalog = remote_er::assemble(vec![orders, customers], evidence);
        assert_eq!(catalog.views.len(), 1, "{:?}", catalog.views);
        assert_eq!(catalog.views[0].view.name, "v_plain");
        assert_eq!(
            catalog.null_safe_views.len(),
            2,
            "{:?}",
            catalog.null_safe_views
        );
        assert!(catalog
            .null_safe_views
            .iter()
            .any(|view| view.view.name == "v_null_safe"));
        assert!(catalog
            .null_safe_views
            .iter()
            .any(|view| view.view.name == "v_mixed"));
    }

    #[test]
    fn composite_equality_view_pairs_follow_the_target_key_order() {
        let child = table(
            "child",
            "子表",
            vec![
                column("id", "bigint", true, ""),
                column("tenant_id", "bigint", false, ""),
                column("code", "varchar", false, ""),
            ],
        );
        let parent = table(
            "parent",
            "父表",
            vec![
                column("id", "bigint", true, ""),
                column("tenant_id", "bigint", false, ""),
                column("code", "varchar", false, ""),
            ],
        );
        let indexes = vec![
            index_row("parent", "PRIMARY", 1, Some("id")),
            index_row("parent", "uk_parent", 1, Some("tenant_id")),
            index_row("parent", "uk_parent", 2, Some("code")),
        ];
        let definitions = vec![view_row(
            "v_child_parent",
            "select `c`.`code` AS `code` from `app`.`child` `c` join `app`.`parent` `p` \
             on(((`c`.`code` = `p`.`code`) and (`c`.`tenant_id` = `p`.`tenant_id`)))",
        )];

        let mut evidence =
            evidence_from_rows("app", &[child.clone(), parent.clone()], &[], &indexes);
        attach_views(
            "app",
            &[child.clone(), parent.clone()],
            Ok(definitions),
            &mut evidence,
        );
        let catalog = remote_er::assemble(vec![child, parent], evidence);
        assert_eq!(catalog.views.len(), 1, "{:?}", catalog.views);
        let view = &catalog.views[0];
        assert_eq!(
            view.left.columns,
            ["tenant_id".to_string(), "code".to_string()]
        );
        assert_eq!(
            view.right.columns,
            ["tenant_id".to_string(), "code".to_string()]
        );
    }

    #[test]
    fn unsupported_mysql_view_shapes_abstain_without_losing_index_evidence() {
        let orders = table(
            "orders",
            "订单",
            vec![
                column("id", "bigint", true, ""),
                column("tenant_id", "bigint", false, ""),
                nullable_column("customer_id", "bigint", ""),
            ],
        );
        let customers = table(
            "customers",
            "客户",
            vec![
                column("id", "bigint", true, ""),
                column("tenant_id", "bigint", false, ""),
            ],
        );
        let definitions = vec![
            view_row(
                "v_left",
                "select `app`.`orders`.`id` AS `id` from `app`.`orders` left join `app`.`customers` \
                 on((`app`.`orders`.`customer_id` = `app`.`customers`.`id`))",
            ),
            view_row(
                "v_or",
                "select `app`.`orders`.`id` AS `id` from `app`.`orders` join `app`.`customers` \
                 on((`app`.`orders`.`customer_id` = `app`.`customers`.`id`) \
                 or (`app`.`orders`.`tenant_id` = `app`.`customers`.`tenant_id`))",
            ),
            view_row(
                "v_function",
                "select `app`.`orders`.`id` AS `id` from `app`.`orders` join `app`.`customers` \
                 on((lower(`app`.`orders`.`customer_id`) = `app`.`customers`.`id`))",
            ),
            view_row(
                "v_where",
                "select `app`.`orders`.`id` AS `id` from `app`.`orders` join `app`.`customers` \
                 on((`app`.`orders`.`customer_id` = `app`.`customers`.`id`)) where `app`.`orders`.`id` > 0",
            ),
            view_row(
                "v_other_schema",
                "select `other`.`orders`.`id` AS `id` from `other`.`orders` join `app`.`customers` \
                 on((`other`.`orders`.`customer_id` = `app`.`customers`.`id`))",
            ),
            view_row(
                "v_constant",
                "select `app`.`orders`.`id` AS `id` from `app`.`orders` join `app`.`customers` \
                 on((`app`.`orders`.`customer_id` = `app`.`customers`.`id`) \
                 and (`app`.`customers`.`tenant_id` = 1))",
            ),
        ];
        let indexes = vec![nullable_index_row(
            "orders",
            "uk_customer",
            1,
            Some("customer_id"),
        )];
        let mut evidence =
            evidence_from_rows("app", &[orders.clone(), customers.clone()], &[], &indexes);
        assert_eq!(evidence.non_null_keys.len(), 1);
        attach_views(
            "app",
            &[orders.clone(), customers.clone()],
            Ok(definitions),
            &mut evidence,
        );
        let catalog = remote_er::assemble(vec![orders, customers], evidence);
        assert!(catalog.views.is_empty(), "{:?}", catalog.views);
        assert!(
            catalog.null_safe_views.is_empty(),
            "{:?}",
            catalog.null_safe_views
        );
        assert_eq!(
            catalog.non_null_keys.len(),
            1,
            "{:?}",
            catalog.non_null_keys
        );
    }

    #[test]
    fn view_read_failure_degrades_to_warning_and_keeps_index_evidence() {
        let items = table(
            "items",
            "明细",
            vec![nullable_column("code", "varchar", "")],
        );
        let indexes = vec![nullable_index_row("items", "uk_code", 1, Some("code"))];
        let mut evidence = evidence_from_rows("app", std::slice::from_ref(&items), &[], &indexes);
        assert_eq!(evidence.non_null_keys.len(), 1);

        attach_views(
            "app",
            std::slice::from_ref(&items),
            Err(anyhow::anyhow!("SELECT command denied")),
            &mut evidence,
        );
        assert_eq!(
            evidence.non_null_keys.len(),
            1,
            "{:?}",
            evidence.non_null_keys
        );
        assert_eq!(evidence.keys.len(), 0);
        assert!(
            evidence
                .warnings
                .iter()
                .any(|warning| warning.contains("视图定义") && warning.contains("denied")),
            "{:?}",
            evidence.warnings
        );

        let catalog = remote_er::assemble(vec![items], evidence);
        assert_eq!(catalog.non_null_keys.len(), 1);
    }
}
