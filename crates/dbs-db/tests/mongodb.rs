//! MongoDB 集成测试：需要真实实例，默认不运行。
//!
//! 本地：
//! `MONGO_URI=mongodb://127.0.0.1:27017 cargo test -p dbs-db --test mongodb -- --ignored`
//! CI：由 `mongodb` job 提供 `mongo:7` service。

use dbs_core::DbType;
use dbs_db::{load_tables, ConnectionOptions};
use mongodb::bson::{doc, Document};
use mongodb::Client;

const DB_NAME: &str = "dbs_integration";

fn uri() -> String {
    std::env::var("MONGO_URI").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_string())
}

#[tokio::test]
#[ignore = "需要真实 MongoDB 实例（本地或 CI service）"]
async fn reads_declared_and_sampled_fields_with_indexes() {
    let client = Client::with_uri_str(uri()).await.expect("连接失败");
    let database = client.database(DB_NAME);
    // 幂等：上一轮失败也不影响本轮
    database.drop().await.ok();

    // 声明层：validator 描述 name / email
    database
        .run_command(doc! {
            "create": "users",
            "validator": { "$jsonSchema": {
                "bsonType": "object",
                "required": ["name"],
                "properties": {
                    "name": { "bsonType": "string" },
                    "email": { "bsonType": ["string", "null"] },
                },
            } },
        })
        .await
        .expect("创建 users 失败");
    database
        .run_command(doc! { "create": "orders" })
        .await
        .expect("创建 orders 失败");
    database
        .run_command(doc! {
            "createIndexes": "users",
            "indexes": [
                { "key": { "email": 1_i32 }, "name": "email_1", "unique": true },
                { "key": { "created_at": 1_i32 }, "name": "ttl_1", "expireAfterSeconds": 3600_i64 },
            ],
        })
        .await
        .expect("创建索引失败");

    database
        .collection::<Document>("users")
        .insert_many(vec![
            doc! { "name": "a", "email": "a@example.com", "extra": "sampled" },
            doc! { "name": "b", "extra": 1_i32 },
        ])
        .await
        .expect("写入 users 失败");
    database
        .collection::<Document>("orders")
        .insert_many(vec![
            doc! { "code": "A", "amount": 10.5_f64 },
            doc! { "code": "B" },
        ])
        .await
        .expect("写入 orders 失败");

    let opts = ConnectionOptions::new(DbType::MongoDb, uri(), 27017, "", "", DB_NAME);
    let tables = load_tables(&opts).await.expect("读取集合失败");

    // 声明层：字段来自 validator，非必填字段有标注
    let users = tables
        .iter()
        .find(|t| t.name == "users")
        .expect("缺少 users");
    assert!(
        users.note.contains("validator"),
        "来源标注缺失：{}",
        users.note
    );
    let name = users
        .columns
        .iter()
        .find(|c| c.name == "name")
        .expect("缺少 name");
    assert_eq!(name.data_type, "string");
    assert!(!name.nullable);
    let email = users
        .columns
        .iter()
        .find(|c| c.name == "email")
        .expect("缺少 email");
    assert!(email.nullable, "bsonType 含 null 应标记为允许空");
    assert_eq!(email.comment, "可选（可能缺失）");
    // 采样补充了 validator 未声明的字段
    assert!(
        users.note.contains("采样补充"),
        "应为混合来源：{}",
        users.note
    );
    assert!(users.columns.iter().any(|c| c.name == "extra"));

    // 观察层：没有 validator 时全部来自采样
    let orders = tables
        .iter()
        .find(|t| t.name == "orders")
        .expect("缺少 orders");
    assert!(
        orders.note.contains("采样推断"),
        "来源标注缺失：{}",
        orders.note
    );
    let amount = orders
        .columns
        .iter()
        .find(|c| c.name == "amount")
        .expect("缺少 amount");
    assert_eq!(amount.comment, "出现率 50%");
    assert!(orders
        .columns
        .first()
        .is_some_and(|c| c.name == "_id" && c.is_primary_key));

    // 索引：唯一索引与 TTL 选项
    let email_index = users
        .indexes
        .iter()
        .find(|i| i.name == "email_1")
        .expect("缺少 email_1");
    assert!(email_index.is_unique);
    let ttl_index = users
        .indexes
        .iter()
        .find(|i| i.name == "ttl_1")
        .expect("缺少 ttl_1");
    assert!(ttl_index.columns[0].contains("TTL 3600s"));

    database.drop().await.ok();
}
