use dbs_core::er::{
    build_delivery_diagram, Candidate, Endpoint, ErCatalog, ErProject, ProjectScope, Relation,
    RelationKind, ScopedWarning, TableId,
};
use dbs_core::{ColumnInfo, DbType, ForeignKey, TableInfo};
use dbs_doc::{render_er_report, render_er_svg};

fn table(name: &str, columns: &[&str]) -> TableInfo {
    TableInfo {
        schema: "main".into(),
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

fn catalog() -> (ErCatalog, TableInfo, TableInfo, TableInfo, TableInfo) {
    let mut alpha = table("alpha", &["id", "link"]);
    alpha.comment = "数据库作者的业务注释".into();
    alpha.note = "生成备注：参考 gamma".into();
    let beta = table("beta", &["id"]);
    let gamma = table("gamma", &["id"]);
    let delta = table("delta", &["id"]);
    alpha.foreign_keys.push(ForeignKey {
        name: "fk_external".into(),
        columns: vec!["link".into()],
        ref_schema: "main".into(),
        ref_table: "gamma".into(),
        ref_columns: vec!["id".into()],
    });
    let mut catalog = ErCatalog {
        tables: vec![alpha.clone(), beta.clone(), gamma.clone(), delta.clone()],
        keys: vec![end(&beta, &["id"]), end(&gamma, &["id"])],
        warnings: vec!["全局提示：涉及 gamma".into()],
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
        reason: "视图定义引用 gamma".into(),
    });
    catalog.document_links.candidates.push(candidate(
        &end(&alpha, &["link"]),
        &end(&delta, &["id"]),
        "注释引用 delta",
    ));
    (catalog, alpha, beta, gamma, delta)
}

fn scope() -> ProjectScope {
    ProjectScope {
        db_type: DbType::PostgreSql,
        host: "localhost".into(),
        port: 5432,
        database: "demo".into(),
    }
}

#[test]
fn selected_exports_keep_the_non_null_scope_and_the_safe_kind_of_evidence() {
    use dbs_core::er::{DecisionAction, ErColumnType, ErSnapshot, EvidenceKind};
    let source = table("events", &["id", "beacon_code"]);
    let mut target = table("beacons", &["id", "code"]);
    target.columns[1].nullable = true;
    let outside = table("private_audit", &["id"]);
    let from = end(&source, &["beacon_code"]);
    let to = end(&target, &["code"]);
    let snapshot = ErSnapshot::new(ErCatalog {
        db_type: Some(DbType::MySql),
        non_null_keys: vec![to.clone()],
        types: [&from, &to]
            .iter()
            .map(|e| ErColumnType {
                table: e.table.clone(),
                column: e.columns[0].clone(),
                signature: "int".into(),
            })
            .collect(),
        tables: vec![source, target, outside],
        ..Default::default()
    });
    let selected = [from.table.clone(), to.table.clone()];
    let delivered = snapshot
        .delivery(Some(&selected), &ErProject::default())
        .unwrap();
    assert_eq!(delivered.relations.len(), 1);
    let relation = &delivered.relations[0];
    let condition = relation.condition.as_ref().unwrap();
    assert!(condition.non_null_equality);
    assert_eq!(condition.evidence_kind, Some(EvidenceKind::Naming));
    assert!(relation.reason.contains("常规命名"));
    assert!(delivered.keys.is_empty());
    let markdown = render_er_report(&delivered, "选区", &scope(), false);
    assert!(markdown.contains("普通等号") && markdown.contains("至多一行"));
    assert!(!markdown.contains("private_audit"));
    assert!(snapshot
        .decision(from.clone(), to.clone(), DecisionAction::Confirm)
        .is_err());
    let excluded = snapshot
        .decision(from, to, DecisionAction::Exclude)
        .unwrap();
    let project = ErProject {
        decisions: vec![excluded],
        ..Default::default()
    };
    assert!(snapshot
        .diagram(Some(&selected), &project)
        .unwrap()
        .relations
        .is_empty());
}

#[test]
fn partial_svg_json_and_markdown_share_the_same_delivered_warnings_and_hide_external_names() {
    let (catalog, alpha, beta, _gamma, _delta) = catalog();
    let selected = [TableId::from(&alpha), TableId::from(&beta)];
    let delivered =
        build_delivery_diagram(&catalog, Some(&selected), &ErProject::default()).unwrap();
    let json = serde_json::to_string_pretty(&delivered).unwrap();
    let svg = render_er_svg(&delivered, "alpha 关系图", false);
    let markdown = render_er_report(&delivered, "alpha 关系图", &scope(), false);

    for output in [&json, &svg, &markdown] {
        assert!(
            !output.contains("gamma") && !output.contains("delta"),
            "交付物泄露图外名字：\n{output}"
        );
        assert!(
            output.contains("alpha 的键提示"),
            "缺少可见表提示：\n{output}"
        );
        assert!(
            output.contains("跨范围候选") || output.contains("全部选表时显示"),
            "应保留跨边界数量：\n{output}"
        );
    }
    assert!(svg.contains("跨范围候选：1"), "{svg}");
    assert!(
        !svg.contains("class=\"edge candidate\""),
        "候选不能画成连线：{svg}"
    );
    assert!(
        svg.contains("候选只计数，不画成连线"),
        "SVG 应说明候选不画线：{svg}"
    );
    assert_eq!(
        delivered.tables[0].comment, "数据库作者的业务注释",
        "数据库作者注释必须保留"
    );
}

#[test]
fn full_selection_keeps_the_original_diagnostics_and_candidates() {
    let (catalog, alpha, beta, _gamma, _delta) = catalog();
    let selected = [TableId::from(&alpha), TableId::from(&beta)];
    let partial = build_delivery_diagram(&catalog, Some(&selected), &ErProject::default()).unwrap();
    assert!(partial.candidates.is_empty());
    assert_eq!(partial.external_candidate_count, 1);

    let full = build_delivery_diagram(&catalog, None, &ErProject::default()).unwrap();
    let json = serde_json::to_string_pretty(&full).unwrap();
    let svg = render_er_svg(&full, "全部关系图", false);
    let markdown = render_er_report(&full, "全部关系图", &scope(), false);
    for output in [&json, &svg, &markdown] {
        assert!(output.contains("gamma 的键提示"), "{output}");
        assert!(output.contains("全局提示：涉及 gamma"), "{output}");
    }
    assert!(full
        .candidates
        .iter()
        .any(|candidate| candidate.reason.contains("delta")));
    assert_eq!(full.external_candidate_count, 0);
    assert_eq!(full.tables[0].foreign_keys.len(), 1);
}
