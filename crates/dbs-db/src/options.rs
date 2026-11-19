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
            DbType::Dameng => 5236,
            DbType::MongoDb => 27017,
            DbType::Sqlite => 0,
        }
    }

    /// MongoDB 连接串。
    ///
    /// `host` 本身是连接串时原样使用，可覆盖副本集 / TLS / `authSource` 等场景；
    /// 否则按用户名密码拼接，并对账号密码做 URL 编码。
    pub fn mongodb_uri(&self) -> String {
        let host = self.host.trim();
        if is_uri(host) {
            return host.to_string();
        }
        if self.user.trim().is_empty() {
            return format!("mongodb://{}:{}/", host, self.port);
        }
        format!(
            "mongodb://{}:{}@{}:{}/?authSource=admin",
            percent_encode(&self.user),
            percent_encode(&self.password),
            host,
            self.port
        )
    }

    /// 供日志展示的脱敏连接描述（不含密码）。
    pub fn describe(&self) -> String {
        // SQLite 是文件库，没有主机/端口概念
        if self.db_type == DbType::Sqlite {
            return format!("sqlite://{}", self.database);
        }
        // MongoDB 的 host 可能是完整连接串，里面带密码
        if self.db_type == DbType::MongoDb && is_uri(self.host.trim()) {
            return mask_uri_password(self.host.trim());
        }
        format!(
            "{}://{}@{}:{}/{}",
            self.db_type, self.user, self.host, self.port, self.database
        )
    }
}

/// 是否是 MongoDB 连接串（`mongodb://` / `mongodb+srv://`）。
fn is_uri(host: &str) -> bool {
    host.starts_with("mongodb://") || host.starts_with("mongodb+srv://")
}

/// userinfo 的百分号编码：只保留 RFC 3986 的 unreserved 字符。
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 把连接串里的密码替换为 `***`，避免日志与错误信息泄漏。
fn mask_uri_password(uri: &str) -> String {
    let Some(scheme_end) = uri.find("://") else {
        return uri.to_string();
    };
    let rest_start = scheme_end + 3;
    let Some(at_rel) = uri[rest_start..].find('@') else {
        return uri.to_string();
    };
    let at = rest_start + at_rel;
    // @ 之前出现 /，说明不在 userinfo 段，保持原样
    if uri[rest_start..at].contains('/') {
        return uri.to_string();
    }
    match uri[rest_start..at].rfind(':') {
        Some(colon_rel) => {
            let colon = rest_start + colon_rel;
            format!("{}***{}", &uri[..colon + 1], &uri[at..])
        }
        None => uri.to_string(),
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
        assert_eq!(ConnectionOptions::default_port(DbType::MongoDb), 27017);
    }

    #[test]
    fn mongodb_uri_encodes_credentials_and_keeps_connection_string() {
        // 普通 host：拼接，密码做 URL 编码
        let opts = ConnectionOptions::new(
            DbType::MongoDb,
            "10.0.0.5",
            27017,
            "app",
            "p@ss:w/rd",
            "shop",
        );
        assert_eq!(
            opts.mongodb_uri(),
            "mongodb://app:p%40ss%3Aw%2Frd@10.0.0.5:27017/?authSource=admin"
        );

        // 连接串：原样使用，可覆盖副本集 / TLS / authSource
        let with_uri = ConnectionOptions::new(
            DbType::MongoDb,
            "mongodb+srv://u:p@cluster0.example.net/?retryWrites=true",
            27017,
            "ignored",
            "ignored",
            "shop",
        );
        assert_eq!(
            with_uri.mongodb_uri(),
            "mongodb+srv://u:p@cluster0.example.net/?retryWrites=true"
        );

        // 无用户名：不带认证段
        let anonymous = ConnectionOptions::new(DbType::MongoDb, "127.0.0.1", 27017, "", "", "shop");
        assert_eq!(anonymous.mongodb_uri(), "mongodb://127.0.0.1:27017/");
    }

    #[test]
    fn describe_masks_mongodb_uri_password() {
        let opts = ConnectionOptions::new(
            DbType::MongoDb,
            "mongodb://admin:secret@db.example.net:27017/shop?authSource=admin",
            27017,
            "",
            "",
            "shop",
        );
        let text = opts.describe();
        assert!(!text.contains("secret"), "连接描述泄漏了密码：{text}");
        assert!(text.contains("db.example.net"));
    }
}
