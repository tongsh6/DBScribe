//! 字段结构的纯逻辑：采样聚合、validator 解析、两层合并。
//!
//! 这里不碰数据库连接，全部是可单测的纯函数；驱动只负责取回 BSON。

use dbs_core::ColumnInfo;
use mongodb::bson::{Bson, Document};
use std::collections::{BTreeMap, BTreeSet};

/// 嵌套展开的最大层级；更深的层级只显示为 `object`。
const MAX_DEPTH: usize = 3;

/// 一条字段路径的采样统计。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FieldStats {
    /// 展示类型 → 出现次数（如 `string` / `array<string>`）。
    pub types: BTreeMap<String, usize>,
    /// 值为 null 的次数。
    pub nulls: usize,
    /// 出现该字段的文档数。
    pub present: usize,
}

impl FieldStats {
    fn record_type(&mut self, name: impl Into<String>) {
        *self.types.entry(name.into()).or_insert(0) += 1;
    }

    /// 类型展示：次数降序，同次数按名称，保证两次采样结果可 diff。
    fn type_text(&self) -> String {
        if self.types.is_empty() {
            return "unknown".to_string();
        }
        let mut entries: Vec<(&String, &usize)> = self.types.iter().collect();
        entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        entries
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

/// 采样文档 → 字段路径统计。
pub fn aggregate_documents(docs: &[Document]) -> BTreeMap<String, FieldStats> {
    let mut fields = BTreeMap::new();
    for doc in docs {
        walk_document(doc, "", 0, &mut fields);
    }
    fields
}

fn walk_document(
    doc: &Document,
    prefix: &str,
    depth: usize,
    out: &mut BTreeMap<String, FieldStats>,
) {
    for (key, value) in doc {
        let path = join_path(prefix, key);
        let stats = out.entry(path.clone()).or_default();
        stats.present += 1;

        match value {
            Bson::Null => {
                stats.nulls += 1;
                stats.record_type("null");
            }
            Bson::Document(child) => {
                stats.record_type("object");
                if depth + 1 < MAX_DEPTH {
                    walk_document(child, &path, depth + 1, out);
                }
            }
            Bson::Array(items) => {
                let (type_text, children) = array_shape(items);
                stats.record_type(type_text);
                if depth + 1 < MAX_DEPTH {
                    let element_path = join_path(&path, "[]");
                    for child in children {
                        walk_document(child, &element_path, depth + 1, out);
                    }
                }
            }
            other => stats.record_type(display_type(other)),
        }
    }
}

/// 数组元素类型与需要展开的子文档。
fn array_shape(items: &[Bson]) -> (String, Vec<&Document>) {
    if items.is_empty() {
        return ("array<unknown>".to_string(), Vec::new());
    }
    let mut types = BTreeSet::new();
    let mut children = Vec::new();
    for item in items {
        match item {
            Bson::Document(doc) => {
                types.insert("object".to_string());
                children.push(doc);
            }
            Bson::Array(_) => {
                // 不展开数组套数组
                types.insert("array".to_string());
            }
            Bson::Null => {
                types.insert("null".to_string());
            }
            other => {
                types.insert(display_type(other));
            }
        }
    }
    let inner = types.into_iter().collect::<Vec<_>>().join(" | ");
    (format!("array<{inner}>"), children)
}

fn display_type(value: &Bson) -> String {
    let name = match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Array(_) => "array",
        Bson::Document(_) => "object",
        Bson::Binary(_) => "binary",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int32(_) => "int32",
        Bson::Int64(_) => "int64",
        Bson::Decimal128(_) => "decimal128",
        _ => "unknown",
    };
    name.to_string()
}

fn join_path(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// 采样统计 → 列定义；`_id` 固定第一，其余字母序。
pub fn columns_from_sample(
    fields: &BTreeMap<String, FieldStats>,
    sample_size: usize,
) -> Vec<ColumnInfo> {
    let mut paths: Vec<&String> = fields.keys().collect();
    paths.sort_by(|a, b| {
        let rank = |path: &str| usize::from(path != "_id");
        rank(a.as_str())
            .cmp(&rank(b.as_str()))
            .then_with(|| a.as_str().cmp(b.as_str()))
    });

    paths
        .into_iter()
        .enumerate()
        .map(|(idx, path)| ColumnInfo {
            order: (idx + 1) as u32,
            name: path.clone(),
            data_type: fields[path].type_text(),
            length: String::new(),
            is_primary_key: path.as_str() == "_id",
            is_auto_increment: false,
            nullable: fields[path].nulls > 0,
            default_value: String::new(),
            comment: sample_comment(path, &fields[path], sample_size),
        })
        .collect()
}

/// 出现率只对「文档级字段」有意义；数组元素路径与 100% 出现的字段不写。
fn sample_comment(path: &str, stats: &FieldStats, sample_size: usize) -> String {
    if path.contains("[]") || sample_size == 0 || stats.present >= sample_size {
        return String::new();
    }
    format!("出现率 {}%", stats.present * 100 / sample_size)
}

/// validator 声明的一个字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredField {
    pub path: String,
    pub types: Vec<String>,
    /// 是否在所在对象的 `required` 列表里。
    pub required: bool,
}

/// 解析 validator 的 `$jsonSchema` 内容。
pub fn parse_json_schema(schema: &Document) -> Vec<DeclaredField> {
    let mut fields = Vec::new();
    walk_schema(schema, "", 0, &mut fields);
    fields
}

fn walk_schema(schema: &Document, prefix: &str, depth: usize, out: &mut Vec<DeclaredField>) {
    let required: BTreeSet<String> = schema
        .get_array("required")
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let Ok(properties) = schema.get_document("properties") else {
        return;
    };

    for (name, spec) in properties {
        let Some(spec) = spec.as_document() else {
            continue;
        };
        let path = join_path(prefix, name);
        let types = declared_types(spec);
        out.push(DeclaredField {
            path: path.clone(),
            types: types.clone(),
            required: required.contains(name),
        });

        if depth + 1 >= MAX_DEPTH {
            continue;
        }
        if types.iter().any(|kind| kind == "object") {
            walk_schema(spec, &path, depth + 1, out);
        }
        if types.iter().any(|kind| kind.starts_with("array")) {
            let title = join_path(&path, "[]");
            if let Ok(items) = spec.get_document("items") {
                walk_schema(items, &title, depth + 1, out);
            }
        }
    }
}

/// validator 的类型声明；数组把元素类型写进 `array<...>`。
fn declared_types(spec: &Document) -> Vec<String> {
    let raw: Vec<String> = match spec.get("bsonType") {
        Some(Bson::String(name)) => vec![validator_type(name)],
        Some(Bson::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(validator_type)
            .collect(),
        _ => vec!["unknown".to_string()],
    };

    if !raw.iter().any(|kind| kind == "array") {
        return raw;
    }
    let element = spec
        .get_document("items")
        .map(declared_types)
        .ok()
        .filter(|kinds| !kinds.is_empty())
        .map(|kinds| kinds.join(" | "))
        .unwrap_or_else(|| "unknown".to_string());
    raw.into_iter()
        .map(|kind| {
            if kind == "array" {
                format!("array<{element}>")
            } else {
                kind
            }
        })
        .collect()
}

/// validator 的类型名 → 文档展示名。
fn validator_type(name: &str) -> String {
    let shown = match name {
        "int" => "int32",
        "long" => "int64",
        "decimal" => "decimal128",
        "binData" => "binary",
        other => other,
    };
    shown.to_string()
}

/// 字段来源，决定表级标注。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldOrigin {
    /// 全部来自 validator 声明。
    Declared,
    /// 全部来自采样观察。
    Sampled {
        sample_size: usize,
        /// 集合估算文档数；用于说明采样覆盖率，统计失败时为 None。
        total_docs: Option<u64>,
    },
    /// 声明为主，采样补缺。
    Mixed {
        declared: usize,
        sampled: usize,
        sample_size: usize,
        /// 集合估算文档数；用于说明采样覆盖率，统计失败时为 None。
        total_docs: Option<u64>,
    },
}

/// 覆盖率说明；没有总数时为空，避免「0 条文档」这类噪音。
fn coverage(total_docs: Option<u64>) -> String {
    match total_docs {
        Some(total) if total > 0 => format!("（共约 {total} 条文档）"),
        _ => String::new(),
    }
}

impl FieldOrigin {
    /// 表级标注文案。
    pub fn note(&self) -> String {
        match self {
            FieldOrigin::Declared => "字段来源：collection validator（$jsonSchema 声明）".to_string(),
            FieldOrigin::Sampled {
                sample_size,
                total_docs,
            } => format!(
                "字段来源：{sample_size} 条采样推断{}，字段集合可能随数据变化",
                coverage(*total_docs)
            ),
            FieldOrigin::Mixed {
                declared,
                sampled,
                sample_size,
                total_docs,
            } => format!(
                "字段来源：validator 声明 {declared} 个字段 + {sample_size} 条采样补充 {sampled} 个字段{}",
                coverage(*total_docs)
            ),
        }
    }
}

/// 合并声明层与观察层。
///
/// - 声明字段优先，按 validator 里的顺序；
/// - 采样补充字段按字母序追加；
/// - `_id` 始终存在且排第一（空集合也保留主键列）。
pub fn merge_columns(
    declared: &[DeclaredField],
    sampled: Option<&BTreeMap<String, FieldStats>>,
    sample_size: usize,
    total_docs: Option<u64>,
) -> (Vec<ColumnInfo>, Option<FieldOrigin>) {
    let mut columns: Vec<ColumnInfo> = Vec::new();
    let mut declared_paths: BTreeSet<&str> = BTreeSet::new();
    // 采样真正补充进来的字段数；不能用总列数倒推，`_id` 与空集合补的主键不算
    let mut sampled_extras = 0usize;

    if declared.is_empty() {
        if let Some(fields) = sampled.filter(|fields| !fields.is_empty()) {
            columns = columns_from_sample(fields, sample_size);
        }
    } else {
        for field in declared {
            declared_paths.insert(field.path.as_str());
            columns.push(ColumnInfo {
                order: 0,
                name: field.path.clone(),
                data_type: if field.types.is_empty() {
                    "unknown".to_string()
                } else {
                    field.types.join(" | ")
                },
                length: String::new(),
                is_primary_key: field.path == "_id",
                is_auto_increment: false,
                // validator 的 null 类型才是真正的「值可为空」；缺失写在列说明
                nullable: field.types.iter().any(|kind| kind == "null"),
                default_value: String::new(),
                comment: if field.required {
                    String::new()
                } else {
                    "可选（可能缺失）".to_string()
                },
            });
        }

        if let Some(fields) = sampled {
            let mut extras: Vec<&String> = fields
                .keys()
                .filter(|path| !declared_paths.contains(path.as_str()) && path.as_str() != "_id")
                .collect();
            extras.sort();
            sampled_extras = extras.len();
            for path in extras {
                columns.push(ColumnInfo {
                    order: 0,
                    name: path.clone(),
                    data_type: fields[path].type_text(),
                    length: String::new(),
                    is_primary_key: false,
                    is_auto_increment: false,
                    nullable: fields[path].nulls > 0,
                    default_value: String::new(),
                    comment: sample_comment(path, &fields[path], sample_size),
                });
            }
        }
    }

    ensure_id_column(&mut columns, sampled);
    for (idx, column) in columns.iter_mut().enumerate() {
        column.order = (idx + 1) as u32;
    }

    let sampled_columns = sampled.map(|fields| fields.len()).unwrap_or(0);
    let origin = field_origin(
        declared,
        sampled_columns,
        sampled_extras,
        sample_size,
        total_docs,
    );
    (columns, origin)
}

/// 空集合也要有主键列；类型优先用采样实测值，完全没数据时才按 ObjectId 假定。
fn ensure_id_column(columns: &mut Vec<ColumnInfo>, sampled: Option<&BTreeMap<String, FieldStats>>) {
    if columns.iter().any(|column| column.name == "_id") {
        return;
    }
    let data_type = sampled
        .and_then(|fields| fields.get("_id"))
        .map(|stats| stats.type_text())
        .unwrap_or_else(|| "objectId".to_string());
    columns.insert(
        0,
        ColumnInfo {
            order: 1,
            name: "_id".to_string(),
            length: String::new(),
            data_type,
            is_primary_key: true,
            is_auto_increment: false,
            nullable: false,
            default_value: String::new(),
            comment: String::new(),
        },
    );
}

fn field_origin(
    declared: &[DeclaredField],
    sampled_columns: usize,
    sampled_extras: usize,
    sample_size: usize,
    total_docs: Option<u64>,
) -> Option<FieldOrigin> {
    if declared.is_empty() {
        return if sampled_columns == 0 {
            None
        } else {
            Some(FieldOrigin::Sampled {
                sample_size,
                total_docs,
            })
        };
    }
    if sampled_extras == 0 {
        Some(FieldOrigin::Declared)
    } else {
        Some(FieldOrigin::Mixed {
            declared: declared.len(),
            sampled: sampled_extras,
            sample_size,
            total_docs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::{doc, oid::ObjectId};

    #[test]
    fn sample_aggregates_nested_arrays_and_mixed_types() {
        let docs = vec![
            doc! { "_id": ObjectId::new(), "name": "a", "addr": { "city": "x" }, "tags": ["a"], "score": 1_i32 },
            doc! { "_id": ObjectId::new(), "name": "b", "addr": { "city": null }, "score": "high" },
        ];
        let fields = aggregate_documents(&docs);

        assert_eq!(fields["_id"].present, 2);
        assert_eq!(fields["addr.city"].present, 2);
        assert_eq!(fields["addr.city"].nulls, 1);
        assert_eq!(fields["tags"].type_text(), "array<string>");
        assert_eq!(fields["score"].type_text(), "int32 | string");
        assert!(fields["tags"].types.contains_key("array<string>"));
    }

    #[test]
    fn sample_columns_put_id_first_and_mark_nulls() {
        let docs = vec![
            doc! { "_id": ObjectId::new(), "name": "a", "nick": "x" },
            doc! { "_id": ObjectId::new(), "name": "b" },
        ];
        let fields = aggregate_documents(&docs);
        let columns = columns_from_sample(&fields, 2);

        assert_eq!(columns[0].name, "_id");
        assert!(columns[0].is_primary_key);
        assert!(!columns[0].nullable);
        let nick = columns.iter().find(|c| c.name == "nick").unwrap();
        assert_eq!(nick.comment, "出现率 50%");
        assert!(!nick.nullable);
    }

    #[test]
    fn validator_maps_types_required_and_nesting() {
        let schema = doc! {
            "bsonType": "object",
            "required": ["name"],
            "properties": {
                "name": { "bsonType": "string" },
                "age": { "bsonType": "int" },
                "addr": { "bsonType": "object", "properties": {
                    "city": { "bsonType": ["string", "null"] }
                } },
                "tags": { "bsonType": "array", "items": { "bsonType": "string" } },
            },
        };
        let fields = parse_json_schema(&schema);
        let by_path = |path: &str| fields.iter().find(|f| f.path == path).unwrap();

        assert_eq!(by_path("name").types, vec!["string"]);
        assert!(by_path("name").required);
        assert_eq!(by_path("age").types, vec!["int32"]);
        assert!(!by_path("age").required);
        assert_eq!(by_path("addr.city").types, vec!["string", "null"]);
        assert_eq!(by_path("tags").types, vec!["array<string>"]);
    }

    #[test]
    fn merge_prefers_declaration_and_appends_observation() {
        let declared = vec![
            DeclaredField {
                path: "name".into(),
                types: vec!["string".into()],
                required: true,
            },
            DeclaredField {
                path: "age".into(),
                types: vec!["int32".into()],
                required: false,
            },
        ];
        let docs = vec![doc! { "_id": ObjectId::new(), "name": "a", "age": 1_i32, "extra": "x" }];
        let sampled = aggregate_documents(&docs);

        let (columns, origin) = merge_columns(&declared, Some(&sampled), 1, None);
        assert_eq!(columns[0].name, "_id");
        assert!(columns[0].is_primary_key);
        assert_eq!(columns[1].name, "name");
        assert_eq!(columns[2].comment, "可选（可能缺失）");
        assert_eq!(columns[3].name, "extra");
        assert_eq!(
            origin,
            Some(FieldOrigin::Mixed {
                declared: 2,
                sampled: 1,
                sample_size: 1,
                total_docs: None
            })
        );
        assert!(origin.unwrap().note().contains("validator 声明 2"));
    }

    #[test]
    fn empty_collection_still_has_primary_key() {
        let (columns, origin) = merge_columns(&[], None, 0, None);
        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0].name, "_id");
        assert!(columns[0].is_primary_key);
        assert_eq!(origin, None);
    }

    #[test]
    fn id_type_follows_sample_instead_of_guessing() {
        let declared = vec![DeclaredField {
            path: "name".into(),
            types: vec!["string".into()],
            required: true,
        }];
        let docs = vec![doc! { "_id": "uuid-1", "name": "a" }];
        let sampled = aggregate_documents(&docs);
        let (columns, _) = merge_columns(&declared, Some(&sampled), 1, None);
        assert_eq!(columns[0].name, "_id");
        assert_eq!(columns[0].data_type, "string");
    }

    #[test]
    fn note_reports_sample_coverage() {
        let with_total = FieldOrigin::Sampled {
            sample_size: 200,
            total_docs: Some(120_000),
        };
        assert!(with_total.note().contains("共约 120000 条文档"));

        let without_total = FieldOrigin::Sampled {
            sample_size: 200,
            total_docs: None,
        };
        assert!(!without_total.note().contains("共约"));
    }

    #[test]
    fn deep_nesting_stops_at_third_level() {
        let docs = vec![doc! { "a": { "b": { "c": { "d": 1_i32 } } } }];
        let fields = aggregate_documents(&docs);
        assert!(fields.contains_key("a.b.c"));
        assert!(!fields.contains_key("a.b.c.d"));
    }
}
