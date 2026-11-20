//! MongoDB 的 ER 目录读取。
//!
//! 首版只读声明层元数据：`listCollections` / `listIndexes` 游标与视图定义。
//! 不执行视图、不采样、不读业务行，也不把 URI 凭证写进任何关系或警告。
//!
//! MongoDB 没有物理外键。`$lookup` 的 `localField` / `foreignField` 是视图定义里
//! 可追溯的查询关系，只能写入 [`ErCatalog::document_links`]，不能伪装成外键。

use crate::ConnectionOptions;
use anyhow::{Context, Result};
use dbs_core::er::{
    Analysis, Candidate, Endpoint, ErCatalog, ErColumnType, Relation, RelationKind, TableId,
};
use dbs_core::{ColumnInfo, IndexInfo, TableInfo};
use mongodb::bson::{self, Bson, Document};
use mongodb::results::{CollectionSpecification, CollectionType};
use mongodb::{Database, IndexModel};
use std::collections::{BTreeMap, BTreeSet};

/// validator 字段展开的最大层级，与普通数据字典的路径规则保持一致。
const MAX_DEPTH: usize = 3;

/// 一个基础集合的原始元数据。
#[derive(Debug, Clone)]
struct CollectionMeta {
    name: String,
    kind: MetaKind,
    validator: Option<Document>,
    enforced: bool,
    /// 集合默认 collation 的规范化文档；用于字符串比较规则和唯一索引核对。
    collation: Option<Document>,
    indexes: Vec<IndexMeta>,
}

#[derive(Debug, Clone)]
enum MetaKind {
    Base,
    View {
        view_on: Option<String>,
        pipeline: Vec<Document>,
    },
}

/// `listIndexes` 里真正需要的原始索引信息。
#[derive(Debug, Clone)]
struct IndexMeta {
    name: String,
    /// 原始键位顺序；值保留 BSON 类型，用来识别表达式 / hashed / 文本索引。
    keys: Vec<(String, Bson)>,
    unique: bool,
    sparse: bool,
    partial: Option<Document>,
    hidden: bool,
    text: bool,
    /// 索引显式声明的 collation；`None` 表示继承集合默认值。
    collation: Option<Document>,
}

/// 字段可证明的程度。只有 `Scalar` 才能作为普通标量键或类型签名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldShape {
    Scalar,
    Container,
    Mixed,
    Unknown,
    UnknownPath,
}

#[derive(Debug, Clone, Default)]
struct FieldFacts {
    shape: Option<FieldShape>,
    signature: Option<String>,
    /// 容器是否为数组；数组字段的点号路径会被 MongoDB 展开成多值。
    array: bool,
}

impl FieldFacts {
    fn declared(types: &[String], collation: Option<&str>) -> Self {
        let non_null: Vec<&str> = types
            .iter()
            .map(String::as_str)
            .filter(|kind| *kind != "null")
            .collect();
        let container = types
            .iter()
            .any(|kind| kind == "object" || kind.starts_with("array"));
        let array = types
            .iter()
            .any(|kind| kind == "array" || kind.starts_with("array<"));
        let mixed = non_null.len() > 1;
        let scalar = non_null.len() == 1 && !container;
        let signature = scalar
            .then(|| scalar_signature(non_null[0], collation))
            .flatten();
        let shape = if mixed {
            FieldShape::Mixed
        } else if container {
            FieldShape::Container
        } else if scalar && signature.is_some() {
            FieldShape::Scalar
        } else {
            FieldShape::Unknown
        };
        Self {
            shape: Some(shape),
            signature,
            array,
        }
    }

    fn unknown() -> Self {
        Self {
            shape: Some(FieldShape::Unknown),
            signature: None,
            array: false,
        }
    }

    fn scalar(&self) -> bool {
        self.shape == Some(FieldShape::Scalar) && self.signature.is_some()
    }
}

/// 从 BSON 类型名得到可比较的标量签名；对象、数组、混合类型和未知类型一律不给签名。
fn scalar_signature(bson_type: &str, collation: Option<&str>) -> Option<String> {
    let base = match bson_type {
        "string" => "string",
        "int32" | "int64" | "double" | "decimal128" | "number" => bson_type,
        "bool" | "date" | "objectId" | "binary" | "timestamp" | "regex" => bson_type,
        _ => return None,
    };
    let mut signature = format!("bson:{base}");
    if base == "string" {
        if let Some(collation) = collation.filter(|value| !value.is_empty()) {
            signature.push_str(" collation:");
            signature.push_str(collation);
        }
    }
    Some(signature)
}

/// 一个集合的待装配结构。
struct Draft {
    table: TableInfo,
    facts: BTreeMap<String, FieldFacts>,
}

/// 视图里的 $lookup 证据，先收集，最后统一裁决和去重。
#[derive(Debug, Clone)]
struct LinkEvidence {
    source: Endpoint,
    target: Endpoint,
    candidate: bool,
    reason: String,
}

/// 读取指定库的 ER 目录。只走元数据游标，不执行视图，不采样业务行。
pub async fn load(opts: &ConnectionOptions) -> Result<ErCatalog> {
    if opts.database.trim().is_empty() {
        anyhow::bail!("未指定 MongoDB 数据库");
    }
    let client = super::connect(opts).await?;
    let database = client.database(&opts.database);

    let mut warnings = Vec::new();
    let mut collections = Vec::new();
    let mut cursor = database
        .list_collections()
        .await
        .context("读取集合列表失败")?;
    // 驱动游标会自动 getMore，不能用 firstBatch 代表整个目录。
    while cursor.advance().await.context("读取集合列表失败")? {
        let spec = cursor.deserialize_current().context("解析集合元信息失败")?;
        if let Some(meta) = collection_meta(spec) {
            collections.push(meta);
        }
    }

    for meta in &mut collections {
        if !matches!(meta.kind, MetaKind::Base) {
            continue;
        }
        match load_indexes(&database, &meta.name).await {
            Ok(indexes) => meta.indexes = indexes,
            Err(err) => warnings.push(format!(
                "集合 {} 的索引读取失败，只保留 _id 与声明字段：{err}",
                meta.name
            )),
        }
    }

    let mut catalog = build_catalog(collections);
    // mongos 不保证任意集合的 _id 在各分片间唯一。仅凭普通目录权限无法证明分片键。
    let hello = client
        .database("admin")
        .run_command(mongodb::bson::doc! {"hello":1})
        .await;
    let local_keys = hello
        .as_ref()
        .is_ok_and(|info| info.get_str("msg") != Ok("isdbgrid") && !info.contains_key("serviceId"));
    if !local_keys {
        catalog.keys.clear();
        catalog.guarded_keys.clear();
        warnings.push("当前连接经过分片路由或无法确认拓扑，未把目录索引用作跨分片的全局唯一键；视图查询关联仍可展示".into());
    }
    catalog.warnings.extend(warnings);
    catalog.warnings.sort();
    catalog.warnings.dedup();
    Ok(catalog)
}

/// `listCollections` 的一条结果 → 内部元数据；系统集合 / 时序集合不作为实体。
fn collection_meta(spec: CollectionSpecification) -> Option<CollectionMeta> {
    if spec.name.starts_with("system.") {
        return None;
    }
    let collation = spec
        .options
        .collation
        .as_ref()
        .and_then(|collation| bson::to_document(collation).ok());
    let validator = spec
        .options
        .validator
        .as_ref()
        .and_then(|rule| rule.get_document("$jsonSchema").ok())
        .cloned();
    use mongodb::options::{ValidationAction, ValidationLevel};
    let enforced = matches!(
        spec.options.validation_level,
        None | Some(ValidationLevel::Strict)
    ) && matches!(
        spec.options.validation_action,
        None | Some(ValidationAction::Error)
    );
    let view_on = spec.options.view_on.clone();
    let pipeline = spec.options.pipeline.clone().unwrap_or_default();
    let timeseries = spec.options.timeseries.is_some();

    match spec.collection_type {
        CollectionType::View => Some(CollectionMeta {
            name: spec.name,
            kind: MetaKind::View { view_on, pipeline },
            validator: None,
            enforced: false,
            collation,
            indexes: Vec::new(),
        }),
        CollectionType::Timeseries => None,
        _ if timeseries => None,
        _ => Some(CollectionMeta {
            name: spec.name,
            kind: MetaKind::Base,
            validator,
            enforced,
            collation,
            indexes: Vec::new(),
        }),
    }
}

/// 通过驱动游标分页读取一个集合的全部索引。
async fn load_indexes(database: &Database, collection: &str) -> Result<Vec<IndexMeta>> {
    let mut cursor = database
        .collection::<Document>(collection)
        .list_indexes()
        .await
        .with_context(|| format!("读取集合 {collection} 的索引失败"))?;
    let mut indexes = Vec::new();
    while cursor
        .advance()
        .await
        .with_context(|| format!("读取集合 {collection} 的索引游标失败"))?
    {
        indexes.push(index_meta(
            cursor
                .deserialize_current()
                .with_context(|| format!("解析集合 {collection} 的索引失败"))?,
        ));
    }
    indexes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(indexes)
}

fn index_meta(index: IndexModel) -> IndexMeta {
    let keys: Vec<(String, Bson)> = index.keys.into_iter().collect();
    let inferred_name = keys
        .iter()
        .map(|(key, value)| format!("{key}_{value}"))
        .collect::<Vec<_>>()
        .join("_");
    let options = index.options.unwrap_or_default();
    let name = options.name.unwrap_or(inferred_name);
    let collation = options
        .collation
        .as_ref()
        .and_then(|collation| bson::to_document(collation).ok());
    let text = options.weights.is_some()
        || keys
            .iter()
            .any(|(_, value)| matches!(value, Bson::String(value) if value == "text"));
    IndexMeta {
        name,
        keys,
        unique: options.unique.unwrap_or(false),
        sparse: options.sparse.unwrap_or(false),
        partial: options.partial_filter_expression,
        hidden: options.hidden.unwrap_or(false),
        text,
        collation,
    }
}

/// 纯装配：相同元数据输入得到相同目录。
fn build_catalog(collections: Vec<CollectionMeta>) -> ErCatalog {
    let mut warnings = Vec::new();
    let mut bases = Vec::new();
    let mut views = Vec::new();
    for collection in collections {
        match &collection.kind {
            MetaKind::Base => bases.push(collection),
            MetaKind::View { .. } => views.push(collection),
        }
    }
    bases.sort_by(|a, b| a.name.cmp(&b.name));
    views.sort_by(|a, b| a.name.cmp(&b.name));

    let mut drafts: BTreeMap<String, Draft> = BTreeMap::new();
    for meta in &bases {
        let mut draft = build_draft(meta);
        if meta.validator.is_some() && !meta.enforced {
            warnings.push(format!(
                "集合 {} 的验证器未严格执行，声明字段仅供展示，暂缓类型推断",
                meta.name
            ));
            for value in draft.facts.values_mut() {
                value.signature = None;
            }
        }
        drafts.insert(meta.name.clone(), draft);
    }

    // 索引引用了该路径，即使尚无任何文档包含它；保留路径，不推测类型。
    for meta in &bases {
        let Some(draft) = drafts.get_mut(&meta.name) else {
            continue;
        };
        for index in &meta.indexes {
            for (field, _) in &index.keys {
                if plain_path(field) {
                    ensure_unknown_field(draft, field);
                }
            }
        }
    }

    let mut links = Vec::new();
    analyze_views(&views, &mut drafts, &mut links, &mut warnings);

    let mut keys = Vec::new();
    let mut guarded_keys = Vec::new();
    for meta in &bases {
        let Some(draft) = drafts.get(&meta.name) else {
            continue;
        };
        keys.push(Endpoint {
            table: TableId::from(&draft.table),
            columns: vec!["_id".to_string()],
        });
        for index in &meta.indexes {
            if let Some(key) = guarded_index(meta, draft, index) {
                guarded_keys.push(key);
            }
            if let Some(key) = index_key(meta, draft, index, &mut warnings) {
                keys.push(key);
            }
        }
    }
    keys.sort();
    keys.dedup();

    let mut types = Vec::new();
    for draft in drafts.values() {
        let table = TableId::from(&draft.table);
        for column in &draft.table.columns {
            // 数组元素可以显示其 BSON 类型，但其路径在集合级比较中是多值的。
            if column.name.contains("[]") {
                continue;
            }
            if let Some(signature) = draft
                .facts
                .get(&column.name)
                .and_then(|facts| facts.signature.clone())
            {
                types.push(ErColumnType {
                    table: table.clone(),
                    column: column.name.clone(),
                    signature,
                });
            }
        }
    }
    types.sort_by(|a, b| {
        (&a.table, &a.column, &a.signature).cmp(&(&b.table, &b.column, &b.signature))
    });

    let mut tables: Vec<TableInfo> = drafts.into_values().map(|draft| draft.table).collect();
    tables.sort_by(|a, b| a.name.cmp(&b.name));
    for (index, table) in tables.iter_mut().enumerate() {
        table.order = (index + 1) as u32;
        for (column_index, column) in table.columns.iter_mut().enumerate() {
            column.order = (column_index + 1) as u32;
        }
    }

    let document_links = finish_links(links);
    let mut catalog = ErCatalog {
        tables,
        keys,
        guarded_keys,
        types,
        document_links,
        db_type: Some(dbs_core::DbType::MongoDb),
        warnings,
        ..Default::default()
    };
    catalog.warnings.sort();
    catalog.warnings.dedup();
    catalog
}

fn build_draft(meta: &CollectionMeta) -> Draft {
    let collation = meta
        .collation
        .as_ref()
        .map(canonical_collation)
        .filter(|text| !text.is_empty());
    let mut table = TableInfo {
        schema: String::new(),
        name: meta.name.clone(),
        note: meta
            .validator
            .as_ref()
            .map(|_| "字段来源：collection validator（$jsonSchema 声明）；未采样业务行".to_string())
            .unwrap_or_else(|| {
                "字段来源：无 validator，仅保留 _id 与索引/视图端点补充字段，类型未知；未采样业务行"
                    .to_string()
            }),
        ..Default::default()
    };
    let mut facts = BTreeMap::new();

    if let Some(schema) = &meta.validator {
        parse_declared(schema, collation.as_deref(), &mut table, &mut facts);
        table.comment = schema
            .get_str("description")
            .map(str::trim)
            .unwrap_or("")
            .to_string();
    }

    ensure_id_column(&mut table, &mut facts);
    table.indexes = meta.indexes.iter().map(index_info).collect();
    Draft { table, facts }
}

fn parse_declared(
    schema: &Document,
    collation: Option<&str>,
    table: &mut TableInfo,
    facts: &mut BTreeMap<String, FieldFacts>,
) {
    let descriptions = schema_descriptions(schema);
    let mut optional = Vec::new();
    for field in super::fields::parse_json_schema(schema) {
        let path = normalize_path(&field.path);
        if facts.contains_key(&path) {
            continue;
        }
        let fact = FieldFacts::declared(&field.types, collation);
        let comment = descriptions.get(&path).cloned().unwrap_or_default();
        if !field.required && path != "_id" {
            optional.push(path.clone());
        }
        table.columns.push(ColumnInfo {
            order: 0,
            name: path.clone(),
            data_type: if field.types.is_empty() {
                "unknown".to_string()
            } else {
                field.types.join(" | ")
            },
            length: String::new(),
            is_primary_key: path == "_id",
            is_auto_increment: false,
            nullable: field.types.iter().any(|kind| kind == "null"),
            default_value: String::new(),
            comment,
        });
        facts.insert(path, fact);
    }
    if !optional.is_empty() {
        table
            .note
            .push_str(&format!("；可缺失的声明字段：{}", optional.join("、")));
    }
}

fn ensure_id_column(table: &mut TableInfo, facts: &mut BTreeMap<String, FieldFacts>) {
    if let Some(position) = table.columns.iter().position(|column| column.name == "_id") {
        let mut column = table.columns.remove(position);
        column.is_primary_key = true;
        table.columns.insert(0, column);
    } else {
        table.columns.insert(
            0,
            ColumnInfo {
                order: 1,
                name: "_id".to_string(),
                data_type: "unknown".to_string(),
                length: String::new(),
                is_primary_key: true,
                is_auto_increment: false,
                nullable: false,
                default_value: String::new(),
                comment: String::new(),
            },
        );
        facts.insert("_id".to_string(), FieldFacts::unknown());
    }
    for column in &mut table.columns {
        if column.name == "_id" {
            column.is_primary_key = true;
        }
    }
}

/// 补充目录中被引用的路径，不能由此断定业务文档中存在具体值。
fn ensure_unknown_field(draft: &mut Draft, path: &str) {
    if !plain_path(path) || draft.facts.contains_key(path) {
        return;
    }
    draft.table.columns.push(ColumnInfo {
        order: 0,
        name: path.to_string(),
        data_type: "unknown".to_string(),
        length: String::new(),
        is_primary_key: false,
        is_auto_increment: false,
        nullable: true,
        default_value: String::new(),
        // 此处没有数据库作者的注释。字段的出处已保留在独立的索引/视图目录中。
        comment: String::new(),
    });
    draft.facts.insert(path.to_string(), FieldFacts::unknown());
}

fn plain_path(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('$') && !path.contains("[]") && !path.contains('\0')
}

/// 仅完整保留可理解的合取等值谓词；不能把不认识的条件从索引上剥掉。
fn guarded_index(
    meta: &CollectionMeta,
    draft: &Draft,
    index: &IndexMeta,
) -> Option<dbs_core::er::GuardedKey> {
    use dbs_core::er::{GuardedKey, RequiredEquality};
    let filter = index.partial.as_ref()?;
    if !index.unique || index.sparse || index.text || index.keys.len() < 2 {
        return None;
    }
    if index
        .collation
        .as_ref()
        .is_some_and(|c| !index_collation_inherits(c, meta.collation.as_ref()))
    {
        return None;
    }
    let mut seen = BTreeSet::new();
    for (name, direction) in &index.keys {
        if !plain_path(name) || !plain_direction(direction) || !seen.insert(name) {
            return None;
        }
        if draft
            .facts
            .get(name)
            .is_some_and(|f| matches!(f.shape, Some(FieldShape::Container | FieldShape::Mixed)))
        {
            return None;
        }
        if name.split('.').count() > 1 && !draft.facts.get(name).is_some_and(|f| f.scalar()) {
            return None;
        }
    }
    let mut predicates = BTreeMap::new();
    equality_filter(filter, &mut predicates)?;
    if predicates.is_empty() {
        return None;
    }
    Some(GuardedKey {
        key: Endpoint {
            table: TableId::from(&draft.table),
            columns: index.keys.iter().map(|(k, _)| k.clone()).collect(),
        },
        required_target: predicates
            .into_iter()
            .map(|(column, value)| RequiredEquality { column, value })
            .collect(),
    })
}

fn equality_filter(
    doc: &Document,
    out: &mut BTreeMap<String, dbs_core::er::ScalarValue>,
) -> Option<()> {
    use dbs_core::er::ScalarValue;
    if doc.is_empty() {
        return None;
    }
    for (field, value) in doc {
        if field == "$and" {
            let terms = value.as_array()?;
            if terms.is_empty() {
                return None;
            }
            for t in terms {
                equality_filter(t.as_document()?, out)?;
            }
            continue;
        }
        if !plain_path(field) || field.split('.').any(|p| p.is_empty() || p.starts_with('$')) {
            return None;
        }
        let term = match value {
            Bson::Document(d) if d.len() == 1 => d.get("$eq")?,
            other => other,
        };
        let literal = match term {
            Bson::Boolean(v) => ScalarValue::Bool(*v),
            Bson::Int32(v) => ScalarValue::Integer(i64::from(*v)),
            // 桌面端通过 JSON Number 展示，超出可精确表示的范围时保留为不支持的谓词。
            Bson::Int64(v) if v.unsigned_abs() < (1_u64 << 53) => ScalarValue::Integer(*v),
            Bson::String(v) => ScalarValue::Text(v.clone()),
            _ => return None,
        };
        if out.get(field).is_some_and(|previous| previous != &literal) {
            return None;
        }
        out.insert(field.clone(), literal);
    }
    Some(())
}

/// 从原始唯一索引证明全表唯一键；范围受限的索引只用于条件候选。
fn index_key(
    meta: &CollectionMeta,
    draft: &Draft,
    index: &IndexMeta,
    warnings: &mut Vec<String>,
) -> Option<Endpoint> {
    if is_id_index(index) || !index.unique {
        return None;
    }
    let mut gaps = Vec::new();
    if index.sparse {
        gaps.push("索引为 sparse（稀疏），未覆盖缺少该字段的文档".to_string());
    }
    if index.partial.is_some() {
        gaps.push("索引为 partial（部分），只覆盖满足过滤条件的文档".to_string());
    }
    if index.text {
        gaps.push("索引是 text/表达式索引，不是普通字段键".to_string());
    }
    if index.keys.is_empty() {
        gaps.push("索引没有键位".to_string());
    }
    if let Some(index_collation) = &index.collation {
        if !index_collation_inherits(index_collation, meta.collation.as_ref()) {
            gaps.push("索引比较规则与集合默认比较规则不一致，不能证明普通等值".to_string());
        }
    }

    let mut columns = Vec::new();
    let mut seen = BTreeSet::new();
    for (field, direction) in &index.keys {
        if !plain_path(field) || field.contains("[]") {
            gaps.push(format!("键位 {field} 不是普通字段路径"));
            continue;
        }
        if !plain_direction(direction) {
            gaps.push(format!("键位 {field} 不是普通升序/降序键"));
            continue;
        }
        if !seen.insert(field.clone()) {
            gaps.push(format!("键位列 {field} 重复出现"));
            continue;
        }
        match draft.facts.get(field) {
            Some(facts) if facts.scalar() => columns.push(field.clone()),
            Some(facts) if facts.shape == Some(FieldShape::Container) => {
                gaps.push(format!("字段 {field} 是对象/数组，不是可证明的标量键"))
            }
            Some(facts) if facts.shape == Some(FieldShape::Mixed) => {
                gaps.push(format!("字段 {field} 是混合类型，不是普通标量键"))
            }
            _ => gaps.push(format!("字段 {field} 的类型未知，无法证明不是多键数组")),
        }
    }

    if !gaps.is_empty() {
        warnings.push(format!(
            "集合 {} 的唯一索引 {} 不能作为全表唯一键：{}",
            draft.table.name,
            index.name,
            gaps.join("；")
        ));
        return None;
    }
    Some(Endpoint {
        table: TableId::from(&draft.table),
        columns,
    })
}

/// 索引键值必须是普通的 1 / -1；hashed、text、2dsphere、通配符和表达式都不算。
fn plain_direction(value: &Bson) -> bool {
    match value {
        Bson::Int32(value) => *value == 1 || *value == -1,
        Bson::Int64(value) => *value == 1 || *value == -1,
        Bson::Double(value) => *value == 1.0 || *value == -1.0,
        _ => false,
    }
}

/// `None` 表示继承集合默认值；显式声明时必须与集合默认规则完全一致。
fn index_collation_inherits(index: &Document, collection: Option<&Document>) -> bool {
    match collection {
        Some(collection) => canonical_collation(index) == canonical_collation(collection),
        None => false,
    }
}

fn is_id_index(index: &IndexMeta) -> bool {
    index.name == "_id_" || (index.keys.len() == 1 && index.keys[0].0 == "_id")
}

fn index_info(index: &IndexMeta) -> IndexInfo {
    let primary = is_id_index(index);
    let mut columns = if index.text {
        vec!["$text".to_string()]
    } else {
        index.keys.iter().map(|(field, _)| field.clone()).collect()
    };
    let decorations = [
        index.sparse.then_some("sparse"),
        index.partial.is_some().then_some("partial"),
        index.hidden.then_some("hidden"),
    ];
    let suffix: Vec<&str> = decorations.into_iter().flatten().collect();
    if !suffix.is_empty() {
        let suffix = format!(" ({})", suffix.join(", "));
        for column in &mut columns {
            column.push_str(&suffix);
        }
    }
    IndexInfo {
        name: index.name.clone(),
        columns,
        is_unique: primary || index.unique,
        is_primary: primary,
    }
}

/// 解析视图定义，只接受同库基础集合上的简单 `$lookup`。
fn analyze_views(
    views: &[CollectionMeta],
    drafts: &mut BTreeMap<String, Draft>,
    links: &mut Vec<LinkEvidence>,
    warnings: &mut Vec<String>,
) {
    for view in views {
        let MetaKind::View { view_on, pipeline } = &view.kind else {
            continue;
        };
        let Some(view_on) = view_on.as_deref().filter(|name| !name.is_empty()) else {
            warnings.push(format!("视图 {} 没有 viewOn，无法追溯查询关系", view.name));
            continue;
        };
        if !drafts.contains_key(view_on) {
            warnings.push(format!(
                "视图 {} 的 viewOn {} 不是当前库的基础集合，跳过其 $lookup",
                view.name, view_on
            ));
            continue;
        }
        if pipeline.is_empty() {
            // 直接投影基础集合，没有等值查询关系。
            continue;
        }

        let limited = pipeline
            .iter()
            .any(|stage| stage.len() != 1 || !stage.contains_key("$lookup"));
        let mut overwritten: Vec<String> = Vec::new();
        let mut original_fields = true;
        for stage in pipeline {
            let Ok(lookup) = stage.get_document("$lookup") else {
                // 这些阶段只限制文档集合或顺序。其他变换后的字段不能直接归属到基础集合。
                if !matches!(
                    stage_name(stage).as_str(),
                    "$match" | "$limit" | "$skip" | "$sort"
                ) {
                    original_fields = false;
                }
                continue;
            };
            let local = lookup.get_str("localField").ok();
            let prior_output = local.is_some_and(|local| {
                overwritten.iter().any(|output| {
                    local == output
                        || local
                            .strip_prefix(output.as_str())
                            .is_some_and(|s| s.starts_with('.'))
                        || output
                            .strip_prefix(local)
                            .is_some_and(|s| s.starts_with('.'))
                })
            });
            if !original_fields || prior_output {
                warnings.push(format!(
                    "视图 {} 的 $lookup 来源字段已被前序阶段改写，无法定位到基础集合",
                    view.name
                ));
            } else {
                process_lookup(
                    view,
                    view_on,
                    lookup,
                    !limited && stage.len() == 1,
                    drafts,
                    links,
                    warnings,
                );
                if limited {
                    warnings.push(format!(
                        "视图 {} 的 pipeline 含过滤/变换，查询关联只作为候选",
                        view.name
                    ));
                }
            }
            if let Ok(field) = lookup.get_str("as") {
                overwritten.push(field.to_owned());
            } else {
                original_fields = false;
            }
        }
    }
}

fn stage_name(stage: &Document) -> String {
    stage
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "未知阶段".to_string())
}

struct ParsedLookup {
    from: String,
    local_field: String,
    foreign_field: String,
    as_field: String,
    as_plain: bool,
}

fn parse_lookup(lookup: &Document) -> std::result::Result<ParsedLookup, String> {
    let field = |name: &str| {
        lookup
            .get_str(name)
            .ok()
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.starts_with('$'))
            .map(str::to_string)
            .ok_or_else(|| format!("缺少或无法识别 {name}"))
    };
    let from = field("from")?;
    let local_field = field("localField")?;
    let foreign_field = field("foreignField")?;
    let as_field = field("as")?;
    let unsupported: Vec<&str> = lookup
        .keys()
        .map(String::as_str)
        .filter(|key| !matches!(*key, "from" | "localField" | "foreignField" | "as"))
        .collect();
    if !unsupported.is_empty() {
        return Err(format!(
            "使用了非简单形式（{}），需要静态 from/localField/foreignField/as",
            unsupported.join(", ")
        ));
    }
    Ok(ParsedLookup {
        as_plain: !as_field.contains('.'),
        from,
        local_field,
        foreign_field,
        as_field,
    })
}

fn process_lookup(
    view: &CollectionMeta,
    view_on: &str,
    lookup: &Document,
    simple: bool,
    drafts: &mut BTreeMap<String, Draft>,
    links: &mut Vec<LinkEvidence>,
    warnings: &mut Vec<String>,
) {
    let parsed = match parse_lookup(lookup) {
        Ok(parsed) => parsed,
        Err(gap) => {
            warnings.push(format!("视图 {} 的 $lookup 未解析：{gap}", view.name));
            return;
        }
    };
    if !drafts.contains_key(&parsed.from) {
        warnings.push(format!(
            "视图 {} 的 $lookup.from {} 不是当前库的基础集合，跳过该查询关系",
            view.name, parsed.from
        ));
        return;
    }

    let source_shape = {
        let draft = drafts
            .get_mut(view_on)
            .expect("viewOn 已在前置检查中确认存在");
        ensure_endpoint_field(
            draft,
            &parsed.local_field,
            &format!("视图 {} 的 $lookup localField", view.name),
            warnings,
        )
    };
    let target_shape = {
        let draft = drafts
            .get_mut(&parsed.from)
            .expect("$lookup.from 已在前置检查中确认存在");
        ensure_endpoint_field(
            draft,
            &parsed.foreign_field,
            &format!("视图 {} 的 $lookup foreignField", view.name),
            warnings,
        )
    };
    let (Some(source_shape), Some(target_shape)) = (source_shape, target_shape) else {
        warnings.push(format!(
            "视图 {} 的 $lookup 端点路径无法静态确定（{} / {}），跳过该查询关系",
            view.name, parsed.local_field, parsed.foreign_field
        ));
        return;
    };

    let source_draft = &drafts[view_on];
    let target_draft = &drafts[&parsed.from];
    let mut gaps = Vec::new();
    if !simple {
        gaps.push("视图 pipeline 含过滤/变换阶段，查询结果受限".to_string());
    }
    if !parsed.as_plain {
        gaps.push("$lookup.as 使用路径重写".to_string());
    }
    for (role, shape, field) in [
        ("localField", source_shape, parsed.local_field.as_str()),
        ("foreignField", target_shape, parsed.foreign_field.as_str()),
    ] {
        match shape {
            FieldShape::Scalar | FieldShape::Unknown => {}
            FieldShape::Container => gaps.push(format!(
                "{role} 字段 {field} 是对象/数组，可能是多态或数组展开引用"
            )),
            FieldShape::Mixed => gaps.push(format!(
                "{role} 字段 {field} 是混合类型，无法断言单一标量关系"
            )),
            FieldShape::UnknownPath => gaps.push(format!(
                "{role} 字段 {field} 的前缀类型未知，无法证明不是数组路径"
            )),
        }
    }
    if let (Some(source), Some(target)) = (
        source_draft
            .facts
            .get(&parsed.local_field)
            .and_then(|facts| facts.signature.as_deref()),
        target_draft
            .facts
            .get(&parsed.foreign_field)
            .and_then(|facts| facts.signature.as_deref()),
    ) {
        if let Some(gap) = lookup_type_gap(source, target) {
            gaps.push(gap);
        }
    }

    let source = Endpoint {
        table: TableId::from(&source_draft.table),
        columns: vec![parsed.local_field.clone()],
    };
    let target = Endpoint {
        table: TableId::from(&target_draft.table),
        columns: vec![parsed.foreign_field.clone()],
    };
    let base = format!(
        "MongoDB 视图「{}」的 $lookup：{}.{} = {}.{}（输出 {}，静态引用原始字段的查询关系，不是物理外键）",
        view.name,
        source.table.label(),
        parsed.local_field,
        target.table.label(),
        parsed.foreign_field,
        parsed.as_field
    );
    let reason = if gaps.is_empty() {
        base
    } else {
        format!("{base}；{}，仅作候选", gaps.join("；"))
    };
    links.push(LinkEvidence {
        source,
        target,
        candidate: !gaps.is_empty(),
        reason,
    });
}

/// 检查 $lookup 端点字段；能证明存在的补成 unknown 字段，路径本身有歧义时返回 None。
fn ensure_endpoint_field(
    draft: &mut Draft,
    field: &str,
    source: &str,
    warnings: &mut Vec<String>,
) -> Option<FieldShape> {
    if !plain_path(field) {
        warnings.push(format!("{source} 不是普通字段路径：{field}"));
        return None;
    }
    if !field.contains('.') {
        if !draft.facts.contains_key(field) {
            ensure_unknown_field(draft, field);
        }
        return draft.facts.get(field).map(shape_of);
    }

    let top = field.split('.').next().unwrap_or(field);
    match draft.facts.get(top) {
        Some(facts) if facts.array => {
            warnings.push(format!(
                "{source} 路径 {field} 穿过数组字段 {top}，可能是多态引用，跳过"
            ));
            return None;
        }
        Some(_) => {}
        None => {
            // 前缀未知：保留完整路径作为弱证据，但只作为候选。
            ensure_unknown_field(draft, field);
            return Some(FieldShape::UnknownPath);
        }
    }
    if !draft.facts.contains_key(field) {
        ensure_unknown_field(draft, field);
        return Some(FieldShape::UnknownPath);
    }
    draft.facts.get(field).map(shape_of)
}

fn shape_of(facts: &FieldFacts) -> FieldShape {
    facts.shape.unwrap_or(FieldShape::Unknown)
}

/// 已知签名的等值可比性；`None` 表示无法判断，仍保留视图声明的查询关系。
fn lookup_type_gap(source: &str, target: &str) -> Option<String> {
    if source == target {
        return None;
    }
    let source_family = bson_family(source);
    let target_family = bson_family(target);
    if source_family == "number" && target_family == "number" {
        return None;
    }
    if source_family == "string" && target_family == "string" {
        return Some("两端字符串比较规则不同，不能断言等值".to_string());
    }
    if source_family != target_family {
        return Some(format!("两端已知类型不兼容：{source} 与 {target}"));
    }
    None
}

fn bson_family(signature: &str) -> &str {
    let base = signature.strip_prefix("bson:").unwrap_or(signature);
    match base.split_whitespace().next().unwrap_or(base) {
        "int32" | "int64" | "double" | "decimal128" | "number" => "number",
        "string" => "string",
        "objectId" => "objectId",
        "bool" => "bool",
        "date" => "date",
        "timestamp" => "timestamp",
        "binary" => "binary",
        "regex" => "regex",
        other => other,
    }
}

fn finish_links(links: Vec<LinkEvidence>) -> Analysis {
    let mut links = links;
    links.sort_by(|a, b| (&a.source, &a.target, &a.reason).cmp(&(&b.source, &b.target, &b.reason)));
    let mut evidence = BTreeMap::<(Endpoint, Endpoint), BTreeSet<&str>>::new();
    for link in links.iter().filter(|link| !link.candidate) {
        evidence
            .entry((link.source.clone(), link.target.clone()))
            .or_default()
            .insert(&link.reason);
    }
    let relation_pairs: BTreeSet<_> = evidence.keys().cloned().collect();
    let relations = evidence
        .into_iter()
        .map(|((source, target), reasons)| Relation {
            condition: None,
            source,
            target,
            kind: RelationKind::Lookup,
            reason: reasons.into_iter().collect::<Vec<_>>().join("；"),
        })
        .collect();
    let mut candidates = Vec::new();
    let mut seen_candidates = BTreeSet::new();
    for link in links {
        if !link.candidate || relation_pairs.contains(&(link.source.clone(), link.target.clone())) {
            continue;
        }
        if seen_candidates.insert((
            link.source.clone(),
            link.target.clone(),
            link.reason.clone(),
        )) {
            candidates.push(Candidate {
                condition: None,
                source: link.source,
                target: link.target,
                reason: link.reason,
            });
        }
    }
    Analysis {
        relations,
        candidates,
    }
}

/// 与字段展开使用同一层级上限，收集 validator 里的 description。
fn schema_descriptions(schema: &Document) -> BTreeMap<String, String> {
    let mut descriptions = BTreeMap::new();
    walk_descriptions(schema, "", 0, &mut descriptions);
    descriptions
}

fn walk_descriptions(
    schema: &Document,
    prefix: &str,
    depth: usize,
    out: &mut BTreeMap<String, String>,
) {
    let Ok(properties) = schema.get_document("properties") else {
        return;
    };
    for (name, spec) in properties {
        let Some(spec) = spec.as_document() else {
            continue;
        };
        let path = join_path(prefix, name);
        if let Some(description) = spec
            .get_str("description")
            .ok()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            out.insert(path.clone(), description.to_string());
        }
        if depth + 1 >= MAX_DEPTH {
            continue;
        }
        walk_descriptions(spec, &path, depth + 1, out);
        if let Ok(items) = spec.get_document("items") {
            walk_descriptions(items, &join_path(&path, "[]"), depth + 1, out);
        }
    }
}

fn join_path(prefix: &str, key: &str) -> String {
    if key == "[]" {
        return format!("{prefix}[]");
    }
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// 旧字段解析器用 `items.[].sku` 表示数组元素；ER 统一成 `items[].sku`。
fn normalize_path(path: &str) -> String {
    path.replace(".[]", "[]")
}

/// collation 的稳定文本；字段排序后再拼接，避免服务器返回顺序影响签名。
fn canonical_collation(document: &Document) -> String {
    let mut parts: Vec<String> = document
        .iter()
        .map(|(key, value)| format!("{key}={}", canonical_value(value)))
        .collect();
    parts.sort();
    parts.join(";")
}

fn canonical_value(value: &Bson) -> String {
    match value {
        Bson::String(value) => value.clone(),
        Bson::Document(value) => format!("{{{}}}", canonical_collation(value)),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    fn base(name: &str, schema: Option<Document>, indexes: Vec<IndexMeta>) -> CollectionMeta {
        CollectionMeta {
            name: name.to_string(),
            kind: MetaKind::Base,
            validator: schema,
            enforced: true,
            collation: None,
            indexes,
        }
    }

    fn view(name: &str, view_on: &str, pipeline: Vec<Document>) -> CollectionMeta {
        CollectionMeta {
            name: name.to_string(),
            kind: MetaKind::View {
                view_on: Some(view_on.to_string()),
                pipeline,
            },
            validator: None,
            enforced: false,
            collation: None,
            indexes: Vec::new(),
        }
    }

    fn index(name: &str, keys: &[(&str, i32)], unique: bool) -> IndexMeta {
        IndexMeta {
            name: name.to_string(),
            keys: keys
                .iter()
                .map(|(field, direction)| (field.to_string(), Bson::Int32(*direction)))
                .collect(),
            unique,
            sparse: false,
            partial: None,
            hidden: false,
            text: false,
            collation: None,
        }
    }

    fn lookup(from: &str, local: &str, foreign: &str) -> Document {
        doc! {
            "$lookup": {
                "from": from,
                "localField": local,
                "foreignField": foreign,
                "as": "linked",
            }
        }
    }

    fn table_name<'a>(catalog: &'a ErCatalog, name: &str) -> &'a TableInfo {
        catalog
            .tables
            .iter()
            .find(|table| table.name == name)
            .unwrap_or_else(|| panic!("缺少集合 {name}：{:?}", catalog.tables))
    }

    fn column_type<'a>(catalog: &'a ErCatalog, table: &str, column: &str) -> Option<&'a str> {
        catalog
            .types
            .iter()
            .find(|ty| ty.table.name == table && ty.column == column)
            .map(|ty| ty.signature.as_str())
    }

    fn endpoint(table: &str, columns: &[&str]) -> Endpoint {
        Endpoint {
            table: TableId {
                schema: String::new(),
                name: table.to_string(),
            },
            columns: columns.iter().map(|column| column.to_string()).collect(),
        }
    }

    #[test]
    fn no_validator_keeps_collection_and_unknown_id() {
        let catalog = build_catalog(vec![base("users", None, vec![])]);
        let users = table_name(&catalog, "users");
        let id = users
            .columns
            .iter()
            .find(|column| column.name == "_id")
            .expect("没有 _id");
        assert_eq!(id.data_type, "unknown");
        assert!(catalog.keys.contains(&endpoint("users", &["_id"])));
        assert!(
            !catalog.types.iter().any(|ty| ty.column == "_id"),
            "没有 validator 时不得猜 _id 类型"
        );
        assert!(catalog.document_links.relations.is_empty());
        assert!(users.foreign_keys.is_empty());
    }

    #[test]
    fn validator_nested_paths_stay_in_collection_and_only_scalars_get_types() {
        let schema = doc! {
            "bsonType": "object",
            "description": "客户信息表",
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
        let catalog = build_catalog(vec![base("customers", Some(schema), vec![])]);
        assert_eq!(catalog.tables.len(), 1);
        let customers = table_name(&catalog, "customers");
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
        ] {
            assert!(
                columns.contains(&expected),
                "缺少字段 {expected}：{columns:?}"
            );
        }
        assert!(!catalog.tables.iter().any(|table| table.name == "address"));
        let name_comment = &customers
            .columns
            .iter()
            .find(|column| column.name == "name")
            .unwrap()
            .comment;
        assert_eq!(name_comment, "客户名称");
        assert!(customers.note.contains("可缺失的声明字段"));
        assert_eq!(
            column_type(&catalog, "customers", "name"),
            Some("bson:string")
        );
        assert_eq!(
            column_type(&catalog, "customers", "address.city"),
            Some("bson:string")
        );
        assert_eq!(column_type(&catalog, "customers", "tags"), None);
        assert_eq!(column_type(&catalog, "customers", "items[].sku"), None);
        assert_eq!(column_type(&catalog, "customers", "score"), None);
        assert!(catalog.document_links.relations.is_empty());
    }

    #[test]
    fn simple_lookup_is_relation_but_filtered_pipeline_only_candidate() {
        let orders_schema = doc! {
            "bsonType": "object",
            "properties": {
                "_id": { "bsonType": "objectId" },
                "customer_id": { "bsonType": "string" },
                "agent_id": { "bsonType": "string" },
                "status": { "bsonType": "string" },
            },
        };
        let customers_schema = doc! {
            "bsonType": "object",
            "properties": {
                "_id": { "bsonType": "objectId" },
                "code": { "bsonType": "string" },
            },
        };
        let agents_schema = customers_schema.clone();
        let catalog = build_catalog(vec![
            base("orders", Some(orders_schema), vec![]),
            base(
                "customers",
                Some(customers_schema),
                vec![index("uk_customers_code", &[("code", 1)], true)],
            ),
            base("agents", Some(agents_schema), vec![]),
            view(
                "order_customers",
                "orders",
                vec![lookup("customers", "customer_id", "code")],
            ),
            view(
                "filtered_orders",
                "orders",
                vec![
                    doc! { "$match": { "status": "paid" } },
                    lookup("agents", "agent_id", "code"),
                ],
            ),
        ]);

        assert!(!catalog
            .tables
            .iter()
            .any(|table| table.name == "order_customers"));
        let relation = catalog
            .document_links
            .relations
            .iter()
            .find(|relation| relation.target.table.name == "customers")
            .expect("简单 $lookup 应成为查询关系");
        assert_eq!(relation.kind, RelationKind::Lookup);
        assert_eq!(relation.source, endpoint("orders", &["customer_id"]));
        assert_eq!(relation.target, endpoint("customers", &["code"]));
        assert!(relation.reason.contains("order_customers"));
        assert!(relation.reason.contains("orders.customer_id"));
        assert!(relation.reason.contains("customers.code"));
        assert!(relation.reason.contains("不是物理外键"));

        assert!(
            !catalog
                .document_links
                .relations
                .iter()
                .any(|relation| relation.target.table.name == "agents"),
            "带 $match 的 pipeline 不能升格为关系"
        );
        assert!(catalog
            .document_links
            .candidates
            .iter()
            .any(|candidate| candidate.target.table.name == "agents"
                && candidate.reason.contains("filtered_orders")));
        assert!(catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("filtered_orders") && warning.contains("过滤")));
    }

    #[test]
    fn computed_view_fields_never_become_fields_of_the_base_collection() {
        let cat = build_catalog(vec![
            base("a", None, vec![]),
            base("b", None, vec![]),
            view(
                "chain",
                "a",
                vec![
                    doc! {"$lookup": {"from":"b","localField":"owner","foreignField":"_id","as":"previous"}},
                    doc! {"$lookup": {"from":"b","localField":"previous","foreignField":"_id","as":"next"}},
                ],
            ),
            view(
                "projected",
                "a",
                vec![
                    doc! {"$project":{"derived":"$raw"}},
                    lookup("b", "derived", "_id"),
                ],
            ),
        ]);
        let columns = &table_name(&cat, "a").columns;
        assert!(!columns
            .iter()
            .any(|c| c.name == "previous" || c.name == "derived"));
        assert!(cat
            .document_links
            .relations
            .iter()
            .all(|r| r.source.columns == ["owner"]));
        assert!(cat.document_links.candidates.is_empty());
    }

    #[test]
    fn directory_explanations_are_not_business_statements() {
        let c = build_catalog(vec![
            base("ledger", None, vec![]),
            base(
                "ledger_history",
                None,
                vec![index("IDX_LEDGER_HISTORY", &[("visible", 1)], false)],
            ),
        ]);
        let history = c
            .tables
            .iter()
            .find(|t| t.name == "ledger_history")
            .unwrap();
        let field = history
            .columns
            .iter()
            .find(|c| c.name == "visible")
            .unwrap();
        assert!(field.comment.is_empty());
        let analysis = dbs_core::er::infer_relations(&c);
        assert!(analysis.relations.is_empty() && analysis.candidates.is_empty());
        assert!(history
            .indexes
            .iter()
            .any(|i| i.name == "IDX_LEDGER_HISTORY"));
    }

    #[test]
    fn advisory_validator_retains_paths_without_authorizing_type_matches() {
        let mut collection = base(
            "a",
            Some(doc! {"properties":{"owner_id":{"bsonType":"objectId"}}}),
            vec![],
        );
        collection.enforced = false;
        let cat = build_catalog(vec![collection]);
        assert!(table_name(&cat, "a")
            .columns
            .iter()
            .any(|c| c.name == "owner_id"));
        assert!(column_type(&cat, "a", "owner_id").is_none());
    }

    #[test]
    fn partial_key_requires_the_entire_filter_to_be_representable() {
        let mut exact = BTreeMap::new();
        assert!(equality_filter(
            &doc! {"$and":[{"active":{"$eq":true}},{"version":2_i32}]},
            &mut exact
        )
        .is_some());
        assert_eq!(exact["active"], dbs_core::er::ScalarValue::Bool(true));
        assert_eq!(exact["version"], dbs_core::er::ScalarValue::Integer(2));
        for bad in [
            doc! {"active":true,"role":{"$exists":true}},
            doc! {"$or":[{"active":true},{"active":false}]},
            doc! {"$and":[{"active":true},{"active":false}]},
            doc! {"token":9223372036854775807_i64},
        ] {
            assert!(
                equality_filter(&bad, &mut BTreeMap::new()).is_none(),
                "{bad}"
            );
        }
    }

    #[test]
    fn sparse_partial_expression_and_unknown_indexes_are_not_global_keys() {
        let schema = doc! {
            "bsonType": "object",
            "properties": {
                "_id": { "bsonType": "objectId" },
                "email": { "bsonType": "string" },
                "tenant": { "bsonType": "string" },
                "code": { "bsonType": "string" },
            },
        };
        let mut sparse = index("uk_email_sparse", &[("email", 1)], true);
        sparse.sparse = true;
        let mut partial = index("uk_code_partial", &[("code", 1)], true);
        partial.partial = Some(doc! {"enabled":true});
        let mut expression = index("uk_expr", &[("code", 1)], true);
        expression.keys[0].1 = Bson::String("text".to_string());
        let catalog = build_catalog(vec![base(
            "accounts",
            Some(schema),
            vec![
                sparse,
                partial,
                expression,
                index("uk_tenant", &[("tenant", 1)], true),
                index("uk_external", &[("external_id", 1)], true),
            ],
        )]);

        let key_columns: Vec<Vec<String>> = catalog
            .keys
            .iter()
            .filter(|key| key.table.name == "accounts")
            .map(|key| key.columns.clone())
            .collect();
        assert!(key_columns.contains(&vec!["_id".to_string()]));
        assert!(key_columns.contains(&vec!["tenant".to_string()]));
        for rejected in ["email", "code", "external_id"] {
            assert!(
                !key_columns
                    .iter()
                    .any(|columns| columns == &vec![rejected.to_string()]),
                "{rejected} 不应成为全表唯一键：{key_columns:?}"
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
        let accounts = table_name(&catalog, "accounts");
        assert!(accounts
            .indexes
            .iter()
            .any(|index| index.name == "uk_email_sparse"
                && index.columns == vec!["email (sparse)".to_string()]));
        assert!(accounts
            .columns
            .iter()
            .any(|column| column.name == "external_id" && column.data_type == "unknown"));
        assert_eq!(column_type(&catalog, "accounts", "external_id"), None);
    }

    #[test]
    fn lookup_into_array_path_is_skipped_with_a_specific_warning() {
        let schema = doc! {
            "bsonType": "object",
            "properties": {
                "_id": { "bsonType": "objectId" },
                "items": { "bsonType": "array", "items": {
                    "bsonType": "object",
                    "properties": { "sku": { "bsonType": "string" } }
                } },
            },
        };
        let catalog = build_catalog(vec![
            base("orders", Some(schema.clone()), vec![]),
            base("products", Some(schema), vec![]),
            view(
                "order_products",
                "orders",
                vec![lookup("products", "items.sku", "items.sku")],
            ),
        ]);
        assert!(catalog.document_links.relations.is_empty());
        assert!(catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("order_products") && warning.contains("数组")));
    }

    #[test]
    fn string_collation_enters_signature_and_changes_index_rule() {
        let schema = doc! {
            "bsonType": "object",
            "properties": {
                "_id": { "bsonType": "objectId" },
                "code": { "bsonType": "string" },
            },
        };
        let mut collection = base(
            "customers",
            Some(schema),
            vec![index("uk_code", &[("code", 1)], true)],
        );
        collection.collation = Some(doc! { "locale": "en", "strength": 2_i32 });
        let catalog = build_catalog(vec![collection]);
        assert_eq!(
            column_type(&catalog, "customers", "code"),
            Some("bson:string collation:locale=en;strength=2")
        );
        assert!(catalog
            .keys
            .iter()
            .any(|key| key.table.name == "customers" && key.columns == ["code"]));

        let mut different = base(
            "other",
            Some(doc! {
                "bsonType": "object",
                "properties": {
                    "_id": { "bsonType": "objectId" },
                    "code": { "bsonType": "string" },
                },
            }),
            vec![index("uk_code", &[("code", 1)], true)],
        );
        different.collation = Some(doc! { "locale": "en", "strength": 3_i32 });
        different.indexes[0].collation = Some(doc! { "locale": "en", "strength": 2_i32 });
        let catalog = build_catalog(vec![different]);
        assert!(
            !catalog
                .keys
                .iter()
                .any(|key| key.table.name == "other" && key.columns == ["code"]),
            "索引比较规则与集合默认规则不同时不能当普通唯一键"
        );
        assert!(catalog
            .warnings
            .iter()
            .any(|warning| warning.contains("比较规则")));
    }
}
