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
            // 连接串自带认证信息时原样使用：可覆盖副本集 / TLS / authSource 等场景
            if split_uri_password(host).1.is_some() {
                return host.to_string();
            }
            // 保存过的连接串已剥离密码，用凭据里的密码补回
            if !self.password.is_empty() {
                return inject_uri_password(host, &self.password);
            }
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
        if self.db_type == DbType::MongoDb {
            return format!("{}/{}", self.mongo_cluster(), self.database);
        }
        format!(
            "{}://{}@{}:{}/{}",
            self.db_type, self.user, self.host, self.port, self.database
        )
    }
}

impl ConnectionOptions {
    /// 用于报告及项目作用域。只保留协议和种子地址，认证信息与 URI 查询参数全部省略。
    pub(crate) fn mongo_cluster(&self) -> String {
        let raw = self.host.trim();
        let (scheme, hosts, port) = match raw.split_once("://") {
            Some((scheme, rest)) => {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
                let seeds = authority.rsplit('@').next().unwrap_or("");
                (scheme, seeds, 27017)
            }
            None => ("mongodb", raw, self.port),
        };
        let mut seeds: Vec<String> = hosts
            .split(',')
            .map(|s| {
                let s = s.trim();
                if s.to_ascii_lowercase().starts_with("%2f") {
                    return s.to_owned();
                }
                let s = s.to_ascii_lowercase();
                if scheme.eq_ignore_ascii_case("mongodb+srv") {
                    return s;
                }
                if s.starts_with('[') {
                    if s.ends_with(']') {
                        format!("{s}:{port}")
                    } else {
                        s
                    }
                } else {
                    match s.bytes().filter(|b| *b == b':').count() {
                        0 => format!("{s}:{port}"),
                        1 => s,
                        _ => format!("[{s}]:{port}"),
                    }
                }
            })
            .collect();
        seeds.sort();
        seeds.dedup();
        format!("{}://{}", scheme.to_ascii_lowercase(), seeds.join(","))
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

/// 连接串 userinfo 的字节范围 [start, end)（不含 `@`）。
///
/// `@` 之前若出现 `/`，说明 `@` 不在 userinfo 段，返回 None。
fn userinfo_range(uri: &str) -> Option<(usize, usize)> {
    let start = uri.find("://")? + 3;
    let end = start + uri[start..].find('@')?;
    if uri[start..end].contains('/') {
        return None;
    }
    Some((start, end))
}

/// 拆出连接串里的密码：返回（去掉密码的连接串，密码）。
///
/// 密码按原始编码形式返回，写回时原样插入，避免重复编码。
/// 无密码、非连接串时返回 `(uri, None)`。
pub fn split_uri_password(uri: &str) -> (String, Option<String>) {
    let Some((start, end)) = userinfo_range(uri) else {
        return (uri.to_string(), None);
    };
    let Some(colon_rel) = uri[start..end].rfind(':') else {
        return (uri.to_string(), None);
    };
    let colon = start + colon_rel;
    let password = uri[colon + 1..end].to_string();
    (format!("{}{}", &uri[..colon], &uri[end..]), Some(password))
}

/// 把密码写回连接串的 userinfo；已有密码时替换，没有 userinfo 时原样返回。
///
/// 密码按原样插入（调用方负责编码），与 `split_uri_password` 成对使用。
pub fn inject_uri_password(uri: &str, password: &str) -> String {
    let Some((start, end)) = userinfo_range(uri) else {
        return uri.to_string();
    };
    let userinfo = &uri[start..end];
    let user = match userinfo.rfind(':') {
        Some(colon) => &userinfo[..colon],
        None => userinfo,
    };
    format!("{}{}:{}{}", &uri[..start], user, password, &uri[end..])
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
    fn uri_password_can_be_split_and_injected() {
        let uri = "mongodb://u:p%40ss@h:27017/db?authSource=admin";
        let (clean, password) = split_uri_password(uri);
        assert_eq!(clean, "mongodb://u@h:27017/db?authSource=admin");
        assert_eq!(password.as_deref(), Some("p%40ss"));
        assert_eq!(inject_uri_password(&clean, "p%40ss"), uri);

        let (plain, none) = split_uri_password("mongodb://h:27017");
        assert_eq!(plain, "mongodb://h:27017");
        assert!(none.is_none());
    }

    #[test]
    fn saved_uri_without_password_uses_credential_password() {
        // 保存时密码被剥离，连接时从系统凭据补回
        let opts = ConnectionOptions::new(
            DbType::MongoDb,
            "mongodb://u@h:27017/db",
            27017,
            "",
            "secret",
            "db",
        );
        assert_eq!(opts.mongodb_uri(), "mongodb://u:secret@h:27017/db");
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
