//! 弱证据目标召回的行为测试：只通过 `infer_relations` 的输入/输出核对候选层。
//!
//! 这里使用通用的结构词（父级/来源/模块前缀）与中性的表名，不依赖具体业务名词。
use dbs_core::er::{
    build_diagram, infer_relations, Candidate, Endpoint, ErCatalog, ErColumnType, ErProject,
    TableId,
};
use dbs_core::{ColumnInfo, TableInfo};

fn col(name: &str, data_type: &str, pk: bool, comment: &str) -> ColumnInfo {
    ColumnInfo {
        order: 0,
        name: name.into(),
        data_type: data_type.into(),
        length: String::new(),
        is_primary_key: pk,
        is_auto_increment: false,
        nullable: !pk,
        default_value: String::new(),
        comment: comment.into(),
    }
}

fn table(schema: &str, name: &str, columns: Vec<ColumnInfo>) -> TableInfo {
    TableInfo {
        order: 0,
        schema: schema.into(),
        name: name.into(),
        comment: String::new(),
        columns,
        ..Default::default()
    }
}

fn ep(table: &TableInfo, columns: &[&str]) -> Endpoint {
    Endpoint {
        table: TableId::from(table),
        columns: columns.iter().map(|c| (*c).to_string()).collect(),
    }
}

fn catalog(tables: Vec<TableInfo>, keys: Vec<Endpoint>) -> ErCatalog {
    let types = tables
        .iter()
        .flat_map(|t| {
            t.columns.iter().map(move |c| ErColumnType {
                table: TableId::from(t),
                column: c.name.clone(),
                signature: c.data_type.clone(),
            })
        })
        .collect();
    ErCatalog {
        tables,
        keys,
        types,
        ..Default::default()
    }
}

fn weak_candidate(
    analysis: &dbs_core::er::Analysis,
    source: &Endpoint,
    target: &Endpoint,
) -> Candidate {
    analysis
        .candidates
        .iter()
        .find(|c| &c.source == source && &c.target == target)
        .unwrap_or_else(|| {
            panic!(
                "应召回候选 {} → {}：{:?}",
                source.table.label(),
                target.table.label(),
                analysis.candidates
            )
        })
        .clone()
}

fn assert_no_relation_from(analysis: &dbs_core::er::Analysis, source: &Endpoint) {
    assert!(
        !analysis.relations.iter().any(|r| &r.source == source),
        "弱证据不得自动连边 {}：{:?}",
        source.table.label(),
        analysis.relations
    );
}

#[test]
fn source_of_a_record_does_not_by_itself_mean_parent_in_the_same_table() {
    let obj = table(
        "main",
        "units",
        vec![
            col("id", "integer", true, ""),
            col("other_code", "varchar", false, ""),
            col("source_record", "varchar", false, ""),
        ],
    );
    let src = ep(&obj, &["source_record"]);
    let key = ep(&obj, &["other_code"]);
    let out = infer_relations(&catalog(vec![obj], vec![key.clone()]));
    assert!(!out
        .candidates
        .iter()
        .any(|c| c.source == src && c.target == key));
}

#[test]
fn shared_nested_namespaces_build_aliases_without_project_specific_prefixes() {
    let object = table(
        "main",
        "xyz_omega_objects",
        vec![
            col("id", "int", true, ""),
            col("public_no", "varchar", false, ""),
        ],
    );
    let reader = table(
        "main",
        "xyz_omega_entries",
        vec![col("object_code", "varchar", false, "")],
    );
    let independent = table(
        "main",
        "xyz_other_summary",
        vec![col("id", "int", true, "")],
    );
    let cat = catalog(
        vec![object.clone(), reader.clone(), independent],
        vec![ep(&object, &["id"]), ep(&object, &["public_no"])],
    );
    let src = ep(&reader, &["object_code"]);
    let target = ep(&object, &["public_no"]);
    let found = infer_relations(&cat);
    weak_candidate(&found, &src, &target);
    assert_no_relation_from(&found, &src);
    let mut missing = cat;
    missing.types.retain(|t| t.table != src.table);
    let unknown = infer_relations(&missing);
    assert!(!unknown
        .candidates
        .iter()
        .any(|c| c.source == src && c.target == target));
}

#[test]
fn parent_record_recalls_the_same_table_alternate_key_as_a_candidate() {
    let nodes = table(
        "main",
        "nodes",
        vec![
            col("id", "bigint", true, ""),
            col("node_code", "varchar", false, ""),
            col("parent_record", "varchar", false, ""),
            col("label", "varchar", false, ""),
        ],
    );
    let catalog = catalog(
        vec![nodes.clone()],
        vec![ep(&nodes, &["id"]), ep(&nodes, &["node_code"])],
    );

    let analysis = infer_relations(&catalog);
    let source = ep(&nodes, &["parent_record"]);
    let target = ep(&nodes, &["node_code"]);
    assert_no_relation_from(&analysis, &source);
    let candidate = weak_candidate(&analysis, &source, &target);
    assert_eq!(candidate.condition, None, "普通弱候选不应带条件");
    assert!(
        candidate.reason.contains("同表") || candidate.reason.contains("父子"),
        "理由需说明同表父子关系：{}",
        candidate.reason
    );
    // 存在备用业务键时，不把纯主键也拉成候选。
    assert!(
        !analysis
            .candidates
            .iter()
            .any(|c| c.source == source && c.target == ep(&nodes, &["id"])),
        "存在业务键时不应召回纯 ID：{:?}",
        analysis.candidates
    );

    // CLI/GUI 共用同一份候选投影，不另建召回通道。
    let diagram = build_diagram(&catalog, None, &ErProject::default()).unwrap();
    assert!(
        diagram
            .candidates
            .iter()
            .any(|c| c.source == source && c.target == target),
        "共享投影应保留弱候选：{:?}",
        diagram.candidates
    );
}

#[test]
fn source_prefixed_field_recalls_the_entity_business_key_without_an_edge() {
    let devices = table("main", "devices", vec![col("id", "varchar", true, "")]);
    let events = table(
        "main",
        "events",
        vec![
            col("id", "bigint", true, ""),
            col("source_device_code", "varchar", false, ""),
        ],
    );
    let catalog = catalog(
        vec![devices.clone(), events.clone()],
        vec![ep(&devices, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    let source = ep(&events, &["source_device_code"]);
    let target = ep(&devices, &["id"]);
    assert_no_relation_from(&analysis, &source);
    let candidate = weak_candidate(&analysis, &source, &target);
    assert!(
        candidate.reason.contains("前缀") || candidate.reason.contains("实体"),
        "理由需说明角色/模块前缀：{}",
        candidate.reason
    );
    assert!(
        candidate.reason.contains("主键") || candidate.reason.contains("回退"),
        "目标只有主键时需说明回退依据：{}",
        candidate.reason
    );
}

#[test]
fn module_prefixed_tables_and_role_prefixed_fields_are_still_recalled() {
    let accounts = table(
        "main",
        "core_accounts",
        vec![
            col("id", "bigint", true, ""),
            col("account_no", "varchar", false, ""),
        ],
    );
    let devices = table(
        "main",
        "core_devices",
        vec![
            col("id", "bigint", true, ""),
            col("device_no", "varchar", false, ""),
        ],
    );
    let events = table(
        "main",
        "core_events",
        vec![
            col("id", "bigint", true, ""),
            col("owner_account_code", "varchar", false, ""),
            col("device_code", "varchar", false, ""),
        ],
    );
    let catalog = catalog(
        vec![accounts.clone(), devices.clone(), events.clone()],
        vec![
            ep(&accounts, &["id"]),
            ep(&accounts, &["account_no"]),
            ep(&devices, &["id"]),
            ep(&devices, &["device_no"]),
        ],
    );

    let analysis = infer_relations(&catalog);
    let owner = ep(&events, &["owner_account_code"]);
    let device = ep(&events, &["device_code"]);
    assert_no_relation_from(&analysis, &owner);
    assert_no_relation_from(&analysis, &device);
    weak_candidate(&analysis, &owner, &ep(&accounts, &["account_no"]));
    weak_candidate(&analysis, &device, &ep(&devices, &["device_no"]));
    // 键列名不同，只能由实体词索引（含模块前缀变体）发现目标。
    assert!(
        !analysis
            .candidates
            .iter()
            .any(|c| c.source == owner && c.target == ep(&accounts, &["id"])),
        "{}",
        "键用途已明确为编号，不应把纯 ID 也作为同强度候选"
    );
}

#[test]
fn record_suffix_on_the_source_field_still_reaches_the_entity() {
    let devices = table(
        "main",
        "devices",
        vec![
            col("id", "bigint", true, ""),
            col("device_code", "varchar", false, ""),
        ],
    );
    let events = table(
        "main",
        "events",
        vec![
            col("id", "bigint", true, ""),
            col("source_device_record", "varchar", false, ""),
        ],
    );
    let catalog = catalog(
        vec![devices.clone(), events.clone()],
        vec![ep(&devices, &["id"]), ep(&devices, &["device_code"])],
    );

    let analysis = infer_relations(&catalog);
    let source = ep(&events, &["source_device_record"]);
    assert_no_relation_from(&analysis, &source);
    let candidate = weak_candidate(&analysis, &source, &ep(&devices, &["device_code"]));
    assert!(
        candidate.reason.contains("实体") || candidate.reason.contains("后缀"),
        "理由需说明记录后缀已剥离：{}",
        candidate.reason
    );
}

#[test]
fn one_sided_selection_keeps_the_out_of_scope_weak_target() {
    let devices = table("other", "devices", vec![col("id", "varchar", true, "")]);
    let events = table(
        "main",
        "events",
        vec![
            col("id", "bigint", true, ""),
            col("source_device_code", "varchar", false, ""),
        ],
    );
    let catalog = catalog(
        vec![devices.clone(), events.clone()],
        vec![ep(&devices, &["id"])],
    );
    let selected = [TableId::from(&events)];

    let diagram = build_diagram(&catalog, Some(&selected), &ErProject::default()).unwrap();
    assert_eq!(diagram.tables.len(), 1);
    assert!(diagram.relations.is_empty(), "{:?}", diagram.relations);
    assert_eq!(diagram.external_candidate_count, 1);
    assert!(
        diagram.candidates.iter().any(|c| {
            c.source == ep(&events, &["source_device_code"]) && c.target == ep(&devices, &["id"])
        }),
        "只选来源端时仍应保留范围外弱目标：{:?}",
        diagram.candidates
    );
}

#[test]
fn multiple_weak_targets_stay_as_real_competition() {
    let first = table(
        "main",
        "alpha_nodes",
        vec![
            col("id", "bigint", true, ""),
            col("node_code", "varchar", false, ""),
        ],
    );
    let second = table(
        "main",
        "beta_nodes",
        vec![
            col("id", "bigint", true, ""),
            col("node_code", "varchar", false, ""),
        ],
    );
    let events = table(
        "main",
        "events",
        vec![
            col("id", "bigint", true, ""),
            col("source_node_code", "varchar", false, ""),
        ],
    );
    let catalog = catalog(
        vec![first.clone(), second.clone(), events.clone()],
        vec![
            ep(&first, &["id"]),
            ep(&first, &["node_code"]),
            ep(&second, &["id"]),
            ep(&second, &["node_code"]),
        ],
    );

    let analysis = infer_relations(&catalog);
    let source = ep(&events, &["source_node_code"]);
    assert_no_relation_from(&analysis, &source);
    let left = weak_candidate(&analysis, &source, &ep(&first, &["node_code"]));
    let right = weak_candidate(&analysis, &source, &ep(&second, &["node_code"]));
    assert!(
        left.reason.contains("竞争") && right.reason.contains("竞争"),
        "竞争目标需在理由中共存：{} / {}",
        left.reason,
        right.reason
    );
}

#[test]
fn pure_id_status_audit_and_unknown_entities_do_not_expand_candidates() {
    let devices = table(
        "main",
        "devices",
        vec![
            col("id", "bigint", true, ""),
            col("device_code", "varchar", false, ""),
        ],
    );
    let events = table(
        "main",
        "events",
        vec![
            col("id", "varchar", true, ""),
            col("status", "varchar", false, ""),
            col("created_at", "timestamp", false, ""),
            col("creator_id", "bigint", false, ""),
            col("record_code", "varchar", false, ""),
            col("source_status", "varchar", false, ""),
            col("created_by_user_code", "varchar", false, ""),
            col("source_mystery_code", "varchar", false, ""),
        ],
    );
    let users = table(
        "main",
        "users",
        vec![
            col("id", "varchar", true, ""),
            col("user_code", "varchar", false, ""),
        ],
    );
    let catalog = catalog(
        vec![devices.clone(), events.clone(), users.clone()],
        vec![
            ep(&devices, &["id"]),
            ep(&devices, &["device_code"]),
            ep(&events, &["id"]),
            ep(&users, &["id"]),
            ep(&users, &["user_code"]),
        ],
    );

    let analysis = infer_relations(&catalog);
    assert!(
        analysis.candidates.is_empty(),
        "纯 ID/状态/审计列与无实体目标的字段不得扩展候选：{:?}",
        analysis.candidates
    );
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
}

#[test]
fn same_table_key_column_and_high_frequency_keys_do_not_fan_out() {
    let items = table(
        "main",
        "items",
        vec![
            col("id", "bigint", true, ""),
            col("item_code", "varchar", false, ""),
        ],
    );
    let same = catalog(
        vec![items.clone()],
        vec![ep(&items, &["id"]), ep(&items, &["item_code"])],
    );
    let analysis = infer_relations(&same);
    assert!(
        !analysis
            .candidates
            .iter()
            .any(|c| c.source == ep(&items, &["item_code"])),
        "同表同字段的键列不得召回自身：{:?}",
        analysis.candidates
    );

    let mut tables = Vec::new();
    for name in ["alpha", "beta", "gamma", "delta"] {
        tables.push(table(
            "main",
            name,
            vec![
                col("id", "bigint", true, ""),
                col("widget_code", "varchar", false, ""),
            ],
        ));
    }
    let events = table(
        "main",
        "events",
        vec![
            col("id", "bigint", true, ""),
            col("widget_code", "varchar", false, ""),
        ],
    );
    tables.push(events);
    let keys = tables
        .iter()
        .flat_map(|t| [ep(t, &["id"]), ep(t, &["widget_code"])])
        .collect();
    let spread = infer_relations(&catalog(tables, keys));
    assert!(
        spread.candidates.is_empty(),
        "高频键不得向多张表扇出候选：{:?}",
        spread.candidates
    );
}

#[test]
fn existing_naming_relation_is_not_duplicated_or_downgraded() {
    let users = table("main", "users", vec![col("id", "bigint", true, "")]);
    let orders = table(
        "main",
        "orders",
        vec![
            col("id", "bigint", true, ""),
            col("user_id", "bigint", false, ""),
        ],
    );
    let catalog = catalog(
        vec![users.clone(), orders.clone()],
        vec![ep(&users, &["id"]), ep(&orders, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    let source = ep(&orders, &["user_id"]);
    let target = ep(&users, &["id"]);
    assert!(
        analysis
            .relations
            .iter()
            .any(|r| r.source == source && r.target == target),
        "原有命名自动线必须保留：{:?}",
        analysis.relations
    );
    assert!(
        !analysis
            .candidates
            .iter()
            .any(|c| c.source == source && c.target == target),
        "同一端点不得重复降级为候选：{:?}",
        analysis.candidates
    );
}
