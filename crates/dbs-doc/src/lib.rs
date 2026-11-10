//! 文档生成层。
//!
//! 首个实现是 Excel（`.xlsx`），结构与原 SmartSQL 对齐：
//! 修订日志 / 目录 / 表清单；随后按同样接口补 Markdown / HTML / JSON / XML / Word / PDF。

pub mod excel;

pub use excel::{export_excel, ExcelExportOptions, ExportReport};
