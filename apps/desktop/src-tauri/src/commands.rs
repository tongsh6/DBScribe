//! Tauri 命令实现：连接测试、库表浏览、文档导出、本地配置与历史。
//!
//! 数据相关逻辑全部复用 `dbs-db` / `dbs-doc`，与 CLI 共享同一套核心实现。

use crate::models::{
    AppState, ColumnView, ConnectionParams, ExportParams, ExportProgress, ExportResult,
    HistoryEntry, Preferences, RenamedSheet, SavedConnection, TableSummary,
};
use crate::secrets;
use crate::store;
use chrono::Local;
use tauri::Emitter;
use dbs_core::{DatabaseInfo, DbType, OutputFormat, SheetMode};
use dbs_db::{ConnectionOptions, list_databases, load_tables};
use dbs_doc::{ExportOptions, export};
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

/// 把界面参数转成驱动层连接参数（按所选数据库类型）
fn to_options(conn: &ConnectionParams, database: &str) -> Result<ConnectionOptions, String> {
    let db_type: DbType = conn
        .db_type
        .parse()
        .map_err(|e: dbs_core::CoreError| e.to_string())?;
    Ok(ConnectionOptions::new(
        db_type,
        conn.host.clone(),
        conn.port,
        conn.user.clone(),
        conn.password.clone(),
        database,
    ))
}

fn to_msg(err: anyhow::Error) -> String {
    format!("{err:#}")
}

/// 测试连接，返回可访问的数据库数量。
#[tauri::command]
pub async fn test_connection(
    conn: ConnectionParams,
    database: Option<String>,
) -> Result<String, String> {
    // Oracle 通过服务名建立连接，其余数据库此处不需要库名
    let opts = to_options(&conn, database.as_deref().unwrap_or(""))?;
    let databases = list_databases(&opts).await.map_err(to_msg)?;
    Ok(format!("连接成功，可访问 {} 个数据库", databases.len()))
}

/// 列出可导出的数据库。
#[tauri::command]
pub async fn list_databases_cmd(
    conn: ConnectionParams,
    database: Option<String>,
) -> Result<Vec<String>, String> {
    let opts = to_options(&conn, database.as_deref().unwrap_or(""))?;
    list_databases(&opts).await.map_err(to_msg)
}

/// 列出指定库的表（含注释与列数）。
#[tauri::command]
pub async fn list_tables_cmd(
    conn: ConnectionParams,
    database: String,
) -> Result<Vec<TableSummary>, String> {
    let opts = to_options(&conn, &database)?;
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
    let opts = to_options(&conn, &database)?;
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

/// 新增或更新一个连接；`id` 为空时由后端生成。
///
/// `password` 为 `Some(非空)` 时同步写入系统凭据存储，`Some("")` 表示清除已保存密码，
/// `None` 表示不改变现有密码。保存后自动设为当前连接。
#[tauri::command]
pub async fn save_connection(
    app: tauri::AppHandle,
    conn: SavedConnection,
    password: Option<String>,
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

    // 同步密码到系统凭据存储
    if let Some(pwd) = password {
        if pwd.is_empty() {
            secrets::delete_password(&saved.id);
            saved.has_password = false;
        } else {
            secrets::save_password(&saved.id, &pwd).map_err(to_msg)?;
            saved.has_password = true;
        }
        if let Some(existing) = state.connections.iter_mut().find(|c| c.id == saved.id) {
            existing.has_password = saved.has_password;
        }
    }

    state.active_connection = Some(saved.id.clone());
    store::save(&app, &state).map_err(to_msg)?;
    Ok(saved)
}

/// 读取某个连接在系统凭据存储中的密码。
#[tauri::command]
pub async fn get_connection_password(id: String) -> Result<Option<String>, String> {
    Ok(secrets::load_password(&id))
}

/// 删除一个连接；若删除的是当前连接，自动切换到剩下的第一个。
#[tauri::command]
pub async fn delete_connection(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let mut state = store::load(&app);
    state.connections.retain(|c| c.id != id);
    secrets::delete_password(&id);
    if state.active_connection.as_deref() == Some(id.as_str()) {
        state.active_connection = state.connections.first().map(|c| c.id.clone());
    }
    store::save(&app, &state).map_err(to_msg)
}

/// 保存界面偏好（上次使用的格式 / Sheet 模式 / 导出目录）。
#[tauri::command]
pub async fn save_preferences(
    app: tauri::AppHandle,
    preferences: Preferences,
) -> Result<(), String> {
    let mut state = store::load(&app);
    state.preferences = preferences;
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

    let opts = to_options(&conn, &params.database)?;
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

    // 目标文件已存在时自动追加时间戳，避免静默覆盖上一次导出的结果
    let target = if out.exists() {
        let stem = out
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("document");
        let ext = out.extension().and_then(|s| s.to_str()).unwrap_or("xlsx");
        let stamp = Local::now().format("%Y%m%d_%H%M%S");
        out.with_file_name(format!("{stem}_{stamp}.{ext}"))
    } else {
        out.to_path_buf()
    };
    if target != out {
        eprintln!(
            "[DBScribe] 目标文件已存在，改写入 {}",
            target.display()
        );
    }

    // 把生成进度通过事件推给前端（每完成一张表一次）
    let app_progress = app.clone();
    let mut on_progress = move |done: usize, total: usize| {
        let _ = app_progress.emit("export-progress", ExportProgress { done, total });
    };
    let report = export(&tables, &options, &target, &mut on_progress).map_err(to_msg)?;
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

    // 记录导出历史（同一路径只保留最新一条，避免重复堆积）
    let mut state = store::load(&app);
    state.history.retain(|h| h.out_path != result.out_path);
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

/// 达梦 ODBC 驱动状态：供界面在连接前给出安装引导。
#[tauri::command]
pub async fn dameng_driver_status() -> Result<serde_json::Value, String> {
    let status = dbs_db::odbc_util::detect();
    let available = status.is_available();
    Ok(serde_json::json!({
        "available": available,
        "description": match &status {
            dbs_db::odbc_util::DriverStatus::Available { description } => description.clone(),
            _ => String::new(),
        },
        "guide": if available { String::new() } else { dbs_db::odbc_util::install_guide().to_string() },
    }))
}