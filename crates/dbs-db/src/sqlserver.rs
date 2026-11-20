//! SQL Server 元数据读取。
//!
//! 通过系统视图（`sys.tables` / `sys.columns` / `sys.extended_properties`）读取表与列，
//! 中文描述存放在扩展属性 `MS_Description` 中。

use crate::options::ConnectionOptions;
use crate::remote_er::{ErEvidence, IndexCandidate, KeyPart};
use anyhow::{Context, Result};
use dbs_core::er::{ErCatalog, ErColumnType, TableId};
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
WHERE i.name IS NOT NULL AND ic.is_included_column = 0 AND ic.key_ordinal > 0
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

/// ER 类型签名：长度、精度、小数位、字符排序规则和用户定义类型都要保留。
const ER_TYPE_SQL: &str = r#"
SELECT SCHEMA_NAME(t.schema_id) AS schema_name,
       t.name AS table_name,
       c.name AS column_name,
       ty.name AS data_type,
       c.max_length,
       c.precision,
       c.scale,
       ISNULL(c.collation_name, '') AS collation_name,
       CASE WHEN ty.is_user_defined = 1
            THEN SCHEMA_NAME(ty.schema_id) ELSE ''
       END AS type_schema
FROM sys.columns c
JOIN sys.tables t ON t.object_id = c.object_id
JOIN sys.types ty ON ty.user_type_id = c.user_type_id
ORDER BY schema_name, t.name, c.column_id
"#;

/// ER 唯一索引：INCLUDE 列不取；筛选、禁用、假设和列存索引保留状态，
/// 由装配器拒绝并说明具体不足。
const ER_INDEX_SQL: &str = r#"
SELECT SCHEMA_NAME(t.schema_id) AS schema_name,
       t.name AS table_name,
       i.name AS index_name,
       i.is_primary_key,
       i.is_disabled,
       i.has_filter,
       i.is_hypothetical,
       i.type,
       ic.key_ordinal,
       c.name AS column_name,
       c.is_nullable
FROM sys.indexes i
JOIN sys.tables t ON t.object_id = i.object_id
JOIN sys.index_columns ic
       ON ic.object_id = i.object_id
      AND ic.index_id = i.index_id
      AND ic.is_included_column = 0
      AND ic.key_ordinal > 0
JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
WHERE i.is_unique = 1 AND i.name IS NOT NULL
ORDER BY schema_name, t.name, i.name, ic.key_ordinal
"#;

#[derive(Debug, Clone)]
struct SqlServerTypeRow {
    schema: String,
    table: String,
    column: String,
    data_type: String,
    max_length: i16,
    precision: u8,
    scale: u8,
    collation: String,
    type_schema: String,
}

#[derive(Debug, Clone)]
struct SqlServerIndexRow {
    schema: String,
    table: String,
    index: String,
    primary: bool,
    disabled: bool,
    filtered: bool,
    hypothetical: bool,
    index_type: u8,
    key_ordinal: u8,
    column: String,
    nullable: bool,
}

/// 读取 ER 专用证据：完整类型签名与可用唯一索引的键位。
pub(crate) async fn load_er_catalog(opts: &ConnectionOptions) -> Result<ErCatalog> {
    let tables = load_tables(opts).await?;
    let evidence = read_er_evidence(opts, &tables).await;
    Ok(crate::remote_er::assemble_read(tables, evidence))
}

async fn read_er_evidence(opts: &ConnectionOptions, tables: &[TableInfo]) -> Result<ErEvidence> {
    let mut client = connect(opts, &opts.database).await?;
    let text = |row: &tiberius::Row, idx: usize| -> String {
        row.get::<&str, _>(idx)
            .map(|s| s.to_string())
            .unwrap_or_default()
    };

    let type_rows = client
        .query(ER_TYPE_SQL, &[])
        .await
        .context("查询 SQL Server ER 类型信息失败")?
        .into_first_result()
        .await
        .context("读取 SQL Server ER 类型信息失败")?;
    let type_rows: Vec<SqlServerTypeRow> = type_rows
        .iter()
        .map(|row| SqlServerTypeRow {
            schema: text(row, 0),
            table: text(row, 1),
            column: text(row, 2),
            data_type: text(row, 3),
            max_length: row.get::<i16, _>(4).unwrap_or(0),
            precision: row.get::<u8, _>(5).unwrap_or(0),
            scale: row.get::<u8, _>(6).unwrap_or(0),
            collation: text(row, 7),
            type_schema: text(row, 8),
        })
        .collect();

    let index_rows = client
        .query(ER_INDEX_SQL, &[])
        .await
        .context("查询 SQL Server ER 唯一索引失败")?
        .into_first_result()
        .await
        .context("读取 SQL Server ER 唯一索引失败")?;
    let index_rows: Vec<SqlServerIndexRow> = index_rows
        .iter()
        .map(|row| SqlServerIndexRow {
            schema: text(row, 0),
            table: text(row, 1),
            index: text(row, 2),
            primary: row.get::<bool, _>(3).unwrap_or(false),
            disabled: row.get::<bool, _>(4).unwrap_or(false),
            filtered: row.get::<bool, _>(5).unwrap_or(false),
            hypothetical: row.get::<bool, _>(6).unwrap_or(false),
            index_type: row.get::<u8, _>(7).unwrap_or(0),
            key_ordinal: row.get::<u8, _>(8).unwrap_or(0),
            column: text(row, 9),
            nullable: row.get::<bool, _>(10).unwrap_or(true),
        })
        .collect();

    Ok(evidence_from_rows(tables, &type_rows, &index_rows))
}

/// 把 SQL Server 目录行装配成 ER 证据；不连接数据库，便于验证筛选/禁用规则。
fn evidence_from_rows(
    tables: &[TableInfo],
    type_rows: &[SqlServerTypeRow],
    index_rows: &[SqlServerIndexRow],
) -> ErEvidence {
    let mut evidence = ErEvidence::default();

    for row in type_rows {
        let signature = sqlserver_type_signature(
            &row.data_type,
            row.max_length,
            row.precision,
            row.scale,
            &row.collation,
            &row.type_schema,
        );
        if signature.is_empty() {
            match tables.iter().find(|table| {
                table.schema.eq_ignore_ascii_case(&row.schema)
                    && table.name.eq_ignore_ascii_case(&row.table)
            }) {
                Some(table) => evidence.warn_table(
                    table,
                    format!(
                        "SQL Server 表 {}.{} 的列 {} 没有可比较的类型签名",
                        row.schema, row.table, row.column
                    ),
                ),
                None => evidence.warn(format!(
                    "SQL Server 表 {}.{} 的列 {} 没有可比较的类型签名",
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

    let mut grouped: std::collections::BTreeMap<
        (String, String),
        std::collections::BTreeMap<String, Vec<&SqlServerIndexRow>>,
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
            rows.sort_by_key(|row| row.key_ordinal);
            let mut candidate =
                IndexCandidate::new(name.clone(), rows.iter().any(|r| r.primary), Vec::new());
            let mut gap = Vec::new();
            for row in rows {
                if row.disabled {
                    gap.push("索引已禁用，不再维护唯一性".to_string());
                }
                if row.filtered {
                    gap.push("筛选索引只覆盖满足条件的行".to_string());
                }
                if row.hypothetical {
                    gap.push("假设索引不实际维护".to_string());
                }
                if !matches!(row.index_type, 1 | 2) {
                    gap.push(format!("索引类型 {} 不是行存 B 树索引", row.index_type));
                }
                candidate
                    .parts
                    .push(KeyPart::column(row.column.clone(), row.nullable));
            }
            if !gap.is_empty() {
                gap.sort();
                gap.dedup();
                candidate = candidate.unusable(gap.join("；"));
            }
            candidates.push(candidate);
        }
        // SQL Server 唯一索引把 NULL 也视为相同值，因此可空列不影响全表唯一性。
        let keys =
            crate::remote_er::collect_keys("SQL Server", table, &candidates, false, &mut evidence);
        evidence.keys.extend(keys);
    }

    evidence
}

/// SQL Server 完整类型签名。
///
/// `nvarchar` 的 `max_length` 是字节数，需要换算成字符；`decimal` 用
/// precision + scale；`datetime2` / `time` / `datetimeoffset` 用小数秒；
/// `float` 用二进制精度；字符列再追加排序规则。
pub(crate) fn sqlserver_type_signature(
    data_type: &str,
    max_length: i16,
    precision: u8,
    scale: u8,
    collation: &str,
    type_schema: &str,
) -> String {
    let name = data_type.trim().to_ascii_lowercase();
    if name.is_empty() {
        return String::new();
    }
    let qualified = if type_schema.trim().is_empty() {
        name.clone()
    } else {
        format!("{}.{}", type_schema.trim(), name)
    };

    let mut signature = match name.as_str() {
        "nvarchar" | "nchar" => format!("{qualified}({})", unicode_length(max_length)),
        "varchar" | "char" | "varbinary" | "binary" => {
            format!("{qualified}({})", byte_length(max_length))
        }
        "decimal" | "numeric" => format!("{qualified}({precision},{scale})"),
        "datetime2" | "time" | "datetimeoffset" => format!("{qualified}({scale})"),
        "float" => format!("{qualified}({precision})"),
        _ => qualified,
    };

    if matches!(
        name.as_str(),
        "nvarchar" | "nchar" | "varchar" | "char" | "text" | "ntext" | "sysname"
    ) && !collation.trim().is_empty()
    {
        signature.push_str(" collate ");
        signature.push_str(collation.trim());
    }
    signature
}

fn unicode_length(max_length: i16) -> String {
    if max_length < 0 {
        "max".to_string()
    } else {
        (max_length / 2).to_string()
    }
}

fn byte_length(max_length: i16) -> String {
    if max_length < 0 {
        "max".to_string()
    } else {
        max_length.to_string()
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

    #[test]
    fn type_signature_keeps_length_scale_and_collation() {
        assert_eq!(
            sqlserver_type_signature("nvarchar", 128, 0, 0, "Latin1_General_CI_AS", ""),
            "nvarchar(64) collate Latin1_General_CI_AS"
        );
        assert_eq!(
            sqlserver_type_signature("nvarchar", -1, 0, 0, "", ""),
            "nvarchar(max)"
        );
        assert_eq!(
            sqlserver_type_signature("decimal", 0, 10, 2, "", ""),
            "decimal(10,2)"
        );
        assert_eq!(
            sqlserver_type_signature("datetime2", 8, 0, 7, "", ""),
            "datetime2(7)"
        );
        assert_eq!(
            sqlserver_type_signature("float", 8, 53, 0, "", ""),
            "float(53)"
        );
        assert_eq!(
            sqlserver_type_signature("varchar", 64, 0, 0, "", ""),
            "varchar(64)"
        );
        assert_eq!(sqlserver_type_signature("int", 4, 0, 0, "", ""), "int");
        assert_eq!(
            sqlserver_type_signature("phone", 40, 0, 0, "", "dbo"),
            "dbo.phone"
        );
    }

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

    fn item_table() -> TableInfo {
        TableInfo {
            order: 1,
            schema: "dbo".into(),
            name: "items".into(),
            columns: vec![
                column("id", false),
                column("code", false),
                column("legacy", true),
            ],
            ..Default::default()
        }
    }

    fn index_row(
        index: &str,
        primary: bool,
        index_type: u8,
        column: &str,
        nullable: bool,
    ) -> SqlServerIndexRow {
        SqlServerIndexRow {
            schema: "dbo".into(),
            table: "items".into(),
            index: index.into(),
            primary,
            disabled: false,
            filtered: false,
            hypothetical: false,
            index_type,
            key_ordinal: 1,
            column: column.into(),
            nullable,
        }
    }

    #[test]
    fn filtered_disabled_hypothetical_and_columnstore_indexes_are_not_keys() {
        let mut rows = vec![
            index_row("pk_items", true, 1, "id", false),
            index_row("uk_code", false, 2, "code", false),
            // SQL Server 唯一索引允许一个 NULL，仍是全表唯一键。
            index_row("uk_legacy", false, 2, "legacy", true),
        ];
        let mut filtered = index_row("uk_filtered", false, 2, "code", false);
        filtered.filtered = true;
        rows.push(filtered);
        let mut disabled = index_row("uk_disabled", false, 2, "code", false);
        disabled.disabled = true;
        rows.push(disabled);
        let mut hypothetical = index_row("uk_hypothetical", false, 2, "code", false);
        hypothetical.hypothetical = true;
        rows.push(hypothetical);
        rows.push(index_row("uk_columnstore", false, 5, "code", false));

        let evidence = evidence_from_rows(&[item_table()], &[], &rows);
        assert_eq!(evidence.keys.len(), 3, "{:?}", evidence.keys);
        for expected in [vec!["id"], vec!["code"], vec!["legacy"]] {
            assert!(
                evidence.keys.iter().any(|key| key.columns == expected),
                "缺少键 {expected:?}：{:?}",
                evidence.keys
            );
        }
        for expected in ["禁用", "筛选", "假设", "行存 B 树"] {
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
