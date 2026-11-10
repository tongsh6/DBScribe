//! 文档生成层。
//!
//! 支持 Excel（.xlsx）、Markdown（.md）、HTML（.html）、JSON（.json）、XML（.xml）。
//! 各格式共用 [`ExportOptions`]，统一入口为 [`export`]。

pub mod common;
pub mod excel;
pub mod html;
pub mod json;
pub mod markdown;
pub mod word;
pub mod xml;

use anyhow::Result;
use dbs_core::{OutputFormat, TableInfo};
use std::path::Path;

pub use common::{column_headers, column_values, ExportOptions, ExportReport, ProgressFn};
pub use excel::export_excel;
pub use html::export_html;
pub use json::export_json;
pub use markdown::export_markdown;
pub use word::export_word;
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
    let prepared = prepare_tables(tables, opts.skip_partitioned);
    match opts.format {
        OutputFormat::Excel => export_excel(&prepared, opts, out, progress),
        OutputFormat::Markdown => export_markdown(&prepared, opts, out, progress),
        OutputFormat::Html => export_html(&prepared, opts, out, progress),
        OutputFormat::Json => export_json(&prepared, opts, out, progress),
        OutputFormat::Xml => export_xml(&prepared, opts, out, progress),
        OutputFormat::Word => export_word(&prepared, opts, out, progress),
    }
}

/// 导出前的统一整理：
///
/// 1. 按表名排序，保证文档顺序稳定、可读；
/// 2. 多 schema 时把 schema 前缀并入表名，避免同名表在文档里互相混淆；
/// 3. 重排序号，让「序号」列与最终顺序一致。
fn prepare_tables(tables: &[TableInfo], skip_partitioned: bool) -> Vec<TableInfo> {
    let mut prepared: Vec<TableInfo> = tables
        .iter()
        .filter(|t| !(skip_partitioned && t.is_partitioned))
        .cloned()
        .collect();

    let multi_schema = prepared
        .iter()
        .map(|t| t.schema.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
        > 1;

    if multi_schema {
        for table in &mut prepared {
            table.name = table.qualified_name();
        }
    }

    prepared.sort_by(|a, b| a.name.cmp(&b.name));
    for (idx, table) in prepared.iter_mut().enumerate() {
        table.order = (idx + 1) as u32;
    }
    prepared
}
