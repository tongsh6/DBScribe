//! 前后端共享的数据结构。

use serde::{Deserialize, Serialize};

/// 数据库连接参数（含密码，仅用于当前会话，不落盘）。
#[derive(Debug, Clone, Deserialize)]
pub struct ConnectionParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

/// 表摘要（用于列表展示，不携带列定义）。
#[derive(Debug, Clone, Serialize)]
pub struct TableSummary {
    pub name: String,
    pub comment: String,
    pub column_count: usize,
}

/// 导出参数。
#[derive(Debug, Clone, Deserialize)]
pub struct ExportParams {
    pub database: String,
    pub title: String,
    /// excel / markdown / html / json / xml
    pub format: String,
    /// multi / single
    pub sheet_mode: String,
    pub out_path: String,
    /// 只导出这些表；`None` 或空表示导出全部
    #[serde(default)]
    pub tables: Option<Vec<String>>,
}

/// 被自动改名的工作表。
#[derive(Debug, Clone, Serialize)]
pub struct RenamedSheet {
    pub original: String,
    pub actual: String,
}

/// 导出结果。
#[derive(Debug, Clone, Serialize)]
pub struct ExportResult {
    pub out_path: String,
    pub table_count: usize,
    pub sheet_count: usize,
    pub renamed: Vec<RenamedSheet>,
    pub elapsed_ms: u128,
}

/// 已保存的数据库连接（**不保存密码**）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedConnection {
    /// 唯一标识；空字符串表示由后端生成。
    #[serde(default)]
    pub id: String,
    /// 显示名称，如「本地 arcteryx_dev」
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
}

impl SavedConnection {
    /// 生成一个唯一 id（纳秒时间戳，够用且无需额外依赖）。
    pub fn new_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("conn_{nanos}")
    }
}

/// 一次导出记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// 形如 `2026-11-10 14:30:00`
    pub time: String,
    pub database: String,
    pub format: String,
    pub out_path: String,
    pub table_count: usize,
    pub elapsed_ms: u128,
}

/// 应用持久化状态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppState {
    #[serde(default)]
    pub connections: Vec<SavedConnection>,
    #[serde(default)]
    pub active_connection: Option<String>,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
}
