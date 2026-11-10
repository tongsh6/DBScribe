//! Tauri 命令实现：连接测试、库表浏览、文档导出、本地配置与历史。
//!
//! 数据相关逻辑全部复用 `dbs-db` / `dbs-doc`，与 CLI 共享同一套核心实现。

use crate::models::{
    AppState, ColumnView, ConnectionParams, ExportParams, ExportResult, HistoryEntry,
    RenamedSheet, SavedConnection, TableSummary,
};
use crate::store;
use chrono::Local;
use dbs_core::{DatabaseInfo, DbType, OutputFormat, SheetMode};
use dbs_db::{MySqlOptions, list_databases, load_tables};
use dbs_doc::{ExportOptions, export};
use std::collections::HashSet;
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

/// 读取单张表的列结构（界面预览用）。
#[tauri::command]
pub async fn get_table_columns_cmd(
    conn: ConnectionParams,
    database: String,
    table: String,
) -> Result<Vec<ColumnView>, String> {
    let opts = mysql_options(&conn, &database);
    let columns = dbs_db::load_table_columns(&opts, &table)
        .await
        .map_err(to_msg)?;
    Ok(columns
        .into_iter()
        .map(|c| ColumnView {
            order: c.order,
            name: c.name,
            data_type: c.data_type,
            length: c.length,
            is_primary_key: c.is_primary_key,
            is_auto_increment: c.is_auto_increment,
            nullable: c.nullable,
            default_value: c.default_value,
            comment: c.comment,
        })
        .collect())
}

/// 读取本地保存的连接配置与导出历史。
#[tauri::command]
pub async fn load_app_state(app: tauri::AppHandle) -> Result<AppState, String> {
    Ok(store::load(&app))
}

/// 新增或更新一个连接（不保存密码）；`id` 为空时由后端生成。保存后自动设为当前连接。
#[tauri::command]
pub async fn save_connection(
    app: tauri::AppHandle,
    conn: SavedConnection,
) -> Result<SavedConnection, String> {
    let mut state = store::load(&app);
    let mut saved = conn;

    if saved.id.trim().is_empty() {
        saved.id = SavedConnection::new_id();
        state.connections.push(saved.clone());
    } else if let Some(existing) = state.connections.iter_mut().find(|c| c.id == saved.id) {
        *existing = saved.clone();
    } else {
        state.connections.push(saved.clone());
    }

    state.active_connection = Some(saved.id.clone());
    store::save(&app, &state).map_err(to_msg)?;
    Ok(saved)
}

/// 删除一个连接；若删除的是当前连接，自动切换到剩下的第一个。
#[tauri::command]
pub async fn delete_connection(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let mut state = store::load(&app);
    state.connections.retain(|c| c.id != id);
    if state.active_connection.as_deref() == Some(id.as_str()) {
        state.active_connection = state.connections.first().map(|c| c.id.clone());
    }
    store::save(&app, &state).map_err(to_msg)
}

/// 切换当前使用的连接。
#[tauri::command]
pub async fn set_active_connection(
    app: tauri::AppHandle,
    id: Option<String>,
) -> Result<(), String> {
    let mut state = store::load(&app);
    state.active_connection = id;
    store::save(&app, &state).map_err(to_msg)
}

/// 清空导出历史。
#[tauri::command]
pub async fn clear_history(app: tauri::AppHandle) -> Result<(), String> {
    let mut state = store::load(&app);
    state.history.clear();
    store::save(&app, &state).map_err(to_msg)
}

/// 导出文档。可只导出勾选的表；成功后写入导出历史。
#[tauri::command]
pub async fn export_document(
    app: tauri::AppHandle,
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
    let mut tables = load_tables(&opts).await.map_err(to_msg)?;
    if tables.is_empty() {
        return Err(format!("数据库 {} 中没有任何基础表", params.database));
    }

    // 仅导出勾选的表
    if let Some(wanted) = params.tables.as_ref().filter(|w| !w.is_empty()) {
        let wanted: HashSet<&str> = wanted.iter().map(String::as_str).collect();
        tables.retain(|t| wanted.contains(t.name.as_str()));
        if tables.is_empty() {
            return Err("选中的表在数据库中不存在，请重新读取表结构".to_string());
        }
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
    let elapsed_ms = started.elapsed().as_millis();

    let result = ExportResult {
        out_path: report.out_path.display().to_string(),
        table_count: report.table_count,
        sheet_count: report.sheet_count,
        renamed: report
            .renamed
            .into_iter()
            .map(|(original, actual)| RenamedSheet { original, actual })
            .collect(),
        elapsed_ms,
    };

    // 记录导出历史（失败不影响导出结果）
    let mut state = store::load(&app);
    state.history.insert(
        0,
        HistoryEntry {
            time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            database: params.database.clone(),
            format: format.to_string(),
            out_path: result.out_path.clone(),
            table_count: result.table_count,
            elapsed_ms,
        },
    );
    if let Err(err) = store::save(&app, &state) {
        eprintln!("[DBScribe] 写入导出历史失败：{err:#}");
    }

    Ok(result)
}
