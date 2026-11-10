//! 数据库元数据读取层。
//!
//! 通过统一的 [`ConnectionOptions`] 调用，按 [`DbType`] 分发到具体实现。
//! 支持 MySQL / MariaDB、PostgreSQL、SQLite、SQL Server 与 Oracle。

pub mod mysql;
pub mod options;
pub mod oracle;
pub mod oracle_client;
pub mod postgres;
pub mod sqlite;
pub mod sqlserver;

use anyhow::Result;
use dbs_core::{ColumnInfo, DbType, TableInfo};

pub use options::ConnectionOptions;
pub use oracle_client::{
    client_download_url, client_lib_name, ensure_client, ensure_initialized, installed_client_dir,
};

/// 列出可访问的数据库（排除系统库 / 模板库）。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    match opts.db_type {
        DbType::MySql => mysql::list_databases(opts).await,
        DbType::PostgreSql => postgres::list_databases(opts).await,
        DbType::Sqlite => sqlite::list_databases(opts).await,
        DbType::SqlServer => sqlserver::list_databases(opts).await,
        DbType::Oracle => oracle::list_databases(opts).await,
    }
}

/// 读取指定库的全部基础表（不含视图）及其列信息。
pub async fn load_tables(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    match opts.db_type {
        DbType::MySql => mysql::load_tables(opts).await,
        DbType::PostgreSql => postgres::load_tables(opts).await,
        DbType::Sqlite => sqlite::load_tables(opts).await,
        DbType::SqlServer => sqlserver::load_tables(opts).await,
        DbType::Oracle => oracle::load_tables(opts).await,
    }
}

/// 读取单张表的列信息（界面预览用）。
pub async fn load_table_columns(opts: &ConnectionOptions, table: &str) -> Result<Vec<ColumnInfo>> {
    match opts.db_type {
        DbType::MySql => mysql::load_table_columns(opts, table).await,
        DbType::PostgreSql => postgres::load_table_columns(opts, table).await,
        DbType::Sqlite => sqlite::load_table_columns(opts, table).await,
        DbType::SqlServer => sqlserver::load_table_columns(opts, table).await,
        DbType::Oracle => oracle::load_table_columns(opts, table).await,
    }
}
