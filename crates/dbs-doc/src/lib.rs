//! 文档生成层。
//!
//! 支持 Excel（.xlsx）、Markdown（.md）、HTML（.html）、JSON（.json）、XML（.xml）。
//! 各格式共用 [`ExportOptions`]，统一入口为 [`export`]。

pub mod common;
pub mod excel;
pub mod html;
pub mod json;
pub mod markdown;
pub mod xml;

use anyhow::Result;
use dbs_core::{OutputFormat, TableInfo};
use std::path::Path;

pub use common::{column_headers, column_values, ExportOptions, ExportReport, ProgressFn};
pub use excel::export_excel;
pub use html::export_html;
pub use json::export_json;
pub use markdown::export_markdown;
pub use xml::export_xml;

/// 按 [`ExportOptions::format`] 生成对应格式的文档。
///
/// `progress` 会随生成进度被反复调用：(已完成表数, 总表数)。
pub fn export(
    tables: &[TableInfo],
    opts: &ExportOptions,
    out: &Path,
    progress: ProgressFn<'_>,
) -> Result<ExportReport> {
    match opts.format {
        OutputFormat::Excel => export_excel(tables, opts, out, progress),
        OutputFormat::Markdown => export_markdown(tables, opts, out, progress),
        OutputFormat::Html => export_html(tables, opts, out, progress),
        OutputFormat::Json => export_json(tables, opts, out, progress),
        OutputFormat::Xml => export_xml(tables, opts, out, progress),
    }
}
