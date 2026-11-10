//! 数据库元数据模型。
//!
//! 字段与原 SmartSQL 导出的 Excel 列一一对应：
//! 序号 / 列名 / 数据类型 / 长度 / 主键 / 自增 / 允许空 / 默认值 / 列说明

use serde::{Deserialize, Serialize};
use std::fmt;

/// 支持的数据库类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbType {
    MySql,
    PostgreSql,
    SqlServer,
    Sqlite,
    Oracle,
}

impl DbType {
    /// 用于连接串 / CLI 参数的短名。
    pub fn as_str(&self) -> &'static str {
        match self {
            DbType::MySql => "mysql",
            DbType::PostgreSql => "postgres",
            DbType::SqlServer => "sqlserver",
            DbType::Sqlite => "sqlite",
            DbType::Oracle => "oracle",
        }
    }

    /// Oracle 的列集合与原版不同（没有「自增」列），保留这个判断以对齐导出格式。
    pub fn has_identity_column(&self) -> bool {
        !matches!(self, DbType::Oracle)
    }
}

impl fmt::Display for DbType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for DbType {
    type Err = crate::CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "mysql" => Ok(DbType::MySql),
            "postgres" | "postgresql" | "pg" => Ok(DbType::PostgreSql),
            "sqlserver" | "mssql" | "sql-server" => Ok(DbType::SqlServer),
            "sqlite" | "sqlite3" => Ok(DbType::Sqlite),
            "oracle" => Ok(DbType::Oracle),
            other => Err(CoreError::UnsupportedDbType(other.to_string())),
        }
    }
}

/// 输出文档格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Excel 工作簿（.xlsx），支持单/多 Sheet
    #[default]
    Excel,
    /// Markdown 文档（.md）
    Markdown,
    /// 单文件 HTML（.html），带目录与样式
    Html,
    /// 结构化 JSON（.json）
    Json,
    /// 结构化 XML（.xml）
    Xml,
}

impl OutputFormat {
    /// 默认文件扩展名（不含点）。
    pub fn extension(&self) -> &'static str {
        match self {
            OutputFormat::Excel => "xlsx",
            OutputFormat::Markdown => "md",
            OutputFormat::Html => "html",
            OutputFormat::Json => "json",
            OutputFormat::Xml => "xml",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            OutputFormat::Excel => "excel",
            OutputFormat::Markdown => "markdown",
            OutputFormat::Html => "html",
            OutputFormat::Json => "json",
            OutputFormat::Xml => "xml",
        }
    }

    /// 是否支持「每表一个 Sheet」这类工作表拆分。
    pub fn supports_sheet_mode(&self) -> bool {
        matches!(self, OutputFormat::Excel)
    }

    /// 由文件扩展名推断格式。
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "xlsx" => Some(OutputFormat::Excel),
            "md" | "markdown" => Some(OutputFormat::Markdown),
            "html" | "htm" => Some(OutputFormat::Html),
            "json" => Some(OutputFormat::Json),
            "xml" => Some(OutputFormat::Xml),
            _ => None,
        }
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for OutputFormat {
    type Err = crate::CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "excel" | "xlsx" | "xls" => Ok(OutputFormat::Excel),
            "md" | "markdown" => Ok(OutputFormat::Markdown),
            "html" | "htm" => Ok(OutputFormat::Html),
            "json" => Ok(OutputFormat::Json),
            "xml" => Ok(OutputFormat::Xml),
            other => Err(CoreError::UnsupportedFormat(other.to_string())),
        }
    }
}

/// 一列的结构信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    /// 列序号（从 1 开始）。
    pub order: u32,
    pub name: String,
    /// 类型名，如 `varchar(64)` / `bigint`。
    pub data_type: String,
    /// 长度/精度展示文本，已由驱动层格式化好。
    pub length: String,
    pub is_primary_key: bool,
    pub is_auto_increment: bool,
    pub nullable: bool,
    pub default_value: String,
    pub comment: String,
}

/// 一张表的结构信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableInfo {
    /// 表序号（从 1 开始）。
    pub order: u32,
    pub name: String,
    pub comment: String,
    pub columns: Vec<ColumnInfo>,
}

impl TableInfo {
    /// 标题行用的「表名 注释」文本。
    pub fn title(&self) -> String {
        if self.comment.trim().is_empty() {
            self.name.clone()
        } else {
            format!("{} {}", self.name, self.comment)
        }
    }
}

/// 一次导出所针对的数据库。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatabaseInfo {
    pub db_type: DbType,
    pub name: String,
}

impl DatabaseInfo {
    pub fn new(db_type: DbType, name: impl Into<String>) -> Self {
        Self {
            db_type,
            name: name.into(),
        }
    }
}

/// 工作表拆分方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SheetMode {
    /// 所有表放在同一个工作表里。
    Single,
    /// 每张表一个工作表（原版默认，也是出问题的那种）。
    #[default]
    Multi,
}

impl std::str::FromStr for SheetMode {
    type Err = crate::CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "single" | "one" => Ok(SheetMode::Single),
            "multi" | "multiple" => Ok(SheetMode::Multi),
            other => Err(CoreError::UnsupportedSheetMode(other.to_string())),
        }
    }
}

/// 核心层错误。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("不支持的数据库类型：{0}")]
    UnsupportedDbType(String),
    #[error("不支持的工作表模式：{0}（可选 single / multi）")]
    UnsupportedSheetMode(String),
    #[error("不支持的输出格式：{0}（可选 excel / markdown / html / json / xml）")]
    UnsupportedFormat(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_type_roundtrip() {
        for t in [
            DbType::MySql,
            DbType::PostgreSql,
            DbType::SqlServer,
            DbType::Sqlite,
            DbType::Oracle,
        ] {
            assert_eq!(t.as_str().parse::<DbType>().unwrap(), t);
        }
        assert_eq!("mysql".parse::<DbType>().unwrap(), DbType::MySql);
        assert_eq!("PostgreSQL".parse::<DbType>().unwrap(), DbType::PostgreSql);
        assert!("db2".parse::<DbType>().is_err());
    }

    #[test]
    fn oracle_has_no_identity_column() {
        assert!(!DbType::Oracle.has_identity_column());
        assert!(DbType::MySql.has_identity_column());
    }

    #[test]
    fn table_title_concatenates_comment() {
        let t = TableInfo {
            order: 1,
            name: "t_user".into(),
            comment: "用户表".into(),
            columns: vec![],
        };
        assert_eq!(t.title(), "t_user 用户表");

        let t2 = TableInfo {
            comment: "  ".into(),
            ..t
        };
        assert_eq!(t2.title(), "t_user");
    }

    #[test]
    fn sheet_mode_parsing() {
        assert_eq!("single".parse::<SheetMode>().unwrap(), SheetMode::Single);
        assert_eq!("Multi".parse::<SheetMode>().unwrap(), SheetMode::Multi);
        assert!("both".parse::<SheetMode>().is_err());
    }
}
