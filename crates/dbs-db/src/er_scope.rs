use crate::ConnectionOptions;
use dbs_core::{er::ProjectScope, DbType};

/// 项目修正按逻辑库和服务器绑定，SQLite 采用数据库文件的规范路径。
pub fn er_project_scope(opts: &ConnectionOptions) -> ProjectScope {
    if opts.db_type == DbType::MongoDb {
        return ProjectScope {
            db_type: opts.db_type,
            host: opts.mongo_cluster(),
            port: 0,
            database: opts.database.clone(),
        };
    }
    if opts.db_type == DbType::Sqlite {
        let path = std::path::Path::new(&opts.database);
        let database = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        ProjectScope {
            db_type: opts.db_type,
            host: String::new(),
            port: 0,
            database: database.to_string_lossy().into_owned(),
        }
    } else {
        ProjectScope {
            db_type: opts.db_type,
            host: opts.host.to_lowercase(),
            port: opts.port,
            database: opts.database.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mongo_scope_uses_the_cluster_and_database_without_credentials_or_connection_options() {
        let uri = ConnectionOptions::new(
            DbType::MongoDb,
            "mongodb://account:p%40ss@LOCALHOST:27017/?authMechanismProperties=TOKEN:private",
            27017,
            "",
            "",
            "catalog",
        );
        let split = ConnectionOptions::new(
            DbType::MongoDb,
            "localhost",
            27017,
            "another",
            "other",
            "catalog",
        );
        let scope = er_project_scope(&uri);
        assert_eq!(scope, er_project_scope(&split));
        assert!(!format!("{scope:?} {}", uri.describe()).contains("private"));
        assert!(!scope.host.contains("account"));
    }
}
