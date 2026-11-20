use dbs_core::er::{
    build_delivery_diagram, build_diagram, deliver_diagram, Candidate, Endpoint, ErCatalog,
    ErDiagram, ErProject, Relation, RelationKind, ScopedWarning, TableId,
};
use dbs_core::{ColumnInfo, ForeignKey, TableInfo};

fn table(schema: &str, name: &str, columns: &[&str]) -> TableInfo {
    TableInfo {
        schema: schema.into(),
        name: name.into(),
        columns: columns
            .iter()
            .enumerate()
            .map(|(index, name)| ColumnInfo {
                order: index as u32 + 1,
                name: (*name).into(),
                data_type: "integer".into(),
                length: String::new(),
                is_primary_key: index == 0,
                is_auto_increment: false,
                nullable: false,
                default_value: String::new(),
                comment: String::new(),
            })
            .collect(),
        ..Default::default()
    }
}

fn end(table: &TableInfo, columns: &[&str]) -> Endpoint {
    Endpoint {
        table: TableId::from(table),
        columns: columns.iter().map(|name| (*name).to_string()).collect(),
    }
}

fn candidate(source: &Endpoint, target: &Endpoint, reason: &str) -> Candidate {
    Candidate {
        source: source.clone(),
        target: target.clone(),
        reason: reason.into(),
        condition: None,
    }
}

#[test]
fn selecting_another_schema_or_case_variant_does_not_disclose_foreign_key_metadata() {
    let mut a = table("owned", "source", &["id", "link"]);
    let outside = table("outside", "Target", &["id"]);
    let visible = table("owned", "target", &["id"]);
    let mut fk = external_foreign_key(&outside);
    fk.ref_schema.clear();
    a.foreign_keys.push(fk);
    let id = TableId::from(&a);
    let other = TableId::from(&visible);
    let staged = ErCatalog {
        tables: vec![a, outside, visible],
        ..Default::default()
    };
    let out = build_delivery_diagram(&staged, Some(&[id, other]), &ErProject::default()).unwrap();
    assert!(out.tables.iter().all(|t| t.foreign_keys.is_empty()));
}

fn external_foreign_key(target: &TableInfo) -> ForeignKey {
    ForeignKey {
        name: "fk_external".into(),
        columns: vec!["link".into()],
        ref_schema: target.schema.clone(),
        ref_table: target.name.clone(),
        ref_columns: vec!["id".into()],
    }
}

fn assert_absent(text: &str, hidden: &[&str]) {
    for needle in hidden {
        assert!(!text.contains(needle), "不应泄露「{needle}」：\n{text}");
    }
}

#[test]
fn partial_delivery_keeps_visible_adjudication_but_hides_external_endpoints() {
    let mut alpha = table("main", "alpha", &["id", "link"]);
    alpha.comment = "作者注释：alpha 的业务说明".into();
    alpha.note = "生成备注：参考 gamma".into();
    alpha
        .foreign_keys
        .push(external_foreign_key(&table("main", "gamma", &["id"])));
    let beta = table("main", "beta", &["id"]);
    let gamma = table("main", "gamma", &["id"]);
    let delta = table("main", "delta", &["id"]);
    let mut catalog = ErCatalog {
        tables: vec![alpha.clone(), beta.clone(), gamma.clone(), delta.clone()],
        keys: vec![end(&beta, &["id"]), end(&gamma, &["id"])],
        warnings: vec!["全局提示：涉及 delta".into()],
        scoped_warnings: vec![
            ScopedWarning {
                table: TableId::from(&alpha),
                message: "alpha 的键提示".into(),
            },
            ScopedWarning {
                table: TableId::from(&gamma),
                message: "gamma 的键提示".into(),
            },
        ],
        ..Default::default()
    };
    catalog.document_links.relations.push(Relation {
        condition: None,
        source: end(&alpha, &["link"]),
        target: end(&beta, &["id"]),
        kind: RelationKind::Lookup,
        reason: "视图 view_hidden 引用 gamma".into(),
    });
    catalog.document_links.candidates.push(candidate(
        &end(&alpha, &["link"]),
        &end(&delta, &["id"]),
        "注释引用 delta",
    ));
    let selected = [TableId::from(&alpha), TableId::from(&beta)];

    let local = build_diagram(&catalog, Some(&selected), &ErProject::default()).unwrap();
    assert_eq!(local.tables.len(), 2);
    assert_eq!(local.relations.len(), 1);
    assert_eq!(local.candidates.len(), 1);
    assert_eq!(local.external_candidate_count, 1);
    assert!(local.candidates[0].reason.contains("delta"));
    assert_eq!(local.tables[0].note, "生成备注：参考 gamma");
    assert_eq!(local.tables[0].foreign_keys.len(), 1);
    assert!(local
        .warnings
        .iter()
        .any(|warning| warning == "alpha 的键提示"));
    assert!(local
        .warnings
        .iter()
        .any(|warning| warning.contains("仅在全部选表时显示")));
    assert_absent(&local.warnings.join("\n"), &["gamma", "delta"]);

    let delivered =
        build_delivery_diagram(&catalog, Some(&selected), &ErProject::default()).unwrap();
    assert_eq!(delivered.tables.len(), 2);
    assert_eq!(delivered.relations.len(), 1);
    assert!(delivered.candidates.is_empty());
    assert_eq!(delivered.external_candidate_count, 1);
    assert_eq!(delivered.tables[0].foreign_keys.len(), 0);
    assert!(delivered.tables[0].note.is_empty());
    assert_eq!(
        delivered.tables[0].comment, "作者注释：alpha 的业务说明",
        "数据库作者的注释必须保留"
    );
    assert_eq!(delivered.relations[0].source, local.relations[0].source);
    assert_eq!(delivered.relations[0].target, local.relations[0].target);
    assert_eq!(delivered.relations[0].kind, local.relations[0].kind);
    assert_absent(&delivered.relations[0].reason, &["gamma", "view_hidden"]);
    assert_absent(&delivered.warnings.join("\n"), &["gamma", "delta"]);

    let delivered_again = deliver_diagram(&delivered, false);
    assert_eq!(delivered_again.external_candidate_count, 1);
    assert!(delivered_again.candidates.is_empty());
    assert_absent(&delivered_again.warnings.join("\n"), &["gamma", "delta"]);

    let full = build_delivery_diagram(&catalog, None, &ErProject::default()).unwrap();
    assert_eq!(full.tables.len(), 4);
    assert_eq!(full.candidates.len(), 1);
    assert_eq!(full.external_candidate_count, 0);
    assert_eq!(full.tables[0].foreign_keys.len(), 1);
    assert!(full
        .warnings
        .iter()
        .any(|warning| warning == "gamma 的键提示"));
    assert!(full
        .warnings
        .iter()
        .any(|warning| warning == "全局提示：涉及 delta"));
    assert!(
        full.relations[0].reason.contains("view_hidden"),
        "全选时保留原始理由：{}",
        full.relations[0].reason
    );

    let all: Vec<TableId> = catalog.tables.iter().map(TableId::from).collect();
    let explicit_full =
        build_delivery_diagram(&catalog, Some(&all), &ErProject::default()).unwrap();
    assert_eq!(explicit_full.warnings, full.warnings);
    assert_eq!(explicit_full.tables.len(), full.tables.len());
    assert_eq!(explicit_full.external_candidate_count, 0);
    assert_eq!(explicit_full.tables[0].foreign_keys.len(), 1);
}

#[test]
fn same_named_tables_in_two_schemas_are_scoped_by_structured_id() {
    let first = table("first", "items", &["id"]);
    let second = table("second", "items", &["id"]);
    let mut first_with_fk = first.clone();
    first_with_fk
        .foreign_keys
        .push(external_foreign_key(&second));
    let mut catalog = ErCatalog {
        tables: vec![first_with_fk.clone(), second.clone()],
        keys: vec![end(&first, &["id"]), end(&second, &["id"])],
        warnings: vec!["全局提示：second 未选".into()],
        scoped_warnings: vec![
            ScopedWarning {
                table: TableId::from(&first),
                message: "first.items 的提示".into(),
            },
            ScopedWarning {
                table: TableId::from(&second),
                message: "second.items 的提示".into(),
            },
        ],
        ..Default::default()
    };
    catalog.document_links.candidates.push(candidate(
        &end(&first, &["id"]),
        &end(&second, &["id"]),
        "second.items 的候选理由",
    ));
    let selected = [TableId::from(&first)];

    let delivered =
        build_delivery_diagram(&catalog, Some(&selected), &ErProject::default()).unwrap();
    assert_eq!(delivered.tables.len(), 1);
    assert_eq!(delivered.tables[0].schema, "first");
    assert!(delivered.tables[0].foreign_keys.is_empty());
    assert!(delivered.candidates.is_empty());
    assert_eq!(delivered.external_candidate_count, 1);
    assert!(delivered
        .warnings
        .iter()
        .any(|warning| warning == "first.items 的提示"));
    assert_absent(&delivered.warnings.join("\n"), &["second"]);

    let full = build_delivery_diagram(&catalog, None, &ErProject::default()).unwrap();
    assert_eq!(full.warnings.len(), 3);
    assert!(full
        .warnings
        .iter()
        .any(|warning| warning == "second.items 的提示"));
    assert_eq!(full.tables[0].foreign_keys.len(), 1);
    assert_eq!(full.candidates.len(), 1);
}

#[test]
fn delivery_never_turns_a_candidate_into_an_edge() {
    let alpha = table("main", "alpha", &["id"]);
    let beta = table("main", "beta", &["id"]);
    let gamma = table("main", "gamma", &["id"]);
    let mut catalog = ErCatalog {
        tables: vec![alpha.clone(), beta.clone(), gamma.clone()],
        keys: vec![end(&beta, &["id"]), end(&gamma, &["id"])],
        ..Default::default()
    };
    catalog.document_links.relations.push(Relation {
        condition: None,
        source: end(&alpha, &["id"]),
        target: end(&beta, &["id"]),
        kind: RelationKind::Lookup,
        reason: "视图定义".into(),
    });
    catalog.document_links.candidates.push(candidate(
        &end(&alpha, &["id"]),
        &end(&gamma, &["id"]),
        "候选",
    ));
    let selected = [TableId::from(&alpha), TableId::from(&beta)];
    let delivered =
        build_delivery_diagram(&catalog, Some(&selected), &ErProject::default()).unwrap();
    assert_eq!(delivered.relations.len(), 1);
    assert_eq!(delivered.relations[0].kind, RelationKind::Lookup);
    assert!(delivered.candidates.is_empty());
    assert_eq!(delivered.external_candidate_count, 1);
}

#[test]
fn delivery_projection_rewrites_unprovable_generated_text_but_keeps_comments() {
    let mut alpha = table("main", "alpha", &["id"]);
    alpha.comment = "数据库作者写的说明".into();
    alpha.note = "生成备注：参考 gamma".into();
    let beta = table("main", "beta", &["id"]);
    let alpha_end = end(&alpha, &["id"]);
    let beta_end = end(&beta, &["id"]);
    let relation = Relation {
        condition: None,
        source: alpha_end.clone(),
        target: beta_end.clone(),
        kind: RelationKind::Inferred,
        reason: "字段注释引用 gamma".into(),
    };
    let diagram = ErDiagram {
        tables: vec![alpha, beta],
        relations: vec![relation],
        candidates: vec![candidate(&alpha_end, &beta_end, "gamma 候选")],
        ..Default::default()
    };
    let delivered = deliver_diagram(&diagram, false);
    assert_eq!(delivered.tables[0].comment, "数据库作者写的说明");
    assert!(delivered.tables[0].note.is_empty());
    assert_absent(&delivered.relations[0].reason, &["gamma"]);
    assert_absent(&delivered.candidates[0].reason, &["gamma"]);
}
