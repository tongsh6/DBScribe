use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// 同一次读取的原始结构与全域裁决。桌面端重新勾表或校对时复用这份快照。
pub struct ErSnapshot {
    catalog: ErCatalog,
    analysis: Analysis,
}

impl ErSnapshot {
    pub fn new(catalog: ErCatalog) -> Self {
        let analysis = infer_relations(&catalog);
        Self { catalog, analysis }
    }

    pub fn diagram(
        &self,
        selected: Option<&[TableId]>,
        project: &ErProject,
    ) -> Result<ErDiagram, String> {
        project_diagram(&self.catalog, self.analysis.clone(), selected, project)
    }

    /// 对外交付投影：与 CLI 产物、桌面 SVG 预览/保存共用。
    ///
    /// 本地预览仍用 [`ErSnapshot::diagram`] 保留可见来源的全部候选；这里只
    /// 收敛外发内容，不改变入选范围内的裁决。
    pub fn delivery(
        &self,
        selected: Option<&[TableId]>,
        project: &ErProject,
    ) -> Result<ErDiagram, String> {
        let diagram = self.diagram(selected, project)?;
        let full_selection = diagram.tables.len() == self.catalog.tables.len();
        Ok(deliver_diagram(&diagram, full_selection))
    }

    pub fn decision(
        &self,
        source: Endpoint,
        target: Endpoint,
        action: DecisionAction,
    ) -> Result<Decision, String> {
        reviewed_decision(&self.catalog, &self.analysis, source, target, action)
    }
}

/// 基于原始目录复核人工决定；只在全库裁决完成后处理勾选范围。
pub fn build_diagram(
    catalog: &ErCatalog,
    selected: Option<&[TableId]>,
    project: &ErProject,
) -> Result<ErDiagram, String> {
    project_diagram(catalog, infer_relations(catalog), selected, project)
}

/// 构建只用于对外交付的图。
///
/// CLI 的 SVG/JSON/Markdown、桌面 SVG 预览与保存都应使用本函数，或使用
/// [`ErSnapshot::delivery`]。需要完整竞争态的前端预览继续使用
/// [`build_diagram`] / [`ErSnapshot::diagram`]。
pub fn build_delivery_diagram(
    catalog: &ErCatalog,
    selected: Option<&[TableId]>,
    project: &ErProject,
) -> Result<ErDiagram, String> {
    let diagram = project_diagram(catalog, infer_relations(catalog), selected, project)?;
    let full_selection = diagram.tables.len() == catalog.tables.len();
    Ok(deliver_diagram(&diagram, full_selection))
}

/// 把本地分析图收敛为可外发的交付图。
///
/// 全部选表时原样返回，保留原始诊断；部分选表时：
/// - 只保留两端都在范围内的关系，候选目标在图外的只计数；
/// - 过滤指向图外的原始外键，清空无法证明范围的生成备注；
/// - 生成理由改为只依赖可见端点的通用说明，避免回显图外表名；
/// - 目录提示已由 `project_diagram` 按结构化 TableId 剪裁。
pub fn deliver_diagram(diagram: &ErDiagram, full_selection: bool) -> ErDiagram {
    if full_selection {
        return diagram.clone();
    }
    let visible: BTreeSet<_> = diagram.tables.iter().map(TableId::from).collect();
    let mut tables = diagram.tables.clone();
    for table in &mut tables {
        table
            .foreign_keys
            .retain(|foreign_key| references_visible_table(&table.schema, foreign_key, &visible));
        // note 是生成文本，无法证明其只涉及可见表；数据库作者的 comment 保留。
        table.note.clear();
    }

    let mut candidates = Vec::new();
    // 幂等：本地图已经把外部候选计数写在字段里；对交付图再次投影时保留该计数。
    let external_in_input = diagram
        .candidates
        .iter()
        .filter(|candidate| !visible.contains(&candidate.target.table))
        .count();
    let external_candidate_count = diagram.external_candidate_count.max(external_in_input);
    for candidate in &diagram.candidates {
        if visible.contains(&candidate.target.table) {
            let mut delivered = candidate.clone();
            delivered.reason = "待判断候选（目标在当前选表范围内）".into();
            if let Some(kind) = candidate.condition.as_ref().and_then(|c| c.evidence_kind) {
                delivered.reason = format!("{}提供了目标线索，关系仍待判断", kind.label());
            }
            candidates.push(delivered);
        }
    }

    let mut relations = diagram.relations.clone();
    for relation in &mut relations {
        relation.reason = match relation.condition.as_ref().and_then(|c| c.evidence_kind) {
            Some(kind) => format!(
                "{}支持上述字段指向该目标键；按普通等号比较且各键位非空时，目标至多匹配一行",
                kind.label()
            ),
            None => delivered_relation_reason(relation.kind).into(),
        };
    }

    ErDiagram {
        db_type: diagram.db_type,
        tables,
        keys: diagram.keys.clone(),
        relations,
        candidates,
        external_count: diagram.external_count,
        external_candidate_count,
        warnings: diagram.warnings.clone(),
    }
}

fn references_visible_table(
    owner: &str,
    foreign_key: &crate::ForeignKey,
    visible: &BTreeSet<TableId>,
) -> bool {
    visible.contains(&TableId {
        schema: if foreign_key.ref_schema.is_empty() {
            owner.into()
        } else {
            foreign_key.ref_schema.clone()
        },
        name: foreign_key.ref_table.clone(),
    })
}

fn delivered_relation_reason(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Declared => "数据库声明外键",
        RelationKind::Lookup => "视图定义中的查询关联",
        RelationKind::Inferred => "字段名称、注释或类型匹配（推断）",
        RelationKind::Confirmed => "项目内人工确认",
    }
}

fn project_diagram(
    catalog: &ErCatalog,
    mut analysis: Analysis,
    selected: Option<&[TableId]>,
    project: &ErProject,
) -> Result<ErDiagram, String> {
    let included = selected.or(project.selected.as_deref());
    let mut tables: Vec<_> = catalog.tables.iter().collect();
    tables.sort_by_key(|t| (&t.schema, &t.name));
    if let Some(ids) = included {
        let selected: BTreeSet<_> = ids.iter().collect();
        tables.retain(|t| selected.contains(&TableId::from(*t)));
        if tables.len() != selected.len() {
            return Err("所选表已变化，请刷新表结构后重新选择".into());
        }
    }
    if tables.is_empty() {
        return Err("没有可导出的表".into());
    }
    let full_selection = tables.len() == catalog.tables.len();
    let visible: BTreeSet<_> = tables.iter().map(|t| TableId::from(*t)).collect();
    let mut warnings = scoped_warnings(catalog, &visible, full_selection);
    apply_decisions(
        catalog,
        &project.decisions,
        &mut analysis,
        &mut warnings,
        &visible,
        full_selection,
    );
    let mut external_count = 0;
    analysis.relations.retain(|r| {
        let source = visible.contains(&r.source.table);
        let target = visible.contains(&r.target.table);
        external_count += usize::from(source != target);
        source && target
    });
    // 保留可见来源的全部竞争目标，不能因未勾选其中一端而隐藏歧义。
    analysis
        .candidates
        .retain(|r| visible.contains(&r.source.table));
    let external_candidate_count = analysis
        .candidates
        .iter()
        .filter(|candidate| !visible.contains(&candidate.target.table))
        .count();
    analysis
        .relations
        .sort_by(|a, b| (&a.source, &a.target, &a.reason).cmp(&(&b.source, &b.target, &b.reason)));
    analysis
        .candidates
        .sort_by(|a, b| (&a.source, &a.target, &a.reason).cmp(&(&b.source, &b.target, &b.reason)));
    dedup_warning_messages(&mut warnings);
    Ok(ErDiagram {
        db_type: catalog.db_type,
        tables: tables.into_iter().cloned().collect(),
        keys: catalog
            .keys
            .iter()
            .filter(|key| visible.contains(&key.table))
            .cloned()
            .collect(),
        relations: analysis.relations,
        candidates: analysis.candidates,
        external_count,
        external_candidate_count,
        warnings,
    })
}

/// 目录提示按结构化归属剪裁：部分选表时不外发未标归属的全局自由文本。
fn scoped_warnings(
    catalog: &ErCatalog,
    visible: &BTreeSet<TableId>,
    full_selection: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if full_selection {
        warnings.extend(catalog.warnings.iter().cloned());
    } else if !catalog.warnings.is_empty() {
        warnings.push(format!(
            "另有 {} 条目录级提示仅在全部选表时显示",
            catalog.warnings.len()
        ));
    }
    let mut hidden = 0;
    for warning in &catalog.scoped_warnings {
        if visible.contains(&warning.table) {
            warnings.push(warning.message.clone());
        } else {
            hidden += 1;
        }
    }
    if !full_selection && hidden > 0 {
        warnings.push(format!(
            "另有 {} 条与未选表相关的目录提示仅在全部选表时显示",
            hidden
        ));
    }
    warnings
}

fn dedup_warning_messages(warnings: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    warnings.retain(|warning| seen.insert(warning.clone()));
}

/// 用当下结构和证据构建可复核的项目修正。
pub fn make_decision(
    catalog: &ErCatalog,
    source: Endpoint,
    target: Endpoint,
    action: DecisionAction,
) -> Result<Decision, String> {
    reviewed_decision(catalog, &infer_relations(catalog), source, target, action)
}

fn reviewed_decision(
    catalog: &ErCatalog,
    analysis: &Analysis,
    source: Endpoint,
    target: Endpoint,
    action: DecisionAction,
) -> Result<Decision, String> {
    let basis = decision_basis(catalog, analysis, &source, &target, action)?;
    Ok(Decision {
        source,
        target,
        action,
        basis,
    })
}

fn decision_basis(
    catalog: &ErCatalog,
    analysis: &Analysis,
    source: &Endpoint,
    target: &Endpoint,
    action: DecisionAction,
) -> Result<String, String> {
    if source.columns.is_empty() || source.columns.len() != target.columns.len() {
        return Err("关系两端的字段必须非空且逐列对应".into());
    }
    if source == target {
        return Err("关系的来源与目标不能是同一组字段".into());
    }
    let mut stamp = Basis::new();
    let source_types = stamp_endpoint(catalog, source, &mut stamp)?;
    let target_types = stamp_endpoint(catalog, target, &mut stamp)?;
    let unique = catalog.keys.contains(target);
    stamp.add(if unique {
        "unique"
    } else if catalog.non_null_keys.contains(target) {
        "non-null-equality"
    } else {
        "unknown"
    });
    if action == DecisionAction::Confirm {
        let conditional_relation = analysis
            .relations
            .iter()
            .any(|r| &r.source == source && &r.target == target && r.condition.is_some());
        if conditional_relation
            || analysis
                .candidates
                .iter()
                .any(|c| &c.source == source && &c.target == target && c.condition.is_some())
        {
            return Err("这条关系带有附加条件，不能采纳为无条件关系".into());
        }
        if !unique {
            return Err("目标字段组缺少可验证的完整唯一键".into());
        }
        if !source_types
            .iter()
            .zip(target_types.iter())
            .all(|(a, b)| super::inference::compatible_types(a, b) == Some(true))
        {
            return Err("缺少两端字段类型可直接比较的依据".into());
        }
    }
    let declared: Vec<_> = analysis
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::Declared && &r.source == source)
        .collect();
    if analysis
        .relations
        .iter()
        .any(|r| r.kind == RelationKind::Lookup && &r.source == source && &r.target == target)
    {
        return Err("视图定义已记录该查询关系，无需以人工决定替换".into());
    }
    if !declared.is_empty()
        && (action == DecisionAction::Confirm || declared.iter().any(|r| &r.target == target))
    {
        return Err("当前来源已有数据库声明的外键，请复核人工决定".into());
    }
    // 记录该来源的全部竞争解释。新增竞争键或锚点消失时旧决定回到待复核。
    let mut reasons: Vec<_> = analysis
        .relations
        .iter()
        .filter(|r| &r.source == source)
        .map(|r| (&r.target, &r.reason))
        .chain(
            analysis
                .candidates
                .iter()
                .filter(|r| &r.source == source)
                .map(|r| (&r.target, &r.reason)),
        )
        .collect();
    reasons.sort();
    for (end, reason) in reasons {
        stamp.add(&end.table.schema);
        stamp.add(&end.table.name);
        for col in &end.columns {
            stamp.add(col);
        }
        stamp.add(reason);
    }
    Ok(stamp.finish())
}

fn stamp_endpoint<'a>(
    catalog: &'a ErCatalog,
    end: &Endpoint,
    stamp: &mut Basis,
) -> Result<Vec<&'a str>, String> {
    let table = catalog
        .tables
        .iter()
        .find(|t| t.schema == end.table.schema && t.name == end.table.name)
        .ok_or_else(|| format!("表已不存在：{}", end.table.label()))?;
    if end.columns.iter().collect::<BTreeSet<_>>().len() != end.columns.len() {
        return Err("同一端的字段不能重复".into());
    }
    stamp.add(&table.schema);
    stamp.add(&table.name);
    stamp.add(&table.comment);
    let mut signatures = Vec::new();
    for name in &end.columns {
        let col = table
            .columns
            .iter()
            .find(|c| &c.name == name)
            .ok_or_else(|| format!("字段已不存在：{}.{}", end.table.label(), name))?;
        let signature = catalog
            .types
            .iter()
            .find(|t| t.table == end.table && &t.column == name)
            .map(|t| t.signature.as_str())
            .unwrap_or_default();
        for value in [
            name.as_str(),
            &col.data_type,
            &col.length,
            &col.comment,
            signature,
        ] {
            stamp.add(value);
        }
        signatures.push(signature);
    }
    Ok(signatures)
}

// 带长度的稳定 FNV-1a 指纹，仅用于发现校对依据的变化。
struct Basis(u64);
impl Basis {
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }
    fn add(&mut self, value: &str) {
        for byte in (value.len() as u64)
            .to_le_bytes()
            .iter()
            .chain(value.as_bytes())
        {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
    }
    fn finish(self) -> String {
        format!("v1-{:016x}", self.0)
    }
}

fn apply_decisions(
    catalog: &ErCatalog,
    decisions: &[Decision],
    analysis: &mut Analysis,
    warnings: &mut Vec<String>,
    visible: &BTreeSet<TableId>,
    full_selection: bool,
) {
    let mut valid = BTreeMap::<&Endpoint, Vec<&Decision>>::new();
    for d in decisions {
        if d.action == DecisionAction::Confirm
            && analysis.relations.iter().any(|r| {
                matches!(r.kind, RelationKind::Declared | RelationKind::Lookup)
                    && r.source == d.source
                    && r.target == d.target
            })
        {
            continue;
        }
        let result = decision_basis(catalog, analysis, &d.source, &d.target, d.action);
        match result {
            Ok(basis) if basis == d.basis => valid.entry(&d.source).or_default().push(d),
            _ => {
                if full_selection || visible.contains(&d.source.table) {
                    // 部分选表时目标可能在范围外；只保留可见来源的可解释说明。
                    if full_selection || visible.contains(&d.target.table) {
                        warnings.push(format!(
                            "人工决定已失效，请复核 {} ({}) → {} ({})",
                            d.source.table.label(),
                            d.source.columns.join(", "),
                            d.target.table.label(),
                            d.target.columns.join(", ")
                        ));
                    } else {
                        warnings.push(format!(
                            "人工决定已失效，请复核 {} ({})；目标不在当前选表范围",
                            d.source.table.label(),
                            d.source.columns.join(", ")
                        ));
                    }
                }
            }
        }
    }
    for (source, group) in valid {
        let confirmed: BTreeSet<_> = group
            .iter()
            .filter(|d| d.action == DecisionAction::Confirm)
            .map(|d| &d.target)
            .collect();
        let excluded: BTreeSet<_> = group
            .iter()
            .filter(|d| d.action == DecisionAction::Exclude)
            .map(|d| &d.target)
            .collect();
        if confirmed.len() > 1 || !confirmed.is_disjoint(&excluded) {
            if full_selection || visible.contains(&source.table) {
                warnings.push(format!(
                    "人工决定互相冲突，请复核 {} ({})",
                    source.table.label(),
                    source.columns.join(", ")
                ));
            }
            let mut held = Vec::new();
            analysis.relations.retain(|r| {
                if r.kind == RelationKind::Inferred && &r.source == source {
                    held.push(Candidate {
                        condition: r.condition.clone(),
                        source: r.source.clone(),
                        target: r.target.clone(),
                        reason: "人工决定冲突，需重新选择".into(),
                    });
                    false
                } else {
                    true
                }
            });
            analysis.candidates.extend(held);
            continue;
        }
        analysis.relations.retain(|r| {
            &r.source != source
                || matches!(r.kind, RelationKind::Declared | RelationKind::Lookup)
                || (confirmed.is_empty() && !excluded.contains(&r.target))
        });
        analysis.candidates.retain(|r| {
            &r.source != source || (confirmed.is_empty() && !excluded.contains(&r.target))
        });
        if let Some(target) = confirmed.first() {
            analysis.relations.push(Relation {
                condition: None,
                source: source.clone(),
                target: (*target).clone(),
                kind: RelationKind::Confirmed,
                reason: "项目内人工确认".into(),
            });
        }
    }
}
