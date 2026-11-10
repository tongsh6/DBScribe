//! SQL Server 元数据读取。
//!
//! 通过系统视图（`sys.tables` / `sys.columns` / `sys.extended_properties`）读取表与列，
//! 中文描述存放在扩展属性 `MS_Description` 中。

use crate::options::ConnectionOptions;
use anyhow::{Context, Result};
use dbs_core::{ColumnInfo, ForeignKey, IndexInfo, TableInfo};
use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type SqlServerClient = Client<Compat<TcpStream>>;

const DATABASE_SQL: &str = r#"
SELECT name
FROM sys.databases
WHERE database_id > 4
ORDER BY name
"#;

const TABLE_SQL: &str = r#"
SELECT t.name,
       ISNULL(CAST(ep.value AS NVARCHAR(MAX)), ''),
       SCHEMA_NAME(t.schema_id) AS schema_name
FROM sys.tables t
LEFT JOIN sys.extended_properties ep
       ON ep.major_id = t.object_id
      AND ep.minor_id = 0
      AND ep.name = 'MS_Description'
ORDER BY SCHEMA_NAME(t.schema_id), t.name
"#;

const COLUMN_SQL: &str = r#"
SELECT t.name AS table_name,
       c.column_id,
       c.name AS column_name,
       ty.name AS data_type,
       c.max_length,
       c.precision,
       c.scale,
       c.is_nullable,
       c.is_identity,
       ISNULL(CAST(dc.definition AS NVARCHAR(MAX)), '') AS default_value,
       ISNULL(CAST(ep.value AS NVARCHAR(MAX)), '') AS comment,
       CAST(CASE WHEN pk.column_id IS NOT NULL THEN 1 ELSE 0 END AS BIT) AS is_primary,
       SCHEMA_NAME(t.schema_id) AS schema_name
FROM sys.columns c
JOIN sys.tables t ON t.object_id = c.object_id
JOIN sys.types ty ON ty.user_type_id = c.user_type_id
LEFT JOIN sys.default_constraints dc
       ON dc.parent_object_id = c.object_id
      AND dc.parent_column_id = c.column_id
LEFT JOIN sys.extended_properties ep
       ON ep.major_id = c.object_id
      AND ep.minor_id = c.column_id
      AND ep.name = 'MS_Description'
LEFT JOIN (
    SELECT ic.object_id, ic.column_id
    FROM sys.index_columns ic
    JOIN sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id
    WHERE i.is_primary_key = 1
) pk ON pk.object_id = c.object_id AND pk.column_id = c.column_id
ORDER BY SCHEMA_NAME(t.schema_id), t.name, c.column_id
"#;

const INDEX_SQL: &str = r#"
SELECT SCHEMA_NAME(t.schema_id) AS schema_name,
       t.name AS table_name,
       i.name AS index_name,
       i.is_unique,
       i.is_primary_key,
       c.name AS column_name
FROM sys.indexes i
JOIN sys.tables t ON t.object_id = i.object_id
JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id
JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
WHERE i.name IS NOT NULL AND ic.is_included_column = 0
ORDER BY schema_name, t.name, i.name, ic.key_ordinal
"#;

const FK_SQL: &str = r#"
SELECT SCHEMA_NAME(t.schema_id) AS schema_name,
       t.name AS table_name,
       fk.name AS fk_name,
       pc.name AS column_name,
       SCHEMA_NAME(rt.schema_id) AS ref_schema,
       rt.name AS ref_table,
       rc.name AS ref_column
FROM sys.foreign_keys fk
JOIN sys.tables t ON t.object_id = fk.parent_object_id
JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
JOIN sys.tables rt ON rt.object_id = fk.referenced_object_id
JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id
ORDER BY schema_name, t.name, fk.name, fkc.constraint_column_id
"#;

const PARTITION_SQL: &str = r#"
SELECT SCHEMA_NAME(t.schema_id) AS schema_name, t.name AS table_name
FROM sys.tables t
JOIN sys.indexes i ON i.object_id = t.object_id AND i.index_id IN (0, 1)
JOIN sys.partitions p ON p.object_id = t.object_id AND p.index_id = i.index_id
WHERE p.partition_number > 1
GROUP BY SCHEMA_NAME(t.schema_id), t.name
"#;

async fn connect(opts: &ConnectionOptions, database: &str) -> Result<SqlServerClient> {
    let mut config = Config::new();
    config.host(&opts.host);
    config.port(opts.port);
    config.authentication(AuthMethod::sql_server(&opts.user, &opts.password));
    if !database.is_empty() {
        config.database(database);
    }
    // 本地/内网自建实例通常使用自签证书
    config.trust_cert();

    let tcp = TcpStream::connect(config.get_addr())
        .await
        .with_context(|| format!("连接 SQL Server 失败：{}:{}", opts.host, opts.port))?;
    tcp.set_nodelay(true).ok();

    Client::connect(config, tcp.compat_write())
        .await
        .context("SQL Server 握手失败（请检查账号密码与 TLS 设置）")
}

/// 列出用户数据库（排除系统库）。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    let mut client = connect(opts, "").await?;
    let rows = client
        .query(DATABASE_SQL, &[])
        .await
        .context("查询数据库清单失败")?
        .into_first_result()
        .await
        .context("读取数据库清单失败")?;

    Ok(rows
        .iter()
        .filter_map(|row| row.get::<&str, _>(0).map(|s| s.to_string()))
        .collect())
}

/// 读取指定库的全部表（不含视图）及其列信息。
pub async fn load_tables(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    let mut client = connect(opts, &opts.database).await?;

    let table_rows = client
        .query(TABLE_SQL, &[])
        .await
        .context("查询表清单失败")?
        .into_first_result()
        .await
        .context("读取表清单失败")?;

    let column_rows = client
        .query(COLUMN_SQL, &[])
        .await
        .context("查询列信息失败")?
        .into_first_result()
        .await
        .context("读取列信息失败")?;

    let index_rows = client
        .query(INDEX_SQL, &[])
        .await
        .context("查询索引失败")?
        .into_first_result()
        .await
        .context("读取索引失败")?;

    let fk_rows = client
        .query(FK_SQL, &[])
        .await
        .context("查询外键失败")?
        .into_first_result()
        .await
        .context("读取外键失败")?;

    let partition_rows = client
        .query(PARTITION_SQL, &[])
        .await
        .context("查询分区失败")?
        .into_first_result()
        .await
        .context("读取分区失败")?;

    let text = |row: &tiberius::Row, idx: usize| -> String {
        row.get::<&str, _>(idx)
            .map(|s| s.to_string())
            .unwrap_or_default()
    };

    // key = "schema.table"
    let mut columns_by_table: std::collections::HashMap<String, Vec<ColumnInfo>> =
        std::collections::HashMap::new();

    for row in &column_rows {
        let table_name = text(row, 0);
        let schema = text(row, 12);
        let key = format!("{schema}.{table_name}");
        let data_type = text(row, 3);
        let max_length = row.get::<i16, _>(4).unwrap_or(0);
        let precision = row.get::<u8, _>(5).unwrap_or(0);
        let scale = row.get::<u8, _>(6).unwrap_or(0);

        columns_by_table.entry(key).or_default().push(ColumnInfo {
            order: row.get::<i32, _>(1).unwrap_or(0) as u32,
            name: text(row, 2),
            length: format_length(&data_type, max_length, precision, scale),
            data_type,
            is_primary_key: row.get::<bool, _>(11).unwrap_or(false),
            is_auto_increment: row.get::<bool, _>(8).unwrap_or(false),
            nullable: row.get::<bool, _>(7).unwrap_or(false),
            default_value: text(row, 9),
            comment: text(row, 10),
        });
    }

    // 索引
    let mut indexes_by_table: std::collections::HashMap<String, Vec<IndexInfo>> =
        std::collections::HashMap::new();
    for row in &index_rows {
        let schema = text(row, 0);
        let table_name = text(row, 1);
        let index_name = text(row, 2);
        let is_unique = row.get::<bool, _>(3).unwrap_or(false);
        let is_primary = row.get::<bool, _>(4).unwrap_or(false);
        let column_name = text(row, 5);
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
    let mut fks_by_table: std::collections::HashMap<String, Vec<ForeignKey>> =
        std::collections::HashMap::new();
    for row in &fk_rows {
        let schema = text(row, 0);
        let table_name = text(row, 1);
        let fk_name = text(row, 2);
        let column_name = text(row, 3);
        let ref_schema = text(row, 4);
        let ref_table = text(row, 5);
        let ref_column = text(row, 6);
        let key = format!("{schema}.{table_name}");

        let list = fks_by_table.entry(key).or_default();
        match list.iter_mut().find(|f| f.name == fk_name) {
            Some(existing) => {
                existing.columns.push(column_name);
                existing.ref_columns.push(ref_column);
            }
            None => list.push(ForeignKey {
                name: fk_name,
                columns: vec![column_name],
                ref_schema,
                ref_table,
                ref_columns: vec![ref_column],
            }),
        }
    }

    // 分区表
    let partitioned: std::collections::HashSet<String> = partition_rows
        .iter()
        .map(|row| format!("{}.{}", text(row, 0), text(row, 1)))
        .collect();

    let mut tables = Vec::with_capacity(table_rows.len());
    for (idx, row) in table_rows.iter().enumerate() {
        let name = text(row, 0);
        let comment = text(row, 1);
        let schema = text(row, 2);
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
    let tables = load_tables(opts).await?;
    tables
        .into_iter()
        .find(|t| t.name == table)
        .map(|t| t.columns)
        .with_context(|| format!("表 {table} 不存在"))
}

/// 把 SQL Server 的类型元数据拼成展示用的长度。
///
/// `nvarchar(64)` 的 `max_length` 是字节数（128），需要除以 2；
/// `decimal(10,2)` 用 precision + scale。
fn format_length(data_type: &str, max_length: i16, precision: u8, scale: u8) -> String {
    match data_type.to_lowercase().as_str() {
        "nvarchar" | "nchar" => {
            if max_length < 0 {
                "MAX".to_string()
            } else {
                (max_length / 2).to_string()
            }
        }
        "varchar" | "char" | "varbinary" | "binary" => {
            if max_length < 0 {
                "MAX".to_string()
            } else {
                max_length.to_string()
            }
        }
        "decimal" | "numeric" => format!("{precision},{scale}"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_length_by_type() {
        assert_eq!(format_length("nvarchar", 128, 0, 0), "64");
        assert_eq!(format_length("varchar", 64, 0, 0), "64");
        assert_eq!(format_length("nvarchar", -1, 0, 0), "MAX");
        assert_eq!(format_length("decimal", 0, 10, 2), "10,2");
        assert_eq!(format_length("int", 4, 0, 0), "");
        assert_eq!(format_length("datetime2", 8, 0, 0), "");
    }
}
