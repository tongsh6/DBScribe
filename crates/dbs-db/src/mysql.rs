//! MySQL / MariaDB 元数据读取。
//!
//! 直接查 `information_schema`，一次拿全库表清单与列信息，避免 N+1 查询。

use anyhow::{Context, Result};
use dbs_core::{ColumnInfo, TableInfo};
use mysql_async::prelude::*;
use mysql_async::{OptsBuilder, Pool, Row};

/// MySQL 连接参数。
#[derive(Debug, Clone)]
pub struct MySqlOptions {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
}

impl MySqlOptions {
    /// 供日志展示的脱敏连接描述（不含密码）。
    pub fn describe(&self) -> String {
        format!(
            "mysql://{}@{}:{}/{}",
            self.user, self.host, self.port, self.database
        )
    }
}

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

/// 读取指定库的全部基础表（不含视图）及其列信息。
pub async fn load_tables(opts: &MySqlOptions) -> Result<Vec<TableInfo>> {
    let builder = OptsBuilder::default()
        .ip_or_hostname(opts.host.clone())
        .tcp_port(opts.port)
        .user(Some(opts.user.clone()))
        .pass(Some(opts.password.clone()))
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
        let ordinal = get_u32(&row, 1);
        let column_name = get_string(&row, 2);
        let data_type = get_string(&row, 3);
        let column_type = get_string(&row, 4);
        let column_key = get_string(&row, 5);
        let extra = get_string(&row, 6);
        let is_nullable = get_string(&row, 7);
        let default_value = get_opt_string(&row, 8);
        let comment = get_string(&row, 9);

        columns_by_table
            .entry(table_name)
            .or_default()
            .push(ColumnInfo {
                order: ordinal,
                name: column_name,
                // 原版 Excel 的「数据类型」列显示基础类型（如 varchar）
                data_type: data_type.to_lowercase(),
                // 长度从 COLUMN_TYPE 的括号里取：varchar(64) -> 64，decimal(10,2) -> 10,2
                length: extract_length(&column_type),
                is_primary_key: column_key.eq_ignore_ascii_case("PRI"),
                is_auto_increment: extra.to_lowercase().contains("auto_increment"),
                nullable: is_nullable.eq_ignore_ascii_case("YES"),
                default_value: default_value.unwrap_or_default(),
                comment,
            });
    }

    let mut tables = Vec::with_capacity(table_rows.len());
    for (idx, row) in table_rows.into_iter().enumerate() {
        let name = get_string(&row, 0);
        let comment = get_string(&row, 1);
        let columns = columns_by_table.remove(&name).unwrap_or_default();
        tables.push(TableInfo {
            order: (idx + 1) as u32,
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

    #[test]
    fn describe_masks_password() {
        let o = MySqlOptions {
            host: "127.0.0.1".into(),
            port: 3306,
            user: "root".into(),
            password: "secret".into(),
            database: "demo".into(),
        };
        let d = o.describe();
        assert!(!d.contains("secret"));
        assert!(d.contains("mysql://root@127.0.0.1:3306/demo"));
    }
}
