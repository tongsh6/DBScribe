//! 数据库元数据读取层。
//!
//! 通过统一的 [`ConnectionOptions`] 调用，按 [`DbType`] 分发到具体实现。
//! 支持 MySQL / MariaDB、PostgreSQL、SQLite、SQL Server、Oracle、达梦 DM8 与 MongoDB。

mod er_scope;
mod mysql_views;
mod remote_er;
pub use er_scope::er_project_scope;

/// 只读取结构和目录证据；MongoDB 集合关系走独立的元数据读取，不采样。
pub async fn load_er_catalog(opts: &ConnectionOptions) -> anyhow::Result<dbs_core::er::ErCatalog> {
    let mut catalog = match opts.db_type {
        DbType::MongoDb => mongodb::er::load(opts).await,
        DbType::Sqlite => sqlite::er::load(opts).await,
        _ => remote_er::load(opts).await,
    }?;
    catalog.db_type = Some(opts.db_type);
    Ok(catalog)
}

pub mod dameng;
pub mod mongodb;
pub mod mysql;
pub mod odbc_util;
pub mod options;
pub mod oracle;
pub mod oracle_client;
pub mod postgres;
pub mod sqlite;
pub mod sqlserver;

use anyhow::Result;
use dbs_core::{ColumnInfo, DbType, TableInfo};

pub use options::{split_uri_password, ConnectionOptions};
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
        DbType::Dameng => dameng::list_databases(opts).await,
        DbType::MongoDb => mongodb::list_databases(opts).await,
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
        DbType::Dameng => dameng::load_tables(opts).await,
        DbType::MongoDb => mongodb::load_tables(opts).await,
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
        DbType::Dameng => dameng::load_table_columns(opts, table).await,
        DbType::MongoDb => mongodb::load_table_columns(opts, table).await,
    }
}
