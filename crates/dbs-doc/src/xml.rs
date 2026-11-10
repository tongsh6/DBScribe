//! 结构化 XML 文档生成。
//!
//! 手写序列化，避免为一次简单输出引入额外依赖；文本统一做 XML 转义。

use crate::common::{ExportOptions, ExportReport, ProgressFn};
use anyhow::{Context, Result};
use chrono::Local;
use dbs_core::TableInfo;
use std::fmt::Write as _;
use std::path::Path;

/// 生成 XML 文档。
pub fn export_xml(
    tables: &[TableInfo],
    opts: &ExportOptions,
    out: &Path,
    progress: ProgressFn<'_>,
) -> Result<ExportReport> {
    let mut xml = String::with_capacity(64 * 1024);

    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<document>\n");
    let _ = writeln!(xml, "  <title>{}</title>", escape(&opts.title));
    let _ = writeln!(
        xml,
        "  <database type=\"{}\" name=\"{}\"/>",
        opts.database.db_type.as_str(),
        escape(&opts.database.name)
    );
    let _ = writeln!(
        xml,
        "  <generatedAt>{}</generatedAt>",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    let _ = writeln!(xml, "  <tableCount>{}</tableCount>", tables.len());
    xml.push_str("  <tables>\n");

    for table in tables {
        let _ = writeln!(
            xml,
            "    <table order=\"{}\" name=\"{}\">",
            table.order,
            escape(&table.name)
        );
        let _ = writeln!(xml, "      <comment>{}</comment>", escape(&table.comment));
        xml.push_str("      <columns>\n");
        for column in &table.columns {
            let _ = writeln!(
                xml,
                "        <column order=\"{}\" name=\"{}\" dataType=\"{}\" length=\"{}\" \
                 primaryKey=\"{}\" autoIncrement=\"{}\" nullable=\"{}\">",
                column.order,
                escape(&column.name),
                escape(&column.data_type),
                escape(&column.length),
                column.is_primary_key,
                column.is_auto_increment,
                column.nullable
            );
            let _ = writeln!(
                xml,
                "          <defaultValue>{}</defaultValue>",
                escape(&column.default_value)
            );
            let _ = writeln!(
                xml,
                "          <comment>{}</comment>",
                escape(&column.comment)
            );
            xml.push_str("        </column>\n");
        }
        xml.push_str("      </columns>\n");
        xml.push_str("    </table>\n");
    }

    xml.push_str("  </tables>\n</document>\n");

    progress(tables.len(), tables.len());
    std::fs::write(out, xml).with_context(|| format!("写入 XML 失败：{}", out.display()))?;
    Ok(ExportReport::simple(out.to_path_buf(), tables.len()))
}

/// XML 转义，并剔除 XML 1.0 非法控制字符。
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbs_core::{ColumnInfo, DatabaseInfo, DbType, OutputFormat, SheetMode};

    fn table() -> TableInfo {
        TableInfo {
            order: 1,
            name: "t_user<&>".into(),
            comment: "用户\"表\"".into(),
            columns: vec![ColumnInfo {
                order: 1,
                name: "id".into(),
                data_type: "bigint".into(),
                length: "20".into(),
                is_primary_key: true,
                is_auto_increment: false,
                nullable: false,
                default_value: String::new(),
                comment: "主键".into(),
            }],
        }
    }

    fn opts() -> ExportOptions {
        ExportOptions::new(
            "文档<标题>",
            SheetMode::Multi,
            DatabaseInfo::new(DbType::MySql, "demo"),
            OutputFormat::Xml,
        )
    }

    #[test]
    fn escape_handles_xml_specials() {
        assert_eq!(escape("a&b<c>\"d\""), "a&amp;b&lt;c&gt;&quot;d&quot;");
    }

    #[test]
    fn writes_wellformed_xml() {
        let dir = std::env::temp_dir().join("dbs-doc-xml");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("doc.xml");
        let report = export_xml(&[table()], &opts(), &out, &mut |_, _| {}).unwrap();
        assert_eq!(report.table_count, 1);

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(content.contains("name=\"t_user&lt;&amp;&gt;\""));
        assert!(content.contains("primaryKey=\"true\""));
        // 简易良构校验：标签数量配平
        assert_eq!(
            content.matches("<column ").count(),
            content.matches("</column>").count()
        );
        let _ = std::fs::remove_file(&out);
    }
}
