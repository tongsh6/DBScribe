//! 数据库元数据读取层。
//!
//! 当前实现：MySQL / MariaDB（通过 `information_schema`）。
//! 后续按同样接口补 PostgreSQL / SQL Server / SQLite / Oracle。

pub mod mysql;

pub use mysql::{list_databases, load_tables, MySqlOptions};
