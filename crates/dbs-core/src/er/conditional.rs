//! 依据目录中的索引列组发现带限定条件的业务键候选。索引说明唯一性范围，不证明引用方向。
use super::*;
use std::collections::{BTreeMap, BTreeSet};

struct Source<'a> {
    id: TableId,
    columns: BTreeSet<&'a str>,
    indexes: Vec<BTreeSet<String>>,
}

impl<'a> Source<'a> {
    fn new(t: &'a TableInfo) -> Self {
        let columns = t.columns.iter().map(|c| c.name.as_str()).collect();
        let indexes = t
            .indexes
            .iter()
            .map(|i| i.columns.iter().cloned().collect())
            .collect();
        Self {
            id: TableId::from(t),
            columns,
            indexes,
        }
    }
}

fn boolean_column(c: &crate::ColumnInfo) -> bool {
    let ty = c.data_type.trim().to_ascii_lowercase();
    matches!(ty.as_str(), "bool" | "boolean")
        || (matches!(ty.as_str(), "bit" | "tinyint") && c.length.trim() == "1")
}

pub(super) fn append(catalog: &ErCatalog, analysis: &mut Analysis) {
    let tables: BTreeMap<_, _> = catalog
        .tables
        .iter()
        .map(|t| (TableId::from(t), t))
        .collect();
    let mut patterns: BTreeMap<Endpoint, BTreeSet<KeyCondition>> = BTreeMap::new();
    let non_null: BTreeSet<&Endpoint> = catalog.non_null_keys.iter().collect();
    // 布尔键位既可能是身份的一部分，也可能被固定为某个状态。没有查询谓词时不替业务选值。
    for key in &catalog.keys {
        if key.columns.len() < 3 {
            continue;
        }
        let Some(t) = tables.get(&key.table) else {
            continue;
        };
        let (qualifiers, binding): (Vec<_>, Vec<_>) =
            key.columns.iter().cloned().partition(|name| {
                t.columns
                    .iter()
                    .find(|c| &c.name == name)
                    .is_some_and(boolean_column)
            });
        if qualifiers.is_empty() || binding.len() < 2 {
            continue;
        }
        patterns
            .entry(Endpoint {
                table: key.table.clone(),
                columns: binding,
            })
            .or_default()
            .insert(KeyCondition {
                non_null_equality: false,
                evidence_kind: None,
                required_target: Vec::new(),
                unbound_target_columns: qualifiers,
            });
    }
    for k in &catalog.guarded_keys {
        if k.key.columns.len() >= 2 && !k.required_target.is_empty() {
            patterns
                .entry(k.key.clone())
                .or_default()
                .insert(KeyCondition {
                    non_null_equality: non_null.contains(&k.key),
                    evidence_kind: None,
                    required_target: k.required_target.clone(),
                    unbound_target_columns: Vec::new(),
                });
        }
    }
    if patterns.is_empty() {
        return;
    }
    let mut srcs: Vec<_> = catalog.tables.iter().map(Source::new).collect();
    let mut postings: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, src) in srcs.iter_mut().enumerate() {
        for key in catalog
            .guarded_keys
            .iter()
            .filter(|k| k.key.table == src.id)
        {
            src.indexes.push(key.key.columns.iter().cloned().collect());
        }
        let indexed: BTreeSet<_> = src
            .indexes
            .iter()
            .flatten()
            .filter(|s| src.columns.contains(s.as_str()))
            .collect();
        for term in indexed {
            postings.entry(term.clone()).or_default().push(i);
        }
    }
    let types: BTreeMap<_, _> = catalog
        .types
        .iter()
        .map(|c| ((&c.table, c.column.as_str()), c.signature.as_str()))
        .collect();
    let existing: BTreeSet<_> = analysis
        .relations
        .iter()
        .map(|r| (&r.source, &r.target))
        .collect();
    let mut additions = Vec::new();
    for (target, options) in &patterns {
        // 不能把不同部分索引谓词拼成一个 AND，也不能按遍历顺序任选一个。
        if options.len() != 1 {
            continue;
        }
        let Some(condition) = options.first() else {
            continue;
        };
        let names = &target.columns;
        let Some(sources) = names
            .iter()
            .filter_map(|name| postings.get(name))
            .min_by_key(|p| p.len())
        else {
            continue;
        };
        for &i in sources {
            let s = &srcs[i];
            if s.id == target.table || s.id.schema != target.table.schema {
                continue;
            }
            if !s
                .indexes
                .iter()
                .any(|cols| names.iter().all(|n| cols.contains(n)))
            {
                continue;
            }
            if !names.iter().all(|c| s.columns.contains(c.as_str())) {
                continue;
            }
            let source = Endpoint {
                table: s.id.clone(),
                columns: names.clone(),
            };
            if existing.contains(&(&source, target)) || patterns.contains_key(&source) {
                continue;
            }
            let mut unknown = false;
            let incompatible = source
                .columns
                .iter()
                .zip(&target.columns)
                .any(|(left, right)| {
                    let result = types
                        .get(&(&source.table, left.as_str()))
                        .zip(types.get(&(&target.table, right.as_str())))
                        .and_then(|(a, b)| super::inference::compatible_types(a, b));
                    unknown |= result.is_none();
                    result == Some(false)
                });
            if incompatible {
                continue;
            }
            let mut reason =
                String::from("索引中的同名字段可以成组配对；目录不足以证明业务引用方向。");
            if !condition.required_target.is_empty() {
                reason.push_str("目标部分唯一键仅在指定条件下生效；尚无证据表明来源使用这些条件。");
            }
            if !condition.unbound_target_columns.is_empty() {
                reason.push_str(&format!(
                    "目标完整键还包含 {}，无法推定这些列的绑定或常量。",
                    condition.unbound_target_columns.join("、")
                ));
            }
            if unknown {
                reason.push_str("部分字段的标量类型或比较规则未证实。");
            }
            additions.push(Candidate {
                source,
                target: target.clone(),
                reason,
                condition: Some(condition.clone()),
            });
        }
    }
    // 有条件的候选不得被更弱的同端点无条件提示覆盖。
    let enriched: BTreeSet<_> = additions.iter().map(|c| (&c.source, &c.target)).collect();
    analysis
        .candidates
        .retain(|c| !enriched.contains(&(&c.source, &c.target)));
    analysis.candidates.extend(additions);
}
