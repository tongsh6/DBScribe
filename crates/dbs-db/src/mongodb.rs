//! MongoDB 元数据读取。
//!
//! MongoDB 没有强制 schema。本驱动把可读信息分成两层（见 docs/databases/mongodb.md）：
//!
//! - **声明层**：集合、索引、validator 字段结构 —— 可复现
//! - **观察层**：采样推断的字段 —— 不可复现，必须标注
//!
//! 字段结构优先读 validator 声明，没有声明时用采样观察兜底，并在表级标注来源。

mod fields;

use crate::options::ConnectionOptions;
use anyhow::{anyhow, Context, Result};
use dbs_core::{ColumnInfo, IndexInfo, TableInfo};
use mongodb::bson::{doc, Bson, Document};
use mongodb::{Client, Database};
use tokio::task::JoinSet;

/// 系统库不参与导出。
const SYSTEM_DATABASES: [&str; 3] = ["admin", "local", "config"];

/// 每个集合的采样条数。
const DEFAULT_SAMPLE_SIZE: usize = 200;

/// 采样并发上限：集合很多时不把数据库打满。
const MAX_CONCURRENCY: usize = 4;

/// 采样与字段推断配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleConfig {
    /// 每个集合的采样条数；0 表示不采样。
    pub sample_size: usize,
    /// 是否用采样补全 validator 未声明的字段；关闭时只读权威层（validator + 索引）。
    pub infer_fields: bool,
}

impl Default for SampleConfig {
    fn default() -> Self {
        Self {
            sample_size: DEFAULT_SAMPLE_SIZE,
            infer_fields: true,
        }
    }
}

/// 建立连接。URI 可能来自完整连接串，也可能由参数拼接。
async fn connect(opts: &ConnectionOptions) -> Result<Client> {
    Client::with_uri_str(opts.mongodb_uri())
        .await
        .with_context(|| format!("连接 MongoDB 失败：{}", opts.describe()))
}

/// 列出可导出的库（排除系统库），按字母序。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    let client = connect(opts).await?;
    let mut names = client
        .list_database_names()
        .await
        .context("列出 MongoDB 数据库失败")?;
    names.retain(|name| !SYSTEM_DATABASES.contains(&name.as_str()));
    names.sort();
    Ok(names)
}

/// 一个集合的声明信息。
#[derive(Debug, Clone)]
struct CollectionSpec {
    name: String,
    /// validator 的 `$jsonSchema` 内容；存在时优先于采样观察。
    validator: Option<Document>,
}

/// (集合声明, 索引, 字段, 字段来源)
type LoadedCollection = (
    CollectionSpec,
    Vec<IndexInfo>,
    Vec<ColumnInfo>,
    Option<fields::FieldOrigin>,
);

/// 读取指定库的全部基础集合与索引。
///
/// 字段优先来自 validator 声明，没有声明时用采样观察兜底；两者都会在表级标注来源。
/// 视图与 timeseries 不是基础表：跳过并记录数量，避免静默丢失。
pub async fn load_tables(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    load_tables_with(opts, &SampleConfig::default()).await
}

/// 带采样配置的读取：桌面端使用默认值，CLI 暴露参数。
pub async fn load_tables_with(
    opts: &ConnectionOptions,
    config: &SampleConfig,
) -> Result<Vec<TableInfo>> {
    let client = connect(opts).await?;
    let database = client.database(&opts.database);

    let reply = database
        .run_command(doc! { "listCollections": 1, "nameOnly": false })
        .await
        .context("读取集合列表失败")?;
    let specs = parse_collection_specs(&reply);
    let skipped = parse_skipped_count(&reply);
    if skipped > 0 {
        tracing::info!("跳过 {skipped} 个视图 / 时序集合（只导出基础集合）");
    }

    let mut loaded = fetch_collections(&database, &specs, config).await?;
    loaded.sort_by(|a, b| a.0.name.cmp(&b.0.name));

    let tables: Vec<TableInfo> = loaded
        .into_iter()
        .enumerate()
        .map(|(idx, (spec, indexes, columns, origin))| TableInfo {
            order: (idx + 1) as u32,
            // 单库导出：schema 留空，表名不带库前缀（与关系型单 schema 一致）
            schema: String::new(),
            name: spec.name,
            // MongoDB 没有集合注释
            comment: String::new(),
            note: origin.map(|origin| origin.note()).unwrap_or_default(),
            columns,
            indexes,
            foreign_keys: Vec::new(),
            is_partitioned: false,
        })
        .collect();

    tracing::info!(
        "已读取 {} 个集合、{} 个索引、{} 个字段",
        tables.len(),
        tables.iter().map(|t| t.indexes.len()).sum::<usize>(),
        tables.iter().map(|t| t.columns.len()).sum::<usize>()
    );
    Ok(tables)
}

/// 并发读取每个集合的字段与索引（上限 MAX_CONCURRENCY）。
async fn fetch_collections(
    database: &Database,
    specs: &[CollectionSpec],
    config: &SampleConfig,
) -> Result<Vec<LoadedCollection>> {
    let mut set: JoinSet<Result<LoadedCollection>> = JoinSet::new();
    let mut pending = specs.iter().cloned();
    let mut loaded = Vec::with_capacity(specs.len());
    let mut in_flight = 0usize;

    loop {
        while in_flight < MAX_CONCURRENCY {
            let Some(spec) = pending.next() else {
                break;
            };
            let database = database.clone();
            let config = config.clone();
            set.spawn(async move {
                let (columns, origin) = load_columns(&database, &spec, &config).await?;
                let indexes = load_indexes(&database, &spec.name).await?;
                Ok((spec, indexes, columns, origin))
            });
            in_flight += 1;
        }
        if in_flight == 0 {
            break;
        }
        if let Some(result) = set.join_next().await {
            loaded.push(result.context("集合读取任务异常退出")??);
            in_flight -= 1;
        }
    }
    Ok(loaded)
}

/// 读取单个集合的列信息（界面预览用）。
pub async fn load_table_columns(opts: &ConnectionOptions, table: &str) -> Result<Vec<ColumnInfo>> {
    let client = connect(opts).await?;
    let database = client.database(&opts.database);
    let spec = load_collection_spec(&database, table).await?;
    let (columns, _) = load_columns(&database, &spec, &SampleConfig::default()).await?;
    Ok(columns)
}

async fn load_collection_spec(database: &Database, name: &str) -> Result<CollectionSpec> {
    let reply = database
        .run_command(doc! {
            "listCollections": 1,
            "nameOnly": false,
            "filter": { "name": name },
        })
        .await
        .with_context(|| format!("读取集合 {name} 的元信息失败"))?;
    parse_collection_specs(&reply)
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("集合 {name} 不存在"))
}

/// 一个集合的字段：声明优先，采样兜底。
async fn load_columns(
    database: &Database,
    spec: &CollectionSpec,
    config: &SampleConfig,
) -> Result<(Vec<ColumnInfo>, Option<fields::FieldOrigin>)> {
    // 关闭推断或采样数为 0 时完全不采样：只读 validator 声明
    let infer = config.infer_fields && config.sample_size > 0;
    let docs = if infer {
        sample_documents(database, &spec.name, config.sample_size).await?
    } else {
        Vec::new()
    };
    let observed = fields::aggregate_documents(&docs);
    let declared = spec
        .validator
        .as_ref()
        .map(fields::parse_json_schema)
        .unwrap_or_default();
    Ok(fields::merge_columns(
        &declared,
        infer.then_some(&observed),
        docs.len(),
    ))
}

/// 采样一个集合；`$sample` 本身是随机的，因此结果必须带来源标注。
async fn sample_documents(
    database: &Database,
    collection: &str,
    size: usize,
) -> Result<Vec<Document>> {
    if size == 0 {
        return Ok(Vec::new());
    }
    let reply = database
        .run_command(doc! {
            "aggregate": collection,
            "pipeline": [{ "$sample": { "size": size as i64 } }],
            "cursor": Document::new(),
        })
        .await
        .with_context(|| format!("采样集合 {collection} 失败"))?;
    Ok(collection_docs(&reply).cloned().collect())
}

async fn load_indexes(database: &Database, collection: &str) -> Result<Vec<IndexInfo>> {
    let reply = database
        .run_command(doc! { "listIndexes": collection })
        .await?;
    Ok(parse_indexes(&reply))
}

/// 从 `listCollections` 的返回里取出基础集合与 validator，按字母序。
fn parse_collection_specs(reply: &Document) -> Vec<CollectionSpec> {
    let mut specs: Vec<CollectionSpec> = collection_docs(reply)
        .filter(|doc| is_base_collection(doc))
        .filter_map(|doc| {
            Some(CollectionSpec {
                name: doc.get_str("name").ok()?.to_string(),
                validator: validator_of(doc),
            })
        })
        .collect();
    specs.sort_by(|a, b| a.name.cmp(&b.name));
    specs
}

/// validator 的 `$jsonSchema` 内容；非 JSON Schema 形式的 validator 返回 None。
fn validator_of(doc: &Document) -> Option<Document> {
    doc.get_document("options")
        .ok()?
        .get_document("validator")
        .ok()?
        .get_document("$jsonSchema")
        .ok()
        .cloned()
}

/// 被跳过的视图 / 时序集合数量（系统集合不计入）。
fn parse_skipped_count(reply: &Document) -> usize {
    collection_docs(reply)
        .filter(|doc| !is_system_collection(doc))
        .filter(|doc| {
            doc.get_str("type")
                .map(|kind| kind != "collection")
                .unwrap_or(false)
        })
        .count()
}

fn is_base_collection(doc: &Document) -> bool {
    let is_collection = doc
        .get_str("type")
        .map(|kind| kind == "collection")
        .unwrap_or(false);
    is_collection && !is_system_collection(doc)
}

fn is_system_collection(doc: &Document) -> bool {
    doc.get_str("name")
        .map(|name| name.starts_with("system."))
        .unwrap_or(true)
}

fn collection_docs(reply: &Document) -> impl Iterator<Item = &Document> + '_ {
    reply
        .get_document("cursor")
        .ok()
        .and_then(|cursor| cursor.get_array("firstBatch").ok())
        .into_iter()
        .flatten()
        .filter_map(Bson::as_document)
}

/// 从 `listIndexes` 的返回里解析索引；主键 `_id_` 保留，由导出层去重。
fn parse_indexes(reply: &Document) -> Vec<IndexInfo> {
    let mut indexes: Vec<IndexInfo> = collection_docs(reply).map(index_info).collect();
    indexes.sort_by(|a, b| a.name.cmp(&b.name));
    indexes
}

/// 单个索引文档 → `IndexInfo`。
///
/// TTL / sparse / hidden / partial 这些选项写进列文本，不改公共模型。
fn index_info(doc: &Document) -> IndexInfo {
    let name = doc.get_str("name").unwrap_or_default().to_string();
    let is_primary = name == "_id_";
    let is_unique = is_primary || doc.get_bool("unique").unwrap_or(false);
    IndexInfo {
        name,
        columns: index_columns(doc)
            .into_iter()
            .map(|column| decorate_column(column, doc))
            .collect(),
        is_unique,
        is_primary,
    }
}

/// 索引键：文本索引用 `$text` 表达，其余用字段名。
fn index_columns(doc: &Document) -> Vec<String> {
    if doc.contains_key("weights") {
        return vec!["$text".to_string()];
    }
    doc.get_document("key")
        .map(|key| key.keys().cloned().collect())
        .unwrap_or_default()
}

/// 把索引选项追加到列文本上，例如 `created_at (TTL 3600s)`。
fn decorate_column(column: String, doc: &Document) -> String {
    let mut text = column;
    if let Some(seconds) = expiry_seconds(doc) {
        text = format!("{text} (TTL {seconds}s)");
    }
    if doc.get_bool("sparse").unwrap_or(false) {
        text = format!("{text} (sparse)");
    }
    if doc.get_bool("hidden").unwrap_or(false) {
        text = format!("{text} (hidden)");
    }
    if let Ok(partial) = doc.get_document("partialFilterExpression") {
        text = format!("{text} (partial: {})", truncate(&partial.to_string(), 40));
    }
    text
}

/// `expireAfterSeconds` 可能是 int32 / int64 / double，统一取值。
fn expiry_seconds(doc: &Document) -> Option<i64> {
    match doc.get("expireAfterSeconds") {
        Some(Bson::Int32(value)) => Some(*value as i64),
        Some(Bson::Int64(value)) => Some(*value),
        Some(Bson::Double(value)) => Some(*value as i64),
        _ => None,
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    fn cursor_reply(batch: Vec<Document>) -> Document {
        doc! {
            "cursor": {
                "firstBatch": batch.iter().map(|d| Bson::Document(d.clone())).collect::<Vec<_>>(),
                "id": 0_i64,
            }
        }
    }

    #[test]
    fn sample_config_defaults_are_stable() {
        let config = SampleConfig::default();
        assert_eq!(config.sample_size, 200);
        assert!(config.infer_fields);
    }

    #[test]
    fn collections_keep_base_only_and_sort() {
        let reply = cursor_reply(vec![
            doc! { "name": "orders", "type": "collection" },
            doc! { "name": "users", "type": "collection" },
            doc! { "name": "order_view", "type": "view" },
            doc! { "name": "system.views", "type": "collection" },
        ]);
        let specs = parse_collection_specs(&reply);
        let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, vec!["orders", "users"]);
        assert_eq!(parse_skipped_count(&reply), 1);
    }

    #[test]
    fn validator_is_extracted_from_collection_options() {
        let reply = cursor_reply(vec![doc! {
            "name": "users",
            "type": "collection",
            "options": {
                "validator": { "$jsonSchema": {
                    "bsonType": "object",
                    "required": ["name"],
                    "properties": { "name": { "bsonType": "string" } },
                } },
            },
        }]);
        let specs = parse_collection_specs(&reply);
        assert_eq!(specs.len(), 1);
        let validator = specs[0].validator.as_ref().expect("应解析出 validator");
        let declared = fields::parse_json_schema(validator);
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].path, "name");
        assert!(declared[0].required);
    }

    #[test]
    fn index_keeps_key_order_and_flags() {
        let reply = cursor_reply(vec![
            doc! { "name": "_id_", "key": { "_id": 1_i32 } },
            doc! { "name": "idx_user_time", "key": { "user_id": 1_i32, "created_at": -1_i32 }, "unique": true },
        ]);
        let indexes = parse_indexes(&reply);
        assert_eq!(indexes.len(), 2);
        assert_eq!(indexes[0].name, "_id_");
        assert!(indexes[0].is_primary);
        assert!(indexes[0].is_unique);
        assert_eq!(indexes[1].columns, vec!["user_id", "created_at"]);
        assert!(indexes[1].is_unique);
        assert!(!indexes[1].is_primary);
    }

    #[test]
    fn index_options_are_written_into_column_text() {
        let reply = cursor_reply(vec![
            doc! { "name": "ttl_idx", "key": { "created_at": 1_i32 }, "expireAfterSeconds": 3600_i32 },
            doc! { "name": "sparse_idx", "key": { "email": 1_i32 }, "sparse": true },
        ]);
        let indexes = parse_indexes(&reply);
        let columns_of = |name: &str| {
            indexes
                .iter()
                .find(|index| index.name == name)
                .map(|index| index.columns.clone())
                .unwrap_or_default()
        };
        assert_eq!(columns_of("ttl_idx"), vec!["created_at (TTL 3600s)"]);
        assert_eq!(columns_of("sparse_idx"), vec!["email (sparse)"]);
    }

    #[test]
    fn text_index_is_shown_as_dollar_text() {
        let reply = cursor_reply(vec![doc! {
            "name": "text_idx",
            "key": { "_fts": "text", "_ftsx": 1_i32 },
            "weights": { "title": 1_i32 },
        }]);
        let indexes = parse_indexes(&reply);
        assert_eq!(indexes[0].columns, vec!["$text"]);
    }

    #[test]
    fn truncate_keeps_char_boundary() {
        assert_eq!(truncate("短文本", 10), "短文本");
        assert_eq!(truncate("一二三四五", 3), "一二三…");
    }
}
