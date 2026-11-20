//! 远程方言的实库验证入口。
//!
//! 这些测试默认忽略；只有提供真实实例时才运行：
//! `cargo test --offline -p dbs-db --test er_remote_live -- --ignored`
//!
//! MySQL 用例会创建并删除一个带业务注释、完全没有外键的临时库，用公开入口
//! `load_er_catalog` 验证真实唯一键和注释推断。其余方言只读取现有 schema，
//! 核对可信键、类型覆盖和目录一致性，不创建对象。

use dbs_core::er::{infer_relations, ErCatalog, TableId};
use dbs_core::DbType;
use dbs_db::{load_er_catalog, ConnectionOptions};
use mysql_async::prelude::*;
use mysql_async::{OptsBuilder, Pool};
use std::collections::BTreeSet;

fn value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required(name: &str) -> String {
    value(name).unwrap_or_else(|| panic!("需要环境变量 {name} 才能运行实库测试"))
}

fn port(name: &str, default: u16) -> u16 {
    value(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// 公开入口读出的目录必须自洽：键位存在且不重复，每列都有唯一类型签名。
fn assert_catalog_invariants(dialect: &str, catalog: &ErCatalog) {
    assert!(!catalog.tables.is_empty(), "{dialect} 没有读到任何表");
    assert!(
        !catalog.keys.is_empty(),
        "{dialect} 没有读到可信键；警告：{:?}",
        catalog.warnings
    );

    for key in &catalog.keys {
        let table = catalog
            .tables
            .iter()
            .find(|table| TableId::from(*table) == key.table)
            .unwrap_or_else(|| panic!("{dialect} 键指向目录外的表 {:?}", key.table));
        assert!(
            !key.columns.is_empty(),
            "{dialect} 表 {} 出现空键",
            table.qualified_name()
        );
        let mut seen = BTreeSet::new();
        for column in &key.columns {
            assert!(
                table.columns.iter().any(|c| &c.name == column),
                "{dialect} 表 {} 的键列 {column} 不在列清单中",
                table.qualified_name()
            );
            assert!(
                seen.insert(column.to_ascii_lowercase()),
                "{dialect} 表 {} 的键 {} 重复使用列 {column}",
                table.qualified_name(),
                key.columns.join(", ")
            );
        }
    }

    for table in &catalog.tables {
        for column in &table.columns {
            assert!(
                catalog
                    .types
                    .iter()
                    .any(|ty| { ty.table == TableId::from(table) && ty.column == column.name }),
                "{dialect} 表 {} 的列 {} 缺少完整类型签名",
                table.qualified_name(),
                column.name
            );
        }
    }

    // 推断必须能在该目录上正常完成，不读取业务行。
    let analysis = infer_relations(catalog);
    eprintln!(
        "{dialect}：{} 表 / {} 键 / {} 类型；推断 {} 条、候选 {} 条；警告 {} 条",
        catalog.tables.len(),
        catalog.keys.len(),
        catalog.types.len(),
        analysis.relations.len(),
        analysis.candidates.len(),
        catalog.warnings.len() + catalog.scoped_warnings.len()
    );
}

#[tokio::test]
#[ignore = "需要真实 MySQL；设置 DBS_TEST_MYSQL_HOST/USER/PASSWORD"]
async fn mysql_public_entry_infers_business_key_without_foreign_keys() {
    let host = required("DBS_TEST_MYSQL_HOST");
    let port = port("DBS_TEST_MYSQL_PORT", 3306);
    let user = required("DBS_TEST_MYSQL_USER");
    let password = value("DBS_TEST_MYSQL_PASSWORD").unwrap_or_default();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let database = format!("dbscribe_er_test_{}_{nonce}", std::process::id());

    let pool = Pool::new(
        OptsBuilder::default()
            .ip_or_hostname(host.clone())
            .tcp_port(port)
            .user(Some(user.clone()))
            .pass(Some(password.clone()))
            .init(vec!["SET NAMES utf8mb4".to_string()]),
    );
    {
        let mut conn = pool.get_conn().await.expect("连接 MySQL 失败");
        conn.query_drop(format!(
            "CREATE DATABASE `{database}` DEFAULT CHARACTER SET utf8mb4"
        ))
        .await
        .expect("创建测试库失败");
        conn.query_drop(format!("USE `{database}`"))
            .await
            .expect("切换测试库失败");
        // 业务零外键：只有主键和唯一键，关系完全依赖注释与命名。
        conn.query_drop(
            r#"CREATE TABLE users (
                   id BIGINT NOT NULL AUTO_INCREMENT,
                   customer_no VARCHAR(32) NOT NULL COMMENT '客户业务编号',
                   name VARCHAR(64) NOT NULL,
                   PRIMARY KEY (id),
                   UNIQUE KEY uk_users_customer_no (customer_no)
               ) COMMENT='客户信息表'"#,
        )
        .await
        .expect("创建 users 失败");
        conn.query_drop(
            r#"CREATE TABLE invoices (
                   id BIGINT NOT NULL AUTO_INCREMENT,
                   payer_ref VARCHAR(32) NOT NULL COMMENT '关联付款客户的业务编号',
                   amount DECIMAL(10,2) NOT NULL,
                   PRIMARY KEY (id)
               ) COMMENT='发票表'"#,
        )
        .await
        .expect("创建 invoices 失败");
        conn.query_drop("CREATE TABLE locator_keys (sector INT NOT NULL, token VARBINARY(16), UNIQUE KEY(sector, token))").await.unwrap();
        conn.query_drop("CREATE TABLE imports (region INT, payload VARBINARY(16))")
            .await
            .unwrap();
        conn.query_drop("CREATE VIEW audit_pairs AS SELECT i.region FROM imports i JOIN locator_keys k ON i.payload <=> k.token AND i.region = k.sector").await.unwrap();
    }

    let opts = ConnectionOptions::new(DbType::MySql, host, port, user, password, database.clone());
    let catalog = load_er_catalog(&opts).await;

    // 先清理临时库，再传播读取/断言失败，避免失败时留下对象。
    if let Ok(mut conn) = pool.get_conn().await {
        let _ = conn
            .query_drop(format!("DROP DATABASE IF EXISTS `{database}`"))
            .await;
    }
    pool.disconnect().await.ok();

    let catalog = catalog.expect("公开入口读取 MySQL ER 目录失败");
    let analysis = infer_relations(&catalog);
    assert_catalog_invariants("MySQL", &catalog);
    let target = catalog
        .non_null_keys
        .iter()
        .find(|k| k.table.name == "locator_keys")
        .unwrap();
    assert_eq!(target.columns, ["sector", "token"]);
    assert!(!catalog.keys.contains(target));
    let view = catalog
        .null_safe_views
        .iter()
        .find(|v| v.view.name == "audit_pairs")
        .unwrap();
    assert_eq!(&view.right, target);
    assert_eq!(view.left.columns, ["region", "payload"]);
    assert!(
        catalog
            .keys
            .iter()
            .any(|key| key.table.name == "users" && key.columns == ["customer_no"]),
        "没有把 users.customer_no 识别为可信唯一键：{:?}",
        catalog.keys
    );
    assert_eq!(
        analysis.relations.len(),
        1,
        "业务零外键注释推断结果：{:?}",
        analysis
    );
    let relation = &analysis.relations[0];
    assert_eq!(relation.source.table.name, "invoices");
    assert_eq!(relation.source.columns, ["payer_ref"]);
    assert_eq!(relation.target.table.name, "users");
    assert_eq!(relation.target.columns, ["customer_no"]);
}

#[tokio::test]
#[ignore = "需要真实 PostgreSQL；设置 DBS_TEST_PG_HOST/USER/DATABASE"]
async fn postgres_public_entry_reads_trusted_keys_and_types() {
    let opts = ConnectionOptions::new(
        DbType::PostgreSql,
        required("DBS_TEST_PG_HOST"),
        port("DBS_TEST_PG_PORT", 5432),
        required("DBS_TEST_PG_USER"),
        value("DBS_TEST_PG_PASSWORD").unwrap_or_default(),
        required("DBS_TEST_PG_DATABASE"),
    );
    let catalog = load_er_catalog(&opts)
        .await
        .expect("公开入口读取 PostgreSQL ER 目录失败");
    assert_catalog_invariants("PostgreSQL", &catalog);
}

#[tokio::test]
#[ignore = "需要真实 SQL Server；设置 DBS_TEST_MSSQL_HOST/USER/DATABASE"]
async fn sqlserver_public_entry_reads_trusted_keys_and_types() {
    let opts = ConnectionOptions::new(
        DbType::SqlServer,
        required("DBS_TEST_MSSQL_HOST"),
        port("DBS_TEST_MSSQL_PORT", 1433),
        required("DBS_TEST_MSSQL_USER"),
        value("DBS_TEST_MSSQL_PASSWORD").unwrap_or_default(),
        required("DBS_TEST_MSSQL_DATABASE"),
    );
    let catalog = load_er_catalog(&opts)
        .await
        .expect("公开入口读取 SQL Server ER 目录失败");
    assert_catalog_invariants("SQL Server", &catalog);
}

#[tokio::test]
#[ignore = "需要真实 Oracle 与客户端库；设置 DBS_TEST_ORACLE_HOST/USER/DATABASE"]
async fn oracle_public_entry_reads_trusted_keys_and_types() {
    let opts = ConnectionOptions::new(
        DbType::Oracle,
        required("DBS_TEST_ORACLE_HOST"),
        port("DBS_TEST_ORACLE_PORT", 1521),
        required("DBS_TEST_ORACLE_USER"),
        value("DBS_TEST_ORACLE_PASSWORD").unwrap_or_default(),
        required("DBS_TEST_ORACLE_DATABASE"),
    );
    let catalog = load_er_catalog(&opts)
        .await
        .expect("公开入口读取 Oracle ER 目录失败");
    assert_catalog_invariants("Oracle", &catalog);
}

#[tokio::test]
#[ignore = "需要真实达梦与 ODBC 驱动；设置 DBS_TEST_DAMENG_HOST/USER"]
async fn dameng_public_entry_reads_trusted_keys_and_types() {
    let opts = ConnectionOptions::new(
        DbType::Dameng,
        required("DBS_TEST_DAMENG_HOST"),
        port("DBS_TEST_DAMENG_PORT", 5236),
        required("DBS_TEST_DAMENG_USER"),
        value("DBS_TEST_DAMENG_PASSWORD").unwrap_or_default(),
        value("DBS_TEST_DAMENG_DATABASE").unwrap_or_default(),
    );
    let catalog = load_er_catalog(&opts)
        .await
        .expect("公开入口读取达梦 ER 目录失败");
    assert_catalog_invariants("达梦", &catalog);
}
