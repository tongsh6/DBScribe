//! 数据库元数据读取层。
//!
//! 通过统一的 [`ConnectionOptions`] 调用，按 [`DbType`] 分发到具体实现。
//! 当前支持 MySQL / MariaDB 与 PostgreSQL。

pub mod mysql;
pub mod options;
pub mod postgres;

use anyhow::{bail, Result};
use dbs_core::{ColumnInfo, DbType, TableInfo};

pub use options::ConnectionOptions;

/// 列出可访问的数据库（排除系统库 / 模板库）。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    match opts.db_type {
        DbType::MySql => mysql::list_databases(opts).await,
        DbType::PostgreSql => postgres::list_databases(opts).await,
        other => bail!("暂不支持的数据库类型：{other}（当前支持 mysql、postgres）"),
    }
}

/// 读取指定库的全部基础表（不含视图）及其列信息。
pub async fn load_tables(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    match opts.db_type {
        DbType::MySql => mysql::load_tables(opts).await,
        DbType::PostgreSql => postgres::load_tables(opts).await,
        other => bail!("暂不支持的数据库类型：{other}（当前支持 mysql、postgres）"),
    }
}

/// 读取单张表的列信息（界面预览用）。
pub async fn load_table_columns(opts: &ConnectionOptions, table: &str) -> Result<Vec<ColumnInfo>> {
    match opts.db_type {
        DbType::MySql => mysql::load_table_columns(opts, table).await,
        DbType::PostgreSql => postgres::load_table_columns(opts, table).await,
        other => bail!("暂不支持的数据库类型：{other}（当前支持 mysql、postgres）"),
    }
}
