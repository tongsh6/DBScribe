use dbs_core::er::{
    build_diagram, make_decision, DecisionAction, Endpoint, ErCatalog, ErColumnType, ErProject,
    RelationKind, TableId,
};
use dbs_core::TableInfo;

#[test]
fn selecting_one_of_two_namesakes_keeps_its_schema_and_isolated_node() {
    let tables = ["first", "second"]
        .map(|schema| TableInfo {
            schema: schema.into(),
            name: "items".into(),
            ..Default::default()
        })
        .to_vec();
    let catalog = ErCatalog {
        tables,
        ..Default::default()
    };
    let selected = [TableId {
        schema: "second".into(),
        name: "items".into(),
    }];
    let diagram = build_diagram(&catalog, Some(&selected), &ErProject::default()).unwrap();
    assert_eq!(diagram.tables.len(), 1);
    assert_eq!(diagram.tables[0].schema, "second");
}

#[test]
fn a_catalog_lookup_is_view_evidence_even_without_declared_field_types() {
    let src = table("events", "owner");
    let dst = table("profiles", "_id");
    let from = Endpoint {
        table: TableId::from(&src),
        columns: vec!["owner".into()],
    };
    let to = Endpoint {
        table: TableId::from(&dst),
        columns: vec!["_id".into()],
    };
    let mut cat = ErCatalog {
        tables: vec![src, dst],
        ..Default::default()
    };
    cat.document_links.relations.push(dbs_core::er::Relation {
        condition: None,
        source: from,
        target: to,
        kind: RelationKind::Lookup,
        reason: "视图定义中的 $lookup".into(),
    });
    let g = build_diagram(&cat, None, &ErProject::default()).unwrap();
    assert_eq!(g.relations.len(), 1);
    assert_eq!(g.relations[0].kind, RelationKind::Lookup);
}

fn table(name: &str, col: &str) -> TableInfo {
    TableInfo {
        schema: "main".into(),
        name: name.into(),
        columns: vec![dbs_core::ColumnInfo {
            order: 1,
            name: col.into(),
            data_type: "integer".into(),
            length: String::new(),
            is_primary_key: false,
            is_auto_increment: false,
            nullable: false,
            default_value: String::new(),
            comment: String::new(),
        }],
        ..Default::default()
    }
}

fn manual_catalog() -> (ErCatalog, Endpoint, Endpoint) {
    let left = table("left_side", "link");
    let right = table("right_side", "value");
    let source = Endpoint {
        table: TableId::from(&left),
        columns: vec!["link".into()],
    };
    let target = Endpoint {
        table: TableId::from(&right),
        columns: vec!["value".into()],
    };
    let types = [&source, &target]
        .into_iter()
        .map(|e| ErColumnType {
            table: e.table.clone(),
            column: e.columns[0].clone(),
            signature: "integer".into(),
        })
        .collect();
    let catalog = ErCatalog {
        tables: vec![left, right],
        keys: vec![target.clone()],
        types,
        ..Default::default()
    };
    (catalog, source, target)
}

#[test]
fn a_confirmed_relation_is_rechecked_after_the_key_changes() {
    let (mut catalog, source, target) = manual_catalog();
    let decision = make_decision(&catalog, source, target, DecisionAction::Confirm).unwrap();
    let project = ErProject {
        decisions: vec![decision],
        ..Default::default()
    };
    let diagram = build_diagram(&catalog, None, &project).unwrap();
    assert_eq!(diagram.relations.len(), 1);
    assert_eq!(diagram.relations[0].kind, RelationKind::Confirmed);
    catalog.keys.clear();
    let stale = build_diagram(&catalog, None, &project).unwrap();
    assert!(stale.relations.is_empty());
    assert_eq!(stale.warnings.len(), 1);
}

#[test]
fn excluded_and_confirmed_decisions_for_one_mapping_require_review() {
    let (catalog, source, target) = manual_catalog();
    let decisions = [DecisionAction::Confirm, DecisionAction::Exclude]
        .into_iter()
        .map(|action| make_decision(&catalog, source.clone(), target.clone(), action).unwrap())
        .collect();
    let result = build_diagram(
        &catalog,
        None,
        &ErProject {
            decisions,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(result.relations.is_empty());
    assert!(result
        .warnings
        .iter()
        .any(|warning| warning.contains("冲突")));
}

#[test]
fn an_empty_selection_does_not_export_the_entire_catalog() {
    let (catalog, _, _) = manual_catalog();
    assert!(build_diagram(&catalog, Some(&[]), &ErProject::default()).is_err());
}

#[test]
fn an_identity_mapping_cannot_be_added_as_a_relation() {
    let (catalog, _, target) = manual_catalog();
    assert!(make_decision(&catalog, target.clone(), target, DecisionAction::Confirm).is_err());
}

#[test]
fn switching_the_preview_selection_keeps_the_catalog_available_for_corrections() {
    let (catalog, source, target) = manual_catalog();
    let snapshot = dbs_core::er::ErSnapshot::new(catalog);
    let decision = snapshot
        .decision(source.clone(), target.clone(), DecisionAction::Confirm)
        .unwrap();
    let project = ErProject {
        decisions: vec![decision],
        ..Default::default()
    };
    let subset = snapshot.diagram(Some(&[source.table]), &project).unwrap();
    assert_eq!(subset.tables.len(), 1);
    assert_eq!(subset.external_count, 1);
    assert!(subset.relations.is_empty());
    let all = snapshot.diagram(None, &project).unwrap();
    assert_eq!(all.relations[0].target, target);
}

#[test]
fn a_new_database_constraint_supersedes_the_same_manual_confirmation() {
    let (mut catalog, source, target) = manual_catalog();
    let decision = make_decision(
        &catalog,
        source.clone(),
        target.clone(),
        DecisionAction::Confirm,
    )
    .unwrap();
    catalog.tables[0].foreign_keys.push(dbs_core::ForeignKey {
        name: "declared_ref".into(),
        columns: source.columns,
        ref_schema: target.table.schema,
        ref_table: target.table.name,
        ref_columns: target.columns,
    });
    let project = ErProject {
        decisions: vec![decision],
        ..Default::default()
    };
    let diagram = build_diagram(&catalog, None, &project).unwrap();
    assert_eq!(diagram.relations.len(), 1);
    assert_eq!(diagram.relations[0].kind, RelationKind::Declared);
    assert!(diagram.warnings.is_empty());
}

#[test]
fn automatic_inference_preserves_database_type_qualifiers() {
    let (mut catalog, source, target) = manual_catalog();
    catalog.tables[0].columns[0].comment = "引用 main.right_side.value".into();
    for (a, b) in [
        ("int unsigned", "int"),
        ("bigint", "smallint"),
        (
            "varchar(64) collate utf8mb4_bin",
            "varchar(64) collate utf8mb4_general_ci",
        ),
        ("decimal(18,4)", "decimal(18,0)"),
    ] {
        catalog.types[0].signature = a.into();
        catalog.types[1].signature = b.into();
        let graph = dbs_core::er::infer_relations(&catalog);
        assert!(
            !graph
                .relations
                .iter()
                .any(|r| r.source == source && r.target == target),
            "{a} / {b}"
        );
    }
}
