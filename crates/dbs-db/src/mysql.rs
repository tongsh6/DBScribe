//! MySQL / MariaDB 元数据读取。
//!
//! 直接查 `information_schema`，一次拿全库表清单与列信息，避免 N+1 查询。

use anyhow::{Context, Result};
use dbs_core::{ColumnInfo, ForeignKey, IndexInfo, TableInfo};
use mysql_async::prelude::*;
use mysql_async::{OptsBuilder, Pool, Row};

use crate::options::ConnectionOptions;

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

/// 列出可导出的数据库（排除系统库）。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    let builder = OptsBuilder::default()
        .ip_or_hostname(opts.host.clone())
        .tcp_port(opts.port)
        .user(Some(opts.user.clone()))
        .pass(Some(opts.password.clone()))
        .init(vec!["SET NAMES utf8mb4".to_string()]);

    let pool = Pool::new(builder);
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
    let builder = OptsBuilder::default()
        .ip_or_hostname(opts.host.clone())
        .tcp_port(opts.port)
        .user(Some(opts.user.clone()))
        .pass(Some(opts.password.clone()))
        .init(vec!["SET NAMES utf8mb4".to_string()])
        .db_name(Some(opts.database.clone()));

    let pool = Pool::new(builder);
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
    let builder = OptsBuilder::default()
        .ip_or_hostname(opts.host.clone())
        .tcp_port(opts.port)
        .user(Some(opts.user.clone()))
        .pass(Some(opts.password.clone()))
        .init(vec!["SET NAMES utf8mb4".to_string()])
        .db_name(Some(opts.database.clone()));

    let pool = Pool::new(builder);
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

    #[test]
    fn extract_length_variants() {
        assert_eq!(extract_length("varchar(64)"), "64");
        assert_eq!(extract_length("decimal(10,2)"), "10,2");
        assert_eq!(extract_length("bigint(20) unsigned"), "20");
        assert_eq!(extract_length("datetime"), "");
        assert_eq!(extract_length("enum('a','b')"), "'a','b'");
        assert_eq!(extract_length("bad("), "");
    }
}
