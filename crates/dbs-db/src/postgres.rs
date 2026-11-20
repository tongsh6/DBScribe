//! PostgreSQL 元数据读取。
//!
//! 通过系统目录（`pg_class` / `pg_attribute` / `pg_index`）读取表与列信息，
//! 一次查询取全库，避免 N+1。

use crate::options::ConnectionOptions;
use crate::remote_er::{ErEvidence, IndexCandidate, KeyPart};
use anyhow::{Context, Result};
use dbs_core::er::{ErCatalog, ErColumnType, TableId};
use dbs_core::{ColumnInfo, ForeignKey, IndexInfo, TableInfo};
use tokio_postgres::{Client, NoTls};

const DATABASE_SQL: &str = r#"
SELECT datname::text
FROM pg_database
WHERE datistemplate = false AND datallowconn
ORDER BY datname
"#;

const TABLE_SQL: &str = r#"
SELECT c.relname::text AS table_name,
       COALESCE(obj_description(c.oid, 'pg_class'), '')::text AS comment,
       n.nspname::text AS schema_name
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE c.relkind = 'r'
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\_%'
ORDER BY n.nspname, c.relname
"#;

/// 索引：按 INDKEY 的键位顺序展开，INCLUDE 列不进入键列。
const INDEX_SQL: &str = r#"
SELECT t.relname::text AS table_name,
       i.relname::text AS index_name,
       ix.indisunique AS is_unique,
       ix.indisprimary AS is_primary,
       COALESCE(a.attname, pg_get_indexdef(i.oid, k.ord::int, true))::text AS column_name,
       n.nspname::text AS schema_name
FROM pg_class t
JOIN pg_namespace n ON n.oid = t.relnamespace
JOIN pg_index ix ON ix.indrelid = t.oid
JOIN pg_class i ON i.oid = ix.indexrelid
JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord)
  ON k.ord <= ix.indnkeyatts
LEFT JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum AND k.attnum <> 0
WHERE t.relkind = 'r'
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\_%'
ORDER BY n.nspname, t.relname, i.relname, k.ord
"#;

/// 外键：pg_constraint（contype = 'f'）。
const FK_SQL: &str = r#"
SELECT t.relname::text AS table_name,
       c.conname::text AS constraint_name,
       a.attname::text AS column_name,
       nt.nspname::text AS ref_schema,
       rt.relname::text AS ref_table,
       af.attname::text AS ref_column,
       n.nspname::text AS schema_name
FROM pg_constraint c
JOIN pg_class t ON t.oid = c.conrelid
JOIN pg_namespace n ON n.oid = t.relnamespace
JOIN pg_class rt ON rt.oid = c.confrelid
JOIN pg_namespace nt ON nt.oid = rt.relnamespace
JOIN LATERAL unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord) ON true
JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum
JOIN LATERAL unnest(c.confkey) WITH ORDINALITY AS fk(attnum, ord) ON fk.ord = k.ord
JOIN pg_attribute af ON af.attrelid = c.confrelid AND af.attnum = fk.attnum
WHERE c.contype = 'f'
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\_%'
ORDER BY n.nspname, t.relname, c.conname, k.ord
"#;

/// 分区表（PG 10+）。
const PARTITION_SQL: &str = r#"
SELECT c.relname::text AS table_name, n.nspname::text AS schema_name
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE c.relkind = 'p'
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\_%'
"#;

const COLUMN_SQL: &str = r#"
SELECT c.relname::text AS table_name,
       a.attnum::int4 AS attnum,
       a.attname::text AS attname,
       format_type(a.atttypid, a.atttypmod)::text AS full_type,
       a.attnotnull AS not_null,
       COALESCE(pg_get_expr(d.adbin, d.adrelid), '')::text AS default_value,
       COALESCE(col_description(a.attrelid, a.attnum), '')::text AS comment,
       COALESCE(i.indisprimary, false) AS is_primary,
       (a.attidentity <> '' OR COALESCE(pg_get_expr(d.adbin, d.adrelid), '') LIKE 'nextval%') AS is_identity,
       n.nspname::text AS schema_name
FROM pg_attribute a
JOIN pg_class c ON c.oid = a.attrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
LEFT JOIN pg_index i ON i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey)
WHERE c.relkind = 'r'
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\_%'
  AND a.attnum > 0
  AND NOT a.attisdropped
ORDER BY n.nspname, c.relname, a.attnum
"#;

async fn connect(opts: &ConnectionOptions, database: &str) -> Result<Client> {
    let mut config = tokio_postgres::Config::new();
    config
        .host(&opts.host)
        .port(opts.port)
        .user(&opts.user)
        .password(&opts.password);
    if !database.is_empty() {
        config.dbname(database);
    }

    let (client, connection) = config
        .connect(NoTls)
        .await
        .context("连接 PostgreSQL 失败（请检查地址、端口、账号密码）")?;

    // 连接对象必须被驱动，否则查询会一直挂起
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            eprintln!("[DBScribe] PostgreSQL 连接中断：{err}");
        }
    });

    Ok(client)
}

/// 列出可连接的非模板数据库。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    // 优先连 postgres 维护库；没有的话退回默认库
    let client = match connect(opts, "postgres").await {
        Ok(client) => client,
        Err(_) => connect(opts, "").await.context("无法连接到 PostgreSQL")?,
    };

    let rows = client
        .query(DATABASE_SQL, &[])
        .await
        .context("查询数据库清单失败")?;

    Ok(rows.iter().map(|row| row.get::<_, String>(0)).collect())
}

/// 读取指定库的全部表（不含视图）及其列信息。
pub async fn load_tables(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    let client = connect(opts, &opts.database).await?;

    let table_rows = client
        .query(TABLE_SQL, &[])
        .await
        .context("查询表清单失败")?;

    let column_rows = client
        .query(COLUMN_SQL, &[])
        .await
        .context("查询列信息失败")?;

    // key 统一用 "schema.table"，避免多 schema 同名表互相覆盖
    let mut columns_by_table: std::collections::HashMap<String, Vec<ColumnInfo>> =
        std::collections::HashMap::new();

    for row in &column_rows {
        let table_name: String = row.get(0);
        let schema: String = row.get(9);
        let key = format!("{schema}.{table_name}");
        let full_type: String = row.get(3);
        let (data_type, length) = split_type(&full_type);
        let default_value: String = row.get(5);
        let is_identity: bool = row.get(8);

        columns_by_table.entry(key).or_default().push(ColumnInfo {
            order: row.get::<_, i32>(1) as u32,
            name: row.get(2),
            data_type,
            length,
            is_primary_key: row.get(7),
            // PostgreSQL 没有 MySQL 那种 auto_increment，用 identity / nextval 默认值判断
            is_auto_increment: is_identity,
            nullable: !row.get::<_, bool>(4),
            default_value,
            comment: row.get(6),
        });
    }

    // 索引
    let index_rows = client
        .query(INDEX_SQL, &[])
        .await
        .context("查询索引信息失败")?;
    let mut indexes_by_table: std::collections::HashMap<String, Vec<IndexInfo>> =
        std::collections::HashMap::new();
    for row in &index_rows {
        let table_name: String = row.get(0);
        let index_name: String = row.get(1);
        let is_unique: bool = row.get(2);
        let is_primary: bool = row.get(3);
        let column_name: String = row.get(4);
        let schema: String = row.get(5);
        let key = format!("{schema}.{table_name}");

        let list = indexes_by_table.entry(key).or_default();
        match list.iter_mut().find(|i| i.name == index_name) {
            Some(existing) => existing.columns.push(column_name),
            None => list.push(IndexInfo {
                name: index_name,
                columns: vec![column_name],
                is_unique,
                is_primary,
            }),
        }
    }

    // 外键
    let fk_rows = client
        .query(FK_SQL, &[])
        .await
        .context("查询外键信息失败")?;
    let mut fks_by_table: std::collections::HashMap<String, Vec<ForeignKey>> =
        std::collections::HashMap::new();
    for row in &fk_rows {
        let table_name: String = row.get(0);
        let constraint: String = row.get(1);
        let column_name: String = row.get(2);
        let ref_schema: String = row.get(3);
        let ref_table: String = row.get(4);
        let ref_column: String = row.get(5);
        let schema: String = row.get(6);
        let key = format!("{schema}.{table_name}");

        let list = fks_by_table.entry(key).or_default();
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
    let partition_rows = client
        .query(PARTITION_SQL, &[])
        .await
        .context("查询分区信息失败")?;
    let partitioned: std::collections::HashSet<String> = partition_rows
        .iter()
        .map(|row| {
            let table: String = row.get(0);
            let schema: String = row.get(1);
            format!("{schema}.{table}")
        })
        .collect();

    let mut tables = Vec::with_capacity(table_rows.len());
    for (idx, row) in table_rows.iter().enumerate() {
        let name: String = row.get(0);
        let comment: String = row.get(1);
        let schema: String = row.get(2);
        let key = format!("{schema}.{name}");

        tables.push(TableInfo {
            order: (idx + 1) as u32,
            schema,
            name,
            comment,
            columns: columns_by_table.remove(&key).unwrap_or_default(),
            indexes: indexes_by_table.remove(&key).unwrap_or_default(),
            foreign_keys: fks_by_table.remove(&key).unwrap_or_default(),
            is_partitioned: partitioned.contains(&key),
            note: String::new(),
        });
    }

    tracing::info!(
        "已读取 {} 张表、{} 个列定义",
        tables.len(),
        tables.iter().map(|t| t.columns.len()).sum::<usize>()
    );
    Ok(tables)
}

/// 读取单张表的列信息（界面预览用）。
pub async fn load_table_columns(opts: &ConnectionOptions, table: &str) -> Result<Vec<ColumnInfo>> {
    // PostgreSQL 没有按表过滤的固定 SQL，这里复用全量查询再筛选，逻辑保持一致
    let tables = load_tables(opts).await?;
    tables
        .into_iter()
        .find(|t| t.name == table)
        .map(|t| t.columns)
        .with_context(|| format!("表 {table} 不存在"))
}

/// 把 `format_type` 的结果拆成（类型, 长度）。
///
/// `character varying(64)` -> (`character varying`, `64`)
/// `numeric(10,2)`         -> (`numeric`, `10,2`)
/// `bigint`                -> (`bigint`, ``)
fn split_type(full_type: &str) -> (String, String) {
    match full_type.find('(') {
        Some(pos) => {
            let name = full_type[..pos].trim().to_string();
            let length = full_type[pos + 1..]
                .trim_end_matches(')')
                .trim()
                .to_string();
            (name, length)
        }
        None => (full_type.trim().to_string(), String::new()),
    }
}

/// ER 类型签名：`format_type` 已包含长度/精度；仅当列显式覆盖排序规则时追加，
/// 避免把展示类型当成完整比较依据。
const ER_TYPE_SQL: &str = r#"
SELECT n.nspname::text AS schema_name,
       c.relname::text AS table_name,
       a.attname::text AS column_name,
       format_type(a.atttypid, a.atttypmod)::text AS full_type,
       CASE WHEN a.attcollation <> 0 AND a.attcollation <> ty.typcollation
            THEN (a.attcollation::regcollation)::text
            ELSE ''
       END AS collation_name
FROM pg_attribute a
JOIN pg_class c ON c.oid = a.attrelid
JOIN pg_namespace n ON n.oid = c.relnamespace
JOIN pg_type ty ON ty.oid = a.atttypid
WHERE c.relkind = 'r'
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\_%'
  AND a.attnum > 0
  AND NOT a.attisdropped
ORDER BY n.nspname, c.relname, a.attnum
"#;

/// ER 唯一索引：按 INDKEY 位序读取；表达式键位 attnum=0；INCLUDE 列单独标记，
/// 不参与键列；部分、无效、未就绪的索引保留状态供装配器拒绝并说明原因。
const ER_INDEX_SQL: &str = r#"
SELECT n.nspname::text AS schema_name,
       t.relname::text AS table_name,
       i.relname::text AS index_name,
       ix.indisprimary AS is_primary,
       ix.indisvalid AS is_valid,
       ix.indisready AS is_ready,
       (ix.indpred IS NOT NULL) AS is_partial,
       (k.ord > ix.indnkeyatts) AS is_included,
       k.ord::int4 AS ordinal,
       a.attname::text AS column_name,
       COALESCE(a.attnotnull, false) AS not_null
FROM pg_class t
JOIN pg_namespace n ON n.oid = t.relnamespace
JOIN pg_index ix ON ix.indrelid = t.oid
JOIN pg_class i ON i.oid = ix.indexrelid
JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) ON true
LEFT JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum AND k.attnum <> 0
WHERE t.relkind = 'r'
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND n.nspname NOT LIKE 'pg\_%'
  AND ix.indisunique
ORDER BY n.nspname, t.relname, i.relname, k.ord
"#;

#[derive(Debug, Clone)]
struct PgTypeRow {
    schema: String,
    table: String,
    column: String,
    full_type: String,
    collation: String,
}

#[derive(Debug, Clone)]
struct PgIndexRow {
    schema: String,
    table: String,
    index: String,
    primary: bool,
    valid: bool,
    ready: bool,
    partial: bool,
    included: bool,
    ordinal: i32,
    column: Option<String>,
    not_null: bool,
}

/// 读取 ER 专用证据：完整类型签名与按 INDKEY 位序排列的完整唯一键。
pub(crate) async fn load_er_catalog(opts: &ConnectionOptions) -> Result<ErCatalog> {
    let tables = load_tables(opts).await?;
    let evidence = read_er_evidence(opts, &tables).await;
    Ok(crate::remote_er::assemble_read(tables, evidence))
}

async fn read_er_evidence(opts: &ConnectionOptions, tables: &[TableInfo]) -> Result<ErEvidence> {
    let client = connect(opts, &opts.database).await?;
    let type_rows = client
        .query(ER_TYPE_SQL, &[])
        .await
        .context("查询 PostgreSQL ER 类型信息失败")?;
    let type_rows: Vec<PgTypeRow> = type_rows
        .iter()
        .map(|row| PgTypeRow {
            schema: row.get(0),
            table: row.get(1),
            column: row.get(2),
            full_type: row.get(3),
            collation: row.get(4),
        })
        .collect();

    let index_rows = client
        .query(ER_INDEX_SQL, &[])
        .await
        .context("查询 PostgreSQL ER 唯一索引失败")?;
    let index_rows: Vec<PgIndexRow> = index_rows
        .iter()
        .map(|row| PgIndexRow {
            schema: row.get(0),
            table: row.get(1),
            index: row.get(2),
            primary: row.get(3),
            valid: row.get(4),
            ready: row.get(5),
            partial: row.get(6),
            included: row.get(7),
            ordinal: row.get(8),
            column: row.get(9),
            not_null: row.get(10),
        })
        .collect();

    Ok(evidence_from_rows(tables, &type_rows, &index_rows))
}

/// 把 PostgreSQL 目录行装配成 ER 证据；不连接数据库，便于验证位序与状态规则。
fn evidence_from_rows(
    tables: &[TableInfo],
    type_rows: &[PgTypeRow],
    index_rows: &[PgIndexRow],
) -> ErEvidence {
    let mut evidence = ErEvidence::default();

    for row in type_rows {
        let signature = pg_type_signature(&row.full_type, &row.collation);
        if signature.is_empty() {
            match tables.iter().find(|table| {
                table.schema.eq_ignore_ascii_case(&row.schema)
                    && table.name.eq_ignore_ascii_case(&row.table)
            }) {
                Some(table) => evidence.warn_table(
                    table,
                    format!(
                        "PostgreSQL 表 {}.{} 的列 {} 没有可比较的类型签名",
                        row.schema, row.table, row.column
                    ),
                ),
                None => evidence.warn(format!(
                    "PostgreSQL 表 {}.{} 的列 {} 没有可比较的类型签名",
                    row.schema, row.table, row.column
                )),
            }
            continue;
        }
        evidence.types.push(ErColumnType {
            table: TableId {
                schema: row.schema.clone(),
                name: row.table.clone(),
            },
            column: row.column.clone(),
            signature,
        });
    }

    // (schema, table) -> index -> rows
    let mut grouped: std::collections::BTreeMap<
        (String, String),
        std::collections::BTreeMap<String, Vec<&PgIndexRow>>,
    > = std::collections::BTreeMap::new();
    for row in index_rows {
        grouped
            .entry((
                row.schema.to_ascii_lowercase(),
                row.table.to_ascii_lowercase(),
            ))
            .or_default()
            .entry(row.index.clone())
            .or_default()
            .push(row);
    }

    for table in tables {
        let key = (
            table.schema.to_ascii_lowercase(),
            table.name.to_ascii_lowercase(),
        );
        let Some(indexes) = grouped.get(&key) else {
            continue;
        };

        let mut candidates = Vec::with_capacity(indexes.len());
        for (name, rows) in indexes {
            let mut rows = rows.to_vec();
            rows.sort_by_key(|row| row.ordinal);
            let mut candidate =
                IndexCandidate::new(name.clone(), rows.iter().any(|r| r.primary), Vec::new());
            let mut gap = Vec::new();
            for row in rows {
                if row.included {
                    // INCLUDE 列不参与唯一键，忽略即可。
                    continue;
                }
                if !row.valid {
                    gap.push("索引未通过有效性检查（indisvalid=false）".to_string());
                }
                if !row.ready {
                    gap.push("索引未完成构建（indisready=false）".to_string());
                }
                if row.partial {
                    gap.push("部分索引（带 WHERE 条件）只覆盖部分行".to_string());
                }
                match row.column.as_deref() {
                    Some(column) if !column.is_empty() => {
                        candidate.parts.push(KeyPart::column(column, !row.not_null));
                    }
                    _ => candidate.parts.push(KeyPart::gap(
                        "键位是表达式（INDKEY 中 attnum=0），不是普通列",
                    )),
                }
            }
            if !gap.is_empty() {
                gap.sort();
                gap.dedup();
                candidate = candidate.unusable(gap.join("；"));
            }
            candidates.push(candidate);
        }
        let keys =
            crate::remote_er::collect_keys("PostgreSQL", table, &candidates, true, &mut evidence);
        evidence.keys.extend(keys);
    }

    evidence
}

/// PostgreSQL 类型签名：保留 `format_type` 的完整声明，只有显式排序规则才追加。
pub(crate) fn pg_type_signature(full_type: &str, collation: &str) -> String {
    let full_type = full_type.trim();
    if collation.trim().is_empty() {
        full_type.to_string()
    } else {
        format!("{full_type} collate {}", collation.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_type_variants() {
        assert_eq!(
            split_type("character varying(64)"),
            ("character varying".to_string(), "64".to_string())
        );
        assert_eq!(
            split_type("numeric(10,2)"),
            ("numeric".to_string(), "10,2".to_string())
        );
        assert_eq!(split_type("bigint"), ("bigint".to_string(), String::new()));
        assert_eq!(
            split_type("timestamp without time zone"),
            ("timestamp without time zone".to_string(), String::new())
        );
    }

    #[test]
    fn type_signature_keeps_declared_shape_and_only_explicit_collation() {
        assert_eq!(
            pg_type_signature("character varying(64)", ""),
            "character varying(64)"
        );
        assert_eq!(
            pg_type_signature("character varying(64)", "\"C\""),
            "character varying(64) collate \"C\""
        );
        assert_ne!(
            pg_type_signature("character varying(64)", ""),
            pg_type_signature("character varying(32)", "")
        );
        assert_ne!(
            pg_type_signature("numeric(10,2)", ""),
            pg_type_signature("numeric(12,2)", "")
        );
    }

    fn column(name: &str, not_null: bool) -> ColumnInfo {
        ColumnInfo {
            order: 1,
            name: name.into(),
            data_type: "x".into(),
            length: String::new(),
            is_primary_key: false,
            is_auto_increment: false,
            nullable: !not_null,
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

    fn index_row(
        index: &str,
        ordinal: i32,
        column: Option<&str>,
        not_null: bool,
        included: bool,
    ) -> PgIndexRow {
        PgIndexRow {
            schema: "app".into(),
            table: "items".into(),
            index: index.into(),
            primary: index == "pk_items",
            valid: true,
            ready: true,
            partial: false,
            included,
            ordinal,
            column: column.map(str::to_string),
            not_null,
        }
    }

    #[test]
    fn indkey_order_include_and_unusable_index_rules() {
        let items = table(
            "items",
            vec![
                column("id", true),
                column("tenant_id", true),
                column("code", true),
                column("optional", false),
                column("payload", false),
            ],
        );
        let mut rows = vec![
            // 故意打乱输入顺序：装配器必须按 INDKEY 位序恢复。
            index_row("pk_items", 2, Some("id"), true, false),
            index_row("pk_items", 1, Some("tenant_id"), true, false),
            // INCLUDE 列不参与唯一键。
            index_row("uk_code", 1, Some("code"), true, false),
            index_row("uk_code", 2, Some("payload"), false, true),
            index_row("uk_optional", 1, Some("optional"), false, false),
            index_row("uk_expr", 1, None, false, false),
        ];
        let mut partial = index_row("uk_partial", 1, Some("code"), true, false);
        partial.partial = true;
        rows.push(partial);
        let mut invalid = index_row("uk_invalid", 1, Some("code"), true, false);
        invalid.valid = false;
        rows.push(invalid);

        let evidence = evidence_from_rows(&[items], &[], &rows);
        assert!(
            evidence
                .keys
                .iter()
                .any(|key| key.columns == ["tenant_id", "id"]),
            "{:?}",
            evidence.keys
        );
        assert!(
            evidence.keys.iter().any(|key| key.columns == ["code"]),
            "INCLUDE 列不应进入唯一键：{:?}",
            evidence.keys
        );
        assert_eq!(evidence.keys.len(), 2, "{:?}", evidence.keys);
        for expected in ["可为 NULL", "部分索引", "表达式", "indisvalid=false"] {
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
}
