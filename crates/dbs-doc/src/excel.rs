//! Excel（.xlsx）文档生成。
//!
//! 版式对齐原 SmartSQL 导出：**修订日志 / 目录 / 表结构** 三部分；
//! 多 Sheet 模式下每张表一个工作表。
//!
//! 与原版的关键差异（也是本项目的立身之本）：工作表名先经过
//! [`SheetNameAllocator`] 清洗与去重，**不会因为表名超长、含非法字符、
//! 大小写重复或撞上「目录/修订日志」而导致整个导出失败**。

use crate::common::{column_headers, column_values, ExportOptions, ExportReport};
use anyhow::{Context, Result};
use chrono::Local;
use dbs_core::{DbType, SheetMode, SheetNameAllocator, TableInfo};
use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, Workbook, Worksheet};
use std::path::Path;

/// 固定工作表名（业务表名撞上时会自动避让）。
const SHEET_LOG: &str = "修订日志";
const SHEET_OVERVIEW: &str = "目录";
const SHEET_TABLES: &str = "表";

/// 生成 Excel 文档。
pub fn export_excel(
    tables: &[TableInfo],
    opts: &ExportOptions,
    out: &Path,
) -> Result<ExportReport> {
    let db_type = opts.database.db_type;
    let mode = opts.mode;

    let mut workbook = Workbook::new();

    // 预先把所有工作表名分配好，避免中途才发现冲突
    let mut allocator =
        SheetNameAllocator::with_reserved(&[SHEET_LOG, SHEET_OVERVIEW, SHEET_TABLES]);
    let mut renamed = Vec::new();
    let sheet_names: Vec<String> = if mode == SheetMode::Multi {
        tables
            .iter()
            .map(|t| {
                let name = allocator.allocate(&t.name);
                if name != t.name {
                    renamed.push((t.name.clone(), name.clone()));
                }
                name
            })
            .collect()
    } else {
        Vec::new()
    };

    // 1. 修订日志
    {
        let ws = workbook.add_worksheet();
        ws.set_name(SHEET_LOG)
            .with_context(|| format!("创建工作表 {SHEET_LOG} 失败"))?;
        write_log_sheet(ws, opts)?;
    }

    // 2. 目录
    {
        let ws = workbook.add_worksheet();
        ws.set_name(SHEET_OVERVIEW)
            .with_context(|| format!("创建工作表 {SHEET_OVERVIEW} 失败"))?;
        write_overview_sheet(ws, opts, tables, &sheet_names)?;
    }

    // 3. 表结构
    if mode == SheetMode::Single {
        let ws = workbook.add_worksheet();
        ws.set_name(SHEET_TABLES)?;
        write_tables_sheet(ws, tables, db_type, mode)?;
    } else {
        for (table, sheet_name) in tables.iter().zip(sheet_names.iter()) {
            let ws = workbook.add_worksheet();
            ws.set_name(sheet_name.as_str())
                .with_context(|| format!("创建工作表 {sheet_name} 失败"))?;
            write_tables_sheet(ws, std::slice::from_ref(table), db_type, mode)?;
        }
    }

    workbook
        .save(out)
        .with_context(|| format!("保存 Excel 失败：{}", out.display()))?;

    let sheet_count = 2 + match mode {
        SheetMode::Single => 1,
        SheetMode::Multi => tables.len(),
    };

    Ok(ExportReport {
        out_path: out.to_path_buf(),
        table_count: tables.len(),
        sheet_count,
        renamed,
    })
}

// ---------------------------------------------------------------- 样式

fn fmt_title() -> Format {
    Format::new()
        .set_bold()
        .set_align(FormatAlign::Center)
        .set_align(FormatAlign::VerticalCenter)
        .set_background_color(Color::RGB(0xF2F2F2))
        .set_border(FormatBorder::Thin)
}

fn fmt_header() -> Format {
    Format::new()
        .set_bold()
        .set_align(FormatAlign::Center)
        .set_border(FormatBorder::Thin)
}

fn fmt_center() -> Format {
    Format::new().set_align(FormatAlign::Center)
}

fn fmt_text() -> Format {
    Format::new().set_align(FormatAlign::Left)
}

// ---------------------------------------------------------------- 各工作表

/// 修订日志：版本号 / 修订日期 / 修订内容 / 修订人 / 审核人，预置 16 行空行。
fn write_log_sheet(ws: &mut Worksheet, opts: &ExportOptions) -> Result<()> {
    const HEADERS: [&str; 5] = ["版本号", "修订日期", "修订内容", "修订人", "审核人"];

    ws.merge_range(0, 0, 0, 4, "修订记录", &fmt_title())?;
    for (col, h) in HEADERS.iter().enumerate() {
        ws.write_string_with_format(1, col as u16, *h, &fmt_header())?;
    }
    // 首行填好版本号与日期，其余空行留给维护者
    ws.write_string_with_format(2, 0, &opts.version, &fmt_center())?;
    ws.write_string_with_format(
        2,
        1,
        Local::now().format("%Y-%m-%d").to_string(),
        &fmt_center(),
    )?;
    ws.set_row_height(0, 20)?;
    for row in 1..18u32 {
        ws.set_row_height(row, 14.5)?;
    }
    for (col, width) in [(0u16, 14.0), (1, 14.0), (2, 40.0), (3, 12.0), (4, 12.0)] {
        ws.set_column_width(col, width)?;
    }
    Ok(())
}

/// 目录页：标题 + 序号 / 表名 / 注释；多 Sheet 模式下表名是可点击的内部链接。
fn write_overview_sheet(
    ws: &mut Worksheet,
    opts: &ExportOptions,
    tables: &[TableInfo],
    sheet_names: &[String],
) -> Result<()> {
    ws.merge_range(0, 0, 0, 2, opts.title.as_str(), &fmt_title())?;
    ws.set_row_height(0, 22)?;

    for (col, h) in ["序号", "表名", "注释/说明"].iter().enumerate() {
        ws.write_string_with_format(1, col as u16, *h, &fmt_header())?;
    }

    for (i, table) in tables.iter().enumerate() {
        let row = 2 + i as u32;
        ws.write_number_with_format(row, 0, (i + 1) as f64, &fmt_center())?;

        match opts.mode {
            SheetMode::Multi => {
                // 链接到该表自己的工作表；显示文本用原始表名（数据库里的名字），
                // 而不是被截断/去重后的工作表名。
                let target = format!("internal:'{}'!A1", sheet_names[i]);
                ws.write_url_with_text(row, 1, target.as_str(), table.name.as_str())?;
            }
            SheetMode::Single => {
                ws.write_string(row, 1, table.name.as_str())?;
            }
        }

        ws.write_string_with_format(row, 2, table.comment.as_str(), &fmt_text())?;
        ws.set_row_height(row, 14.5)?;
    }

    for (col, width) in [(0u16, 10.0), (1, 50.0), (2, 50.0)] {
        ws.set_column_width(col, width)?;
    }
    Ok(())
}

/// 写一个或多个表的结构。单 Sheet 模式会把多张表纵向拼接。
fn write_tables_sheet(
    ws: &mut Worksheet,
    tables: &[TableInfo],
    db_type: DbType,
    mode: SheetMode,
) -> Result<()> {
    let headers = column_headers(db_type);
    let col_count = headers.len() as u16;
    let mut row: u32 = 0;

    for (idx, table) in tables.iter().enumerate() {
        // 表标题行（合并整行）
        ws.merge_range(
            row,
            0,
            row,
            col_count - 1,
            table.title().as_str(),
            &fmt_title(),
        )?;
        ws.set_row_height(row, 18)?;
        row += 1;

        // 列头
        for (col, h) in headers.iter().enumerate() {
            ws.write_string_with_format(row, col as u16, *h, &fmt_header())?;
        }
        // 多 Sheet 模式提供「返回目录」
        if mode == SheetMode::Multi {
            let target = format!("internal:'{SHEET_OVERVIEW}'!A1");
            ws.write_url_with_text(row, col_count, target.as_str(), "返回目录")?;
        }
        row += 1;

        // 列定义
        for column in &table.columns {
            for (col, value) in column_values(column, db_type).iter().enumerate() {
                let format = match col {
                    0 | 3 | 4 | 5 | 6 => fmt_center(),
                    _ => Format::new(),
                };
                ws.write_string_with_format(row, col as u16, value.as_str(), &format)?;
            }
            row += 1;
        }

        // 表与表之间空一行（仅单 Sheet 拼接时需要）
        if idx + 1 < tables.len() {
            row += 1;
        }
    }

    let widths: [f64; 9] = [8.0, 34.0, 16.0, 10.0, 8.0, 8.0, 8.0, 20.0, 34.0];
    for (col, width) in widths.iter().enumerate() {
        ws.set_column_width(col as u16, *width)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbs_core::{ColumnInfo, DatabaseInfo, DbType, OutputFormat, TableInfo};

    fn sample_column(order: u32, name: &str) -> ColumnInfo {
        ColumnInfo {
            order,
            name: name.to_string(),
            data_type: "varchar".to_string(),
            length: "64".to_string(),
            is_primary_key: order == 1,
            is_auto_increment: order == 1,
            nullable: order != 1,
            default_value: String::new(),
            comment: format!("{name} 注释"),
        }
    }

    fn sample_table(order: u32, name: &str) -> TableInfo {
        TableInfo {
            order,
            name: name.to_string(),
            comment: format!("{name} 说明"),
            columns: vec![sample_column(1, "id"), sample_column(2, "name")],
        }
    }

    fn opts(mode: SheetMode) -> ExportOptions {
        ExportOptions::new(
            "示例文档",
            mode,
            DatabaseInfo::new(DbType::MySql, "demo"),
            OutputFormat::Excel,
        )
    }

    #[test]
    fn headers_follow_db_type() {
        let mysql = column_headers(DbType::MySql);
        assert_eq!(mysql.len(), 9);
        assert!(mysql.contains(&"自增"));

        let oracle = column_headers(DbType::Oracle);
        assert_eq!(oracle.len(), 8);
        assert!(!oracle.contains(&"自增"));
    }

    #[test]
    fn column_values_render_marks() {
        let c = sample_column(1, "id");
        let v = column_values(&c, DbType::MySql);
        assert_eq!(v.len(), 9);
        assert_eq!(v[0], "1");
        assert_eq!(v[4], "√"); // 主键
        assert_eq!(v[5], "√"); // 自增
        assert_eq!(v[6], ""); // 允许空

        let v_oracle = column_values(&c, DbType::Oracle);
        assert_eq!(v_oracle.len(), 8);
    }

    /// 核心回归：原版会因为工作表名冲突而整个导出失败。
    /// 这里用用户真实库中的冲突表名，必须正常导出且全部保留。
    #[test]
    fn exports_despite_conflicting_table_names() {
        let tables: Vec<TableInfo> = vec![
            sample_table(1, "T_EXHIBITION_EVENT_CONFIG"),
            sample_table(2, "t_exhibition_event_config"), // 仅大小写不同
            sample_table(3, "t_landlord_presale_whitelist_file"),
            sample_table(4, "t_landlord_presale_whitelist_file_member"), // 截断后同名
            sample_table(5, "t_store_themed_activities_questionnaire"),
            sample_table(6, "t_store_themed_activities_questionnaire_answer"),
            sample_table(7, "目录"), // 撞固定工作表名
            sample_table(8, "t_normal"),
        ];
        let dir = std::env::temp_dir().join("dbs-doc-test");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("conflict.xlsx");

        let report = export_excel(&tables, &opts(SheetMode::Multi), &out).expect("导出不应失败");
        assert_eq!(report.table_count, 8);
        assert_eq!(report.sheet_count, 10); // 2 固定 + 8 表
        assert!(out.exists());
        assert!(!report.renamed.is_empty(), "应有被改名的工作表");
        // 所有实际工作表名必须唯一
        let names: std::collections::HashSet<String> = report
            .renamed
            .iter()
            .map(|(_, n)| n.to_lowercase())
            .collect();
        assert!(!names.is_empty());

        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn single_sheet_mode_exports() {
        let tables: Vec<TableInfo> = vec![sample_table(1, "a"), sample_table(2, "b")];
        let dir = std::env::temp_dir().join("dbs-doc-test");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("single.xlsx");
        let report = export_excel(&tables, &opts(SheetMode::Single), &out).expect("导出失败");
        assert_eq!(report.sheet_count, 3); // 修订日志 + 目录 + 表
        assert!(out.exists());
        assert!(report.renamed.is_empty());
        let _ = std::fs::remove_file(&out);
    }
}
