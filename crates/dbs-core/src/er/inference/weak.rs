//! 弱证据目标召回：在既有裁决之后追加独立候选层。
//!
//! 这里只读取目录中已有的键、列名、注释和实体词，不读取推断结果去反推目标；
//! 只产出 `Candidate`，不把父子/来源、尾部同名或模块前缀等弱线索升级为关系。

use super::super::{Analysis, Candidate, Endpoint, TableId};
use super::catalog::{compatible_types, purposes_in_name, CatalogIndex, KeyInfo, Purpose};
use super::text;
use super::{ParsedComment, Scope};
use std::collections::{BTreeMap, BTreeSet};

/// 结构性父子/来源词。它们只说明引用角色，不证明具体端点。
const STRUCTURAL_PREFIXES: &[&str] = &[
    "parent",
    "child",
    "source",
    "origin",
    "root",
    "ancestor",
    "descendant",
    "previous",
    "prior",
    "upstream",
    "downstream",
];

pub(super) fn append(
    index: &CatalogIndex<'_>,
    comments: &BTreeMap<&str, ParsedComment>,
    analysis: &mut Analysis,
) {
    let resolved: BTreeSet<_> = analysis.relations.iter().map(|r| &r.source).collect();
    let mut occupied: BTreeSet<(Endpoint, Endpoint)> = BTreeSet::new();
    for relation in &analysis.relations {
        occupied.insert((relation.source.clone(), relation.target.clone()));
    }
    for candidate in &analysis.candidates {
        occupied.insert((candidate.source.clone(), candidate.target.clone()));
    }

    let key_index = KeyIndex::new(index);
    let aliases = ModuleAliases::new(index);
    let mut additions: BTreeMap<(Endpoint, Endpoint), BTreeSet<String>> = BTreeMap::new();
    let table_ids: Vec<TableId> = index.tables.keys().cloned().collect();

    for table_id in table_ids {
        let Some(table) = index.table(&table_id) else {
            continue;
        };
        for column in &table.columns {
            let source = Endpoint {
                table: table_id.clone(),
                columns: vec![column.name.clone()],
            };
            if resolved.contains(&source) {
                continue;
            }
            let Some(intent) = Intent::parse(
                index,
                &table_id,
                column,
                &aliases,
                &comments[column.comment.as_str()],
            ) else {
                continue;
            };
            let mut targets: BTreeMap<Endpoint, BTreeSet<String>> = BTreeMap::new();

            if intent.self_reference {
                let role = intent.role_word.as_deref().unwrap_or("");
                let reason = format!(
                    "字段名「{}」以父子/来源结构词「{}」开头，去掉角色词和记录后缀后没有其他实体，按同表可信键召回",
                    column.name, role
                );
                collect_table_keys(
                    index,
                    &table_id,
                    &intent.purposes,
                    &source,
                    &reason,
                    &mut targets,
                );
            }

            for target_table in &intent.entity_tables {
                let reason = format!(
                    "字段名「{}」去掉键用途后，后缀「{}」由实体词索引解析到 {}；前缀只说明引用角色或模块，不证明具体端点",
                    column.name,
                    intent.entity_text,
                    target_table.label()
                );
                collect_table_keys(
                    index,
                    target_table,
                    &intent.purposes,
                    &source,
                    &reason,
                    &mut targets,
                );
            }

            for endpoint in key_index.tail_matches(&intent.tokens) {
                let reason = format!(
                    "字段名「{}」的尾部与 {} 的可信键列名「{}」一致（目标表名或来源字段可能带模块/角色前缀），但只有名称尾部匹配",
                    column.name,
                    endpoint.table.label(),
                    endpoint.columns.join(", ")
                );
                targets.entry(endpoint).or_default().insert(reason);
            }

            let competing = targets.len();
            for (target, mut reasons) in targets {
                if target == source || occupied.contains(&(source.clone(), target.clone())) {
                    continue;
                }
                // 源列本身是键列时，不向另一张表的同名列扩散；与命名路径的同类保护一致。
                if is_key_column(index, &source.table, &column.name)
                    && target
                        .columns
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(&column.name))
                {
                    continue;
                }
                // 仅靠名称的弱候选须有可核实的类型依据，缺失信息不构成肯定证据。
                if type_fit(index, &source, &target) != Some(true) {
                    continue;
                }
                if let Some(scope) = intent.scope_note {
                    reasons.insert(format!("注释属于{scope}陈述，仅在其原有范围内保留"));
                }
                if competing > 1 {
                    reasons.insert(format!(
                        "同来源另有 {} 个弱目标竞争，保留待判断",
                        competing - 1
                    ));
                }
                additions
                    .entry((source.clone(), target))
                    .or_default()
                    .extend(reasons);
            }
        }
    }

    for ((source, target), reasons) in additions {
        analysis.candidates.push(Candidate {
            condition: None,
            source,
            target,
            reason: format!(
                "待判断（弱证据目标召回）：{}。只有名称/结构弱证据，不自动确认方向。",
                reasons.into_iter().collect::<Vec<_>>().join("；")
            ),
        });
    }
}

struct Intent {
    tokens: Vec<String>,
    purposes: BTreeSet<Purpose>,
    entity_tables: Vec<TableId>,
    entity_text: String,
    role_word: Option<String>,
    self_reference: bool,
    scope_note: Option<&'static str>,
}

impl Intent {
    fn parse(
        index: &CatalogIndex<'_>,
        table: &TableId,
        column: &crate::ColumnInfo,
        aliases: &ModuleAliases,
        parsed: &ParsedComment,
    ) -> Option<Self> {
        let field_name = if index.document_paths {
            if column.name.contains("[]") {
                return None;
            }
            column.name.rsplit('.').next().unwrap_or(&column.name)
        } else {
            column.name.as_str()
        };
        let tokens = text::tokens_of(field_name);
        if tokens.len() < 2 || is_pure_generic(&tokens) {
            return None;
        }

        let purposes = purposes_in_name(field_name);
        let role_index = weak_role_prefix(&tokens);
        if purposes.is_empty() && role_index.is_none() {
            return None;
        }

        // 明确否定、外部归属或已有可追溯端点时，交给既有裁决，不再扩散弱候选。
        if parsed.clauses.iter().any(|clause| {
            clause.negation || clause.scope == Some(Scope::External) || !clause.refs.is_empty()
        }) {
            return None;
        }
        if index.declared.iter().any(|fk| {
            &fk.source.table == table && fk.source.columns.iter().any(|name| name == &column.name)
        }) {
            return None;
        }

        let mut entity_tokens = tokens.clone();
        text::trim_purpose_suffix(&mut entity_tokens);
        while entity_tokens.len() > 1
            && matches!(entity_tokens.last().map(String::as_str), Some("ref" | "fk"))
        {
            entity_tokens.pop();
        }

        let mut entity_tables = Vec::new();
        let mut entity_text = String::new();
        let mut resolved = resolve_entity(index, aliases, &entity_tokens);
        if resolved.is_none() {
            // 来源字段也可能带“记录”类后缀，例如 xxx_record；剥离后再查实体词。
            let mut stripped = entity_tokens.clone();
            while stripped.len() > 2 && stripped.last().is_some_and(|token| is_record_suffix(token))
            {
                stripped.pop();
            }
            if stripped != entity_tokens {
                resolved = resolve_entity(index, aliases, &stripped);
            }
        }
        if let Some((tables, text)) = resolved {
            entity_tables = tables;
            entity_text = text;
        }

        let role_word = role_index.map(|at| tokens[at].clone());
        // 层级角色可提出同表候选。“来源/原始”单独出现不能定位到本实体。
        let self_reference = role_index.is_some_and(|at| {
            if !matches!(
                tokens[at].as_str(),
                "parent" | "child" | "ancestor" | "descendant" | "root"
            ) {
                return false;
            }
            let mut rest = tokens[at + 1..].to_vec();
            text::trim_purpose_suffix(&mut rest);
            while rest.len() > 1 && matches!(rest.last().map(String::as_str), Some("ref" | "fk")) {
                rest.pop();
            }
            rest.iter()
                .all(|token| is_relation_tail(token) || token.chars().all(|c| c.is_ascii_digit()))
        });

        let scope_note = parsed.clauses.iter().find_map(|clause| match clause.scope {
            Some(Scope::Conditional) => Some("条件"),
            Some(Scope::Historical) => Some("历史"),
            _ => None,
        });

        Some(Self {
            tokens,
            purposes,
            entity_tables,
            entity_text,
            role_word,
            self_reference,
            scope_note,
        })
    }
}

/// 依次尝试更短的实体后缀，让角色/模块前缀不阻断实体词索引的命中。
fn resolve_entity(
    index: &CatalogIndex<'_>,
    aliases: &ModuleAliases,
    entity_tokens: &[String],
) -> Option<(Vec<TableId>, String)> {
    for start in 0..entity_tokens.len() {
        // 审计/创建人前缀只说明写入时间或操作者，不扩大实体引用候选。
        if entity_tokens[..start]
            .iter()
            .any(|token| is_audit_token(token))
        {
            continue;
        }
        let suffix = &entity_tokens[start..];
        if suffix.is_empty() || (suffix.len() == 1 && is_generic_token(&suffix[0])) {
            continue;
        }
        let mut tables = index.entities.tables_for_tokens(suffix);
        tables.extend(aliases.tables_for(suffix));
        tables.sort();
        tables.dedup();
        if !tables.is_empty() {
            return Some((tables, suffix.join(" + ")));
        }
    }
    None
}

/// 按用途交集、备用键、主键的顺序选择目标表的单列可信键。
fn collect_table_keys(
    index: &CatalogIndex<'_>,
    table: &TableId,
    purposes: &BTreeSet<Purpose>,
    source: &Endpoint,
    reason: &str,
    out: &mut BTreeMap<Endpoint, BTreeSet<String>>,
) {
    let keys: Vec<&KeyInfo> = index
        .keys_for_table(table)
        .iter()
        .filter(|key| key.endpoint.columns.len() == 1)
        .collect();
    let mut chosen = Vec::new();
    if !purposes.is_empty() {
        chosen = keys
            .iter()
            .filter(|key| !key.purposes.is_disjoint(purposes))
            .copied()
            .collect();
    }
    let mut fallback_pk = false;
    if chosen.is_empty() {
        chosen = keys
            .iter()
            .filter(|key| !is_primary(index, table, &key.endpoint))
            .copied()
            .collect();
    }
    if chosen.is_empty() {
        chosen = keys;
        fallback_pk = !chosen.is_empty();
    }
    let suffix = if fallback_pk {
        "（目标表没有备用业务键，回退到可信主键）"
    } else {
        ""
    };
    for key in chosen {
        if key.endpoint == *source {
            continue;
        }
        out.entry(key.endpoint.clone()).or_default().insert(format!(
            "{reason}{suffix}，目标键 {} ({})",
            key.endpoint.table.label(),
            key.endpoint.columns.join(", ")
        ));
    }
}

fn is_primary(index: &CatalogIndex<'_>, table: &TableId, endpoint: &Endpoint) -> bool {
    let Some(info) = index.table(table) else {
        return false;
    };
    let primary: BTreeSet<&str> = info
        .columns
        .iter()
        .filter(|column| column.is_primary_key)
        .map(|column| column.name.as_str())
        .collect();
    !primary.is_empty()
        && primary.len() == endpoint.columns.len()
        && endpoint
            .columns
            .iter()
            .all(|column| primary.contains(column.as_str()))
}

fn is_key_column(index: &CatalogIndex<'_>, table: &TableId, column: &str) -> bool {
    index
        .keys_for_table(table)
        .iter()
        .any(|key| key.endpoint.columns.iter().any(|name| name == column))
}

/// `Some(true)` 兼容，`Some(false)` 确定不兼容，`None` 类型签名不足。
fn type_fit(index: &CatalogIndex<'_>, source: &Endpoint, target: &Endpoint) -> Option<bool> {
    if source.columns.len() != target.columns.len() {
        return Some(false);
    }
    let mut unknown = false;
    for (source_column, target_column) in source.columns.iter().zip(&target.columns) {
        match (
            index.signature(&source.table, source_column),
            index.signature(&target.table, target_column),
        ) {
            (Some(a), Some(b)) => match compatible_types(a, b) {
                Some(false) => return Some(false),
                Some(true) => {}
                None => unknown = true,
            },
            _ => unknown = true,
        }
    }
    (!unknown).then_some(true)
}

struct KeyIndex {
    by_tokens: BTreeMap<Vec<String>, BTreeSet<Endpoint>>,
}

impl KeyIndex {
    fn new(index: &CatalogIndex<'_>) -> Self {
        let mut by_tokens: BTreeMap<Vec<String>, BTreeSet<Endpoint>> = BTreeMap::new();
        for key in index.keys.values() {
            if key.endpoint.columns.len() != 1 {
                continue;
            }
            let tokens = normalized_tokens(&key.endpoint.columns[0]);
            if tokens.len() < 2 || is_pure_generic(&tokens) {
                continue;
            }
            by_tokens
                .entry(tokens)
                .or_default()
                .insert(key.endpoint.clone());
        }
        Self { by_tokens }
    }

    /// 只按最长的可信尾部匹配一次；高频键直接抑制，不做全表扫描。
    fn tail_matches(&self, tokens: &[String]) -> Vec<Endpoint> {
        let normalized: Vec<String> = tokens.iter().map(|token| text::singular(token)).collect();
        for len in (2..=normalized.len()).rev() {
            let at = normalized.len() - len;
            // 与实体后缀一致：审计/创建人前缀不按尾部键名扩散。
            if normalized[..at].iter().any(|token| is_audit_token(token)) {
                continue;
            }
            let suffix = &normalized[at..];
            let Some(endpoints) = self.by_tokens.get(suffix) else {
                continue;
            };
            // 单靠重复键名不能决定实体。如果有竞争，键的实体部分还要能独立对上每个目标表；
            // 不按固定的候选条数截断，也不选排序第一项。
            if endpoints.len() > 1 {
                let mut stem = suffix.to_vec();
                text::trim_purpose_suffix(&mut stem);
                if stem.is_empty()
                    || !endpoints.iter().all(|e| {
                        let name = normalized_tokens(&e.table.name);
                        name.windows(stem.len()).any(|w| w == stem.as_slice())
                    })
                {
                    continue;
                }
            }
            return endpoints.iter().cloned().collect();
        }
        Vec::new()
    }
}

/// 表名的弱变体：去掉同 schema 内高频出现的模块前缀，以及记录/信息类后缀。
struct ModuleAliases {
    map: BTreeMap<Vec<String>, BTreeSet<TableId>>,
}

impl ModuleAliases {
    fn new(index: &CatalogIndex<'_>) -> Self {
        // 在一份目录里，相同路径后实际出现不同实体词才是可尝试省略的命名空间。
        // 逐层学习，且不把分表编号当作独立实体词。
        let mut branches: BTreeMap<(&str, Vec<String>), BTreeSet<String>> = BTreeMap::new();
        for table in index.tables.keys() {
            let words = normalized_tokens(&table.name);
            for cut in 1..words.len() {
                let next = &words[cut];
                if !is_generic_token(next) && !next.chars().all(|c| c.is_ascii_digit()) {
                    branches
                        .entry((&table.schema, words[..cut].to_vec()))
                        .or_default()
                        .insert(next.clone());
                }
            }
        }
        let mut map: BTreeMap<Vec<String>, BTreeSet<TableId>> = BTreeMap::new();
        for table in index.tables.keys() {
            let words = normalized_tokens(&table.name);
            for cut in 1..words.len() {
                if !branches
                    .get(&(table.schema.as_str(), words[..cut].to_vec()))
                    .is_some_and(|b| b.len() > 1)
                {
                    continue;
                }
                let mut tail = &words[cut..];
                loop {
                    if !is_pure_generic(tail) {
                        map.entry(tail.to_vec()).or_default().insert(table.clone());
                    }
                    if tail.len() < 2 || !is_record_suffix(tail.last().unwrap()) {
                        break;
                    }
                    tail = &tail[..tail.len() - 1];
                }
            }
        }
        Self { map }
    }

    fn tables_for(&self, tokens: &[String]) -> Vec<TableId> {
        let normalized: Vec<String> = tokens.iter().map(|token| text::singular(token)).collect();
        self.map
            .get(&normalized)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect()
    }
}

fn normalized_tokens(name: &str) -> Vec<String> {
    text::tokens_of(name)
        .into_iter()
        .map(|token| text::singular(&token))
        .collect()
}

fn weak_role_prefix(tokens: &[String]) -> Option<usize> {
    let first = tokens.first()?;
    STRUCTURAL_PREFIXES
        .iter()
        .any(|prefix| prefix == first)
        .then_some(0)
}

fn is_record_suffix(token: &str) -> bool {
    matches!(
        token,
        "record"
            | "records"
            | "info"
            | "information"
            | "data"
            | "detail"
            | "details"
            | "list"
            | "entity"
            | "master"
            | "table"
            | "tables"
            | "row"
            | "rows"
    )
}

/// 父子/来源前缀之后只剩“记录/行/引用”时，才允许回退到本表键；
/// `status`、`type`、`name` 等状态/描述词没有引用含义，不能触发同表召回。
fn is_relation_tail(token: &str) -> bool {
    matches!(
        token,
        "record" | "records" | "row" | "rows" | "ref" | "fk" | "key"
    )
}

fn is_pure_generic(tokens: &[String]) -> bool {
    !tokens.is_empty()
        && tokens
            .iter()
            .all(|token| is_generic_token(token) || token.chars().all(|c| c.is_ascii_digit()))
}

fn is_audit_token(token: &str) -> bool {
    matches!(
        token,
        "created"
            | "create"
            | "inserted"
            | "insert"
            | "updated"
            | "update"
            | "deleted"
            | "delete"
            | "creator"
            | "updater"
            | "modifier"
            | "audit"
            | "by"
    )
}

fn is_generic_token(token: &str) -> bool {
    const GENERIC: &[&str] = &[
        "id",
        "code",
        "no",
        "num",
        "number",
        "name",
        "title",
        "status",
        "state",
        "type",
        "kind",
        "remark",
        "remarks",
        "description",
        "desc",
        "note",
        "memo",
        "content",
        "version",
        "created",
        "create",
        "updated",
        "update",
        "deleted",
        "delete",
        "creator",
        "updater",
        "modifier",
        "audit",
        "at",
        "time",
        "date",
        "timestamp",
        "record",
        "records",
        "row",
        "rows",
        "data",
        "info",
        "information",
        "detail",
        "details",
        "list",
        "entity",
        "master",
        "table",
        "tables",
        "ref",
        "fk",
        "key",
        "value",
        "by",
        "to",
        "from",
    ];
    GENERIC.contains(&token)
}
