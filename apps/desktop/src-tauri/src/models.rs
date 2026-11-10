//! 前后端共享的数据结构。

use serde::{Deserialize, Serialize};

/// 数据库连接参数。
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
