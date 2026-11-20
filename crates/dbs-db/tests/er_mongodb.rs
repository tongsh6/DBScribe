//! MongoDB ER 元数据接入的实库回归。
//!
//! 只连专用测试实例：`MONGO_URI=mongodb://127.0.0.1:27017 cargo test --offline -p dbs-db \
//!   --test er_mongodb -- --ignored`
//!
//! 每个用例创建随机唯一库，结束后清理；绝不预先 drop 已有库，也不碰用户业务库。
//! 测试只建集合 / 索引 / 视图元数据，不插入业务文档，不执行视图。

use dbs_core::er::RelationKind;
use dbs_core::DbType;
use dbs_db::load_er_catalog as load;
use dbs_db::ConnectionOptions;
use mongodb::bson::{doc, Document};
use mongodb::{Client, Database};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DB: AtomicU64 = AtomicU64::new(0);

async fn create_collection(db: &Database, name: &str, validator: Option<Document>) {
    let mut command = doc! { "create": name };
    if let Some(schema) = validator {
        command.insert("validator", doc! { "$jsonSchema": schema });
    }
    db.run_command(command)
        .await
        .unwrap_or_else(|err| panic!("创建集合 {name} 失败：{err}"));
}

async fn create_view(db: &Database, name: &str, view_on: &str, pipeline: Vec<Document>) {
    db.run_command(doc! {
        "create": name,
        "viewOn": view_on,
        "pipeline": pipeline,
    })
    .await
    .unwrap_or_else(|err| panic!("创建视图 {name} 失败：{err}"));
}

async fn create_indexes(db: &Database, collection: &str, indexes: Vec<Document>) {
    db.run_command(doc! {
        "createIndexes": collection,
        "indexes": indexes,
    })
    .await
    .unwrap_or_else(|err| panic!("为集合 {collection} 创建索引失败：{err}"));
}

fn index(name: &str, key: Document, unique: bool) -> Document {
    doc! {
        "key": key,
        "name": name,
        "unique": unique,
    }
}

/// 每个测试独立随机库；场景 panic 时也先清理再继续抛出。
async fn with_db<F, Fut>(test: F)
where
    F: FnOnce(Database, ConnectionOptions) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let uri = std::env::var("MONGO_URI").expect("运行 MongoDB ER 实库测试需要设置专用 MONGO_URI");
    let client = Client::with_uri_str(&uri)
        .await
        .expect("连接 MONGO_URI 失败");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间异常")
        .as_nanos();
    let sequence = NEXT_DB.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "dbscribe_er_mongo_{}_{}_{}",
        std::process::id(),
        nanos,
        sequence
    );
    let database = client.database(&name);
    let opts = ConnectionOptions::new(DbType::MongoDb, uri, 27017, "", "", name.clone());

    let outcome = tokio::spawn(test(database, opts)).await;
    let _ = client.database(&name).drop().await;
    match outcome {
        Ok(()) => {}
        Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
        Err(err) => panic!("MongoDB ER 测试任务异常退出：{err}"),
    }
}

#[tokio::test]
#[ignore = "需要专用 MongoDB；设置 MONGO_URI"]
async fn metadata_only_keeps_id_unknown_without_sampling() {
    with_db(|db, opts| async move {
        create_collection(&db, "users", None).await;
        create_collection(&db, "orphan", None).await;

        let catalog = load(&opts).await.expect("读取 MongoDB ER 目录失败");
        let names: Vec<&str> = catalog
            .tables
            .iter()
            .map(|table| table.name.as_str())
            .collect();
        assert_eq!(names, ["orphan", "users"]);
        for table in &catalog.tables {
            assert_eq!(
                table.columns.len(),
                1,
                "无 validator 且不采样时应只有 _id：{table:?}"
            );
            assert_eq!(table.columns[0].name, "_id");
            assert_eq!(
                table.columns[0].data_type, "unknown",
                "不得把 _id 猜成 ObjectId"
            );
            assert!(table.columns[0].is_primary_key);
            assert!(table.foreign_keys.is_empty());
            assert!(catalog
                .keys
                .iter()
                .any(|key| { key.table.name == table.name && key.columns == ["_id"] }));
            assert!(
                !catalog
                    .types
                    .iter()
                    .any(|ty| ty.table.name == table.name && ty.column == "_id"),
                "没有 validator 时不得伪造 _id 类型签名"
            );
        }
        assert!(catalog.document_links.relations.is_empty());
        assert!(catalog.document_links.candidates.is_empty());
    })
    .await;
}

#[tokio::test]
#[ignore = "需要专用 MongoDB；设置 MONGO_URI"]
async fn validator_nested_paths_stay_in_collection() {
    with_db(|db, opts| async move {
        let schema = doc! {
            "bsonType": "object",
            "description": "客户信息表",
            "required": ["name"],
            "properties": {
                "_id": { "bsonType": "objectId" },
                "name": { "bsonType": "string", "description": "客户名称" },
                "address": { "bsonType": "object", "properties": {
                    "city": { "bsonType": "string" }
                } },
                "tags": { "bsonType": "array", "items": { "bsonType": "string" } },
                "items": { "bsonType": "array", "items": {
                    "bsonType": "object",
                    "properties": { "sku": { "bsonType": "string" } }
                } },
                "score": { "bsonType": ["int", "string"] },
            },
        };
        create_collection(&db, "customers", Some(schema)).await;

        let catalog = load(&opts).await.expect("读取 MongoDB ER 目录失败");
        assert_eq!(catalog.tables.len(), 1, "嵌入对象不能变成独立实体");
        let customers = &catalog.tables[0];
        assert_eq!(customers.comment, "客户信息表");
        let columns: Vec<&str> = customers
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        for expected in [
            "_id",
            "name",
            "address",
            "address.city",
            "tags",
            "items[].sku",
            "score",
        ] {
            assert!(
                columns.contains(&expected),
                "缺少字段 {expected}：{columns:?}"
            );
        }
        let signature = |column: &str| {
            catalog
                .types
                .iter()
                .find(|ty| ty.table.name == "customers" && ty.column == column)
                .map(|ty| ty.signature.as_str())
        };
        assert_eq!(signature("name"), Some("bson:string"));
        assert_eq!(signature("address.city"), Some("bson:string"));
        assert_eq!(signature("tags"), None, "数组不能当普通标量证据");
        assert_eq!(signature("items"), None, "对象不能当普通标量证据");
        assert_eq!(signature("score"), None, "混合类型不能当普通标量证据");
        assert!(catalog.document_links.relations.is_empty());
    })
    .await;
}

#[tokio::test]
#[ignore = "需要专用 MongoDB；设置 MONGO_URI"]
async fn simple_lookup_is_relation_but_filtered_pipeline_only_candidate() {
    with_db(|db, opts| async move {
        create_collection(
            &db,
            "orders",
            Some(doc! {
                "bsonType": "object",
                "properties": {
                    "_id": { "bsonType": "objectId" },
                    "customer_id": { "bsonType": "string" },
                    "agent_id": { "bsonType": "string" },
                    "status": { "bsonType": "string" },
                },
            }),
        )
        .await;
        let customer_schema = doc! {
            "bsonType": "object",
            "properties": {
                "_id": { "bsonType": "objectId" },
                "code": { "bsonType": "string" },
            },
        };
        create_collection(&db, "customers", Some(customer_schema.clone())).await;
        create_collection(&db, "agents", Some(customer_schema)).await;
        create_indexes(
            &db,
            "customers",
            vec![index("uk_customers_code", doc! { "code": 1_i32 }, true)],
        )
        .await;

        create_view(
            &db,
            "order_customers",
            "orders",
            vec![doc! {
                "$lookup": {
                    "from": "customers",
                    "localField": "customer_id",
                    "foreignField": "code",
                    "as": "customer",
                }
            }],
        )
        .await;
        create_view(
            &db,
            "filtered_orders",
            "orders",
            vec![
                doc! { "$match": { "status": "paid" } },
                doc! {
                    "$lookup": {
                        "from": "agents",
                        "localField": "agent_id",
                        "foreignField": "code",
                        "as": "agent",
                    }
                },
            ],
        )
        .await;

        let catalog = load(&opts).await.expect("读取 MongoDB ER 目录失败");
        let names: Vec<&str> = catalog
            .tables
            .iter()
            .map(|table| table.name.as_str())
            .collect();
        assert_eq!(names, ["agents", "customers", "orders"]);
        assert!(!names.contains(&"order_customers"));
        assert!(!names.contains(&"filtered_orders"));

        let relation = catalog
            .document_links
            .relations
            .iter()
            .find(|relation| relation.target.table.name == "customers")
            .expect("简单 $lookup 应成为文档查询关系");
        assert_eq!(relation.kind, RelationKind::Lookup);
        assert_eq!(relation.source.table.name, "orders");
        assert_eq!(relation.source.columns, ["customer_id"]);
        assert_eq!(relation.target.table.name, "customers");
        assert_eq!(relation.target.columns, ["code"]);
        assert!(
            relation.reason.contains("order_customers")
                && relation.reason.contains("orders.customer_id")
                && relation.reason.contains("customers.code"),
            "reason 必须可追溯到视图和字段路径：{}",
            relation.reason
        );
        assert!(
            relation.reason.contains("不是物理外键"),
            "查询关系不能伪装成物理外键：{}",
            relation.reason
        );
        assert!(
            !catalog
                .document_links
                .relations
                .iter()
                .any(|relation| relation.target.table.name == "agents"),
            "带 $match 的 pipeline 不能升格为关系"
        );
        assert!(catalog.document_links.candidates.iter().any(|candidate| {
            candidate.target.table.name == "agents" && candidate.reason.contains("filtered_orders")
        }));
        assert!(catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("filtered_orders") && warning.contains("过滤")));
    })
    .await;
}

#[tokio::test]
#[ignore = "需要专用 MongoDB；设置 MONGO_URI"]
async fn sparse_partial_and_unknown_unique_indexes_are_not_global_keys() {
    with_db(|db, opts| async move {
        create_collection(
            &db,
            "accounts",
            Some(doc! {
                "bsonType": "object",
                "properties": {
                    "_id": { "bsonType": "objectId" },
                    "email": { "bsonType": "string" },
                    "tenant": { "bsonType": "string" },
                    "code": { "bsonType": "string" },
                    "active": { "bsonType": "bool" },
                },
            }),
        )
        .await;
        create_indexes(
            &db,
            "accounts",
            vec![
                doc! {
                    "key": { "email": 1_i32 },
                    "name": "uk_email_sparse",
                    "unique": true,
                    "sparse": true,
                },
                doc! {
                    "key": { "code": 1_i32 },
                    "name": "uk_code_partial",
                    "unique": true,
                    "partialFilterExpression": { "active": true },
                },
                index("uk_tenant", doc! { "tenant": 1_i32 }, true),
                index("uk_external", doc! { "external_id": 1_i32 }, true),
            ],
        )
        .await;

        let catalog = load(&opts).await.expect("读取 MongoDB ER 目录失败");
        let keys: Vec<Vec<String>> = catalog
            .keys
            .iter()
            .filter(|key| key.table.name == "accounts")
            .map(|key| key.columns.clone())
            .collect();
        assert!(keys.contains(&vec!["_id".to_string()]));
        assert!(
            keys.contains(&vec!["tenant".to_string()]),
            "普通唯一索引应保留：{keys:?}"
        );
        for rejected in ["email", "code", "external_id"] {
            assert!(
                !keys.contains(&vec![rejected.to_string()]),
                "{rejected} 不应成为全表唯一键：{keys:?}"
            );
        }
        for expected in ["sparse", "partial", "类型未知"] {
            assert!(
                catalog
                    .warnings
                    .iter()
                    .any(|warning| warning.contains(expected)),
                "缺少 {expected} 警告：{:?}",
                catalog.warnings
            );
        }
        let accounts = catalog
            .tables
            .iter()
            .find(|table| table.name == "accounts")
            .unwrap();
        assert!(accounts.indexes.iter().any(|index| {
            index.name == "uk_email_sparse" && index.columns == vec!["email (sparse)".to_string()]
        }));
        assert!(accounts
            .columns
            .iter()
            .any(|column| column.name == "external_id" && column.data_type == "unknown"));
        assert!(
            !catalog
                .types
                .iter()
                .any(|ty| { ty.table.name == "accounts" && ty.column == "external_id" }),
            "索引补回的未知字段不得推测类型"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "需要专用 MongoDB；设置 MONGO_URI"]
async fn string_collation_is_part_of_type_signature() {
    with_db(|db, opts| async move {
        let schema = doc! {
            "bsonType": "object",
            "properties": {
                "_id": { "bsonType": "objectId" },
                "code": { "bsonType": "string" },
            },
        };
        create_collection(&db, "customers_en", Some(schema.clone())).await;
        let _ = db
            .run_command(doc! {
                "create": "customers_ci",
                "validator": { "$jsonSchema": schema },
                "collation": { "locale": "en", "strength": 2_i32 },
            })
            .await
            .expect("创建带 collation 的集合失败");
        for collection in ["customers_en", "customers_ci"] {
            create_indexes(
                &db,
                collection,
                vec![index("uk_code", doc! { "code": 1_i32 }, true)],
            )
            .await;
        }

        let catalog = load(&opts).await.expect("读取 MongoDB ER 目录失败");
        let signature = |table: &str| {
            catalog
                .types
                .iter()
                .find(|ty| ty.table.name == table && ty.column == "code")
                .map(|ty| ty.signature.as_str())
                .unwrap_or_else(|| panic!("缺少 {table}.code 类型签名：{:?}", catalog.types))
        };
        assert_eq!(signature("customers_en"), "bson:string");
        assert!(
            signature("customers_ci").starts_with("bson:string collation:"),
            "字符串比较规则必须进入类型签名：{}",
            signature("customers_ci")
        );
        for table in ["customers_en", "customers_ci"] {
            assert!(
                catalog
                    .keys
                    .iter()
                    .any(|key| key.table.name == table && key.columns == ["code"]),
                "{table}.code 的普通唯一索引应被信任：{:?}",
                catalog.keys
            );
        }
    })
    .await;
}

#[tokio::test]
#[ignore = "需要专用 MongoDB；设置 MONGO_URI"]
async fn list_collections_pagination_reads_every_collection() {
    with_db(|db, opts| async move {
        // listCollections 的首批不是整个目录；超过默认批次大小才能验证 getMore。
        let expected = 110usize;
        for index in 0..expected {
            create_collection(&db, &format!("page_{index:03}"), None).await;
        }
        let catalog = load(&opts).await.expect("读取 MongoDB ER 目录失败");
        assert_eq!(
            catalog.tables.len(),
            expected,
            "分页读取漏掉了集合：{:?}",
            catalog
                .tables
                .iter()
                .map(|table| table.name.as_str())
                .collect::<Vec<_>>()
        );
        assert!(catalog
            .tables
            .iter()
            .all(|table| table.columns.len() == 1 && table.columns[0].name == "_id"));
    })
    .await;
}
