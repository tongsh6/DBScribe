//! SQLite 元数据读取。
//!
//! SQLite 是单文件数据库：`ConnectionOptions.database` 存的是 `.db` 文件路径。
//! 它没有原生的表 / 列注释，对应字段留空；自增表现为 `INTEGER PRIMARY KEY`
//! （rowid 别名）。

use crate::options::ConnectionOptions;
use anyhow::{Context, Result};
use dbs_core::{ColumnInfo, ForeignKey, IndexInfo, TableInfo};
use rusqlite::Connection;

/// SQLite 只有一个「库」，就是文件本身。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    if opts.database.trim().is_empty() {
        anyhow::bail!("SQLite 需要指定数据库文件路径");
    }
    Ok(vec![opts.database.clone()])
}

/// 读取该文件中所有表及其列信息。
pub async fn load_tables(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    let path = opts.database.clone();
    tokio::task::spawn_blocking(move || load_tables_blocking(&path))
        .await
        .context("SQLite 读取任务异常退出")?
}

/// 读取单张表的列信息（界面预览用）。
pub async fn load_table_columns(opts: &ConnectionOptions, table: &str) -> Result<Vec<ColumnInfo>> {
    let path = opts.database.clone();
    let table = table.to_string();
    tokio::task::spawn_blocking(move || {
        let conn =
            Connection::open(&path).with_context(|| format!("打开 SQLite 文件失败：{path}"))?;
        load_columns_blocking(&conn, &table)
    })
    .await
    .context("SQLite 读取任务异常退出")?
}

fn load_tables_blocking(path: &str) -> Result<Vec<TableInfo>> {
    let conn = Connection::open(path)
        .with_context(|| format!("打开 SQLite 文件失败：{path}（文件不存在或不是数据库）"))?;

    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut tables = Vec::with_capacity(names.len());
    for (idx, name) in names.iter().enumerate() {
        let columns = load_columns_blocking(&conn, name)
            .with_context(|| format!("读取表 {name} 的列信息失败"))?;
        let indexes = load_indexes_blocking(&conn, name)?;
        let foreign_keys = load_foreign_keys_blocking(&conn, name)?;
        tables.push(TableInfo {
            order: (idx + 1) as u32,
            // SQLite 单文件单 schema
            schema: "main".to_string(),
            name: name.clone(),
            // SQLite 没有表注释
            comment: String::new(),
            columns,
            indexes,
            foreign_keys,
            is_partitioned: false,
        });
    }

    tracing::info!(
        "已读取 {} 张表、{} 个列定义",
        tables.len(),
        tables.iter().map(|t| t.columns.len()).sum::<usize>()
    );
    Ok(tables)
}

/// 索引：`PRAGMA index_list` + `PRAGMA index_info`。
fn load_indexes_blocking(conn: &Connection, table: &str) -> Result<Vec<IndexInfo>> {
    let sql = format!("PRAGMA index_list({})", quote_ident(table));
    let mut stmt = conn.prepare(&sql)?;
    let entries = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?, // name
                row.get::<_, i64>(2)?,    // unique
                row.get::<_, String>(4)?, // origin: c=CREATE INDEX, u=UNIQUE, pk=PRIMARY KEY
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut indexes = Vec::with_capacity(entries.len());
    for (name, unique, origin) in entries {
        // origin 'pk' 对应主键索引，列标签里已有标记，但这里保留完整信息由导出层去重
        let index_sql = format!("PRAGMA index_info({})", quote_ident(&name));
        let mut index_stmt = conn.prepare(&index_sql)?;
        let columns: Vec<String> = index_stmt
            .query_map([], |row| row.get::<_, Option<String>>(2))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        indexes.push(IndexInfo {
            name,
            columns,
            is_unique: unique != 0,
            is_primary: origin == "pk",
        });
    }
    Ok(indexes)
}

/// 外键：`PRAGMA foreign_key_list`，同一个 id 的是复合外键。
fn load_foreign_keys_blocking(conn: &Connection, table: &str) -> Result<Vec<ForeignKey>> {
    let sql = format!("PRAGMA foreign_key_list({})", quote_ident(table));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,            // id（同组复合外键）
                row.get::<_, String>(2)?,         // 引用表
                row.get::<_, String>(3)?,         // 本表列
                row.get::<_, Option<String>>(4)?, // 引用列（NULL 表示主键）
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut result: Vec<ForeignKey> = Vec::new();
    for (id, ref_table, column, ref_column) in rows {
        let name = format!("fk_{table}_{id}");
        match result.iter_mut().find(|f| f.name == name) {
            Some(existing) => {
                existing.columns.push(column);
                existing.ref_columns.push(ref_column.unwrap_or_default());
            }
            None => result.push(ForeignKey {
                name,
                columns: vec![column],
                // SQLite 的外键可以跨 schema（ATTACH），这里默认 main
                ref_schema: "main".to_string(),
                ref_table,
                ref_columns: vec![ref_column.unwrap_or_default()],
            }),
        }
    }
    Ok(result)
}

fn load_columns_blocking(conn: &Connection, table: &str) -> Result<Vec<ColumnInfo>> {
    // PRAGMA 不支持参数绑定，表名需要做标识符转义
    let sql = format!("PRAGMA table_info({})", quote_ident(table));
    let mut stmt = conn.prepare(&sql)?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;

    let mut columns = Vec::new();
    for row in rows {
        let (cid, name, declared_type, not_null, default_value, pk) = row?;
        let (data_type, length) = split_declared_type(&declared_type);
        // INTEGER PRIMARY KEY 是 rowid 的别名，等价于自增
        let is_identity = pk > 0 && data_type.eq_ignore_ascii_case("INTEGER");

        columns.push(ColumnInfo {
            order: (cid + 1) as u32,
            name,
            data_type,
            length,
            is_primary_key: pk > 0,
            is_auto_increment: is_identity,
            // INTEGER PRIMARY KEY 隐式非空，PRAGMA 的 notnull 对它是 0
            nullable: not_null == 0 && pk == 0,
            default_value: default_value.unwrap_or_default(),
            comment: String::new(),
        });
    }
    Ok(columns)
}

/// SQLite 标识符用双引号转义。
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// 拆分 SQLite 声明类型。
///
/// `VARCHAR(64)` -> (`VARCHAR`, `64`)
/// `INTEGER`     -> (`INTEGER`, ``)
fn split_declared_type(declared: &str) -> (String, String) {
    let declared = declared.trim();
    match declared.find('(') {
        Some(pos) => {
            let name = declared[..pos].trim().to_string();
            let length = declared[pos + 1..].trim_end_matches(')').trim().to_string();
            (name, length)
        }
        None => (declared.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_declared_types() {
        assert_eq!(
            split_declared_type("VARCHAR(64)"),
            ("VARCHAR".to_string(), "64".to_string())
        );
        assert_eq!(
            split_declared_type("DECIMAL(10,2)"),
            ("DECIMAL".to_string(), "10,2".to_string())
        );
        assert_eq!(
            split_declared_type("INTEGER"),
            ("INTEGER".to_string(), String::new())
        );
    }

    #[test]
    fn quote_ident_escapes_double_quotes() {
        assert_eq!(quote_ident("normal"), "\"normal\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
    }
}
