//! DBScribe 核心数据模型。
//!
//! 本 crate 不依赖任何数据库驱动或文件格式库，只承载跨平台共享的数据结构与
//! 「工作表命名」这条关键业务规则（原 .NET 版本正是在这里出过 bug）。

pub mod model;
pub mod sheets;

pub use model::*;
pub use sheets::{SheetNameAllocator, MAX_SHEET_NAME_LEN};
