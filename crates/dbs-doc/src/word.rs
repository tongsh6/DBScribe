//! Word（.docx）文档生成。
//!
//! 版式与 Markdown/HTML 保持一致：表标题 → 列结构表 → 索引/外键辅助区块，
//! 辅助信息用更小的字号，保持「小而美」的层级感。

use crate::common::{
    column_headers, column_values, display_indexes, ExportOptions, ExportReport, ProgressFn,
};
use anyhow::{Context, Result};
use chrono::Local;
use dbs_core::TableInfo;
use docx_rs::{Docx, Paragraph, Run, Table, TableCell, TableRow};
use std::path::Path;

/// 生成 Word 文档。
pub fn export_word(
    tables: &[TableInfo],
    opts: &ExportOptions,
    out: &Path,
    progress: ProgressFn<'_>,
) -> Result<ExportReport> {
    let mut doc = Docx::new();

    // 文档标题
    doc = doc.add_paragraph(
        Paragraph::new()
            .add_run(Run::new().add_text(&opts.title).bold().size(36))
            .align(docx_rs::AlignmentType::Center),
    );

    // 概要信息
    doc = doc.add_paragraph(
        Paragraph::new().add_run(
            Run::new()
                .add_text(format!(
                    "数据库：{}（{}）　　表数量：{}　　生成时间：{}",
                    opts.database.name,
                    opts.database.db_type.as_str(),
                    tables.len(),
                    Local::now().format("%Y-%m-%d %H:%M:%S")
                ))
                .size(20),
        ),
    );

    for table in tables {
        // 表标题
        doc = doc.add_paragraph(
            Paragraph::new().add_run(Run::new().add_text(table.title()).bold().size(28)),
        );

        // 列结构表
        let db_type = opts.database.db_type;
        let mut rows: Vec<Vec<String>> = vec![column_headers(db_type)
            .iter()
            .map(|h| h.to_string())
            .collect()];
        for column in &table.columns {
            rows.push(column_values(column, db_type));
        }
        doc = doc.add_table(build_table(&rows));

        // 索引（去冗余）
        let indexes = display_indexes(table);
        if !indexes.is_empty() {
            doc = doc.add_paragraph(
                Paragraph::new().add_run(Run::new().add_text("索引").bold().size(22)),
            );
            let mut index_rows: Vec<Vec<String>> = vec![vec![
                "名称".to_string(),
                "列".to_string(),
                "唯一".to_string(),
            ]];
            for index in indexes {
                index_rows.push(vec![
                    index.name.clone(),
                    index.columns.join(", "),
                    if index.is_unique { "是" } else { "" }.to_string(),
                ]);
            }
            doc = doc.add_table(build_table(&index_rows));
        }

        // 外键
        if !table.foreign_keys.is_empty() {
            doc = doc.add_paragraph(
                Paragraph::new().add_run(Run::new().add_text("外键").bold().size(22)),
            );
            let mut fk_rows: Vec<Vec<String>> = vec![vec!["列".to_string(), "引用".to_string()]];
            for fk in &table.foreign_keys {
                let target = if fk.ref_schema.is_empty() {
                    fk.ref_table.clone()
                } else {
                    format!("{}.{}", fk.ref_schema, fk.ref_table)
                };
                let refs: Vec<String> = fk
                    .ref_columns
                    .iter()
                    .map(|column| format!("{target}.{column}"))
                    .collect();
                fk_rows.push(vec![fk.columns.join(", "), refs.join(", ")]);
            }
            doc = doc.add_table(build_table(&fk_rows));
        }
    }

    progress(tables.len(), tables.len());
    let file = std::fs::File::create(out)
        .with_context(|| format!("创建 Word 文件失败：{}", out.display()))?;
    doc.build()
        .pack(file)
        .with_context(|| format!("写入 Word 文件失败：{}", out.display()))?;

    Ok(ExportReport::simple(out.to_path_buf(), tables.len()))
}

/// 由二维文本构造带边框的表格。
fn build_table(rows: &[Vec<String>]) -> Table {
    let table_rows: Vec<TableRow> = rows
        .iter()
        .map(|row| {
            TableRow::new(
                row.iter()
                    .map(|cell| {
                        TableCell::new().add_paragraph(
                            Paragraph::new().add_run(Run::new().add_text(cell.clone())),
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    Table::new(table_rows)
}
