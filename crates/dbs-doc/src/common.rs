//! 各输出格式共用的参数与结果类型。

use dbs_core::{ColumnInfo, DatabaseInfo, DbType, OutputFormat, SheetMode};
use std::path::PathBuf;

/// 列头（Oracle 没有「自增」列，与既有版式保持一致）。
pub fn column_headers(db_type: DbType) -> Vec<&'static str> {
    let mut headers = vec!["序号", "列名", "数据类型", "长度", "主键"];
    if db_type.has_identity_column() {
        headers.push("自增");
    }
    headers.extend(["允许空", "默认值", "列说明"]);
    headers
}

/// 一行列的取值；布尔项渲染为 `√` / 空。
pub fn column_values(column: &ColumnInfo, db_type: DbType) -> Vec<String> {
    let mark = |b: bool| if b { "√".to_string() } else { String::new() };

    let mut values = vec![
        column.order.to_string(),
        column.name.clone(),
        column.data_type.clone(),
        column.length.clone(),
        mark(column.is_primary_key),
    ];
    if db_type.has_identity_column() {
        values.push(mark(column.is_auto_increment));
    }
    values.push(mark(column.nullable));
    values.push(column.default_value.clone());
    values.push(column.comment.clone());
    values
}

/// 导出参数（所有格式共用）。
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// 文档标题。
    pub title: String,
    /// 工作表拆分方式，仅 Excel 有意义，其余格式忽略。
    pub mode: SheetMode,
    pub database: DatabaseInfo,
    /// 修订日志里的初始版本号。
    pub version: String,
    /// 输出格式。
    pub format: OutputFormat,
}

impl ExportOptions {
    pub fn new(
        title: impl Into<String>,
        mode: SheetMode,
        database: DatabaseInfo,
        format: OutputFormat,
    ) -> Self {
        Self {
            title: title.into(),
            mode,
            database,
            version: "1.0".to_string(),
            format,
        }
    }
}

/// 导出结果摘要。
#[derive(Debug, Clone)]
pub struct ExportReport {
    pub out_path: PathBuf,
    pub table_count: usize,
    /// 生成的工作表数量；无工作表概念的格式为 0。
    pub sheet_count: usize,
    /// 被改名的工作表：(原表名, 实际工作表名)，仅 Excel 多 Sheet 会出现。
    pub renamed: Vec<(String, String)>,
}

impl ExportReport {
    /// 供没有工作表概念的格式使用。
    pub fn simple(out_path: PathBuf, table_count: usize) -> Self {
        Self {
            out_path,
            table_count,
            sheet_count: 0,
            renamed: Vec::new(),
        }
    }
}
