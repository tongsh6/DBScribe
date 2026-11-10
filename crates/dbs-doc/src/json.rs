//! 结构化 JSON 文档生成。
//!
//! 输出便于二次处理（如导入其他系统、做 diff、喂给 AI 分析）。

use crate::common::{ExportOptions, ExportReport};
use anyhow::{Context, Result};
use chrono::Local;
use dbs_core::TableInfo;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct JsonDocument<'a> {
    title: &'a str,
    database: JsonDatabase<'a>,
    generated_at: String,
    table_count: usize,
    tables: Vec<JsonTable<'a>>,
}

#[derive(Serialize)]
struct JsonDatabase<'a> {
    db_type: &'a str,
    name: &'a str,
}

#[derive(Serialize)]
struct JsonTable<'a> {
    order: u32,
    name: &'a str,
    comment: &'a str,
    columns: Vec<JsonColumn<'a>>,
}

#[derive(Serialize)]
struct JsonColumn<'a> {
    order: u32,
    name: &'a str,
    data_type: &'a str,
    length: &'a str,
    primary_key: bool,
    auto_increment: bool,
    nullable: bool,
    default_value: &'a str,
    comment: &'a str,
}

/// 生成 JSON 文档。
pub fn export_json(tables: &[TableInfo], opts: &ExportOptions, out: &Path) -> Result<ExportReport> {
    let document = JsonDocument {
        title: &opts.title,
        database: JsonDatabase {
            db_type: opts.database.db_type.as_str(),
            name: &opts.database.name,
        },
        generated_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        table_count: tables.len(),
        tables: tables
            .iter()
            .map(|t| JsonTable {
                order: t.order,
                name: &t.name,
                comment: &t.comment,
                columns: t
                    .columns
                    .iter()
                    .map(|c| JsonColumn {
                        order: c.order,
                        name: &c.name,
                        data_type: &c.data_type,
                        length: &c.length,
                        primary_key: c.is_primary_key,
                        auto_increment: c.is_auto_increment,
                        nullable: c.nullable,
                        default_value: &c.default_value,
                        comment: &c.comment,
                    })
                    .collect(),
            })
            .collect(),
    };

    let json = serde_json::to_string_pretty(&document).context("序列化 JSON 失败")?;
    std::fs::write(out, json).with_context(|| format!("写入 JSON 失败：{}", out.display()))?;
    Ok(ExportReport::simple(out.to_path_buf(), tables.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbs_core::{ColumnInfo, DatabaseInfo, DbType, OutputFormat, SheetMode};

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
                default_value: "0".into(),
                comment: "主键".into(),
            }],
        }
    }

    fn opts() -> ExportOptions {
        ExportOptions::new(
            "测试文档",
            SheetMode::Multi,
            DatabaseInfo::new(DbType::MySql, "demo"),
            OutputFormat::Json,
        )
    }

    #[test]
    fn writes_valid_json() {
        let dir = std::env::temp_dir().join("dbs-doc-json");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("doc.json");
        let report = export_json(&[table()], &opts(), &out).unwrap();
        assert_eq!(report.table_count, 1);

        let content = std::fs::read_to_string(&out).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["title"], "测试文档");
        assert_eq!(parsed["database"]["db_type"], "mysql");
        assert_eq!(parsed["tables"][0]["name"], "t_user");
        assert_eq!(parsed["tables"][0]["columns"][0]["primary_key"], true);
        let _ = std::fs::remove_file(&out);
    }
}
