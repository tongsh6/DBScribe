//! 跨数据库的统一连接参数。

use dbs_core::DbType;

/// 一个数据库连接所需的参数（各数据库共用）。
#[derive(Debug, Clone)]
pub struct ConnectionOptions {
    pub db_type: DbType,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    /// 目标数据库；列出所有库时可以为空。
    pub database: String,
}

impl ConnectionOptions {
    pub fn new(
        db_type: DbType,
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        password: impl Into<String>,
        database: impl Into<String>,
    ) -> Self {
        Self {
            db_type,
            host: host.into(),
            port,
            user: user.into(),
            password: password.into(),
            database: database.into(),
        }
    }

    /// 各数据库的默认端口。
    pub fn default_port(db_type: DbType) -> u16 {
        match db_type {
            DbType::MySql => 3306,
            DbType::PostgreSql => 5432,
            DbType::SqlServer => 1433,
            DbType::Oracle => 1521,
            DbType::Sqlite => 0,
        }
    }

    /// 供日志展示的脱敏连接描述（不含密码）。
    pub fn describe(&self) -> String {
        // SQLite 是文件库，没有主机/端口概念
        if self.db_type == DbType::Sqlite {
            return format!("sqlite://{}", self.database);
        }
        format!(
            "{}://{}@{}:{}/{}",
            self.db_type, self.user, self.host, self.port, self.database
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_masks_password() {
        let opts =
            ConnectionOptions::new(DbType::MySql, "127.0.0.1", 3306, "root", "secret", "demo");
        let text = opts.describe();
        assert!(!text.contains("secret"));
        assert_eq!(text, "mysql://root@127.0.0.1:3306/demo");
    }

    #[test]
    fn default_ports_follow_db_type() {
        assert_eq!(ConnectionOptions::default_port(DbType::MySql), 3306);
        assert_eq!(ConnectionOptions::default_port(DbType::PostgreSql), 5432);
        assert_eq!(ConnectionOptions::default_port(DbType::SqlServer), 1433);
    }
}
