//! Tauri 命令实现：连接测试、库表浏览、文档导出。
//!
//! 全部复用 `dbs-db` / `dbs-doc`，与 CLI 共享同一套核心逻辑。

use crate::models::{
    ConnectionParams, ExportParams, ExportResult, RenamedSheet, TableSummary,
};
use dbs_core::{DatabaseInfo, DbType, OutputFormat, SheetMode};
use dbs_db::{MySqlOptions, list_databases, load_tables};
use dbs_doc::{ExportOptions, export};
use std::path::Path;
use std::time::Instant;

fn mysql_options(conn: &ConnectionParams, database: &str) -> MySqlOptions {
    MySqlOptions {
        host: conn.host.clone(),
        port: conn.port,
        user: conn.user.clone(),
        password: conn.password.clone(),
        database: database.to_string(),
    }
}

fn to_msg(err: anyhow::Error) -> String {
    format!("{err:#}")
}

/// 测试连接，返回可访问的数据库数量。
#[tauri::command]
pub async fn test_connection(conn: ConnectionParams) -> Result<String, String> {
    let opts = mysql_options(&conn, "");
    let databases = list_databases(&opts).await.map_err(to_msg)?;
    Ok(format!("连接成功，可访问 {} 个数据库", databases.len()))
}

/// 列出可导出的数据库。
#[tauri::command]
pub async fn list_databases_cmd(conn: ConnectionParams) -> Result<Vec<String>, String> {
    let opts = mysql_options(&conn, "");
    list_databases(&opts).await.map_err(to_msg)
}

/// 列出指定库的表（含注释与列数）。
#[tauri::command]
pub async fn list_tables_cmd(
    conn: ConnectionParams,
    database: String,
) -> Result<Vec<TableSummary>, String> {
    let opts = mysql_options(&conn, &database);
    let tables = load_tables(&opts).await.map_err(to_msg)?;
    Ok(tables
        .into_iter()
        .map(|t| TableSummary {
            name: t.name,
            comment: t.comment,
            column_count: t.columns.len(),
        })
        .collect())
}

/// 导出文档。
#[tauri::command]
pub async fn export_document(
    conn: ConnectionParams,
    params: ExportParams,
) -> Result<ExportResult, String> {
    let started = Instant::now();

    let format: OutputFormat = params
        .format
        .parse()
        .map_err(|e: dbs_core::CoreError| e.to_string())?;
    let mode: SheetMode = params
        .sheet_mode
        .parse()
        .map_err(|e: dbs_core::CoreError| e.to_string())?;

    let out = Path::new(&params.out_path);
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建输出目录失败：{e}"))?;
        }
    }

    let opts = mysql_options(&conn, &params.database);
    let tables = load_tables(&opts).await.map_err(to_msg)?;
    if tables.is_empty() {
        return Err(format!("数据库 {} 中没有任何基础表", params.database));
    }

    let title = if params.title.trim().is_empty() {
        format!("{}数据库设计文档", params.database)
    } else {
        params.title.clone()
    };
    let options = ExportOptions::new(
        title,
        mode,
        DatabaseInfo::new(DbType::MySql, params.database.clone()),
        format,
    );

    let report = export(&tables, &options, out).map_err(to_msg)?;

    Ok(ExportResult {
        out_path: report.out_path.display().to_string(),
        table_count: report.table_count,
        sheet_count: report.sheet_count,
        renamed: report
            .renamed
            .into_iter()
            .map(|(original, actual)| RenamedSheet { original, actual })
            .collect(),
        elapsed_ms: started.elapsed().as_millis(),
    })
}
