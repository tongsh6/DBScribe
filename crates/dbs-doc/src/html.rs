//! 单文件 HTML 文档生成。
//!
//! 输出自带样式与目录，可直接双击打开或托管成静态站点。

use crate::common::{column_headers, column_values, ExportOptions, ExportReport, ProgressFn};
use anyhow::{Context, Result};
use chrono::Local;
use dbs_core::{DbType, TableInfo};
use std::path::Path;

const STYLE: &str = r#"
:root { color-scheme: light dark; }
body {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "Microsoft YaHei", Roboto, sans-serif;
  max-width: 1100px; margin: 0 auto; padding: 2rem 2.5rem; line-height: 1.65;
  color: #1f2328; background: #fff;
}
h1 { border-bottom: 2px solid #e5e7eb; padding-bottom: .5rem; font-size: 1.8rem; }
h2 { margin-top: 2.5rem; font-size: 1.25rem; border-left: 4px solid #2563eb; padding-left: .6rem; }
.meta { color: #6b7280; font-size: .88rem; margin: .2rem 0; }
nav { background: #f9fafb; border: 1px solid #e5e7eb; border-radius: 8px; padding: 1rem 1.4rem; margin: 1.5rem 0 2.5rem; }
nav ol { margin: 0; padding-left: 1.4rem; columns: 2; column-gap: 2.5rem; }
nav li { margin: .2rem 0; break-inside: avoid; }
a { color: #2563eb; text-decoration: none; }
a:hover { text-decoration: underline; }
table { border-collapse: collapse; width: 100%; margin: .8rem 0 2rem; font-size: .88rem; }
th, td { border: 1px solid #e5e7eb; padding: .38rem .6rem; text-align: left; vertical-align: top; }
th { background: #f3f4f6; font-weight: 600; white-space: nowrap; }
tr:nth-child(even) td { background: #fafafa; }
td.center { text-align: center; }
.section { scroll-margin-top: 1rem; }
footer { margin-top: 4rem; padding-top: 1rem; border-top: 1px solid #e5e7eb; color: #9ca3af; font-size: .82rem; }
@media (prefers-color-scheme: dark) {
  body { background: #0d1117; color: #e6edf3; }
  h1 { border-color: #30363d; }
  nav { background: #161b22; border-color: #30363d; }
  th { background: #161b22; }
  th, td { border-color: #30363d; }
  tr:nth-child(even) td { background: #0f141a; }
  .meta { color: #8b949e; }
  footer { border-color: #30363d; color: #6e7681; }
}
"#;

enum CellKind {
    Text,
    Center,
}

/// 生成 HTML 文档。
pub fn export_html(
    tables: &[TableInfo],
    opts: &ExportOptions,
    out: &Path,
    progress: ProgressFn<'_>,
) -> Result<ExportReport> {
    let db_type = opts.database.db_type;
    let mut html = String::with_capacity(64 * 1024);

    html.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n");
    html.push_str("<meta charset=\"utf-8\">\n");
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str(&format!("<title>{}</title>\n", escape(&opts.title)));
    html.push_str(&format!("<style>{STYLE}</style>\n</head>\n<body>\n"));

    html.push_str(&format!("<h1>{}</h1>\n", escape(&opts.title)));
    html.push_str(&format!(
        "<p class=\"meta\">数据库：{}（{}）</p>\n",
        escape(&opts.database.name),
        opts.database.db_type
    ));
    html.push_str(&format!("<p class=\"meta\">表数量：{}</p>\n", tables.len()));
    html.push_str(&format!(
        "<p class=\"meta\">生成时间：{}</p>\n",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    ));

    // 目录
    html.push_str("<nav><ol>\n");
    for table in tables {
        html.push_str(&format!(
            "<li><a href=\"#{}\">{}</a>{}</li>\n",
            anchor_of(table.order),
            escape(&table.name),
            if table.comment.trim().is_empty() {
                String::new()
            } else {
                format!(" —— {}", escape(&table.comment))
            }
        ));
    }
    html.push_str("</ol></nav>\n");

    // 各表
    for table in tables {
        html.push_str(&format!(
            "<div class=\"section\" id=\"t{}\">\n<h2>{}</h2>\n",
            table.order,
            escape(&table.title())
        ));
        write_table(&mut html, table, db_type);
        html.push_str("</div>\n");
    }

    html.push_str(&format!(
        "<footer>由 DBScribe 生成 · {}</footer>\n</body>\n</html>\n",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    ));

    progress(tables.len(), tables.len());
    std::fs::write(out, html).with_context(|| format!("写入 HTML 失败：{}", out.display()))?;
    Ok(ExportReport::simple(out.to_path_buf(), tables.len()))
}

fn write_table(html: &mut String, table: &TableInfo, db_type: DbType) {
    let headers = column_headers(db_type);
    html.push_str("<table>\n<thead><tr>");
    for h in &headers {
        html.push_str(&format!("<th>{}</th>", escape(h)));
    }
    html.push_str("</tr></thead>\n<tbody>\n");

    for column in &table.columns {
        html.push_str("<tr>");
        for (idx, value) in column_values(column, db_type).iter().enumerate() {
            let kind = if matches!(idx, 0 | 3 | 4 | 5 | 6) {
                CellKind::Center
            } else {
                CellKind::Text
            };
            let class = match kind {
                CellKind::Center => " class=\"center\"",
                CellKind::Text => "",
            };
            html.push_str(&format!("<td{class}>{}</td>", escape(value)));
        }
        html.push_str("</tr>\n");
    }
    html.push_str("</tbody>\n</table>\n");
}

/// 目录锚点：使用表序号，避免表名里的特殊字符导致链接失效。
fn anchor_of(order: u32) -> String {
    format!("t{order}")
}

/// HTML 转义，并剔除 XML/HTML 非法控制字符。
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbs_core::{ColumnInfo, DatabaseInfo, OutputFormat, SheetMode};

    fn table() -> TableInfo {
        TableInfo {
            order: 1,
            name: "t_user<script>".into(),
            comment: "用户 <表>".into(),
            columns: vec![ColumnInfo {
                order: 1,
                name: "id".into(),
                data_type: "bigint".into(),
                length: "20".into(),
                is_primary_key: true,
                is_auto_increment: true,
                nullable: false,
                default_value: String::new(),
                comment: "主键".into(),
            }],
        }
    }

    fn opts() -> ExportOptions {
        ExportOptions::new(
            "测试 <文档>",
            SheetMode::Multi,
            DatabaseInfo::new(DbType::MySql, "demo"),
            OutputFormat::Html,
        )
    }

    #[test]
    fn escape_handles_specials() {
        assert_eq!(escape("<a & b>"), "&lt;a &amp; b&gt;");
        assert_eq!(escape("it's"), "it&#39;s");
    }

    #[test]
    fn writes_html_file() {
        let dir = std::env::temp_dir().join("dbs-doc-html");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("doc.html");
        let report = export_html(&[table()], &opts(), &out, &mut |_, _| {}).unwrap();
        assert_eq!(report.table_count, 1);

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.starts_with("<!DOCTYPE html>"));
        assert!(content.contains("测试 &lt;文档&gt;"));
        assert!(content.contains("t_user&lt;script&gt;"));
        assert!(content.contains("id=\"t1\""));
        let _ = std::fs::remove_file(&out);
    }
}
