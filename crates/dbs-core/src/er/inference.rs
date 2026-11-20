//! 首版 ER 关系推断纯内核。
//!
//! 流程固定为：只从原始目录建立索引与角色锚点 → 生成候选并集 → 按来源字段组
//! 依次做结构资格、陈述适用性、业务路径和竞争裁决。预测边不参与锚点或候选生成。

mod catalog;
pub(super) mod text;
mod weak;

pub(super) use catalog::compatible_types;

use super::{
    Analysis, Candidate, Endpoint, ErCatalog, KeyCondition, Relation, RelationKind, TableId,
};
use catalog::{purposes_in_name, purposes_in_text, CatalogIndex, Purpose, ResolvedRef, RoleKey};
use std::collections::{BTreeMap, BTreeSet};

const DIRECT: u8 = 3;
const BUSINESS: u8 = 2;
const MIGRATION: u8 = 2;
const NAMING: u8 = 1;
const VIEW: u8 = 1;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Naming,
    View,
    ViewMapping,
    Migration,
    Business,
    Direct,
}

impl EvidenceKind {
    fn strength(self) -> u8 {
        match self {
            EvidenceKind::Direct => DIRECT,
            EvidenceKind::Business => BUSINESS,
            EvidenceKind::Migration => MIGRATION,
            EvidenceKind::Naming => NAMING,
            EvidenceKind::View => VIEW,
            EvidenceKind::ViewMapping => BUSINESS,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            EvidenceKind::Direct => "明确引用",
            EvidenceKind::Business => "业务描述",
            EvidenceKind::Migration => "角色迁移",
            EvidenceKind::Naming => "常规命名",
            EvidenceKind::View => "视图佐证",
            EvidenceKind::ViewMapping => "视图字段映射",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    Historical,
    Conditional,
    External,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Scope::Historical => "历史",
            Scope::Conditional => "条件",
            Scope::External => "外部归属",
        }
    }
}

#[derive(Debug, Clone)]
struct Evidence {
    origin: String,
    kind: EvidenceKind,
    complete: bool,
    scope: Option<Scope>,
    role: RoleKey,
    detail: String,
}

impl Evidence {
    fn new(
        kind: EvidenceKind,
        complete: bool,
        scope: Option<Scope>,
        role: &RoleKey,
        origin: &str,
        detail: String,
    ) -> Self {
        Self {
            kind,
            complete,
            scope,
            role: role.clone(),
            origin: origin.into(),
            detail: format!("{detail}（依据 {origin}）"),
        }
    }
    fn strength(&self) -> u8 {
        self.kind.strength()
    }
}

#[derive(Debug, Clone)]
struct Combo {
    source: Endpoint,
    target: Endpoint,
    evidence: BTreeMap<String, Evidence>,
    negatives: BTreeMap<String, Evidence>,
    gaps: BTreeSet<String>,
}

impl Combo {
    fn new(source: Endpoint, target: Endpoint) -> Self {
        Self {
            source,
            target,
            evidence: BTreeMap::new(),
            negatives: BTreeMap::new(),
            gaps: BTreeSet::new(),
        }
    }

    fn insert_positive(&mut self, evidence: Evidence) {
        let replace = match self.evidence.get(&evidence.origin) {
            None => true,
            Some(old) => {
                (evidence.scope.is_none() && old.scope.is_some())
                    || evidence.strength() > old.strength()
                    || (evidence.complete && !old.complete && evidence.strength() == old.strength())
            }
        };
        if replace {
            self.evidence.insert(evidence.origin.clone(), evidence);
        }
    }

    fn insert_negative(&mut self, evidence: Evidence) {
        self.negatives
            .entry(evidence.origin.clone())
            .or_insert(evidence);
    }

    fn max_positive_strength(&self) -> Option<u8> {
        self.evidence.values().map(Evidence::strength).max()
    }

    /// 历史陈述只说明过去，不能否决当前材料；条件或外部范围的竞争仍应保留。
    fn blocking_strength(&self) -> Option<u8> {
        self.evidence
            .values()
            .filter(|e| e.scope != Some(Scope::Historical))
            .map(Evidence::strength)
            .max()
    }

    fn max_complete_unscoped_strength(&self) -> Option<u8> {
        // 路径缺口只限制该条证据；另一条自足的路径仍可支持同一端点。
        if !self.negatives.is_empty() {
            return None;
        }
        self.evidence
            .values()
            .filter(|e| e.complete && e.scope.is_none())
            .map(Evidence::strength)
            .max()
    }

    fn strongest_complete_kind(&self) -> Option<EvidenceKind> {
        if !self.negatives.is_empty() {
            return None;
        }
        self.evidence
            .values()
            .filter(|e| e.complete && e.scope.is_none())
            .max_by_key(|e| e.strength())
            .map(|e| e.kind)
    }

    fn has_negative(&self) -> bool {
        !self.negatives.is_empty()
    }

    fn effective_role(&self) -> RoleKey {
        let mut best: Option<&Evidence> = None;
        for evidence in self.evidence.values() {
            let replace = match best {
                None => true,
                Some(old) => {
                    evidence.strength() > old.strength()
                        || (evidence.strength() == old.strength()
                            && old.role.is_unknown()
                            && !evidence.role.is_unknown())
                }
            };
            if replace {
                best = Some(evidence);
            }
        }
        best.map(|e| e.role.clone()).unwrap_or(RoleKey::Unknown)
    }

    fn strongest_kind(&self) -> EvidenceKind {
        self.evidence
            .values()
            .map(|e| e.kind)
            .max_by_key(|kind| kind.strength())
            .unwrap_or(EvidenceKind::View)
    }
}

#[derive(Debug, Default)]
struct ComboMap {
    map: BTreeMap<(Endpoint, Endpoint), Combo>,
}

impl ComboMap {
    fn entry(&mut self, source: Endpoint, target: Endpoint) -> &mut Combo {
        self.map
            .entry((source.clone(), target.clone()))
            .or_insert_with(|| Combo::new(source, target))
    }
}

#[derive(Debug, Clone)]
struct ParsedClause {
    text: String,
    negation: bool,
    scope: Option<Scope>,
    reference: bool,
    refs: Vec<ResolvedRef>,
    mentions: Vec<TableId>,
    purposes: BTreeSet<Purpose>,
    role: RoleKey,
}

#[derive(Debug, Clone)]
struct ParsedComment {
    role: RoleKey,
    domain: Option<String>,
    purposes: BTreeSet<Purpose>,
    clauses: Vec<ParsedClause>,
}

#[derive(Debug, Clone)]
struct RoleAnchor {
    domain: Option<String>,
    role: RoleKey,
    source: Endpoint,
    target: Endpoint,
    origin: String,
    detail: String,
}

#[derive(Debug, Clone)]
struct Prepared {
    source: Endpoint,
    gaps: Vec<String>,
    complete: bool,
}

/// 从目录推断关系。纯函数：相同目录输入得到相同输出。
pub fn infer_relations(catalog: &ErCatalog) -> Analysis {
    let index = CatalogIndex::new(catalog);
    // 解析只依赖当前目录和原文。缓存解析结果，不缓存某字段的证据身份或最终裁决。
    let mut comments = BTreeMap::new();
    for field in catalog.tables.iter().flat_map(|table| &table.columns) {
        comments
            .entry(field.comment.as_str())
            .or_insert_with(|| parse_comment(&index, &field.comment));
    }
    let declared = declared_relations(&index);
    let mut declared_by_source: BTreeMap<Endpoint, BTreeSet<Endpoint>> = BTreeMap::new();
    for fk in &index.declared {
        declared_by_source
            .entry(fk.source.clone())
            .or_default()
            .insert(fk.target.clone());
    }
    // 复合主键成员的同名键只在整组都具备独立唯一命名对应时放行；
    // 放行集合只影响常规命名，不参与其他证据路径。
    let relaxed_naming = relaxed_same_name_naming(&index, &comments);

    // 所有锚点来自原始目录，在推断其他字段之前冻结。
    let mut anchors = build_role_anchors(&index, &comments);
    let view_mappings = directed_view_mappings(&index, &catalog.views);
    let mut combos = ComboMap::default();
    for mapping in &view_mappings {
        combos
            .entry(mapping.source.clone(), mapping.target.clone())
            .insert_positive(Evidence {
                origin: mapping.origin.clone(),
                kind: EvidenceKind::ViewMapping,
                complete: true,
                scope: None,
                role: mapping.role.clone(),
                detail: mapping.detail.clone(),
            });
    }
    // 条件视图映射只服务当前端点；只有指向全表键的映射才能成为其他行的迁移锚点。
    anchors.extend(
        view_mappings
            .into_iter()
            .filter(|mapping| index.is_trusted_key(&mapping.target)),
    );
    let table_ids: Vec<TableId> = index.tables.keys().cloned().collect();
    for table_id in table_ids {
        let Some(table) = index.table(&table_id) else {
            continue;
        };
        for column in &table.columns {
            let parsed = &comments[column.comment.as_str()];
            generate_comment_paths(&index, &mut combos, &table_id, column, parsed);
            generate_naming(
                &index,
                &mut combos,
                &table_id,
                column,
                parsed,
                &relaxed_naming,
            );
            generate_migration(&index, &mut combos, &table_id, column, parsed, &anchors);
        }
    }
    attach_view_evidence(&index, &mut combos);

    let mut analysis = adjudicate(&index, combos, &declared_by_source);
    analysis.relations.splice(0..0, declared);
    merge_document_links(&mut analysis, &catalog.document_links);
    super::conditional::append(catalog, &mut analysis);
    // 弱证据只作为裁决之后的独立候选层，不参与关系晋级，也不覆盖已有候选。
    weak::append(&index, &comments, &mut analysis);
    analysis.relations.sort_by(|a, b| {
        (&a.source, &a.target, kind_rank(a.kind), &a.reason).cmp(&(
            &b.source,
            &b.target,
            kind_rank(b.kind),
            &b.reason,
        ))
    });
    analysis.candidates.sort_by(|a, b| {
        (&a.source, &a.target, a.condition.is_none(), &a.reason).cmp(&(
            &b.source,
            &b.target,
            b.condition.is_none(),
            &b.reason,
        ))
    });
    let mut seen_relations = BTreeSet::new();
    analysis
        .relations
        .retain(|r| seen_relations.insert((r.source.clone(), r.target.clone())));
    let relation_keys: BTreeSet<_> = analysis
        .relations
        .iter()
        .map(|r| (r.source.clone(), r.target.clone()))
        .collect();
    let mut seen_candidates = BTreeSet::new();
    analysis.candidates.retain(|c| {
        !relation_keys.contains(&(c.source.clone(), c.target.clone()))
            && seen_candidates.insert((c.source.clone(), c.target.clone()))
    });
    analysis
}

fn kind_rank(kind: RelationKind) -> u8 {
    match kind {
        RelationKind::Declared => 0,
        RelationKind::Lookup => 1,
        RelationKind::Confirmed => 2,
        RelationKind::Inferred => 3,
    }
}

fn merge_document_links(analysis: &mut Analysis, observed: &Analysis) {
    if observed.relations.is_empty() && observed.candidates.is_empty() {
        return;
    }
    let mut lookups = BTreeMap::<&Endpoint, BTreeMap<&Endpoint, BTreeSet<&str>>>::new();
    for r in &observed.relations {
        lookups
            .entry(&r.source)
            .or_default()
            .entry(&r.target)
            .or_default()
            .insert(&r.reason);
    }
    let mut pending = BTreeMap::<&Endpoint, BTreeSet<&Endpoint>>::new();
    for c in &observed.candidates {
        pending.entry(&c.source).or_default().insert(&c.target);
    }
    let mut withheld = observed.candidates.clone();
    analysis.relations.retain(|r| {
        if r.kind != RelationKind::Inferred {
            return true;
        }
        if lookups
            .get(&r.source)
            .is_some_and(|targets| targets.contains_key(&r.target))
        {
            return false; // 用有出处的查询关系替换同端点的推测。
        }
        if lookups.contains_key(&r.source)
            || pending
                .get(&r.source)
                .is_some_and(|targets| targets.iter().any(|t| **t != r.target))
        {
            withheld.push(Candidate {
                condition: None,
                source: r.source.clone(),
                target: r.target.clone(),
                reason: format!("集合目录还包含其他目标或受限查询，需复核。{}", r.reason),
            });
            return false;
        }
        true
    });
    analysis.candidates.extend(withheld);
    for (source, targets) in lookups {
        for (target, reasons) in targets {
            analysis.relations.push(Relation {
                source: source.clone(),
                target: target.clone(),
                kind: RelationKind::Lookup,
                condition: None,
                reason: reasons.into_iter().collect::<Vec<_>>().join("；"),
            });
        }
    }
}

fn declared_relations(index: &CatalogIndex<'_>) -> Vec<Relation> {
    index
        .declared
        .iter()
        .map(|fk| Relation {
            source: fk.source.clone(),
            target: fk.target.clone(),
            kind: RelationKind::Declared,
            condition: None,
            reason: format!(
                "数据库声明外键「{}」：{} ({}) → {} ({}), 两端列序按声明原样保留",
                fk.name,
                fk.source.table.label(),
                fk.source.columns.join(", "),
                fk.target.table.label(),
                fk.target.columns.join(", ")
            ),
        })
        .collect()
}

fn parse_comment(index: &CatalogIndex<'_>, text: &str) -> ParsedComment {
    let mut clauses = Vec::new();
    let mut role = RoleKey::Unknown;
    let mut domain = text::extract_domain(text);
    let mut purposes = BTreeSet::new();
    for clause in text::split_clauses(text) {
        let tokens = text::tokenize(clause);
        let raw_refs = text::parse_explicit_refs(clause);
        let spans: Vec<(usize, usize)> = raw_refs.iter().map(|r| (r.start, r.end)).collect();
        let refs: Vec<ResolvedRef> = raw_refs.iter().map(|r| index.resolve_raw_ref(r)).collect();
        let description = text::tokens_outside_spans(&tokens, &spans);
        let mut mentions: Vec<TableId> = Vec::new();
        for mention in index.entities.mentions(&tokens) {
            if mention.start >= mention.end || mention.end > tokens.len() {
                continue;
            }
            let span = (tokens[mention.start].start, tokens[mention.end - 1].end);
            if spans
                .iter()
                .any(|&explicit| text::spans_overlap(span, explicit))
            {
                continue;
            }
            if !mentions.contains(&mention.table) {
                mentions.push(mention.table);
            }
        }
        mentions.sort();
        let clause_purposes = purposes_in_text(&description);
        let reference = text::has_reference_intent(&description);
        let negation = text::is_negated(&description);
        let scope = if text::is_historical(&description) {
            Some(Scope::Historical)
        } else if text::is_conditional(&description) {
            Some(Scope::Conditional)
        } else if text::is_external(&description) {
            Some(Scope::External)
        } else {
            None
        };
        let role_tokens = text::role_tokens(clause, &spans);
        let clause_role = index.entities.canonical_role(&role_tokens);
        if role.is_unknown() && !clause_role.is_unknown() {
            role = clause_role.clone();
        }
        if domain.is_none() {
            domain = text::extract_domain(clause);
        }
        purposes.extend(clause_purposes.iter().cloned());
        clauses.push(ParsedClause {
            text: clause.to_string(),
            negation,
            scope,
            reference,
            refs,
            mentions,
            purposes: clause_purposes,
            role: clause_role,
        });
    }
    ParsedComment {
        role,
        domain,
        purposes,
        clauses,
    }
}

fn generate_comment_paths(
    index: &CatalogIndex<'_>,
    combos: &mut ComboMap,
    table: &TableId,
    column: &crate::ColumnInfo,
    parsed: &ParsedComment,
) {
    let name_role = role_from_name(index, &column.name);
    let fallback_role = if parsed.role.is_unknown() {
        name_role
    } else {
        parsed.role.clone()
    };
    let location = column_location(table, &column.name);
    let source_is_key_column = index
        .all_keys_for_table(table)
        .iter()
        .any(|key| key.endpoint.columns.iter().any(|c| c == &column.name));
    for clause in &parsed.clauses {
        let role = if clause.role.is_unknown() {
            fallback_role.clone()
        } else {
            clause.role.clone()
        };
        if clause.negation {
            if clause.scope.is_none() {
                let mut targets = Vec::new();
                for resolved in &clause.refs {
                    if let ResolvedRef::Key(target) = resolved {
                        targets.push(target.clone());
                    }
                }
                if targets.is_empty() && !clause.mentions.is_empty() {
                    for mentioned in &clause.mentions {
                        if clause.purposes.is_empty() && clause.reference {
                            targets.extend(
                                index
                                    .all_keys_for_table(mentioned)
                                    .iter()
                                    .map(|k| k.endpoint.clone()),
                            );
                        } else {
                            targets.extend(index.keys_by_purpose(mentioned, &clause.purposes));
                        }
                    }
                }
                targets.sort();
                targets.dedup();
                for target in targets {
                    let detail = format!(
                        "{location} 的注释「{}」明确否定指向 {} ({})",
                        clause.text,
                        target.table.label(),
                        target.columns.join(", ")
                    );
                    add_negative(
                        index,
                        combos,
                        table,
                        &column.name,
                        &target,
                        Evidence::new(
                            EvidenceKind::Direct,
                            true,
                            None,
                            &role,
                            &comment_origin(table, &column.name),
                            detail,
                        ),
                    );
                }
            }
            continue;
        }

        let has_explicit = !clause.refs.is_empty();
        if has_explicit && clause.reference {
            for resolved in &clause.refs {
                match resolved {
                    ResolvedRef::Key(target) => {
                        let detail = format!(
                            "{location} 的注释「{}」明确引用 {} ({}){}",
                            clause.text,
                            target.table.label(),
                            target.columns.join(", "),
                            scope_suffix(clause.scope)
                        );
                        add_positive(
                            index,
                            combos,
                            table,
                            &column.name,
                            target,
                            Evidence::new(
                                EvidenceKind::Direct,
                                true,
                                clause.scope,
                                &role,
                                &comment_origin(table, &column.name),
                                detail,
                            ),
                            Vec::new(),
                        );
                    }
                    ResolvedRef::Incomplete { target, gap } => {
                        let detail = format!(
                            "{location} 的注释「{}」指向 {} ({})，但{}",
                            clause.text,
                            target.table.label(),
                            target.columns.join(", "),
                            gap
                        );
                        add_positive(
                            index,
                            combos,
                            table,
                            &column.name,
                            target,
                            Evidence::new(
                                EvidenceKind::Direct,
                                false,
                                clause.scope,
                                &role,
                                &comment_origin(table, &column.name),
                                detail,
                            ),
                            vec![gap.clone()],
                        );
                    }
                    ResolvedRef::External {
                        table: target_table,
                        columns,
                    } => {
                        let target = Endpoint {
                            table: target_table.clone(),
                            columns: columns.clone(),
                        };
                        let gap =
                            format!("目标表 {} 不在当前目录（外部引用）", target.table.label());
                        let detail = format!(
                            "{location} 的注释「{}」引用目录外的 {} ({})",
                            clause.text,
                            target.table.label(),
                            target.columns.join(", ")
                        );
                        add_untrusted_candidate(
                            combos,
                            table,
                            &column.name,
                            &target,
                            Evidence::new(
                                EvidenceKind::Direct,
                                false,
                                None,
                                &role,
                                &comment_origin(table, &column.name),
                                detail,
                            ),
                            gap,
                        );
                    }
                    ResolvedRef::Ambiguous { targets, gap } => {
                        for target in targets {
                            let detail = format!(
                                "{location} 的注释「{}」指向 {} ({})，但{}",
                                clause.text,
                                target.table.label(),
                                target.columns.join(", "),
                                gap
                            );
                            add_positive(
                                index,
                                combos,
                                table,
                                &column.name,
                                target,
                                Evidence::new(
                                    EvidenceKind::Direct,
                                    false,
                                    clause.scope,
                                    &role,
                                    &comment_origin(table, &column.name),
                                    detail,
                                ),
                                vec![gap.clone()],
                            );
                        }
                    }
                    ResolvedRef::Invalid {
                        table: target_table,
                        columns,
                        gap,
                    } => {
                        let target = Endpoint {
                            table: target_table.clone(),
                            columns: columns.clone(),
                        };
                        let detail = format!(
                            "{location} 的注释「{}」指向 {}，但{}",
                            clause.text,
                            target.table.label(),
                            gap
                        );
                        add_untrusted_candidate(
                            combos,
                            table,
                            &column.name,
                            &target,
                            Evidence::new(
                                EvidenceKind::Direct,
                                false,
                                None,
                                &role,
                                &comment_origin(table, &column.name),
                                detail,
                            ),
                            gap.clone(),
                        );
                    }
                    ResolvedRef::NonKey => {}
                }
            }
            continue;
        }

        if clause.mentions.is_empty() {
            continue;
        }
        // 键列上的“客户编号”之类通常是在说明本表键自身，而不是指向同名表；
        // 没有引用谓词时不在键列上展开业务引用。
        if !clause.reference && source_is_key_column {
            continue;
        }
        if !clause.purposes.is_empty() {
            for mentioned in &clause.mentions {
                for target in index.keys_by_purpose(mentioned, &clause.purposes) {
                    let purpose_labels = clause
                        .purposes
                        .iter()
                        .map(Purpose::label)
                        .collect::<Vec<_>>()
                        .join("/");
                    let detail = format!(
                        "{location} 的注释「{}」表明业务对象 {} 的{}键，目标完整键为 {} ({}){}",
                        clause.text,
                        mentioned.label(),
                        purpose_labels,
                        target.table.label(),
                        target.columns.join(", "),
                        scope_suffix(clause.scope)
                    );
                    add_positive(
                        index,
                        combos,
                        table,
                        &column.name,
                        &target,
                        Evidence::new(
                            EvidenceKind::Business,
                            clause.reference,
                            clause.scope,
                            &role,
                            &comment_origin(table, &column.name),
                            detail,
                        ),
                        Vec::new(),
                    );
                }
            }
        } else if clause.reference {
            for mentioned in &clause.mentions {
                for key in index.all_keys_for_table(mentioned) {
                    let target = &key.endpoint;
                    let detail = format!(
                        "{location} 的注释「{}」提到业务对象 {}，但未说明键用途，保留完整键 {} ({}) 待判断",
                        clause.text,
                        mentioned.label(),
                        target.table.label(),
                        target.columns.join(", ")
                    );
                    add_positive(
                        index,
                        combos,
                        table,
                        &column.name,
                        target,
                        Evidence::new(
                            EvidenceKind::Business,
                            false,
                            clause.scope,
                            &role,
                            &comment_origin(table, &column.name),
                            detail,
                        ),
                        vec!["实体明确但键用途未明，保留仍合理的完整键".into()],
                    );
                }
            }
        }
    }
}

impl ParsedComment {
    /// 字段自身的范围限定约束由命名或角色迁移产生的弱指向；独立的明确陈述仍按分句裁决。
    fn scope_for(&self, target: &TableId) -> Option<Scope> {
        self.clauses.iter().find_map(|c| {
            if c.refs.is_empty() && (c.mentions.is_empty() || c.mentions.contains(target)) {
                c.scope
            } else {
                None
            }
        })
    }
}

/// 普通命名路径的目标解析；不在这里应用“同名键列”抑制，让结构门禁与最终裁决共用同一份解析。
struct NamingCandidates {
    tokens: Vec<String>,
    purposes: BTreeSet<Purpose>,
    role: RoleKey,
    targets: Vec<Endpoint>,
}

fn naming_candidates(
    index: &CatalogIndex<'_>,
    column: &crate::ColumnInfo,
) -> Option<NamingCandidates> {
    let field_name = if index.document_paths {
        if column.name.contains("[]") {
            return None;
        }
        column.name.rsplit('.').next().unwrap_or(&column.name)
    } else {
        column.name.as_str()
    };
    let tokens = text::tokens_of(field_name);
    if generic_column_name(&tokens, field_name) || tokens.len() < 2 {
        return None;
    }
    let purposes = purposes_in_name(field_name);
    if purposes.is_empty() {
        return None;
    }
    let mut entity_tokens = tokens.clone();
    text::trim_purpose_suffix(&mut entity_tokens);
    while entity_tokens.len() > 1
        && matches!(entity_tokens[0].as_str(), "fk" | "ref" | "src" | "source")
    {
        entity_tokens.remove(0);
    }
    if entity_tokens.is_empty() {
        return None;
    }
    let target_tables = index.entities.tables_for_tokens(&entity_tokens);
    if target_tables.is_empty() {
        return None;
    }
    let role = index.entities.canonical_role(&entity_tokens);
    let mut targets = Vec::new();
    for target_table in target_tables {
        for key in index.all_keys_for_table(&target_table) {
            if key.endpoint.columns.len() != 1 {
                continue;
            }
            if !key.purposes.iter().any(|p| purposes.contains(p)) {
                continue;
            }
            targets.push(key.endpoint.clone());
        }
    }
    if targets.is_empty() {
        return None;
    }
    Some(NamingCandidates {
        tokens,
        purposes,
        role,
        targets,
    })
}

/// 通过整组结构门禁的成员：目标键，以及说明整组成员分别落到不同实体的理由。
struct RelaxedNaming {
    target: Endpoint,
    group: String,
}

/// 逐一验证同一完整可信复合主键的每个成员是否都能独立、唯一地落到同 schema 内的不同实体。
/// 任一成员缺对应、自身唯一、跨 schema、重复目标、类型不足、注释有反证，或已有外键/视图
/// 指向别的目标时，整组返回 `None`。
fn relaxed_same_name_naming(
    index: &CatalogIndex<'_>,
    comments: &BTreeMap<&str, ParsedComment>,
) -> BTreeMap<(TableId, String), RelaxedNaming> {
    let mut relaxed = BTreeMap::new();
    // 文档型目录没有真正的复合主键；部分唯一索引不能替代主键参与结构放行。
    if index.document_paths {
        return relaxed;
    }
    let table_ids: Vec<TableId> = index.tables.keys().cloned().collect();
    for table_id in table_ids {
        for key in index.composite_primary_keys(&table_id) {
            let Some(members) = independently_named_composite_members(
                index,
                comments,
                &table_id,
                &key.endpoint.columns,
            ) else {
                continue;
            };
            let group = relaxed_group_detail(&key.endpoint.columns, &members);
            for (column, target) in members {
                relaxed.insert(
                    (table_id.clone(), column),
                    RelaxedNaming {
                        target,
                        group: group.clone(),
                    },
                );
            }
        }
    }
    relaxed
}

fn independently_named_composite_members(
    index: &CatalogIndex<'_>,
    comments: &BTreeMap<&str, ParsedComment>,
    table: &TableId,
    members: &[String],
) -> Option<Vec<(String, Endpoint)>> {
    let source = index.table(table)?;
    let mut entities = BTreeSet::new();
    let mut out = Vec::new();
    for member in members {
        // 成员自身单列唯一时不属于“仅参与复合主键”的情形，维持原有保守抑制。
        if index.is_single_column_key(table, member) {
            return None;
        }
        let column = source
            .columns
            .iter()
            .find(|column| &column.name == member)?;
        // 否定、范围限定或明确引用分别属于语义反证与独立证据路径，不靠结构放行。
        let parsed = comments.get(column.comment.as_str())?;
        if parsed
            .clauses
            .iter()
            .any(|clause| clause.negation || clause.scope.is_some() || !clause.refs.is_empty())
        {
            return None;
        }
        let candidates = naming_candidates(index, column)?;
        // 既有强命名必须唯一落到一个可信单列键；多目标视为实质竞争。
        if candidates.targets.len() != 1 {
            return None;
        }
        let target = candidates.targets.into_iter().next()?;
        // 结构放行也不能把“非空等值”键抬成全表唯一目标。
        if !index.is_trusted_key(&target) {
            return None;
        }
        // 同一成员已被外键或视图指向别的目标时，整组结构证据被反驳；兄弟成员不能作证。
        if member_points_elsewhere(index, table, member, &target) {
            return None;
        }
        // 目标必须与来源自身不同、同 schema，且各成员落到不同实体。
        if target.table == *table
            || target.table.schema != source.schema
            || !entities.insert(target.table.clone())
        {
            return None;
        }
        let target_column = target.columns.first()?;
        let (Some(source_type), Some(target_type)) = (
            index.signature(table, member),
            index.signature(&target.table, target_column),
        ) else {
            return None;
        };
        if compatible_types(source_type, target_type) != Some(true) {
            return None;
        }
        out.push((member.clone(), target));
    }
    Some(out)
}

/// 该成员是否已被目录里的显式外键或视图等值明确指向预期之外的目标。
fn member_points_elsewhere(
    index: &CatalogIndex<'_>,
    table: &TableId,
    member: &str,
    expected: &Endpoint,
) -> bool {
    let declared = index.declared.iter().any(|fk| {
        &fk.source.table == table
            && fk.source.columns.iter().any(|column| column == member)
            && &fk.target != expected
    });
    declared || index.view_compares_elsewhere(table, member, expected)
}

/// 解释这是整组结构证据：成员分别匹配不同实体的唯一键，而不是两次相互独立的命名。
fn relaxed_group_detail(members: &[String], targets: &[(String, Endpoint)]) -> String {
    let pairs = targets
        .iter()
        .map(|(column, target)| {
            format!(
                "{column} → {} ({})",
                target.table.label(),
                target.columns.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("、");
    format!(
        "同组复合主键（{}）的成员分别匹配不同实体的唯一键：{}；这些命名来自同一组结构，不互为独立证据",
        members.join(", "),
        pairs
    )
}

fn generate_naming(
    index: &CatalogIndex<'_>,
    combos: &mut ComboMap,
    table: &TableId,
    column: &crate::ColumnInfo,
    parsed: &ParsedComment,
    relaxed: &BTreeMap<(TableId, String), RelaxedNaming>,
) {
    let Some(candidates) = naming_candidates(index, column) else {
        return;
    };
    let source_is_key_column = index
        .all_keys_for_table(table)
        .iter()
        .any(|key| key.endpoint.columns.iter().any(|c| c == &column.name));
    let source_is_individually_unique = index.is_single_column_key(table, &column.name);
    let purpose_labels = candidates
        .purposes
        .iter()
        .map(Purpose::label)
        .collect::<Vec<_>>()
        .join("/");
    for target in candidates.targets {
        let same_name = target
            .columns
            .first()
            .is_some_and(|name| name.eq_ignore_ascii_case(&column.name));
        let structurally_relaxed = relaxed.get(&(table.clone(), column.name.clone()));
        let bypasses_suppression = same_name
            && source_is_key_column
            && !source_is_individually_unique
            && structurally_relaxed.is_some_and(|relaxed| relaxed.target == target);
        // 单列自身唯一，或仅参与复合键但整组未通过结构门禁时，继续抑制同名键列命名。
        if same_name
            && (source_is_individually_unique || (source_is_key_column && !bypasses_suppression))
        {
            continue;
        }
        let base = format!(
            "字段名「{}」按词边界拆分为 {}，与 {} 的{}用途完整命名匹配，目标键 {} ({})",
            column.name,
            candidates.tokens.join(" + "),
            target.table.label(),
            purpose_labels,
            target.table.label(),
            target.columns.join(", ")
        );
        // 只有本次确实绕过同名键抑制时才追加整组解释；普通命名理由保持原样。
        let detail = match structurally_relaxed.filter(|_| bypasses_suppression) {
            Some(relaxed) => format!("{base}；{}", relaxed.group),
            None => base,
        };
        add_positive(
            index,
            combos,
            table,
            &column.name,
            &target,
            Evidence::new(
                EvidenceKind::Naming,
                true,
                parsed.scope_for(&target.table),
                &candidates.role,
                &name_origin(table, &column.name),
                detail,
            ),
            Vec::new(),
        );
    }
}

fn generate_migration(
    index: &CatalogIndex<'_>,
    combos: &mut ComboMap,
    table: &TableId,
    column: &crate::ColumnInfo,
    parsed: &ParsedComment,
    anchors: &[RoleAnchor],
) {
    let name_role = role_from_name(index, &column.name);
    let role = if parsed.role.is_unknown() {
        name_role
    } else {
        parsed.role.clone()
    };
    if role.is_unknown() {
        return;
    }
    let domain = index.domain_for(table).or(parsed.domain.clone());
    let mut purposes = parsed.purposes.clone();
    purposes.extend(purposes_in_name(&column.name));
    let source = Endpoint {
        table: table.clone(),
        columns: vec![column.name.clone()],
    };
    for anchor in anchors {
        if anchor.role != role || anchor.source == source || anchor.target == source {
            continue;
        }
        let Some(anchor_key) = index.keys.get(&anchor.target) else {
            continue;
        };
        let domain_ok = match (&domain, &anchor.domain) {
            (Some(a), Some(b)) if a == b => true,
            (Some(_), Some(_)) => continue,
            _ => false,
        };
        let purpose_ok =
            !purposes.is_empty() && anchor_key.purposes.iter().any(|p| purposes.contains(p));
        if !purposes.is_empty() && !purpose_ok {
            continue;
        }
        let mut gaps = Vec::new();
        if !independent_role_source(index, table, &anchor.source.table) {
            gaps.push("角色说明可能来自同一对象、分表或复制模板，不能独立佐证".into());
        }
        if !domain_ok {
            gaps.push("来源字段与锚点的业务域未同时明确，角色迁移不能自动确认".into());
        }
        if purposes.is_empty() {
            gaps.push("来源字段的键用途未明，角色迁移不足".into());
        }
        let domain_text = domain.as_deref().unwrap_or("未声明");
        let detail = format!(
            "角色「{}」在业务域「{}」内由锚点 {}（{}）→ {} ({}) 独立记录（{}）；当前列 {} 独立表达同一角色{}",
            role.display(),
            domain_text,
            anchor.source.table.label(),
            anchor.source.columns.join(", "),
            anchor.target.table.label(),
            anchor.target.columns.join(", "),
            anchor.detail,
            column_location(table, &column.name),
            scope_suffix(None)
        );
        add_positive(
            index,
            combos,
            table,
            &column.name,
            &anchor.target,
            Evidence::new(
                EvidenceKind::Migration,
                domain_ok && purpose_ok,
                parsed.scope_for(&anchor.target.table),
                &role,
                &format!("role:{}", anchor.origin),
                detail,
            ),
            gaps,
        );
    }
}

/// 将可追溯的视图等值与来源列自身的标识用途合并。只有恰一侧是完整键时才解释方向。
/// 可空键产生的映射只作为该端点的本地证据，调用方须在扩展迁移锚点时再次筛除。
fn directed_view_mappings(
    index: &CatalogIndex<'_>,
    comparisons: &[super::ViewComparison],
) -> Vec<RoleAnchor> {
    let mut mapped = Vec::new();
    for comparison in comparisons {
        let left_is_key = index.is_known_key(&comparison.left);
        let right_is_key = index.is_known_key(&comparison.right);
        let (source, target) = match (left_is_key, right_is_key) {
            (true, false) => (&comparison.right, &comparison.left),
            (false, true) => (&comparison.left, &comparison.right),
            // 两端都是键时视图不能选方向；两端都不是键时没有可定位的完整目标。
            _ => continue,
        };
        if source.columns.is_empty() || source.columns.len() != target.columns.len() {
            continue;
        }
        let Some(key) = index.key_info(target) else {
            continue;
        };
        let conditional = index.is_conditional_key(target);
        if conditional {
            // 可空键只保留原始有序列对，且必须逐列严格类型/排序规则一致、无 <=> 歧义。
            if !index.non_null_types_match(source, target)
                || index.has_null_safe_comparison(source, target)
            {
                continue;
            }
        } else {
            let compatible = source
                .columns
                .iter()
                .zip(&target.columns)
                .all(|(left, right)| {
                    index
                        .signature(&source.table, left)
                        .zip(index.signature(&target.table, right))
                        .and_then(|(a, b)| compatible_types(a, b))
                        == Some(true)
                });
            if !compatible {
                continue;
            }
        }
        let Some(table) = index.table(&source.table) else {
            continue;
        };
        let role = if conditional {
            let Some(role) = conditional_view_role(index, source, key) else {
                continue;
            };
            role
        } else {
            let mut roles = BTreeSet::new();
            let mut restricted = false;
            for column in table
                .columns
                .iter()
                .filter(|c| source.columns.contains(&c.name))
            {
                let parsed = parse_comment(index, &column.comment);
                if parsed
                    .clauses
                    .iter()
                    .any(|c| c.negation || c.scope.is_some())
                {
                    restricted = true;
                    break;
                }
                let mut purposes = parsed.purposes;
                purposes.extend(purposes_in_name(&column.name));
                let role = if parsed.role.is_unknown() {
                    role_from_name(index, &column.name)
                } else {
                    parsed.role
                };
                if !role.is_unknown() && !purposes.is_disjoint(&key.purposes) {
                    roles.insert(role);
                }
            }
            if restricted || roles.len() != 1 {
                continue;
            }
            roles.pop_first().expect("已确认存在唯一来源角色")
        };
        let detail = if conditional {
            format!(
                "视图 {} 直接比较 {} 与 {}；原始列按序成对，来源字段表达角色「{}」且用途与目标键相符；该列对未发现空安全比较，但目标为可空唯一键，普通等值仅在键位非空时至多一行",
                comparison.view.label(),
                endpoint_text(source),
                endpoint_text(target),
                role.display()
            )
        } else {
            format!(
                "视图 {} 直接比较 {} 与 {}；来源字段表达角色「{}」及标识用途，目标具有完整唯一键",
                comparison.view.label(),
                endpoint_text(source),
                endpoint_text(target),
                role.display()
            )
        };
        mapped.push(RoleAnchor {
            domain: index.domain_for(&source.table),
            role,
            source: source.clone(),
            target: target.clone(),
            origin: format!("view:{}", comparison.view.label()),
            detail,
        });
    }
    mapped
}

/// 条件视图映射的来源角色/用途依据：每个原始来源列都必须能对上一个目标键用途。
fn conditional_view_role(
    index: &CatalogIndex<'_>,
    source: &Endpoint,
    key: &catalog::KeyInfo,
) -> Option<RoleKey> {
    let table = index.table(&source.table)?;
    let mut roles = BTreeSet::new();
    for name in &source.columns {
        let column = table.columns.iter().find(|column| &column.name == name)?;
        let parsed = parse_comment(index, &column.comment);
        if parsed
            .clauses
            .iter()
            .any(|clause| clause.negation || clause.scope.is_some())
        {
            return None;
        }
        let mut purposes = parsed.purposes;
        purposes.extend(purposes_in_name(&column.name));
        // 视图中的全部列对都已核对。用于限定范围的列可以没有标识用途，
        // 但至少一个成员必须由来源自身提供角色和用途。
        if purposes.is_disjoint(&key.purposes) {
            continue;
        }
        let role = if parsed.role.is_unknown() {
            role_from_name(index, &column.name)
        } else {
            parsed.role
        };
        if !role.is_unknown() {
            roles.insert(role);
        }
    }
    if roles.is_empty() {
        return None;
    }
    if roles.len() == 1 {
        return roles.pop_first();
    }
    // 复合键各列可能表达不同角色，或有一列只提供用途；用来源列自身归并出的短语，
    // 但绝不借用目标键身份。
    let combined = role_from_columns(index, &source.columns);
    (!combined.is_unknown()).then_some(combined)
}

fn attach_view_evidence(index: &CatalogIndex<'_>, combos: &mut ComboMap) {
    for combo in combos.map.values_mut() {
        for view in index.views_for(&combo.source, &combo.target) {
            let origin = format!("view:{}", view.label());
            combo.insert_positive(Evidence {
                origin: origin.clone(),
                kind: EvidenceKind::View,
                complete: false,
                scope: None,
                role: RoleKey::Unknown,
                detail: format!(
                    "视图 {} 中 {} = {} 的无方向比较与此既有方向一致；视图本身不提供方向，也不凭等式直接连线（依据 {}）",
                    view.label(),
                    endpoint_text(&combo.source),
                    endpoint_text(&combo.target),
                    origin
                ),
            });
        }
    }
}

fn add_positive(
    index: &CatalogIndex<'_>,
    combos: &mut ComboMap,
    table: &TableId,
    anchor: &str,
    target: &Endpoint,
    mut evidence: Evidence,
    extra_gaps: Vec<String>,
) {
    if !index.is_known_key(target) {
        return;
    }
    let Ok(prepared) = prepare(index, table, anchor, target) else {
        return;
    };
    if prepared.source == *target {
        return;
    }
    let combo = combos.entry(prepared.source.clone(), target.clone());
    for gap in prepared.gaps.iter().chain(extra_gaps.iter()) {
        combo.gaps.insert(gap.clone());
    }
    evidence.complete &= prepared.complete && extra_gaps.is_empty();
    combo.insert_positive(evidence);
}

fn add_negative(
    index: &CatalogIndex<'_>,
    combos: &mut ComboMap,
    table: &TableId,
    anchor: &str,
    target: &Endpoint,
    evidence: Evidence,
) {
    if !index.is_known_key(target) {
        return;
    }
    let source = Endpoint {
        table: table.clone(),
        columns: assemble_source(index, table, target, anchor)
            .unwrap_or_else(|_| vec![anchor.to_string()]),
    };
    if source == *target {
        return;
    }
    let combo = combos.entry(source, target.clone());
    combo.insert_negative(evidence);
}

fn add_untrusted_candidate(
    combos: &mut ComboMap,
    table: &TableId,
    anchor: &str,
    target: &Endpoint,
    evidence: Evidence,
    gap: String,
) {
    if target.columns.is_empty() {
        return;
    }
    let source = Endpoint {
        table: table.clone(),
        columns: vec![anchor.to_string()],
    };
    if source == *target {
        return;
    }
    let combo = combos.entry(source, target.clone());
    combo.gaps.insert(gap);
    combo.insert_positive(evidence);
}

fn prepare(
    index: &CatalogIndex<'_>,
    table: &TableId,
    anchor: &str,
    target: &Endpoint,
) -> Result<Prepared, String> {
    let Some(source_table) = index.table(table) else {
        return Err("来源表不在目录中".into());
    };
    if target.columns.is_empty() {
        return Err("目标键为空".into());
    }
    if !source_table.columns.iter().any(|c| c.name == anchor) {
        return Err(format!("来源列 {} 不存在", anchor));
    }
    let mut gaps = Vec::new();
    let columns = match assemble_source(index, table, target, anchor) {
        Ok(columns) => columns,
        Err(gap) => {
            gaps.push(gap);
            vec![anchor.to_string()]
        }
    };
    let source = Endpoint {
        table: table.clone(),
        columns,
    };
    if source == *target {
        return Err("来源与目标是同一条字段组".into());
    }
    let mut complete = gaps.is_empty();
    if source.columns.len() != target.columns.len() {
        gaps.push("关系两端列数不一致，复合键未完整配对".into());
        complete = false;
    } else {
        for (source_column, target_column) in source.columns.iter().zip(&target.columns) {
            let a = index.signature(table, source_column);
            let b = index.signature(&target.table, target_column);
            match (a, b) {
                (Some(a), Some(b)) => match compatible_types(a, b) {
                    Some(true) => {}
                    Some(false) => {
                        return Err(format!(
                            "类型不兼容：{}.{}={a} 与 {}.{}={b}",
                            table.label(),
                            source_column,
                            target.table.label(),
                            target_column
                        ));
                    }
                    None => {
                        gaps.push(format!(
                            "类型签名无法判断兼容：{}.{}={a} 与 {}.{}={b}",
                            table.label(),
                            source_column,
                            target.table.label(),
                            target_column
                        ));
                        complete = false;
                    }
                },
                _ => {
                    gaps.push(format!(
                        "缺少 catalog.types 类型签名：{}.{} 或 {}.{}",
                        table.label(),
                        source_column,
                        target.table.label(),
                        target_column
                    ));
                    complete = false;
                }
            }
        }
    }
    // 可空唯一键只在逐列类型与排序规则完全一致、可排除隐式转换时才有自动线资格。
    if index.is_conditional_key(target) && source.columns.len() == target.columns.len() {
        let strict = index.non_null_types_match(&source, target);
        if !strict {
            gaps.push("可空唯一键要求逐列完整类型与排序规则已证实一致；存在未知或隐式转换".into());
            complete = false;
        }
    }
    Ok(Prepared {
        source,
        gaps,
        complete,
    })
}

fn assemble_source(
    index: &CatalogIndex<'_>,
    table: &TableId,
    target: &Endpoint,
    anchor: &str,
) -> Result<Vec<String>, String> {
    let Some(source_table) = index.table(table) else {
        return Err("来源表不在目录中".into());
    };
    if target.columns.len() == 1 {
        return Ok(vec![anchor.to_string()]);
    }
    let mut chosen: Vec<String> = Vec::new();
    for key_column in &target.columns {
        let key_tokens = text::tokens_of(key_column);
        let mut exact: Vec<String> = Vec::new();
        let mut suffix: Vec<String> = Vec::new();
        for column in &source_table.columns {
            let tokens = text::tokens_of(&column.name);
            if tokens.len() == key_tokens.len()
                && tokens
                    .iter()
                    .zip(&key_tokens)
                    .all(|(a, b)| text::token_eq(a, b))
            {
                exact.push(column.name.clone());
            } else if tokens.len() > key_tokens.len()
                && tokens[tokens.len() - key_tokens.len()..]
                    .iter()
                    .zip(&key_tokens)
                    .all(|(a, b)| text::token_eq(a, b))
            {
                suffix.push(column.name.clone());
            }
        }
        let candidates = if exact.is_empty() { suffix } else { exact };
        match candidates.len() {
            0 => {
                return Err(format!(
                    "复合键 {} 缺少来源列 {} 的唯一对应列",
                    target.columns.join(", "),
                    key_column
                ))
            }
            1 => chosen.push(candidates[0].clone()),
            _ => {
                return Err(format!(
                    "复合键键位列 {} 在来源表中有多个候选：{}",
                    key_column,
                    candidates.join(", ")
                ))
            }
        }
    }
    if !chosen.iter().any(|c| c == anchor) {
        return Err(format!(
            "锚点列 {} 无法对应复合键 {} 的任一键位",
            anchor,
            target.columns.join(", ")
        ));
    }
    let distinct: BTreeSet<_> = chosen.iter().collect();
    if distinct.len() != chosen.len() {
        return Err("复合键组装复用了同一来源列".into());
    }
    Ok(chosen)
}

fn independent_role_source(index: &CatalogIndex<'_>, source: &TableId, witness: &TableId) -> bool {
    if source == witness {
        return false;
    }
    let (Some(a), Some(b)) = (index.table(source), index.table(witness)) else {
        return false;
    };
    let shape = |t: &crate::TableInfo| {
        t.columns
            .iter()
            .map(|c| (c.name.clone(), c.data_type.clone(), c.is_primary_key))
            .collect::<Vec<_>>()
    };
    if shape(a) != shape(b) {
        return true;
    }
    let family = |name: &str| {
        text::tokens_of(name)
            .into_iter()
            .filter(|part| {
                !part.chars().all(|c| c.is_ascii_digit())
                    && !matches!(
                        part.as_str(),
                        "archive" | "history" | "copy" | "backup" | "snapshot" | "mirror"
                    )
            })
            .collect::<Vec<_>>()
    };
    family(&a.name) != family(&b.name)
        && (a.comment.trim().is_empty() || a.comment.trim() != b.comment.trim())
}

fn build_role_anchors(
    index: &CatalogIndex<'_>,
    comments: &BTreeMap<&str, ParsedComment>,
) -> Vec<RoleAnchor> {
    let mut anchors = Vec::new();
    let mut seen = BTreeSet::new();
    for table_id in index.tables.keys() {
        let Some(table) = index.table(table_id) else {
            continue;
        };
        let table_domain = index.domain_for(table_id);
        for fk in &table.foreign_keys {
            if fk.columns.is_empty() || fk.columns.len() != fk.ref_columns.len() {
                continue;
            }
            let source = Endpoint {
                table: table_id.clone(),
                columns: fk.columns.clone(),
            };
            let target = Endpoint {
                table: TableId {
                    schema: if fk.ref_schema.is_empty() {
                        table_id.schema.clone()
                    } else {
                        fk.ref_schema.clone()
                    },
                    name: fk.ref_table.clone(),
                },
                columns: fk.ref_columns.clone(),
            };
            let comment = fk
                .columns
                .iter()
                .filter_map(|name| table.columns.iter().find(|c| &c.name == name))
                .map(|c| c.comment.as_str())
                .collect::<Vec<_>>()
                .join("；");
            let parsed = parse_comment(index, &comment);
            let role = if parsed.role.is_unknown() {
                role_from_columns(index, &fk.columns)
            } else {
                parsed.role.clone()
            };
            if role.is_unknown() {
                continue;
            }
            let domain = table_domain.clone().or(parsed.domain.clone());
            let origin = format!("fk:{}.{}", table_id.label(), fk.name);
            let detail = format!("数据库声明外键 {}", fk.name);
            let identity = (
                domain.clone(),
                role.clone(),
                source.clone(),
                target.clone(),
                origin.clone(),
            );
            if seen.insert(identity) {
                anchors.push(RoleAnchor {
                    domain,
                    role,
                    source,
                    target,
                    origin,
                    detail,
                });
            }
        }
        for column in &table.columns {
            let parsed = &comments[column.comment.as_str()];
            let role = if parsed.role.is_unknown() {
                role_from_name(index, &column.name)
            } else {
                parsed.role.clone()
            };
            if role.is_unknown() {
                continue;
            }
            let domain = table_domain.clone().or(parsed.domain.clone());
            let source = Endpoint {
                table: table_id.clone(),
                columns: vec![column.name.clone()],
            };
            let origin = comment_origin(table_id, &column.name);
            for clause in &parsed.clauses {
                if clause.negation || clause.scope.is_some() || !clause.reference {
                    continue;
                }
                for resolved in &clause.refs {
                    let ResolvedRef::Key(target) = resolved else {
                        continue;
                    };
                    // “非空等值”键不能充当角色锚点，否则会被其他规则当成无条件事实复用。
                    if !index.is_trusted_key(target) {
                        continue;
                    }
                    let compatible = source.columns.len() == target.columns.len()
                        && source.columns.iter().zip(&target.columns).all(|(s, t)| {
                            index
                                .signature(&source.table, s)
                                .zip(index.signature(&target.table, t))
                                .and_then(|(a, b)| compatible_types(a, b))
                                == Some(true)
                        });
                    if target == &source || !compatible {
                        continue;
                    }
                    let identity = (
                        domain.clone(),
                        role.clone(),
                        source.clone(),
                        target.clone(),
                        origin.clone(),
                    );
                    if seen.insert(identity) {
                        anchors.push(RoleAnchor {
                            domain: domain.clone(),
                            role: role.clone(),
                            source: source.clone(),
                            target: target.clone(),
                            origin: origin.clone(),
                            detail: format!("列注释「{}」", clause.text),
                        });
                    }
                }
            }
        }
    }
    anchors.sort_by(|a, b| {
        (&a.domain, &a.role, &a.source, &a.target, &a.origin)
            .cmp(&(&b.domain, &b.role, &b.source, &b.target, &b.origin))
    });
    anchors
}

struct GroupVerdict {
    group: Vec<Combo>,
    winner: Option<usize>,
    max_any: u8,
    has_conditional: bool,
    note: Option<String>,
}

fn adjudicate(
    index: &CatalogIndex<'_>,
    combos: ComboMap,
    declared_by_source: &BTreeMap<Endpoint, BTreeSet<Endpoint>>,
) -> Analysis {
    let mut groups: BTreeMap<Endpoint, BTreeMap<RoleKey, Vec<Combo>>> = BTreeMap::new();
    for combo in combos.map.into_values() {
        if combo.evidence.is_empty() {
            continue;
        }
        let role = combo.effective_role();
        let source = combo.source.clone();
        groups
            .entry(source)
            .or_default()
            .entry(role)
            .or_default()
            .push(combo);
    }
    let mut relations = Vec::new();
    let mut candidates = Vec::new();
    for (source, by_role) in groups {
        let mut outcomes: Vec<GroupVerdict> = by_role
            .into_values()
            .filter_map(|group| judge_group(index, group))
            .collect();
        reconcile_roles(index, &source, &mut outcomes, declared_by_source);
        for outcome in &outcomes {
            emit_group(
                index,
                outcome,
                declared_by_source,
                &mut relations,
                &mut candidates,
            );
        }
    }
    Analysis {
        relations,
        candidates,
    }
}

fn judge_group(index: &CatalogIndex<'_>, mut group: Vec<Combo>) -> Option<GroupVerdict> {
    group.sort_by(|a, b| a.target.cmp(&b.target));
    group.retain(|combo| {
        !(combo.has_negative() && combo.max_positive_strength().unwrap_or(0) <= NAMING)
    });
    if group.is_empty() {
        return None;
    }
    let complete: Vec<(usize, u8)> = group
        .iter()
        .enumerate()
        .filter_map(|(i, combo)| {
            combo
                .max_complete_unscoped_strength()
                .map(|strength| (i, strength))
        })
        .collect();
    let best = complete.iter().map(|(_, strength)| *strength).max();
    let top: Vec<usize> = match best {
        Some(best) => complete
            .iter()
            .filter(|(_, strength)| *strength == best)
            .map(|(idx, _)| *idx)
            .collect(),
        None => Vec::new(),
    };
    let winner = if top.len() == 1 {
        let idx = top[0];
        // 只有明确端点能消解单凭裸名出现的竞争者。另一边缺少类型证据并不证明它是错的。
        let threshold = if best == Some(DIRECT) {
            BUSINESS
        } else {
            NAMING
        };
        let blocked = group.iter().enumerate().any(|(other_idx, other)| {
            other_idx != idx && other.blocking_strength().unwrap_or(0) >= threshold
        });
        (!blocked).then_some(idx)
    } else {
        None
    };
    // 端点级否决（空安全比较、来源自身是键）同样让该组退回候选。
    let winner = winner.filter(|idx| withhold_reason(index, &group[*idx]).is_none());
    let max_any = group
        .iter()
        .filter_map(Combo::max_positive_strength)
        .max()
        .unwrap_or(0);
    let has_conditional = group
        .iter()
        .any(|combo| index.is_conditional_key(&combo.target));
    Some(GroupVerdict {
        group,
        winner,
        max_any,
        has_conditional,
        note: None,
    })
}

/// 同源不同角色分组的二次裁决。只在有可空键参与且没有声明外键/查询事实时生效：
/// 条件与全表同等或强弱无法唯一消解时两端暂缓；只有直接明确引用能压过弱命名。
fn reconcile_roles(
    index: &CatalogIndex<'_>,
    source: &Endpoint,
    outcomes: &mut [GroupVerdict],
    declared_by_source: &BTreeMap<Endpoint, BTreeSet<Endpoint>>,
) {
    if outcomes.len() < 2
        || index.non_null_keys.is_empty()
        || !outcomes.iter().any(|outcome| outcome.has_conditional)
        || declared_by_source.contains_key(source)
    {
        return;
    }
    let mut losers = vec![false; outcomes.len()];
    for a in 0..outcomes.len() {
        for b in (a + 1)..outcomes.len() {
            if !(outcomes[a].has_conditional || outcomes[b].has_conditional) {
                continue;
            }
            match (outcomes[a].winner, outcomes[b].winner) {
                (Some(ia), Some(ib)) => {
                    let sa = outcomes[a].group[ia]
                        .max_complete_unscoped_strength()
                        .unwrap_or(0);
                    let sb = outcomes[b].group[ib]
                        .max_complete_unscoped_strength()
                        .unwrap_or(0);
                    let direct_a = outcomes[a].group[ia].strongest_complete_kind()
                        == Some(EvidenceKind::Direct);
                    let direct_b = outcomes[b].group[ib].strongest_complete_kind()
                        == Some(EvidenceKind::Direct);
                    let strong_a = direct_a && sa > sb && outcomes[b].max_any <= NAMING;
                    let strong_b = direct_b && sb > sa && outcomes[a].max_any <= NAMING;
                    if strong_a {
                        losers[b] = true;
                    } else if strong_b {
                        losers[a] = true;
                    } else {
                        losers[a] = true;
                        losers[b] = true;
                    }
                }
                (Some(ia), None) => {
                    let direct = outcomes[a].group[ia].strongest_complete_kind()
                        == Some(EvidenceKind::Direct);
                    if !(direct && outcomes[b].max_any <= NAMING) {
                        losers[a] = true;
                    }
                }
                (None, Some(ib)) => {
                    let direct = outcomes[b].group[ib].strongest_complete_kind()
                        == Some(EvidenceKind::Direct);
                    if !(direct && outcomes[a].max_any <= NAMING) {
                        losers[b] = true;
                    }
                }
                (None, None) => {}
            }
        }
    }
    let others_by_index: Vec<Vec<String>> = (0..outcomes.len())
        .map(|idx| {
            outcomes
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != idx)
                .flat_map(|(_, other)| other.group.iter().map(|combo| endpoint_text(&combo.target)))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        })
        .collect();
    for (idx, outcome) in outcomes.iter_mut().enumerate() {
        if losers[idx] {
            outcome.winner = None;
        }
        if let Some(others) = others_by_index.get(idx).filter(|others| !others.is_empty()) {
            outcome.note = Some(format!(
                "；同一来源的其他角色分组也存在竞争目标（{}），已一并裁决",
                others.join("、")
            ));
        }
    }
}

fn emit_group(
    index: &CatalogIndex<'_>,
    outcome: &GroupVerdict,
    declared_by_source: &BTreeMap<Endpoint, BTreeSet<Endpoint>>,
    relations: &mut Vec<Relation>,
    candidates: &mut Vec<Candidate>,
) {
    if let Some(idx) = outcome.winner {
        let combo = &outcome.group[idx];
        match relation_for(index, combo, declared_by_source) {
            Some(relation) => relations.push(relation),
            None => push_candidate(
                index,
                combo,
                &outcome.group,
                declared_by_source,
                outcome.note.as_deref(),
                candidates,
            ),
        }
    } else {
        for combo in &outcome.group {
            push_candidate(
                index,
                combo,
                &outcome.group,
                declared_by_source,
                outcome.note.as_deref(),
                candidates,
            );
        }
    }
}

fn push_candidate(
    index: &CatalogIndex<'_>,
    combo: &Combo,
    group: &[Combo],
    declared_by_source: &BTreeMap<Endpoint, BTreeSet<Endpoint>>,
    note: Option<&str>,
    candidates: &mut Vec<Candidate>,
) {
    if let Some(mut candidate) = candidate_for(index, combo, group, declared_by_source) {
        if let Some(note) = note {
            candidate.reason.push_str(note);
        }
        candidates.push(candidate);
    }
}

fn relation_for(
    index: &CatalogIndex<'_>,
    combo: &Combo,
    declared_by_source: &BTreeMap<Endpoint, BTreeSet<Endpoint>>,
) -> Option<Relation> {
    if withhold_reason(index, combo).is_some() {
        return None;
    }
    let declared = declared_by_source.get(&combo.source);
    if let Some(targets) = declared {
        if targets.contains(&combo.target) {
            return None;
        }
        return None;
    }
    let detail = evidence_details(combo);
    let types = types_text(index, combo);
    let condition = condition_for(index, combo);
    let kind = condition
        .as_ref()
        .and_then(|c| c.evidence_kind)
        .unwrap_or_else(|| combo.strongest_kind());
    let conditional = condition
        .as_ref()
        .is_some_and(|condition| condition.non_null_equality);
    let uniqueness = if conditional {
        "数据库目录确认目标为有序完整键，但可空唯一键不保证全表唯一"
    } else {
        "数据库目录确认目标为完整唯一键"
    };
    let condition_text = if conditional {
        "；普通 `=` 只在目标键位非空时至多匹配一行，NULL 不匹配且不保证目标存在"
    } else {
        ""
    };
    Some(Relation {
        source: combo.source.clone(),
        target: combo.target.clone(),
        kind: RelationKind::Inferred,
        condition,
        reason: format!(
            "推断关系（{}）：{}；{}，逐列类型可比较（{}）；去重后证据 {} 条{}",
            kind.label(),
            detail,
            uniqueness,
            types,
            combo.evidence.len(),
            condition_text
        ),
    })
}

fn condition_for(index: &CatalogIndex<'_>, combo: &Combo) -> Option<KeyCondition> {
    index
        .is_conditional_key(&combo.target)
        .then(|| KeyCondition {
            non_null_equality: true,
            evidence_kind: combo
                .evidence
                .values()
                .filter(|e| e.complete && e.scope.is_none())
                .max_by_key(|e| e.strength())
                .map(|e| e.kind),
            ..Default::default()
        })
}

/// 已经足以生成候选、但不得升级为自动关系的端点级冲突。
fn withhold_reason(index: &CatalogIndex<'_>, combo: &Combo) -> Option<&'static str> {
    if index.is_conditional_key(&combo.target)
        && !index.non_null_types_match(&combo.source, &combo.target)
    {
        return Some("完整键位、逐列类型或排序规则尚未证实，无法保证非空等值的匹配上限");
    }
    if index.is_conditional_key(&combo.target)
        && index.has_null_safe_comparison(&combo.source, &combo.target)
    {
        return Some(
            "视图对同一列对使用空安全比较（<=>）；空安全比较不等于普通等值，不能推断为引用",
        );
    }
    if index.is_conditional_key(&combo.target) && index.is_known_key(&combo.source) {
        return Some("来源字段组自身已是唯一键；指向可空唯一键的关系需人工复核");
    }
    None
}

fn candidate_for(
    index: &CatalogIndex<'_>,
    combo: &Combo,
    group: &[Combo],
    declared_by_source: &BTreeMap<Endpoint, BTreeSet<Endpoint>>,
) -> Option<Candidate> {
    if combo.evidence.is_empty() {
        return None;
    }
    if let Some(targets) = declared_by_source.get(&combo.source) {
        if targets.contains(&combo.target) {
            return None;
        }
    }
    let competitors: Vec<String> = group
        .iter()
        .filter(|other| other.target != combo.target && !other.evidence.is_empty())
        .map(|other| {
            format!(
                "{} ({})",
                other.target.table.label(),
                other.target.columns.join(", ")
            )
        })
        .collect();
    let mut gaps: Vec<String> = combo.gaps.iter().cloned().collect();
    if let Some(reason) = withhold_reason(index, combo) {
        gaps.push(reason.into());
    }
    if combo.has_negative() && combo.max_positive_strength().unwrap_or(0) >= BUSINESS {
        gaps.push("同一目标同时存在明确否定与明确支持".into());
    }
    let has_complete = combo
        .evidence
        .values()
        .any(|e| e.complete && e.scope.is_none() && combo.gaps.is_empty());
    let status = if combo.has_negative() && combo.max_positive_strength().unwrap_or(0) >= BUSINESS {
        "证据冲突".to_string()
    } else if !has_complete {
        let scopes: Vec<&str> = combo
            .evidence
            .values()
            .filter_map(|e| e.scope.map(Scope::label))
            .collect();
        if !scopes.is_empty() {
            format!("仅保留在原有范围（{}）", scopes.join("/"))
        } else {
            "业务路径或结构信息不完整".to_string()
        }
    } else {
        "存在竞争目标".to_string()
    };
    let mut reason = format!(
        "待判断（{}）：{}；缺少的依据：{}；竞争目标：{}；去重后证据 {} 条",
        status,
        evidence_details(combo),
        if gaps.is_empty() {
            "无".to_string()
        } else {
            gaps.join("；")
        },
        if competitors.is_empty() {
            "无".to_string()
        } else {
            competitors.join("、")
        },
        combo.evidence.len()
    );
    if let Some(targets) = declared_by_source.get(&combo.source) {
        if !targets.is_empty() {
            reason.push_str("；当前来源已有数据库声明外键，此推断候选需复核");
        }
    }
    Some(Candidate {
        condition: condition_for(index, combo),
        source: combo.source.clone(),
        target: combo.target.clone(),
        reason,
    })
}

fn evidence_details(combo: &Combo) -> String {
    let mut evidence: Vec<&Evidence> = combo.evidence.values().collect();
    evidence.sort_by(|a, b| {
        b.strength()
            .cmp(&a.strength())
            .then_with(|| a.origin.cmp(&b.origin))
    });
    evidence
        .iter()
        .map(|e| e.detail.as_str())
        .collect::<Vec<_>>()
        .join("；")
}

fn types_text(index: &CatalogIndex<'_>, combo: &Combo) -> String {
    if combo.source.columns.len() != combo.target.columns.len() {
        return "列数不一致".into();
    }
    combo
        .source
        .columns
        .iter()
        .zip(&combo.target.columns)
        .map(|(source_column, target_column)| {
            let a = index
                .signature(&combo.source.table, source_column)
                .unwrap_or("未知");
            let b = index
                .signature(&combo.target.table, target_column)
                .unwrap_or("未知");
            format!("{source_column}:{a}={target_column}:{b}")
        })
        .collect::<Vec<_>>()
        .join("，")
}

fn role_from_name(index: &CatalogIndex<'_>, name: &str) -> RoleKey {
    let mut tokens = text::tokens_of(name);
    if tokens.is_empty() {
        return RoleKey::Unknown;
    }
    text::trim_purpose_suffix(&mut tokens);
    while tokens.len() > 1 && matches!(tokens.last().map(String::as_str), Some("ref" | "fk")) {
        tokens.pop();
    }
    index.entities.canonical_role(&tokens)
}

fn role_from_columns(index: &CatalogIndex<'_>, columns: &[String]) -> RoleKey {
    let mut tokens = Vec::new();
    for column in columns {
        let mut name_tokens = text::tokens_of(column);
        text::trim_purpose_suffix(&mut name_tokens);
        while name_tokens.len() > 1
            && matches!(name_tokens.last().map(String::as_str), Some("ref" | "fk"))
        {
            name_tokens.pop();
        }
        tokens.extend(name_tokens);
    }
    index.entities.canonical_role(&tokens)
}

fn generic_column_name(tokens: &[String], raw: &str) -> bool {
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
        "updated",
        "deleted",
        "creator",
        "updater",
    ];
    if tokens.len() == 1 && GENERIC.contains(&tokens[0].as_str()) {
        return true;
    }
    matches!(
        raw.to_lowercase().as_str(),
        "编号"
            | "编码"
            | "代码"
            | "名称"
            | "状态"
            | "类型"
            | "备注"
            | "描述"
            | "主键"
            | "创建时间"
            | "更新时间"
            | "删除时间"
    )
}

fn comment_origin(table: &TableId, column: &str) -> String {
    format!("comment:{}.{}", table.label(), column)
}

fn name_origin(table: &TableId, column: &str) -> String {
    format!("name:{}.{}", table.label(), column)
}

fn column_location(table: &TableId, column: &str) -> String {
    format!("列 {} 的 {}", table.label(), column)
}

fn endpoint_text(endpoint: &Endpoint) -> String {
    format!(
        "{} ({})",
        endpoint.table.label(),
        endpoint.columns.join(", ")
    )
}

fn scope_suffix(scope: Option<Scope>) -> String {
    match scope {
        Some(scope) => format!("；这是{}陈述，仅在其原有范围内适用", scope.label()),
        None => String::new(),
    }
}
