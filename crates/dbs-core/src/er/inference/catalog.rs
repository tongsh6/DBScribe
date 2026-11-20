//! 目录索引：可信键、类型签名、实体词表、声明外键与视图比较。
//!
//! 推断只从这里读取“已被目录证明的结构”，不会回头读取自己生成的预测边。

use super::super::{Endpoint, ErCatalog, TableId};
use super::text;
use crate::TableInfo;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Purpose {
    Id,
    Code,
    Uuid,
    Key,
}

impl Purpose {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Purpose::Id => "标识",
            Purpose::Code => "编号",
            Purpose::Uuid => "UUID",
            Purpose::Key => "业务键",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum RoleKey {
    Unknown,
    Phrase(Vec<String>),
    Entity(TableId),
}

impl RoleKey {
    pub(crate) fn is_unknown(&self) -> bool {
        matches!(self, RoleKey::Unknown)
    }

    pub(crate) fn display(&self) -> String {
        match self {
            RoleKey::Unknown => String::new(),
            RoleKey::Phrase(tokens) => tokens.join(""),
            RoleKey::Entity(table) => table.label(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct KeyInfo {
    pub endpoint: Endpoint,
    pub purposes: BTreeSet<Purpose>,
}

#[derive(Debug, Clone)]
pub(crate) struct DeclaredFk {
    pub name: String,
    pub source: Endpoint,
    pub target: Endpoint,
}

#[derive(Debug, Clone)]
pub(crate) enum KeyMatch {
    Key(Endpoint),
    Incomplete { key: Endpoint, reason: String },
    NonKey,
}

#[derive(Debug, Clone)]
pub(crate) enum TableLookup {
    Unique(TableId),
    Ambiguous(Vec<TableId>),
    Missing,
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedRef {
    Key(Endpoint),
    Incomplete {
        target: Endpoint,
        gap: String,
    },
    NonKey,
    Invalid {
        table: TableId,
        columns: Vec<String>,
        gap: String,
    },
    Ambiguous {
        targets: Vec<Endpoint>,
        gap: String,
    },
    External {
        table: TableId,
        columns: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Mention {
    pub table: TableId,
    pub start: usize,
    pub end: usize,
}

/// 按归一词序列建前缀树；终点保留所有同名实体，查找时不遍历无关表。
#[derive(Debug, Default, Clone)]
pub(crate) struct EntityIndex {
    next: BTreeMap<String, EntityIndex>,
    tables: BTreeSet<TableId>,
}

pub(crate) struct CatalogIndex<'a> {
    pub document_paths: bool,
    pub tables: BTreeMap<TableId, &'a TableInfo>,
    /// 全表有效的唯一键；只有这里的键能作为无条件事实、角色锚点或视图方向依据。
    pub keys: BTreeMap<Endpoint, KeyInfo>,
    /// MySQL 在“键位非空且用普通 `=` 比较”时至多匹配一行的键。不得当作全表唯一键。
    pub non_null_keys: BTreeMap<Endpoint, KeyInfo>,
    keys_by_table: BTreeMap<TableId, Vec<KeyInfo>>,
    all_keys_by_table: BTreeMap<TableId, Vec<KeyInfo>>,
    pub declared: Vec<DeclaredFk>,
    signatures: BTreeMap<(TableId, String), BTreeSet<String>>,
    pub entities: EntityIndex,
    views: BTreeMap<(Endpoint, Endpoint), Vec<TableId>>,
    /// 只表达 `<=>` 的列对；仅用于阻断普通等值推断，不作为正证。
    null_safe_views: BTreeSet<(Endpoint, Endpoint)>,
}

impl<'a> CatalogIndex<'a> {
    pub(crate) fn new(catalog: &'a ErCatalog) -> Self {
        let mut tables = BTreeMap::new();
        for table in &catalog.tables {
            tables.entry(TableId::from(table)).or_insert(table);
        }
        let mut signatures: BTreeMap<(TableId, String), BTreeSet<String>> = BTreeMap::new();
        for entry in &catalog.types {
            signatures
                .entry((entry.table.clone(), entry.column.clone()))
                .or_default()
                .insert(entry.signature.trim().to_string());
        }
        let keys = build_keys(&tables, &catalog.keys);
        let non_null_keys: BTreeMap<_, _> = build_keys(&tables, &catalog.non_null_keys)
            .into_iter()
            .filter(|(endpoint, _)| !keys.contains_key(endpoint))
            .collect();
        let mut keys_by_table: BTreeMap<TableId, Vec<KeyInfo>> = BTreeMap::new();
        let mut all_keys_by_table: BTreeMap<TableId, Vec<KeyInfo>> = BTreeMap::new();
        for key in keys.values() {
            keys_by_table
                .entry(key.endpoint.table.clone())
                .or_default()
                .push(key.clone());
        }
        // 只为具有可空唯一键的表建立扩展索引，其他表复用已有全表键索引。
        for key in non_null_keys.values() {
            all_keys_by_table
                .entry(key.endpoint.table.clone())
                .or_insert_with(|| {
                    keys_by_table
                        .get(&key.endpoint.table)
                        .cloned()
                        .unwrap_or_default()
                })
                .push(key.clone());
        }
        for list in keys_by_table
            .values_mut()
            .chain(all_keys_by_table.values_mut())
        {
            list.sort_by(|a, b| a.endpoint.cmp(&b.endpoint));
        }
        let declared = build_declared(&tables);
        let entities = EntityIndex::build(&tables);
        let mut views: BTreeMap<(Endpoint, Endpoint), Vec<TableId>> = BTreeMap::new();
        for comparison in &catalog.views {
            if comparison.left.columns.is_empty() || comparison.right.columns.is_empty() {
                continue;
            }
            let pair = normalize_pair(&comparison.left, &comparison.right);
            views.entry(pair).or_default().push(comparison.view.clone());
        }
        for list in views.values_mut() {
            list.sort();
            list.dedup();
        }
        let null_safe_views = catalog
            .null_safe_views
            .iter()
            .filter(|comparison| {
                !comparison.left.columns.is_empty() && !comparison.right.columns.is_empty()
            })
            .map(|comparison| normalize_pair(&comparison.left, &comparison.right))
            .collect();
        Self {
            tables,
            document_paths: catalog.db_type == Some(crate::DbType::MongoDb),
            keys,
            non_null_keys,
            keys_by_table,
            all_keys_by_table,
            declared,
            signatures,
            entities,
            views,
            null_safe_views,
        }
    }

    pub(crate) fn table(&self, id: &TableId) -> Option<&'a TableInfo> {
        self.tables.get(id).copied()
    }

    pub(crate) fn is_trusted_key(&self, endpoint: &Endpoint) -> bool {
        self.keys.contains_key(endpoint)
    }

    /// 可作为普通等值推断目标的键（全表键或“非空等值”键）。
    pub(crate) fn is_known_key(&self, endpoint: &Endpoint) -> bool {
        self.keys.contains_key(endpoint) || self.non_null_keys.contains_key(endpoint)
    }

    /// 该目标只在普通 `=` 且值非空时至多匹配一行；NULL 不匹配，也不证明目标存在。
    pub(crate) fn is_conditional_key(&self, endpoint: &Endpoint) -> bool {
        self.non_null_keys.contains_key(endpoint)
    }

    pub(crate) fn key_info(&self, endpoint: &Endpoint) -> Option<&KeyInfo> {
        self.keys
            .get(endpoint)
            .or_else(|| self.non_null_keys.get(endpoint))
    }

    pub(crate) fn non_null_types_match(&self, source: &Endpoint, target: &Endpoint) -> bool {
        source.columns.len() == target.columns.len()
            && source.columns.iter().zip(&target.columns).all(|(s, t)| {
                self.signature(&source.table, s)
                    .zip(self.signature(&target.table, t))
                    .is_some_and(|(a, b)| a == b && complete_mysql_comparison_type(a))
            })
    }

    /// 推断正/负证据可以指向的全部键；弱证据层仍只读取全表键。
    pub(crate) fn all_keys_for_table(&self, table: &TableId) -> &[KeyInfo] {
        self.all_keys_by_table
            .get(table)
            .map(Vec::as_slice)
            .unwrap_or_else(|| self.keys_for_table(table))
    }

    /// 普通等值视图是否与同一列对使用了 `<=>`。
    ///
    /// 复合/混合视图按逐列配对判断：只要视图把来源的一列与目标的同一配对列做了
    /// 空安全比较，就不能把整组端点当成普通等值。单列端点仍保持精确匹配。
    pub(crate) fn has_null_safe_comparison(&self, left: &Endpoint, right: &Endpoint) -> bool {
        if self.null_safe_views.contains(&normalize_pair(left, right)) {
            return true;
        }
        self.null_safe_views
            .iter()
            .any(|(a, b)| aligned_pair(a, b, left, right) || aligned_pair(b, a, left, right))
    }

    pub(crate) fn signature(&self, table: &TableId, column: &str) -> Option<&str> {
        let set = self.signatures.get(&(table.clone(), column.to_string()))?;
        if set.len() == 1 {
            set.iter().next().map(String::as_str)
        } else {
            None
        }
    }

    pub(crate) fn keys_for_table(&self, table: &TableId) -> &[KeyInfo] {
        self.keys_by_table
            .get(table)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// 该列是否单独构成可信键；仅参与复合键的成员不算自身唯一。
    pub(crate) fn is_single_column_key(&self, table: &TableId, column: &str) -> bool {
        self.keys_for_table(table)
            .iter()
            .any(|key| key.endpoint.columns.len() == 1 && key.endpoint.columns[0] == column)
    }

    /// 目录中由多列组成、且列集合与表主键完全一致的完整可信键。
    /// 主键身份使用目录原始标识精确比较；带引号的大小写不同字段不能被 ASCII 折叠合并。
    pub(crate) fn composite_primary_keys(&self, table: &TableId) -> Vec<&KeyInfo> {
        let Some(info) = self.table(table) else {
            return Vec::new();
        };
        let primary: BTreeSet<&str> = info
            .columns
            .iter()
            .filter(|column| column.is_primary_key)
            .map(|column| column.name.as_str())
            .collect();
        if primary.len() < 2 {
            return Vec::new();
        }
        self.keys_for_table(table)
            .iter()
            .filter(|key| {
                key.endpoint.columns.len() > 1
                    && key.endpoint.columns.len() == primary.len()
                    && key
                        .endpoint
                        .columns
                        .iter()
                        .all(|column| primary.contains(column.as_str()))
            })
            .collect()
    }

    pub(crate) fn keys_by_purpose(
        &self,
        table: &TableId,
        purposes: &BTreeSet<Purpose>,
    ) -> Vec<Endpoint> {
        let mut out: Vec<_> = self
            .all_keys_for_table(table)
            .iter()
            .filter(|key| purposes.is_empty() || key.purposes.iter().any(|p| purposes.contains(p)))
            .map(|key| key.endpoint.clone())
            .collect();
        out.sort();
        out
    }

    pub(crate) fn key_match(&self, table: &TableId, columns: &[String]) -> KeyMatch {
        let keys = self.all_keys_for_table(table);
        for key in keys {
            if key.endpoint.columns == columns {
                return KeyMatch::Key(key.endpoint.clone());
            }
        }
        for key in keys {
            if key.endpoint.columns.len() == columns.len()
                && key.endpoint.columns.iter().all(|a| columns.contains(a))
            {
                return KeyMatch::Incomplete {
                    key: key.endpoint.clone(),
                    reason: format!("列顺序与完整键 {} 不一致", key.endpoint.columns.join(", ")),
                };
            }
            if columns.len() == 1
                && key.endpoint.columns.len() > 1
                && key.endpoint.columns.iter().any(|a| a == &columns[0])
            {
                return KeyMatch::Incomplete {
                    key: key.endpoint.clone(),
                    reason: format!(
                        "注释只给出复合键 {} 的部分列，不能拆成单列关系",
                        key.endpoint.columns.join(", ")
                    ),
                };
            }
        }
        KeyMatch::NonKey
    }

    pub(crate) fn resolve_raw_ref(&self, raw: &text::RawRef) -> ResolvedRef {
        let Some((schema, name, columns)) = split_ref(raw) else {
            return ResolvedRef::Invalid {
                table: TableId {
                    schema: String::new(),
                    name: raw.path.join("."),
                },
                columns: Vec::new(),
                gap: "无法解析限定引用".into(),
            };
        };
        match self.lookup_table(schema.as_deref(), &name, raw.quoted) {
            TableLookup::Unique(table) => {
                let Some(actual) = self.resolve_columns(&table, &columns, raw.quoted) else {
                    return ResolvedRef::Invalid {
                        table,
                        columns,
                        gap: "限定引用中的列在当前表中不存在".into(),
                    };
                };
                match self.key_match(&table, &actual) {
                    KeyMatch::Key(key) => ResolvedRef::Key(key),
                    KeyMatch::Incomplete { key, reason } => ResolvedRef::Incomplete {
                        target: key,
                        gap: reason,
                    },
                    KeyMatch::NonKey => ResolvedRef::NonKey,
                }
            }
            TableLookup::Ambiguous(tables) => {
                let mut targets = Vec::new();
                for table in &tables {
                    if let Some(actual) = self.resolve_columns(table, &columns, raw.quoted) {
                        if let KeyMatch::Key(key) = self.key_match(table, &actual) {
                            targets.push(key);
                        }
                    }
                }
                targets.sort();
                targets.dedup();
                if targets.is_empty() {
                    ResolvedRef::Invalid {
                        table: tables.first().cloned().unwrap_or(TableId {
                            schema: String::new(),
                            name,
                        }),
                        columns,
                        gap: "表名在多个 schema 中不唯一，且无法确定完整键".into(),
                    }
                } else {
                    ResolvedRef::Ambiguous {
                        targets,
                        gap: "表名在多个 schema 中不唯一".into(),
                    }
                }
            }
            TableLookup::Missing => ResolvedRef::External {
                table: TableId {
                    schema: schema.unwrap_or_default(),
                    name,
                },
                columns,
            },
        }
    }

    fn lookup_table(&self, schema: Option<&str>, name: &str, quoted: bool) -> TableLookup {
        let eq = |a: &str, b: &str| a == b || (!quoted && a.eq_ignore_ascii_case(b));
        let mut found: Vec<TableId> = self
            .tables
            .keys()
            .filter(|id| {
                eq(&id.name, name)
                    && match schema {
                        Some(s) => eq(&id.schema, s),
                        None => true,
                    }
            })
            .cloned()
            .collect();
        found.sort();
        match found.len() {
            0 => TableLookup::Missing,
            1 => TableLookup::Unique(found.remove(0)),
            _ => TableLookup::Ambiguous(found),
        }
    }

    fn resolve_columns(
        &self,
        table: &TableId,
        columns: &[String],
        quoted: bool,
    ) -> Option<Vec<String>> {
        let info = self.table(table)?;
        columns
            .iter()
            .map(|name| {
                let mut found = info
                    .columns
                    .iter()
                    .filter(|c| c.name == *name || (!quoted && c.name.eq_ignore_ascii_case(name)));
                let first = found.next()?;
                found.next().is_none().then(|| first.name.clone())
            })
            .collect()
    }

    pub(crate) fn views_for(&self, source: &Endpoint, target: &Endpoint) -> Vec<TableId> {
        let pair = normalize_pair(source, target);
        self.views.get(&pair).cloned().unwrap_or_default()
    }

    /// 单列或复合等值比较都保留原始列对，比较的任意一侧均可能是来源。
    pub(crate) fn view_compares_elsewhere(
        &self,
        table: &TableId,
        member: &str,
        expected: &Endpoint,
    ) -> bool {
        let disagrees = |source: &Endpoint, other: &Endpoint| {
            &source.table == table
                && source.columns.iter().zip(&other.columns).any(|(a, b)| {
                    a == member
                        && (other.table != expected.table || expected.columns.first() != Some(b))
                })
        };
        self.views
            .keys()
            .any(|(left, right)| disagrees(left, right) || disagrees(right, left))
    }

    pub(crate) fn domain_for(&self, table: &TableId) -> Option<String> {
        self.table(table)
            .and_then(|t| text::extract_domain(&t.comment))
    }
}

fn normalize_pair(left: &Endpoint, right: &Endpoint) -> (Endpoint, Endpoint) {
    if left <= right {
        (left.clone(), right.clone())
    } else {
        (right.clone(), left.clone())
    }
}

/// 视图比较的一列对是否同时落在推断端点的同一配对位置。
/// 字符类型由排序规则定义等号语义；两份同样缺少排序规则的签名不能互相佐证。
fn complete_mysql_comparison_type(signature: &str) -> bool {
    let base = signature.split(['(', ' ']).next().unwrap_or_default();
    if signature.trim().is_empty() {
        return false;
    }
    match base.to_ascii_lowercase().as_str() {
        "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set" => {
            signature.rsplit_once(" collate ").is_some_and(|(_, name)| {
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
        }
        _ => true,
    }
}

fn aligned_pair(
    view_left: &Endpoint,
    view_right: &Endpoint,
    source: &Endpoint,
    target: &Endpoint,
) -> bool {
    if view_left.table != source.table || view_right.table != target.table {
        return false;
    }
    view_left
        .columns
        .iter()
        .zip(&view_right.columns)
        .any(|(left, right)| {
            let source_at = source.columns.iter().position(|column| column == left);
            let target_at = target.columns.iter().position(|column| column == right);
            source_at.is_some() && source_at == target_at
        })
}

fn split_ref(raw: &text::RawRef) -> Option<(Option<String>, String, Vec<String>)> {
    if !raw.columns.is_empty() {
        let table = raw.path.last()?.clone();
        let schema = if raw.path.len() >= 2 {
            Some(raw.path[raw.path.len() - 2].clone())
        } else {
            None
        };
        return Some((schema, table, raw.columns.clone()));
    }
    if raw.path.len() < 2 {
        return None;
    }
    let column = raw.path.last()?.clone();
    let table = raw.path[raw.path.len() - 2].clone();
    let schema = if raw.path.len() >= 3 {
        Some(raw.path[raw.path.len() - 3].clone())
    } else {
        None
    };
    Some((schema, table, vec![column]))
}

fn build_keys(
    tables: &BTreeMap<TableId, &TableInfo>,
    endpoints: &[Endpoint],
) -> BTreeMap<Endpoint, KeyInfo> {
    let mut out = BTreeMap::new();
    for endpoint in endpoints {
        let Some(table) = tables.get(&endpoint.table) else {
            continue;
        };
        if endpoint.columns.is_empty() {
            continue;
        }
        let mut seen = BTreeSet::new();
        // 键列身份同样用目录原始标识：带引号的 `Id` 与 `ID` 是两个不同的列。
        if endpoint.columns.iter().any(|c| !seen.insert(c.as_str())) {
            continue;
        }
        if !endpoint
            .columns
            .iter()
            .all(|name| table.columns.iter().any(|c| &c.name == name))
        {
            continue;
        }
        let pk: BTreeSet<_> = table
            .columns
            .iter()
            .filter(|c| c.is_primary_key)
            .map(|c| c.name.as_str())
            .collect();
        let key_set: BTreeSet<_> = endpoint.columns.iter().map(String::as_str).collect();
        let primary = !pk.is_empty() && pk == key_set;
        let mut purposes = BTreeSet::new();
        if primary {
            purposes.insert(Purpose::Id);
        }
        for column in &endpoint.columns {
            let Some(info) = table.columns.iter().find(|c| &c.name == column) else {
                continue;
            };
            for purpose in purposes_in_name(&info.name) {
                purposes.insert(purpose);
            }
            for purpose in purposes_in_text(&text::tokenize(&info.comment)) {
                purposes.insert(purpose);
            }
        }
        out.insert(
            endpoint.clone(),
            KeyInfo {
                endpoint: endpoint.clone(),
                purposes,
            },
        );
    }
    out
}

fn build_declared(tables: &BTreeMap<TableId, &TableInfo>) -> Vec<DeclaredFk> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (source_table, table) in tables {
        for fk in &table.foreign_keys {
            if fk.columns.is_empty() || fk.columns.len() != fk.ref_columns.len() {
                continue;
            }
            let source = Endpoint {
                table: source_table.clone(),
                columns: fk.columns.clone(),
            };
            let target = Endpoint {
                table: TableId {
                    schema: if fk.ref_schema.is_empty() {
                        source_table.schema.clone()
                    } else {
                        fk.ref_schema.clone()
                    },
                    name: fk.ref_table.clone(),
                },
                columns: fk.ref_columns.clone(),
            };
            if source == target {
                continue;
            }
            let identity = (source.clone(), target.clone());
            if seen.insert(identity) {
                out.push(DeclaredFk {
                    name: fk.name.clone(),
                    source,
                    target,
                });
            }
        }
    }
    out.sort_by(|a, b| (&a.source, &a.target, &a.name).cmp(&(&b.source, &b.target, &b.name)));
    out
}

pub(crate) fn purposes_in_name(name: &str) -> BTreeSet<Purpose> {
    let tokens = text::tokenize(name);
    let mut out = BTreeSet::new();
    if let Some(last) = tokens.last() {
        if let Some(purpose) = purpose_of_token(&last.text) {
            out.insert(purpose);
        }
    }
    out.extend(purposes_in_text(&tokens));
    out
}

pub(crate) fn purposes_in_text(tokens: &[text::Token]) -> BTreeSet<Purpose> {
    let mut out = BTreeSet::new();
    for (purpose, phrases) in purpose_phrases() {
        if phrases
            .iter()
            .any(|phrase| text::contains_phrase(tokens, phrase))
        {
            out.insert(purpose);
        }
    }
    out
}

fn purpose_phrases() -> [(Purpose, &'static [&'static str]); 4] {
    [
        (
            Purpose::Id,
            &["主键", "标识", "id", "pk", "identity", "identifier"],
        ),
        (
            Purpose::Code,
            &[
                "编号",
                "编码",
                "代码",
                "业务号",
                "序号",
                "code",
                "no",
                "num",
                "number",
            ],
        ),
        (Purpose::Uuid, &["uuid", "guid"]),
        (Purpose::Key, &["业务键", "键", "key"]),
    ]
}

fn purpose_of_token(token: &str) -> Option<Purpose> {
    match token {
        "id" | "pk" | "identity" | "identifier" => Some(Purpose::Id),
        "code" | "no" | "num" | "number" => Some(Purpose::Code),
        "uuid" | "guid" => Some(Purpose::Uuid),
        "key" => Some(Purpose::Key),
        _ => None,
    }
}

impl EntityIndex {
    fn build(tables: &BTreeMap<TableId, &TableInfo>) -> Self {
        let mut root = Self::default();
        for (id, table) in tables {
            let mut add = |tokens: Vec<String>| {
                if !valid_alias(&tokens) {
                    return;
                }
                let mut node = &mut root;
                for token in tokens {
                    node = node.next.entry(text::singular(&token)).or_default();
                }
                node.tables.insert(id.clone());
            };
            for tokens in name_variants(&table.name) {
                add(tokens);
            }
            let no_domain = text::strip_domain(&table.comment);
            let no_parens = strip_parentheticals(&no_domain);
            for segment in split_segments(&no_parens) {
                for tokens in comment_variants(segment) {
                    add(tokens);
                }
            }
            for alias in explicit_aliases(&table.comment) {
                add(text::tokens_of(&alias));
            }
        }
        root
    }

    pub(crate) fn mentions(&self, tokens: &[text::Token]) -> Vec<Mention> {
        let normalized: Vec<_> = tokens.iter().map(|v| text::singular(&v.text)).collect();
        let mut mentions = Vec::new();
        for start in 0..tokens.len() {
            let mut node = self;
            for (offset, token) in normalized[start..].iter().enumerate() {
                let Some(next) = node.next.get(token) else {
                    break;
                };
                node = next;
                if offset == 0 && token.chars().count() < 2 {
                    continue;
                }
                let end = start + offset + 1;
                mentions.extend(node.tables.iter().map(|t| Mention {
                    table: t.clone(),
                    start,
                    end,
                }));
            }
        }
        mentions.sort_by(|a, b| (&a.table, a.start, a.end).cmp(&(&b.table, b.start, b.end)));
        mentions
    }

    pub(crate) fn tables_for_tokens(&self, tokens: &[String]) -> Vec<TableId> {
        let mut node = self;
        for token in tokens {
            let Some(next) = node.next.get(&text::singular(token)) else {
                return Vec::new();
            };
            node = next;
        }
        node.tables.iter().cloned().collect()
    }

    pub(crate) fn canonical_role(&self, tokens: &[String]) -> RoleKey {
        if tokens.is_empty() {
            return RoleKey::Unknown;
        }
        let tables = self.tables_for_tokens(tokens);
        if tables.len() == 1 {
            RoleKey::Entity(tables[0].clone())
        } else {
            RoleKey::Phrase(tokens.to_vec())
        }
    }
}

fn name_variants(name: &str) -> Vec<Vec<String>> {
    let tokens = text::tokens_of(name);
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut bases = vec![tokens.clone()];
    let prefixes = [
        "t", "tb", "tbl", "table", "sys", "app", "biz", "base", "dim", "fact",
    ];
    if tokens.len() > 1 && prefixes.contains(&tokens[0].as_str()) {
        bases.push(tokens[1..].to_vec());
    }
    let suffixes = [
        "table",
        "tables",
        "info",
        "information",
        "record",
        "records",
        "list",
        "data",
        "entity",
        "master",
        "detail",
        "details",
    ];
    let mut out = Vec::new();
    for base in bases {
        let mut current = base;
        loop {
            let last = current.last().map(String::as_str).unwrap_or("");
            if current.len() > 1 && (last == "表" || suffixes.contains(&last)) {
                current.pop();
                if !out.contains(&current) {
                    out.push(current.clone());
                }
            } else {
                break;
            }
        }
        if !out.contains(&current) {
            out.push(current);
        }
    }
    for base in out.clone() {
        let singular: Vec<_> = base.iter().map(|t| text::singular(t)).collect();
        if singular != base && !out.contains(&singular) {
            out.push(singular);
        }
    }
    out
}

fn comment_variants(segment: &str) -> Vec<Vec<String>> {
    let segment = segment.trim();
    if segment.is_empty() {
        return Vec::new();
    }
    let mut strings = vec![segment.to_string()];
    let mut work = vec![segment.to_string()];
    let suffixes = [
        "信息表",
        "记录表",
        "数据表",
        "明细表",
        "主档表",
        "信息",
        "记录",
        "数据",
        "明细",
        "主档",
        "档案",
        "列表",
        "表",
        "实体",
    ];
    while let Some(current) = work.pop() {
        for suffix in suffixes {
            if let Some(prefix) = current.strip_suffix(suffix) {
                let prefix = prefix.trim().to_string();
                if !prefix.is_empty() && !strings.contains(&prefix) {
                    strings.push(prefix.clone());
                    work.push(prefix);
                }
            }
        }
    }
    let english = [
        "table",
        "tables",
        "info",
        "information",
        "record",
        "records",
        "list",
        "data",
        "entity",
        "master",
        "detail",
        "details",
    ];
    let mut out = Vec::new();
    for value in strings {
        let mut tokens = text::tokens_of(&value);
        while tokens.len() > 1 && english.contains(&tokens.last().map(String::as_str).unwrap_or(""))
        {
            tokens.pop();
            if !out.contains(&tokens) {
                out.push(tokens.clone());
            }
        }
        if !out.contains(&tokens) {
            out.push(tokens);
        }
    }
    out
}

fn strip_parentheticals(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '(' | '（' | '[' | '【' => depth += 1,
            ')' | '）' | ']' | '】' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn split_segments(text: &str) -> Vec<&str> {
    text.split(|c: char| {
        matches!(
            c,
            '，' | ',' | '。' | ';' | '；' | ':' | '：' | '/' | '|' | '、'
        )
    })
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .collect()
}

fn explicit_aliases(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
    for marker in ["别名", "又称", "alias", "also known as", "aka"] {
        let haystack = if marker.is_ascii() {
            lower.as_str()
        } else {
            text
        };
        let mut from = 0usize;
        while let Some(rel) = haystack[from..].find(marker) {
            let at = from + rel;
            let after = at + marker.len();
            let rest = text[after..].trim_start();
            let rest = rest.trim_start_matches(|c: char| {
                matches!(c, ':' | '：' | '=' | '＝' | '(' | '（' | ' ')
            });
            let end = rest
                .char_indices()
                .find(|(_, c)| matches!(c, '，' | ',' | '。' | ';' | '；' | ')' | '）' | '\n'))
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            let value = rest[..end].trim();
            if !value.is_empty() {
                out.push(value.to_string());
            }
            from = after;
        }
    }
    out
}

fn valid_alias(tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    if tokens.len() == 1 && tokens[0].chars().count() < 2 {
        return false;
    }
    let generic: Vec<Vec<String>> = [
        "表",
        "信息",
        "数据",
        "记录",
        "系统",
        "业务",
        "实体",
        "明细",
        "主档",
        "档案",
        "列表",
        "data",
        "info",
        "information",
        "record",
        "records",
        "table",
        "tables",
        "entity",
        "system",
        "business",
        "detail",
        "details",
        "list",
        "base",
    ]
    .iter()
    .map(|s| text::tokens_of(s))
    .collect();
    !generic
        .iter()
        .any(|g| g.len() == tokens.len() && g.iter().zip(tokens).all(|(a, b)| text::token_eq(a, b)))
}

/// 只在 `catalog.types` 两侧都有唯一签名时给出判断；
/// `Some(false)` 才是“确定不兼容”，`None` 表示信息不足。
pub(crate) fn compatible_types(a: &str, b: &str) -> Option<bool> {
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() || b.is_empty() {
        return None;
    }
    if a == b {
        return Some(true);
    }
    if let (Some(a), Some(b)) = (a.strip_prefix("bson:"), b.strip_prefix("bson:")) {
        let a = a.split_whitespace().next().unwrap_or("");
        let b = b.split_whitespace().next().unwrap_or("");
        let numeric = |s: &str| matches!(s, "int32" | "int64" | "double" | "decimal128" | "number");
        return if a == b || (numeric(a) && numeric(b)) {
            None
        } else {
            Some(false)
        };
    }
    // 驱动的完整签名承载符号、精度和排序规则。只归一没有修饰的已知别名。
    let alias = |s: &str| match s.to_ascii_lowercase().as_str() {
        "int" | "integer" => Some("integer"),
        "varchar" | "character varying" => Some("varchar"),
        _ => None,
    };
    if let (Some(x), Some(y)) = (alias(a), alias(b)) {
        return Some(x == y);
    }
    match (family(&base_type(a)), family(&base_type(b))) {
        (Some(x), Some(y)) if x != y => Some(false),
        _ => None,
    }
}

fn base_type(signature: &str) -> String {
    let lower = signature.trim().to_ascii_lowercase();
    let before_paren = lower.split('(').next().unwrap_or(&lower);
    before_paren
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(PartialEq, Eq)]
enum TypeFamily {
    Numeric,
    String,
    Uuid,
    Temporal,
    Boolean,
    Binary,
    Json,
}

fn family(base: &str) -> Option<TypeFamily> {
    let head = base.split_whitespace().next().unwrap_or(base);
    if [
        "tinyint",
        "smallint",
        "mediumint",
        "int",
        "integer",
        "bigint",
        "int2",
        "int4",
        "int8",
        "serial",
        "bigserial",
        "numeric",
        "decimal",
        "dec",
        "number",
        "float",
        "float4",
        "float8",
        "double",
        "real",
        "money",
        "smallmoney",
    ]
    .contains(&head)
    {
        return Some(TypeFamily::Numeric);
    }
    if [
        "char",
        "character",
        "national",
        "varchar",
        "varchar2",
        "nvarchar",
        "nvarchar2",
        "nchar",
        "text",
        "tinytext",
        "mediumtext",
        "longtext",
        "clob",
        "nclob",
        "string",
        "citext",
        "bpchar",
    ]
    .contains(&head)
    {
        return Some(TypeFamily::String);
    }
    if ["uuid", "uniqueidentifier"].contains(&head) {
        return Some(TypeFamily::Uuid);
    }
    if [
        "date",
        "time",
        "timetz",
        "timestamp",
        "timestamptz",
        "datetime",
        "datetime2",
        "datetimeoffset",
        "smalldatetime",
        "year",
        "interval",
    ]
    .contains(&head)
    {
        return Some(TypeFamily::Temporal);
    }
    if ["bool", "boolean", "bit"].contains(&head) {
        return Some(TypeFamily::Boolean);
    }
    if [
        "binary",
        "varbinary",
        "blob",
        "tinyblob",
        "mediumblob",
        "longblob",
        "bytea",
        "image",
        "raw",
    ]
    .contains(&head)
    {
        return Some(TypeFamily::Binary);
    }
    if ["json", "jsonb"].contains(&head) {
        return Some(TypeFamily::Json);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, primary: bool) -> crate::ColumnInfo {
        crate::ColumnInfo {
            order: 0,
            name: name.into(),
            data_type: "integer".into(),
            length: String::new(),
            is_primary_key: primary,
            is_auto_increment: false,
            nullable: !primary,
            default_value: String::new(),
            comment: String::new(),
        }
    }

    #[test]
    fn composite_primary_key_identity_keeps_quoted_case_variants_distinct() {
        let table = TableInfo {
            schema: "public".into(),
            name: "assignments".into(),
            columns: vec![
                column("Id", true),
                column("ID", true),
                column("gauge_id", false),
            ],
            ..Default::default()
        };
        let exact = Endpoint {
            table: TableId::from(&table),
            columns: vec!["Id".into(), "ID".into()],
        };
        let catalog = ErCatalog {
            tables: vec![table],
            keys: vec![exact.clone()],
            ..Default::default()
        };
        let index = CatalogIndex::new(&catalog);

        let keys = index.composite_primary_keys(&exact.table);
        assert_eq!(keys.len(), 1, "大小写不同的两个原始标识应保持为两列主键");
        assert_eq!(keys[0].endpoint, exact);
    }
}
