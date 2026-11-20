use dbs_core::er::{
    infer_relations, make_decision, DecisionAction, Endpoint, ErCatalog, ErColumnType, GuardedKey,
    RequiredEquality, ScalarValue, TableId,
};
use dbs_core::{ColumnInfo, IndexInfo, TableInfo};

fn table(name: &str, names: &[&str]) -> TableInfo {
    let columns = names
        .iter()
        .enumerate()
        .map(|(i, n)| ColumnInfo {
            order: i as u32 + 1,
            name: (*n).into(),
            data_type: if *n == "archived" {
                "boolean"
            } else {
                "varchar"
            }
            .into(),
            length: String::new(),
            is_primary_key: false,
            is_auto_increment: false,
            nullable: false,
            default_value: String::new(),
            comment: String::new(),
        })
        .collect();
    TableInfo {
        name: name.into(),
        schema: "core".into(),
        columns,
        indexes: vec![IndexInfo {
            name: "idx".into(),
            columns: names.iter().map(|n| (*n).into()).collect(),
            is_unique: false,
            is_primary: false,
        }],
        ..Default::default()
    }
}
fn end(t: &TableInfo, cols: &[&str]) -> Endpoint {
    Endpoint {
        table: TableId::from(t),
        columns: cols.iter().map(|s| (*s).into()).collect(),
    }
}
#[test]
fn scoped_binding_requires_context_even_when_two_unique_targets_fit() {
    let src = table("lines", &["tenant_id", "actor_code"]);
    let left = table("actors", &["tenant_id", "actor_code", "archived"]);
    let right = table("actor_statistics", &["tenant_id", "actor_code", "archived"]);
    let mut cat = ErCatalog {
        keys: vec![
            end(&left, &["tenant_id", "actor_code", "archived"]),
            end(&right, &["tenant_id", "actor_code", "archived"]),
        ],
        tables: vec![src.clone(), left.clone(), right.clone()],
        ..Default::default()
    };
    for t in &cat.tables {
        for c in &t.columns {
            cat.types.push(ErColumnType {
                table: TableId::from(t),
                column: c.name.clone(),
                signature: c.data_type.clone(),
            });
        }
    }
    let from = end(&src, &["tenant_id", "actor_code"]);
    let result = infer_relations(&cat);
    for t in [&left, &right] {
        let target = end(t, &["tenant_id", "actor_code"]);
        let found = result
            .candidates
            .iter()
            .find(|c| c.source == from && c.target == target)
            .unwrap();
        assert_eq!(
            found.condition.as_ref().unwrap().unbound_target_columns,
            ["archived"]
        );
        assert!(found.condition.as_ref().unwrap().required_target.is_empty());
        assert!(make_decision(&cat, from.clone(), target, DecisionAction::Confirm).is_err());
    }
    assert!(!result.relations.iter().any(|r| r.source == from));
}
#[test]
fn an_indexed_tuple_does_not_require_domain_words_or_english_identifier_suffixes() {
    let src = table("r", &["甲", "乙"]);
    let dst = table("s", &["甲", "乙"]);
    let to = end(&dst, &["甲", "乙"]);
    let from = end(&src, &["甲", "乙"]);
    let cat = ErCatalog {
        tables: vec![src, dst],
        guarded_keys: vec![GuardedKey {
            key: to.clone(),
            required_target: vec![RequiredEquality {
                column: "层".into(),
                value: ScalarValue::Integer(2),
            }],
        }],
        ..Default::default()
    };
    let graph = infer_relations(&cat);
    assert!(graph
        .candidates
        .iter()
        .any(|c| c.source == from && c.target == to && c.condition.is_some()));
    assert!(graph.relations.is_empty());
}

#[test]
fn a_partial_key_records_the_real_filter_and_never_becomes_an_automatic_reference() {
    let src = table("entries", &["tenant_id", "actor_code"]);
    let target = table("actors", &["tenant_id", "actor_code"]);
    let to = end(&target, &["tenant_id", "actor_code"]);
    let from = end(&src, &["tenant_id", "actor_code"]);
    let term = RequiredEquality {
        column: "enabled".into(),
        value: ScalarValue::Bool(false),
    };
    let cat = ErCatalog {
        guarded_keys: vec![GuardedKey {
            key: to.clone(),
            required_target: vec![term.clone()],
        }],
        tables: vec![src, target],
        ..Default::default()
    };
    let result = infer_relations(&cat);
    let candidate = result
        .candidates
        .iter()
        .find(|r| r.source == from && r.target == to)
        .unwrap();
    assert_eq!(
        candidate.condition.as_ref().unwrap().required_target,
        vec![term]
    );
    assert!(result.relations.is_empty());
}
