//! 达梦 DM8 元数据读取（通过 ODBC）。
//!
//! 达梦的数据字典视图与 Oracle 高度兼容（`all_tables` / `all_tab_columns` /
//! `all_constraints` / `all_indexes`…），因此 SQL 与 [`crate::oracle`] 基本一致，
//! 差异只在于：传输层走 ODBC、参数直接内联（ODBC 占位符风格不同）。

use crate::odbc_util;
use crate::options::ConnectionOptions;
use anyhow::{Context, Result};
use dbs_core::{ColumnInfo, ForeignKey, IndexInfo, TableInfo};

#[cfg(not(target_os = "macos"))]
use odbc_api::{
    Connection, ConnectionOptions as OdbcOptions, Cursor, Environment, ResultSetMetadata,
};

/// 达梦数据字典查询（与 Oracle 版一致，只是当前用户/模式用字面量内联）。
const TABLE_SQL: &str = r#"
SELECT t.table_name, NVL(c.comments, ' ')
FROM all_tables t
LEFT JOIN all_tab_comments c
       ON c.owner = t.owner AND c.table_name = t.table_name
WHERE t.owner = '{OWNER}'
ORDER BY t.table_name
"#;

const COLUMN_SQL: &str = r#"
SELECT c.table_name,
       c.column_id,
       c.column_name,
       c.data_type,
       c.data_length,
       c.data_precision,
       c.data_scale,
       c.nullable,
       'NO' AS ident,
       c.data_default,
       NVL(cc.comments, ' '),
       CASE WHEN pk.column_name IS NULL THEN 0 ELSE 1 END
FROM all_tab_columns c
LEFT JOIN all_col_comments cc
       ON cc.owner = c.owner
      AND cc.table_name = c.table_name
      AND cc.column_name = c.column_name
LEFT JOIN (
    SELECT cc2.owner, cc2.table_name, cc2.column_name
    FROM all_constraints cst
    JOIN all_cons_columns cc2
      ON cc2.owner = cst.owner AND cc2.constraint_name = cst.constraint_name
    WHERE cst.constraint_type = 'P'
) pk ON pk.owner = c.owner
    AND pk.table_name = c.table_name
    AND pk.column_name = c.column_name
WHERE c.owner = '{OWNER}'
ORDER BY c.table_name, c.column_id
"#;

const INDEX_SQL: &str = r#"
SELECT i.table_name, i.index_name, i.uniqueness, ic.column_name
FROM all_indexes i
JOIN all_ind_columns ic
  ON ic.index_owner = i.owner AND ic.index_name = i.index_name
WHERE i.table_owner = '{OWNER}'
ORDER BY i.table_name, i.index_name, ic.column_position
"#;

const PK_INDEX_SQL: &str = r#"
SELECT cc.table_name, c.index_name
FROM all_constraints c
JOIN all_cons_columns cc
  ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name
WHERE c.constraint_type = 'P' AND c.owner = '{OWNER}'
"#;

const FK_SQL: &str = r#"
SELECT c.table_name, c.constraint_name, cc.column_name,
       r.table_name AS ref_table, rcc.column_name AS ref_column
FROM all_constraints c
JOIN all_cons_columns cc
  ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name
JOIN all_constraints r
  ON r.owner = c.r_owner AND r.constraint_name = c.r_constraint_name
JOIN all_cons_columns rcc
  ON rcc.owner = r.owner AND rcc.constraint_name = r.constraint_name
 AND rcc.position = cc.position
WHERE c.constraint_type = 'R' AND c.owner = '{OWNER}'
ORDER BY c.table_name, c.constraint_name, cc.position
"#;

const PARTITION_SQL: &str = r#"
SELECT table_name FROM all_tables WHERE owner = '{OWNER}' AND partitioned = 'YES'
"#;

/// 达梦只有一个实例，这里返回当前用户可见的模式（按用户处理）。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    let owner = owner_of(opts);
    Ok(vec![owner])
}

/// 读取当前用户模式下的全部表。
pub async fn load_tables(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    #[cfg(target_os = "macos")]
    {
        let _ = opts;
        anyhow::bail!("达梦数据库暂不支持 macOS（官方未提供 macOS 版 ODBC 驱动）")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let opts = opts.clone();
        tokio::task::spawn_blocking(move || load_tables_blocking(&opts))
            .await
            .context("达梦读取任务异常退出")?
    }
}

/// 读取单张表的列信息（界面预览用）。
pub async fn load_table_columns(opts: &ConnectionOptions, table: &str) -> Result<Vec<ColumnInfo>> {
    let tables = load_tables(opts).await?;
    tables
        .into_iter()
        .find(|t| t.name.eq_ignore_ascii_case(table))
        .map(|t| t.columns)
        .with_context(|| format!("表 {table} 不存在"))
}

fn owner_of(opts: &ConnectionOptions) -> String {
    // 模式名 = 用户名（达梦默认大写）；去掉单引号防止 SQL 拼接被破坏
    opts.user
        .to_uppercase()
        .replace('\'', "")
        .trim()
        .to_string()
}

/// 构造 ODBC 连接串（先检测驱动，缺失时给出安装指引）。
#[cfg(not(target_os = "macos"))]
fn connection_string(opts: &ConnectionOptions) -> Result<String> {
    let status = odbc_util::detect();
    if !status.is_available() {
        anyhow::bail!("{}", odbc_util::install_guide());
    }
    let driver = match status {
        odbc_util::DriverStatus::Available { description } => description,
        _ => String::from("DM ODBC DRIVER"),
    };

    Ok(format!(
        "DRIVER={{{driver}}};SERVER={};PORT={};UID={};PWD={};",
        opts.host, opts.port, opts.user, opts.password
    ))
}

/// 逐行按文本读取（文本转换由 ODBC 驱动完成，避免为每种列类型写分支）。
#[cfg(not(target_os = "macos"))]
fn query_rows(conn: &Connection<'_>, sql: &str) -> Result<Vec<Vec<String>>> {
    let mut cursor = conn
        .execute(sql, (), None)
        .context("执行查询失败")?
        .context("查询没有返回结果集")?;

    let num_cols = cursor.num_result_cols().context("读取结果列数失败")?;
    let num_cols = u16::try_from(num_cols).unwrap_or(0);

    let mut rows = Vec::new();
    while let Some(mut row) = cursor.next_row().context("读取结果行失败")? {
        let mut values = Vec::with_capacity(num_cols as usize);
        for col in 1..=num_cols {
            let mut buffer = Vec::new();
            let present = row.get_text(col, &mut buffer).context("读取列值失败")?;
            values.push(if present {
                String::from_utf8_lossy(&buffer).trim().to_string()
            } else {
                String::new()
            });
        }
        rows.push(values);
    }
    Ok(rows)
}

#[cfg(not(target_os = "macos"))]
fn load_tables_blocking(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    let connection_string = connection_string(opts)?;
    let env = Environment::new().context("初始化 ODBC 环境失败")?;
    let conn = env
        .connect(&connection_string, "", "", OdbcOptions::default())
        .context("连接达梦失败（请检查地址、端口、账号密码）")?;
    let owner = owner_of(opts);
    let sql = |template: &str| template.replace("{OWNER}", &owner);

    let table_rows = query_rows(&conn, &sql(TABLE_SQL))?;
    let column_rows = query_rows(&conn, &sql(COLUMN_SQL))?;

    let mut columns_by_table: std::collections::HashMap<String, Vec<ColumnInfo>> =
        std::collections::HashMap::new();
    for row in &column_rows {
        let table_name = row.first().cloned().unwrap_or_default();
        let data_type = row.get(3).cloned().unwrap_or_default();
        let data_length = row.get(4).and_then(|v| v.parse::<i64>().ok());
        let precision = row.get(5).and_then(|v| v.parse::<i64>().ok());
        let scale = row.get(6).and_then(|v| v.parse::<i64>().ok());
        let identity = row.get(8).cloned().unwrap_or_default();

        columns_by_table
            .entry(table_name)
            .or_default()
            .push(ColumnInfo {
                order: row.get(1).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0),
                name: row.get(2).cloned().unwrap_or_default(),
                length: format_length(&data_type, data_length, precision, scale),
                data_type,
                is_primary_key: row.get(11).map(|v| v == "1").unwrap_or(false),
                // 达梦 all_tab_columns 没有 identity_column 字段，暂不识别自增
                is_auto_increment: identity.eq_ignore_ascii_case("YES"),
                nullable: row
                    .get(7)
                    .map(|v| v.eq_ignore_ascii_case("Y"))
                    .unwrap_or(false),
                default_value: row.get(9).cloned().unwrap_or_default(),
                comment: row.get(10).cloned().unwrap_or_default(),
            });
    }

    // 主键索引
    let mut pk_indexes: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for row in query_rows(&conn, &sql(PK_INDEX_SQL))? {
        if row.len() >= 2 {
            pk_indexes.insert((row[0].clone(), row[1].clone()));
        }
    }

    // 索引
    let mut indexes_by_table: std::collections::HashMap<String, Vec<IndexInfo>> =
        std::collections::HashMap::new();
    for row in query_rows(&conn, &sql(INDEX_SQL))? {
        let table_name = row.first().cloned().unwrap_or_default();
        let index_name = row.get(1).cloned().unwrap_or_default();
        let uniqueness = row.get(2).cloned().unwrap_or_default();
        let column_name = row.get(3).cloned().unwrap_or_default();
        let is_primary = pk_indexes.contains(&(table_name.clone(), index_name.clone()));

        let list = indexes_by_table.entry(table_name).or_default();
        match list.iter_mut().find(|i| i.name == index_name) {
            Some(existing) => existing.columns.push(column_name),
            None => list.push(IndexInfo {
                name: index_name,
                columns: vec![column_name],
                is_unique: uniqueness.eq_ignore_ascii_case("UNIQUE"),
                is_primary,
            }),
        }
    }

    // 外键
    let mut fks_by_table: std::collections::HashMap<String, Vec<ForeignKey>> =
        std::collections::HashMap::new();
    for row in query_rows(&conn, &sql(FK_SQL))? {
        let table_name = row.first().cloned().unwrap_or_default();
        let constraint = row.get(1).cloned().unwrap_or_default();
        let column_name = row.get(2).cloned().unwrap_or_default();
        let ref_table = row.get(3).cloned().unwrap_or_default();
        let ref_column = row.get(4).cloned().unwrap_or_default();

        let list = fks_by_table.entry(table_name).or_default();
        match list.iter_mut().find(|f| f.name == constraint) {
            Some(existing) => {
                existing.columns.push(column_name);
                existing.ref_columns.push(ref_column);
            }
            None => list.push(ForeignKey {
                name: constraint,
                columns: vec![column_name],
                ref_schema: owner.clone(),
                ref_table,
                ref_columns: vec![ref_column],
            }),
        }
    }

    // 分区表
    let partitioned: std::collections::HashSet<String> = query_rows(&conn, &sql(PARTITION_SQL))?
        .into_iter()
        .filter_map(|row| row.first().cloned())
        .collect();

    let mut tables = Vec::with_capacity(table_rows.len());
    for (idx, row) in table_rows.iter().enumerate() {
        let name = row.first().cloned().unwrap_or_default();
        let comment = row.get(1).cloned().unwrap_or_default();
        tables.push(TableInfo {
            order: (idx + 1) as u32,
            schema: owner.clone(),
            name: name.clone(),
            comment,
            columns: columns_by_table.remove(&name).unwrap_or_default(),
            indexes: indexes_by_table.remove(&name).unwrap_or_default(),
            foreign_keys: fks_by_table.remove(&name).unwrap_or_default(),
            is_partitioned: partitioned.contains(&name),
        });
    }

    tracing::info!(
        "已读取 {} 张表、{} 个列定义",
        tables.len(),
        tables.iter().map(|t| t.columns.len()).sum::<usize>()
    );
    Ok(tables)
}

/// 与 Oracle 一致的类型长度格式化。
fn format_length(
    data_type: &str,
    data_length: Option<i64>,
    precision: Option<i64>,
    scale: Option<i64>,
) -> String {
    match data_type.to_uppercase().as_str() {
        "NCHAR" | "NVARCHAR2" => data_length.map(|v| (v / 2).to_string()).unwrap_or_default(),
        "VARCHAR2" | "CHAR" | "VARCHAR" | "RAW" => {
            data_length.map(|v| v.to_string()).unwrap_or_default()
        }
        "NUMBER" | "DECIMAL" | "NUMERIC" => match (precision, scale) {
            (Some(p), Some(s)) => format!("{p},{s}"),
            (Some(p), None) => p.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_length_variants() {
        assert_eq!(format_length("VARCHAR2", Some(64), None, None), "64");
        assert_eq!(format_length("NVARCHAR2", Some(200), None, None), "100");
        assert_eq!(format_length("NUMBER", None, Some(10), Some(2)), "10,2");
        assert_eq!(format_length("DATE", Some(7), None, None), "");
    }

    #[test]
    fn owner_is_upper_and_stripped() {
        let mut opts = ConnectionOptions::new(
            dbs_core::DbType::Dameng,
            "127.0.0.1",
            5236,
            "SYSDBA",
            "x",
            "",
        );
        assert_eq!(owner_of(&opts), "SYSDBA");
        opts.user = "o'wner".to_string();
        assert_eq!(owner_of(&opts), "O WNER".replace(' ', ""));
    }
}
