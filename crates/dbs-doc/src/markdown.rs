//! Markdown（.md）文档生成。
//!
//! 单文件输出：标题 → 元信息 → 目录（锚点跳转）→ 每张表一个小节。

use crate::common::{column_headers, column_values, ExportOptions, ExportReport, ProgressFn};
use anyhow::{Context, Result};
use chrono::Local;
use dbs_core::{DbType, TableInfo};
use std::path::Path;

/// 生成 Markdown 文档。
pub fn export_markdown(
    tables: &[TableInfo],
    opts: &ExportOptions,
    out: &Path,
    progress: ProgressFn<'_>,
) -> Result<ExportReport> {
    let db_type = opts.database.db_type;
    let mut md = String::new();

    md.push_str(&format!("# {}\n\n", opts.title));
    md.push_str(&format!(
        "> 数据库：{}（{}）  \n",
        opts.database.name, opts.database.db_type
    ));
    md.push_str(&format!("> 表数量：{}  \n", tables.len()));
    md.push_str(&format!(
        "> 生成时间：{}\n\n",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    ));

    // 目录
    md.push_str("## 目录\n\n");
    for (i, table) in tables.iter().enumerate() {
        let anchor = anchor_of(&table.name);
        let comment = if table.comment.trim().is_empty() {
            String::new()
        } else {
            format!(" —— {}", escape_inline(&table.comment))
        };
        md.push_str(&format!(
            "{}. [{}](#{}){}\n",
            i + 1,
            escape_inline(&table.name),
            anchor,
            comment
        ));
    }
    md.push('\n');

    // 各表
    for table in tables {
        write_table(&mut md, table, db_type);
    }

    progress(tables.len(), tables.len());
    std::fs::write(out, md).with_context(|| format!("写入 Markdown 失败：{}", out.display()))?;
    Ok(ExportReport::simple(out.to_path_buf(), tables.len()))
}

fn write_table(md: &mut String, table: &TableInfo, db_type: DbType) {
    md.push_str(&format!("## {}\n\n", escape_inline(&table.title())));

    let headers = column_headers(db_type);
    md.push_str("| ");
    md.push_str(
        &headers
            .iter()
            .map(|h| escape_cell(h))
            .collect::<Vec<_>>()
            .join(" | "),
    );
    md.push_str(" |\n");

    md.push('|');
    for _ in &headers {
        md.push_str(" --- |");
    }
    md.push('\n');

    for column in &table.columns {
        let cells: Vec<String> = column_values(column, db_type)
            .iter()
            .map(|v| escape_cell(v))
            .collect();
        md.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    md.push('\n');
}

/// 表格单元格转义：竖线与换行会破坏 Markdown 表格。
fn escape_cell(text: &str) -> String {
    escape_inline(text).replace('|', "\\|")
}

/// 行内文本转义。
fn escape_inline(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\r', "")
        .replace('\n', " ")
}

/// 生成与常见 Markdown 渲染器一致的标题锚点。
///
/// 小写化 → 非字母数字/`-`/`_` 的字符替换为 `-` → 折叠连续 `-` → 去掉首尾 `-`。
fn anchor_of(name: &str) -> String {
    let mapped: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();

    let mut out = String::with_capacity(mapped.len());
    for c in mapped.chars() {
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbs_core::{ColumnInfo, DatabaseInfo, OutputFormat, SheetMode};

    fn table() -> TableInfo {
        TableInfo {
            order: 1,
            name: "t_user".into(),
            comment: "用户表".into(),
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
            "测试文档",
            SheetMode::Multi,
            DatabaseInfo::new(DbType::MySql, "demo"),
            OutputFormat::Markdown,
        )
    }

    #[test]
    fn escape_pipe_in_cells() {
        assert_eq!(escape_cell("a|b"), "a\\|b");
        assert_eq!(escape_inline("a\nb"), "a b");
    }

    #[test]
    fn anchor_keeps_chinese_and_normalizes_symbols() {
        assert_eq!(anchor_of("t_user"), "t_user");
        assert_eq!(anchor_of("用户 表"), "用户-表");
        assert_eq!(anchor_of("a::b"), "a-b");
    }

    #[test]
    fn writes_markdown_file() {
        let dir = std::env::temp_dir().join("dbs-doc-md");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("doc.md");
        let report = export_markdown(&[table()], &opts(), &out, &mut |_, _| {}).unwrap();
        assert_eq!(report.table_count, 1);

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("# 测试文档"));
        assert!(content.contains("| 序号 | 列名 |"));
        assert!(content.contains("t_user"));
        assert!(content.contains("√"));
        let _ = std::fs::remove_file(&out);
    }
}
