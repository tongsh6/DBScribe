//! ER 关系推断的端到端行为测试：只通过 `infer_relations` 的输入/输出核对裁决结果。
use dbs_core::er::{
    infer_relations, Analysis, Endpoint, ErCatalog, ErColumnType, RelationKind, TableId,
    ViewComparison,
};
use dbs_core::{ColumnInfo, ForeignKey, TableInfo};

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

fn table(schema: &str, name: &str, comment: &str, columns: Vec<ColumnInfo>) -> TableInfo {
    TableInfo {
        order: 0,
        schema: schema.into(),
        name: name.into(),
        comment: comment.into(),
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

fn assert_no_relation_to(analysis: &Analysis, target: &Endpoint) {
    assert!(
        !analysis.relations.iter().any(|r| &r.target == target),
        "不应连到 {}：{:?}",
        target.table.label(),
        analysis.relations
    );
}

fn assert_candidate_to(analysis: &Analysis, target: &Endpoint) {
    assert!(
        analysis.candidates.iter().any(|c| &c.target == target),
        "应保留候选 {}：{:?}",
        target.table.label(),
        analysis.candidates
    );
}

#[test]
fn declared_foreign_key_keeps_real_schema_and_ordered_column_pairs() {
    let customers = table(
        "crm",
        "customers",
        "客户表",
        vec![
            col("tenant_id", "bigint", true, ""),
            col("id", "bigint", true, ""),
        ],
    );
    let mut orders = table(
        "sales",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("tenant_id", "bigint", false, ""),
            col("customer_id", "bigint", false, ""),
        ],
    );
    orders.foreign_keys.push(ForeignKey {
        name: "fk_orders_customer".into(),
        columns: vec!["tenant_id".into(), "customer_id".into()],
        ref_schema: "crm".into(),
        ref_table: "customers".into(),
        ref_columns: vec!["tenant_id".into(), "id".into()],
    });
    let catalog = catalog(
        vec![customers.clone(), orders.clone()],
        vec![ep(&customers, &["tenant_id", "id"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    let r = &analysis.relations[0];
    assert_eq!(r.kind, RelationKind::Declared);
    assert_eq!(r.source, ep(&orders, &["tenant_id", "customer_id"]));
    assert_eq!(r.target, ep(&customers, &["tenant_id", "id"]));
    assert!(
        r.reason.contains("数据库声明"),
        "理由需可追溯：{}",
        r.reason
    );
}

#[test]
fn conventional_naming_finds_plural_singular_relation_without_foreign_key() {
    let users = table(
        "app",
        "users",
        "用户表",
        vec![
            col("id", "bigint", true, ""),
            col("name", "varchar", false, ""),
        ],
    );
    let orders = table(
        "app",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("user_id", "bigint", false, ""),
            col("status", "varchar", false, ""),
            col("name", "varchar", false, ""),
        ],
    );
    let catalog = catalog(
        vec![users.clone(), orders.clone()],
        vec![ep(&users, &["id"]), ep(&orders, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    let r = &analysis.relations[0];
    assert_eq!(r.kind, RelationKind::Inferred);
    assert_eq!(r.source, ep(&orders, &["user_id"]));
    assert_eq!(r.target, ep(&users, &["id"]));
    assert!(
        r.reason.contains("命名") || r.reason.contains("常规"),
        "{}",
        r.reason
    );
    // 无语义字段不得参与全库笛卡尔积。
    assert!(analysis.relations.len() == 1);
    assert!(analysis
        .candidates
        .iter()
        .all(|c| c.source.columns != ["status"]));
    assert!(analysis
        .candidates
        .iter()
        .all(|c| c.source.columns != ["name"]));
}

#[test]
fn naming_matches_on_word_boundaries_not_substrings() {
    let users = table(
        "shop",
        "users",
        "用户表",
        vec![col("id", "bigint", true, "")],
    );
    let superuser = table(
        "shop",
        "superuser",
        "超级用户",
        vec![col("id", "bigint", true, "")],
    );
    let orders = table(
        "shop",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("user_id", "bigint", false, ""),
        ],
    );
    let catalog = catalog(
        vec![users.clone(), superuser.clone(), orders.clone()],
        vec![ep(&users, &["id"]), ep(&superuser, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    assert_eq!(analysis.relations[0].target, ep(&users, &["id"]));
    assert_no_relation_to(&analysis, &ep(&superuser, &["id"]));
    assert!(!analysis
        .candidates
        .iter()
        .any(|c| c.target == ep(&superuser, &["id"])));
}

#[test]
fn chinese_comment_business_description_can_find_an_alias_business_key() {
    let customers = table(
        "crm",
        "customers",
        "客户信息表",
        vec![
            col("id", "bigint", true, ""),
            col("customer_no", "varchar", false, "客户业务编号"),
        ],
    );
    let invoices = table(
        "billing",
        "invoices",
        "发票表",
        vec![
            col("id", "bigint", true, ""),
            col("payer_ref", "varchar", false, "关联付款客户的业务编号"),
        ],
    );
    let catalog = catalog(
        vec![customers.clone(), invoices.clone()],
        vec![ep(&customers, &["id"]), ep(&customers, &["customer_no"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    let r = &analysis.relations[0];
    assert_eq!(r.source, ep(&invoices, &["payer_ref"]));
    assert_eq!(r.target, ep(&customers, &["customer_no"]));
    assert!(r.reason.contains("注释"), "{}", r.reason);
    assert!(r.reason.contains("payer_ref"), "{}", r.reason);
}

#[test]
fn explicit_qualified_reference_resolves_name_only_competition() {
    let users = table(
        "crm",
        "users",
        "用户表",
        vec![col("id", "bigint", true, "")],
    );
    let accounts = table(
        "crm",
        "legacy_accounts",
        "历史账号表",
        vec![col("id", "bigint", true, "")],
    );
    let orders = table(
        "crm",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("user_id", "bigint", false, "引用 crm.legacy_accounts.id"),
        ],
    );
    let catalog = catalog(
        vec![users.clone(), accounts.clone(), orders.clone()],
        vec![ep(&users, &["id"]), ep(&accounts, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    assert_eq!(analysis.relations[0].target, ep(&accounts, &["id"]));
    assert_no_relation_to(&analysis, &ep(&users, &["id"]));
    assert!(!analysis
        .candidates
        .iter()
        .any(|c| c.target == ep(&users, &["id"])));
    assert!(analysis.relations[0].reason.contains("legacy_accounts"));
}

#[test]
fn negated_reference_excludes_the_weakly_named_target() {
    let users = table(
        "app",
        "users",
        "用户表",
        vec![col("id", "bigint", true, "")],
    );
    let orders = table(
        "app",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("user_id", "bigint", false, "不是用户ID，是外部系统编号"),
        ],
    );
    let catalog = catalog(
        vec![users.clone(), orders.clone()],
        vec![ep(&users, &["id"]), ep(&orders, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert_no_relation_to(&analysis, &ep(&users, &["id"]));
    assert!(
        !analysis
            .candidates
            .iter()
            .any(|c| c.target == ep(&users, &["id"])),
        "被明确否定的弱命名目标应排除：{:?}",
        analysis.candidates
    );
}

#[test]
fn historical_statement_is_held_as_candidate_not_connected() {
    let orders = table(
        "sales",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("order_no", "varchar", false, "订单编号"),
        ],
    );
    let snapshots = table(
        "sales",
        "order_snapshots",
        "订单快照表",
        vec![
            col("id", "bigint", true, ""),
            col("order_ref", "varchar", false, "历史订单编号"),
        ],
    );
    let catalog = catalog(
        vec![orders.clone(), snapshots.clone()],
        vec![ep(&orders, &["id"]), ep(&orders, &["order_no"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert_candidate_to(&analysis, &ep(&orders, &["order_no"]));
    let c = analysis
        .candidates
        .iter()
        .find(|c| c.target == ep(&orders, &["order_no"]))
        .unwrap();
    assert!(c.reason.contains("历史"), "{}", c.reason);
}

#[test]
fn conditional_explicit_reference_is_held_as_candidate() {
    let customers = table(
        "crm",
        "customers",
        "客户表",
        vec![col("id", "bigint", true, "")],
    );
    let invoices = table(
        "crm",
        "invoices",
        "发票表",
        vec![
            col("id", "bigint", true, ""),
            col(
                "owner_ref",
                "bigint",
                false,
                "当类型=1时引用 crm.customers.id",
            ),
        ],
    );
    let catalog = catalog(
        vec![customers.clone(), invoices.clone()],
        vec![ep(&customers, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert_candidate_to(&analysis, &ep(&customers, &["id"]));
    let c = analysis
        .candidates
        .iter()
        .find(|c| c.target == ep(&customers, &["id"]))
        .unwrap();
    assert!(c.reason.contains("条件"), "{}", c.reason);
}

#[test]
fn two_plausible_business_targets_stay_competing_candidates() {
    let current = table(
        "crm",
        "customers",
        "客户表",
        vec![
            col("id", "bigint", true, ""),
            col("customer_no", "varchar", false, "客户编号"),
        ],
    );
    let legacy = table(
        "crm",
        "legacy_customers",
        "客户表（历史）",
        vec![
            col("id", "bigint", true, ""),
            col("customer_no", "varchar", false, "客户编号"),
        ],
    );
    let invoices = table(
        "crm",
        "invoices",
        "发票表",
        vec![
            col("id", "bigint", true, ""),
            col("buyer_ref", "varchar", false, "客户编号"),
        ],
    );
    let catalog = catalog(
        vec![current.clone(), legacy.clone(), invoices.clone()],
        vec![
            ep(&current, &["customer_no"]),
            ep(&legacy, &["customer_no"]),
        ],
    );

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert_candidate_to(&analysis, &ep(&current, &["customer_no"]));
    assert_candidate_to(&analysis, &ep(&legacy, &["customer_no"]));
    for c in &analysis.candidates {
        assert!(
            c.reason.contains("竞争") || c.reason.contains("目标"),
            "{}",
            c.reason
        );
    }
}

#[test]
fn two_conflicting_explicit_statements_are_not_forced_into_a_relation() {
    let users = table(
        "crm",
        "users",
        "用户表",
        vec![col("id", "bigint", true, "")],
    );
    let accounts = table(
        "crm",
        "accounts",
        "账号表",
        vec![col("id", "bigint", true, "")],
    );
    let orders = table(
        "crm",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col(
                "owner_ref",
                "bigint",
                false,
                "引用 crm.users.id；也引用 crm.accounts.id",
            ),
        ],
    );
    let catalog = catalog(
        vec![users.clone(), accounts.clone(), orders.clone()],
        vec![ep(&users, &["id"]), ep(&accounts, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert_candidate_to(&analysis, &ep(&users, &["id"]));
    assert_candidate_to(&analysis, &ep(&accounts, &["id"]));
}

#[test]
fn duplicated_evidence_from_the_same_comment_is_deduplicated() {
    let users = table(
        "crm",
        "users",
        "用户表",
        vec![col("id", "bigint", true, "")],
    );
    let orders = table(
        "crm",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col(
                "owner_ref",
                "bigint",
                false,
                "引用 crm.users.id；crm.users.id 是该表的用户主键",
            ),
        ],
    );
    let catalog = catalog(
        vec![users.clone(), orders.clone()],
        vec![ep(&users, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    assert!(
        analysis.relations[0].reason.contains("去重后证据 1"),
        "{}",
        analysis.relations[0].reason
    );
}

#[test]
fn composite_key_is_assembled_in_trusted_key_order_or_held_for_missing_column() {
    let lines = table(
        "sales",
        "order_lines",
        "订单行表",
        vec![
            col("order_id", "bigint", true, ""),
            col("line_no", "int", true, ""),
            col("amount", "decimal", false, ""),
        ],
    );
    let shipments = table(
        "shipping",
        "shipments",
        "发货表",
        vec![
            col("id", "bigint", true, ""),
            col("order_id", "bigint", false, "引用订单行ID"),
            col("order_line_no", "int", false, ""),
        ],
    );
    let catalog = catalog(
        vec![lines.clone(), shipments.clone()],
        vec![ep(&lines, &["order_id", "line_no"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    let r = &analysis.relations[0];
    assert_eq!(r.source, ep(&shipments, &["order_id", "order_line_no"]));
    assert_eq!(r.target, ep(&lines, &["order_id", "line_no"]));
    assert!(r.target.columns == ["order_id", "line_no"]);
}

#[test]
fn composite_key_with_a_missing_source_column_is_held_not_split() {
    let lines = table(
        "sales",
        "order_lines",
        "订单行表",
        vec![
            col("order_id", "bigint", true, ""),
            col("line_no", "int", true, ""),
        ],
    );
    let shipments = table(
        "shipping",
        "shipments",
        "发货表",
        vec![
            col("id", "bigint", true, ""),
            col("order_id", "bigint", false, "引用订单行ID"),
        ],
    );
    let catalog = catalog(
        vec![lines.clone(), shipments.clone()],
        vec![ep(&lines, &["order_id", "line_no"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert_candidate_to(&analysis, &ep(&lines, &["order_id", "line_no"]));
    let c = analysis
        .candidates
        .iter()
        .find(|c| c.target == ep(&lines, &["order_id", "line_no"]))
        .unwrap();
    assert!(
        c.reason.contains("复合") || c.reason.contains("缺"),
        "{}",
        c.reason
    );
}

#[test]
fn same_table_name_in_two_schemas_is_a_real_competition_until_schema_is_qualified() {
    let first = table("a", "users", "用户表", vec![col("id", "bigint", true, "")]);
    let second = table("b", "users", "用户表", vec![col("id", "bigint", true, "")]);
    let orders = table(
        "a",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("user_id", "bigint", false, "用户ID"),
        ],
    );
    let catalog = catalog(
        vec![first.clone(), second.clone(), orders.clone()],
        vec![ep(&first, &["id"]), ep(&second, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert_candidate_to(&analysis, &ep(&first, &["id"]));
    assert_candidate_to(&analysis, &ep(&second, &["id"]));
}

#[test]
fn naming_does_not_win_because_only_one_namesake_has_a_type_signature() {
    let left = table("a", "nodes", "", vec![col("id", "integer", true, "")]);
    let right = table("b", "nodes", "", vec![col("id", "integer", true, "")]);
    let refs = table(
        "a",
        "events",
        "",
        vec![col("node_id", "integer", false, "")],
    );
    let source = ep(&refs, &["node_id"]);
    let target = ep(&left, &["id"]);
    let unknown = ep(&right, &["id"]);
    let mut cat = catalog(
        vec![left, right, refs],
        vec![target.clone(), unknown.clone()],
    );
    cat.types.retain(|ty| ty.table != unknown.table);
    let graph = infer_relations(&cat);
    assert!(!graph.relations.iter().any(|r| r.source == source));
    assert_candidate_to(&graph, &target);
    assert_candidate_to(&graph, &unknown);
}

#[test]
fn qualified_reference_selects_the_real_schema_of_the_namesake() {
    let first = table("a", "users", "用户表", vec![col("id", "bigint", true, "")]);
    let second = table("b", "users", "用户表", vec![col("id", "bigint", true, "")]);
    let orders = table(
        "a",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("user_id", "bigint", false, "用户ID"),
            col("owner_ref", "bigint", false, "引用 b.users.id"),
        ],
    );
    let catalog = catalog(
        vec![first.clone(), second.clone(), orders.clone()],
        vec![ep(&first, &["id"]), ep(&second, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    let owner: Vec<_> = analysis
        .relations
        .iter()
        .filter(|r| r.source == ep(&orders, &["owner_ref"]))
        .collect();
    assert_eq!(owner.len(), 1, "{:?}", analysis.relations);
    assert_eq!(owner[0].target, ep(&second, &["id"]));
    assert!(!analysis
        .candidates
        .iter()
        .any(|c| c.source == ep(&orders, &["owner_ref"])));
}

#[test]
fn role_anchor_migrates_within_the_same_explicit_domain() {
    let employees = table(
        "hr",
        "employees",
        "员工表",
        vec![
            col("id", "bigint", true, ""),
            col("employee_no", "varchar", false, "员工编号"),
        ],
    );
    let orders = table(
        "purchase",
        "orders",
        "采购订单表（业务域：采购）",
        vec![
            col("id", "bigint", true, ""),
            col(
                "purchaser_no",
                "varchar",
                false,
                "采购员编号，引用 hr.employees.employee_no",
            ),
        ],
    );
    let invoices = table(
        "purchase",
        "invoices",
        "采购发票表（业务域：采购）",
        vec![
            col("id", "bigint", true, ""),
            col("buyer_ref", "varchar", false, "采购员编号"),
        ],
    );
    let catalog = catalog(
        vec![employees.clone(), orders.clone(), invoices.clone()],
        vec![ep(&employees, &["employee_no"])],
    );

    let analysis = infer_relations(&catalog);
    let r = analysis
        .relations
        .iter()
        .find(|r| r.source == ep(&invoices, &["buyer_ref"]))
        .unwrap_or_else(|| panic!("角色迁移应自动连线：{:?}", analysis));
    assert_eq!(r.target, ep(&employees, &["employee_no"]));
    assert!(
        r.reason.contains("迁移") || r.reason.contains("角色"),
        "{}",
        r.reason
    );
    assert!(
        r.reason.contains("采购员"),
        "理由需保留角色原文：{}",
        r.reason
    );
}

#[test]
fn copied_table_shapes_are_not_independent_role_witnesses() {
    let key_table = table(
        "core",
        "registry",
        "",
        vec![col("key_no", "varchar", true, "")],
    );
    let first = table(
        "work",
        "form_2025",
        "归档（业务域：录入）",
        vec![col(
            "actor",
            "varchar",
            false,
            "经办人编号，引用 core.registry.key_no",
        )],
    );
    let second = table(
        "work",
        "form_2026",
        "归档（业务域：录入）",
        vec![col("actor", "varchar", false, "经办人编号")],
    );
    let source = ep(&second, &["actor"]);
    let target = ep(&key_table, &["key_no"]);
    let graph = infer_relations(&catalog(
        vec![first, second, key_table],
        vec![target.clone()],
    ));
    assert!(
        !graph
            .relations
            .iter()
            .any(|r| r.source == source && r.target == target),
        "{:?}",
        graph.relations
    );
    assert!(graph
        .candidates
        .iter()
        .any(|c| c.source == source && c.target == target));
}

#[test]
fn role_migration_requires_a_known_compatible_domain() {
    let employees = table(
        "hr",
        "employees",
        "员工表",
        vec![col("employee_no", "varchar", false, "员工编号")],
    );
    let orders = table(
        "purchase",
        "orders",
        "采购订单表（业务域：采购）",
        vec![col(
            "purchaser_no",
            "varchar",
            false,
            "采购员编号，引用 hr.employees.employee_no",
        )],
    );
    let invoices = table(
        "sales",
        "invoices",
        "销售发票表（业务域：销售）",
        vec![col("buyer_ref", "varchar", false, "采购员编号")],
    );
    let catalog = catalog(
        vec![employees.clone(), orders.clone(), invoices.clone()],
        vec![ep(&employees, &["employee_no"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(
        !analysis
            .relations
            .iter()
            .any(|r| r.source == ep(&invoices, &["buyer_ref"])),
        "业务域不相容时不得迁移：{:?}",
        analysis
    );
}

#[test]
fn predicted_edges_do_not_feed_the_role_anchor_map() {
    // 常规命名只能产生预测边，不能反过来充当角色迁移的原始锚点。
    let employees = table(
        "hr",
        "employees",
        "员工表",
        vec![
            col("id", "bigint", true, ""),
            col("employee_no", "varchar", false, "员工编号"),
        ],
    );
    let orders = table(
        "purchase",
        "orders",
        "采购订单表（业务域：采购）",
        vec![
            col("id", "bigint", true, ""),
            col("employee_id", "bigint", false, ""),
        ],
    );
    let invoices = table(
        "purchase",
        "invoices",
        "采购发票表（业务域：采购）",
        vec![
            col("id", "bigint", true, ""),
            col("buyer_ref", "varchar", false, "员工编号"),
        ],
    );
    let catalog = catalog(
        vec![employees.clone(), orders.clone(), invoices.clone()],
        vec![ep(&employees, &["id"]), ep(&orders, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(
        !analysis
            .relations
            .iter()
            .any(|r| r.source == ep(&invoices, &["buyer_ref"])),
        "预测边不得回流为锚点：{:?}",
        analysis.relations
    );
}

#[test]
fn incompatible_declared_types_exclude_the_combination() {
    let customers = table(
        "crm",
        "customers",
        "客户表",
        vec![col("id", "bigint", true, "")],
    );
    let invoices = table(
        "crm",
        "invoices",
        "发票表",
        vec![
            col("id", "bigint", true, ""),
            col("customer_ref", "date", false, "引用 crm.customers.id"),
        ],
    );
    let catalog = catalog(
        vec![customers.clone(), invoices.clone()],
        vec![ep(&customers, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert!(!analysis
        .candidates
        .iter()
        .any(|c| c.target == ep(&customers, &["id"])));
}

#[test]
fn unknown_type_keeps_a_business_clue_as_candidate() {
    let customers = table(
        "crm",
        "customers",
        "客户表",
        vec![col("id", "bigint", true, "")],
    );
    let invoices = table(
        "crm",
        "invoices",
        "发票表",
        vec![
            col("id", "bigint", true, ""),
            col("customer_ref", "bigint", false, "引用 crm.customers.id"),
        ],
    );
    let mut catalog = catalog(
        vec![customers.clone(), invoices.clone()],
        vec![ep(&customers, &["id"])],
    );
    catalog
        .types
        .retain(|t| !(t.table == TableId::from(&invoices) && t.column == "customer_ref"));

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert_candidate_to(&analysis, &ep(&customers, &["id"]));
    let c = analysis
        .candidates
        .iter()
        .find(|c| c.target == ep(&customers, &["id"]))
        .unwrap();
    assert!(c.reason.contains("类型"), "{}", c.reason);
}

#[test]
fn target_must_be_a_trusted_complete_key() {
    let customers = table(
        "crm",
        "customers",
        "客户表",
        vec![
            col("id", "bigint", true, ""),
            col("email", "varchar", false, ""),
        ],
    );
    let invoices = table(
        "crm",
        "invoices",
        "发票表",
        vec![
            col("id", "bigint", true, ""),
            col("email_ref", "varchar", false, "引用 crm.customers.email"),
        ],
    );
    let catalog = catalog(
        vec![customers.clone(), invoices.clone()],
        vec![ep(&customers, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert!(!analysis
        .candidates
        .iter()
        .any(|c| c.target == ep(&customers, &["email"])));
}

#[test]
fn a_simple_view_may_resolve_differently_named_business_keys_from_the_source_role() {
    let target_table = table(
        "main",
        "registry",
        "",
        vec![col("member_code", "varchar", true, "")],
    );
    let source_table = table(
        "main",
        "activity",
        "",
        vec![col("buyer_no", "varchar", false, "")],
    );
    let source = ep(&source_table, &["buyer_no"]);
    let target = ep(&target_table, &["member_code"]);
    let mut catalog = catalog(vec![source_table, target_table], vec![target.clone()]);
    assert_no_relation_to(&infer_relations(&catalog), &target);
    catalog.views.push(ViewComparison {
        view: TableId {
            schema: "main".into(),
            name: "member_lookup".into(),
        },
        left: target.clone(),
        right: source.clone(),
    });
    let analysis = infer_relations(&catalog);
    assert!(analysis
        .relations
        .iter()
        .any(|r| r.source == source && r.target == target && r.reason.contains("member_lookup")));
}

#[test]
fn view_equality_alone_never_creates_a_direction_or_relation() {
    let customers = table(
        "crm",
        "customers",
        "客户表",
        vec![col("id", "bigint", true, "")],
    );
    let invoices = table(
        "crm",
        "invoices",
        "发票表",
        vec![
            col("id", "bigint", true, ""),
            col("customer_ref", "bigint", false, ""),
        ],
    );
    let view = table("crm", "v_invoice_customer", "视图", vec![]);
    let catalog = ErCatalog {
        tables: vec![customers.clone(), invoices.clone(), view.clone()],
        keys: vec![ep(&customers, &["id"])],
        views: vec![ViewComparison {
            view: TableId::from(&view),
            left: ep(&invoices, &["customer_ref"]),
            right: ep(&customers, &["id"]),
        }],
        ..Default::default()
    };

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert!(analysis.candidates.is_empty(), "{:?}", analysis.candidates);
}

#[test]
fn view_comparison_corroborates_an_already_directed_business_reference() {
    let customers = table(
        "crm",
        "customers",
        "客户表",
        vec![col("id", "bigint", true, "")],
    );
    let invoices = table(
        "crm",
        "invoices",
        "发票表",
        vec![
            col("id", "bigint", true, ""),
            col("customer_ref", "bigint", false, "引用 crm.customers.id"),
        ],
    );
    let view = table("crm", "v_invoice_customer", "视图", vec![]);
    let catalog = ErCatalog {
        tables: vec![customers.clone(), invoices.clone(), view.clone()],
        keys: vec![ep(&customers, &["id"])],
        views: vec![ViewComparison {
            view: TableId::from(&view),
            left: ep(&customers, &["id"]),
            right: ep(&invoices, &["customer_ref"]),
        }],
        types: vec![
            ErColumnType {
                table: TableId::from(&customers),
                column: "id".into(),
                signature: "bigint".into(),
            },
            ErColumnType {
                table: TableId::from(&invoices),
                column: "customer_ref".into(),
                signature: "bigint".into(),
            },
        ],
        ..Default::default()
    };

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    let r = &analysis.relations[0];
    assert_eq!(r.target, ep(&customers, &["id"]));
    assert!(r.reason.contains("视图"), "{}", r.reason);
}

#[test]
fn a_quoted_reference_preserves_case_when_columns_only_differ_by_case() {
    let values = table(
        "app",
        "Registry",
        "",
        vec![
            col("Code", "varchar", false, ""),
            col("code", "varchar", true, ""),
        ],
    );
    let input = table(
        "app",
        "entries",
        "",
        vec![col(
            "ref",
            "varchar",
            false,
            r#"引用 "app"."Registry"."Code""#,
        )],
    );
    let target = ep(&values, &["code"]);
    let graph = infer_relations(&catalog(vec![values, input], vec![target.clone()]));
    assert_no_relation_to(&graph, &target);
}

#[test]
fn quoted_schema_qualified_composite_reference_is_parsed() {
    let lines = table(
        "sales",
        "order_lines",
        "订单行表",
        vec![
            col("order_id", "bigint", true, ""),
            col("line_no", "int", true, ""),
        ],
    );
    let shipments = table(
        "shipping",
        "shipments",
        "发货表",
        vec![
            col("id", "bigint", true, ""),
            col(
                "order_id",
                "bigint",
                false,
                "引用 `sales`.`order_lines`(`order_id`, `line_no`)",
            ),
            col("line_no", "int", false, ""),
        ],
    );
    let catalog = catalog(
        vec![lines.clone(), shipments.clone()],
        vec![ep(&lines, &["order_id", "line_no"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    assert_eq!(
        analysis.relations[0].target,
        ep(&lines, &["order_id", "line_no"])
    );
}

#[test]
fn high_frequency_columns_do_not_generate_a_global_cartesian_product() {
    let tables: Vec<_> = ["a", "b", "c", "d"]
        .iter()
        .map(|schema| {
            table(
                schema,
                "records",
                "记录表",
                vec![
                    col("id", "bigint", true, ""),
                    col("code", "varchar", false, ""),
                    col("status", "varchar", false, ""),
                    col("name", "varchar", false, ""),
                    col("created_at", "timestamp", false, ""),
                ],
            )
        })
        .collect();
    let keys = tables.iter().map(|t| ep(t, &["id"])).collect();
    let catalog = catalog(tables, keys);

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert!(analysis.candidates.is_empty(), "{:?}", analysis.candidates);
}

#[test]
fn chinese_conventional_naming_strips_the_whole_key_usage_phrase() {
    let customers = table(
        "crm",
        "customers",
        "客户表",
        vec![
            col("id", "bigint", true, ""),
            col("customer_no", "varchar", false, "客户编号"),
        ],
    );
    let invoices = table(
        "crm",
        "invoices",
        "发票表",
        vec![
            col("id", "bigint", true, ""),
            col("客户编号", "varchar", false, ""),
        ],
    );
    let catalog = catalog(
        vec![customers.clone(), invoices.clone()],
        vec![ep(&customers, &["customer_no"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    assert_eq!(analysis.relations[0].source, ep(&invoices, &["客户编号"]));
    assert_eq!(
        analysis.relations[0].target,
        ep(&customers, &["customer_no"])
    );
}

#[test]
fn a_declared_foreign_key_wins_and_a_conflicting_inference_becomes_review_candidate() {
    let users = table(
        "crm",
        "users",
        "用户表",
        vec![col("id", "bigint", true, "")],
    );
    let accounts = table(
        "crm",
        "accounts",
        "账号表",
        vec![col("id", "bigint", true, "")],
    );
    let mut orders = table(
        "crm",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("user_id", "bigint", false, "引用 crm.accounts.id"),
        ],
    );
    orders.foreign_keys.push(ForeignKey {
        name: "fk_orders_user".into(),
        columns: vec!["user_id".into()],
        ref_schema: "crm".into(),
        ref_table: "users".into(),
        ref_columns: vec!["id".into()],
    });
    let catalog = catalog(
        vec![users.clone(), accounts.clone(), orders.clone()],
        vec![ep(&users, &["id"]), ep(&accounts, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    let declared: Vec<_> = analysis
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::Declared)
        .collect();
    assert_eq!(declared.len(), 1, "{:?}", analysis.relations);
    assert_eq!(declared[0].target, ep(&users, &["id"]));
    assert_no_relation_to(&analysis, &ep(&accounts, &["id"]));
    let candidate = analysis
        .candidates
        .iter()
        .find(|c| c.target == ep(&accounts, &["id"]))
        .unwrap_or_else(|| panic!("冲突推断应保留复核候选：{:?}", analysis.candidates));
    assert!(
        candidate.reason.contains("冲突") || candidate.reason.contains("复核"),
        "{}",
        candidate.reason
    );
}

#[test]
fn historical_alternative_does_not_veto_a_current_explicit_reference() {
    let users = table(
        "crm",
        "users",
        "用户表",
        vec![col("id", "bigint", true, "")],
    );
    let accounts = table(
        "crm",
        "accounts",
        "账号表",
        vec![col("id", "bigint", true, "")],
    );
    let orders = table(
        "crm",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col(
                "owner_ref",
                "bigint",
                false,
                "历史引用 crm.accounts.id；当前引用 crm.users.id",
            ),
        ],
    );
    let catalog = catalog(
        vec![users.clone(), accounts.clone(), orders.clone()],
        vec![ep(&users, &["id"]), ep(&accounts, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis.relations);
    assert_eq!(analysis.relations[0].target, ep(&users, &["id"]));
    assert_no_relation_to(&analysis, &ep(&accounts, &["id"]));
}

#[test]
fn conditional_alternative_prevents_a_weaker_current_path_from_auto_connecting() {
    let users = table(
        "crm",
        "users",
        "用户表",
        vec![col("id", "bigint", true, "")],
    );
    let accounts = table(
        "crm",
        "accounts",
        "账号表",
        vec![col("id", "bigint", true, "")],
    );
    let orders = table(
        "crm",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col("user_id", "bigint", false, "当类型=1时引用 crm.accounts.id"),
        ],
    );
    let catalog = catalog(
        vec![users.clone(), accounts.clone(), orders.clone()],
        vec![ep(&users, &["id"]), ep(&accounts, &["id"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    assert_candidate_to(&analysis, &ep(&users, &["id"]));
    assert_candidate_to(&analysis, &ep(&accounts, &["id"]));
}

#[test]
fn role_migration_without_a_domain_is_recalled_as_candidate_only() {
    let employees = table(
        "hr",
        "employees",
        "员工表",
        vec![col("employee_no", "varchar", false, "员工编号")],
    );
    let orders = table(
        "purchase",
        "orders",
        "采购订单表",
        vec![col(
            "purchaser_no",
            "varchar",
            false,
            "采购员编号，引用 hr.employees.employee_no",
        )],
    );
    let invoices = table(
        "purchase",
        "invoices",
        "采购发票表",
        vec![col("buyer_ref", "varchar", false, "采购员编号")],
    );
    let catalog = catalog(
        vec![employees.clone(), orders.clone(), invoices.clone()],
        vec![ep(&employees, &["employee_no"])],
    );

    let analysis = infer_relations(&catalog);
    assert!(
        !analysis
            .relations
            .iter()
            .any(|r| r.source == ep(&invoices, &["buyer_ref"])),
        "业务域未声明时不得自动迁移：{:?}",
        analysis.relations
    );
    assert_candidate_to(&analysis, &ep(&employees, &["employee_no"]));
    let candidate = analysis
        .candidates
        .iter()
        .find(|c| c.source == ep(&invoices, &["buyer_ref"]))
        .unwrap();
    assert!(candidate.reason.contains("业务域"), "{}", candidate.reason);
}

#[test]
fn document_naming_uses_the_leaf_role_not_the_embedding_object_name() {
    let parent = table("", "profiles", "", vec![col("_id", "objectId", true, "")]);
    let child = table(
        "",
        "entries",
        "",
        vec![
            col("profiles._id", "objectId", false, ""),
            col("details.profile_id", "objectId", false, ""),
        ],
    );
    let key = ep(&parent, &["_id"]);
    let mut c = catalog(vec![parent, child], vec![key]);
    c.db_type = Some(dbs_core::DbType::MongoDb);
    let g = infer_relations(&c);
    assert_eq!(g.relations.len(), 1);
    assert_eq!(g.relations[0].source.columns, ["details.profile_id"]);
}

#[test]
fn a_descriptive_label_only_recalls_the_key() {
    let people = table(
        "main",
        "people",
        "人员主档",
        vec![col("code", "varchar", true, "人员业务编码")],
    );
    let entries = table(
        "main",
        "entries",
        "记录",
        vec![col("actor", "varchar", false, "人员业务编码")],
    );
    let target = ep(&people, &["code"]);
    let graph = infer_relations(&catalog(vec![people, entries], vec![target.clone()]));
    assert_no_relation_to(&graph, &target);
    assert_candidate_to(&graph, &target);
}

#[test]
fn external_annotation_prevents_unconditional_local_naming_inference() {
    let projects = table("main", "projects", "", vec![col("id", "integer", true, "")]);
    let entries = table(
        "main",
        "entries",
        "",
        vec![col("project_id", "integer", false, "外部系统编号")],
    );
    let target = ep(&projects, &["id"]);
    let graph = infer_relations(&catalog(vec![projects, entries], vec![target.clone()]));
    assert_no_relation_to(&graph, &target);
}

#[test]
fn english_table_comment_alias_and_string_type_aliases_are_supported() {
    let customers = table(
        "crm",
        "customers",
        "Customer information table",
        vec![
            col("id", "bigint", true, ""),
            col("customer_no", "character varying", false, "Customer number"),
        ],
    );
    let invoices = table(
        "crm",
        "invoices",
        "Invoice table",
        vec![
            col("id", "bigint", true, ""),
            col(
                "payer_ref",
                "varchar",
                false,
                "references payer customer no",
            ),
        ],
    );
    let catalog = catalog(
        vec![customers.clone(), invoices.clone()],
        vec![ep(&customers, &["customer_no"])],
    );

    let analysis = infer_relations(&catalog);
    assert_eq!(analysis.relations.len(), 1, "{:?}", analysis);
    assert_eq!(analysis.relations[0].source, ep(&invoices, &["payer_ref"]));
    assert_eq!(
        analysis.relations[0].target,
        ep(&customers, &["customer_no"])
    );
}

#[test]
fn external_qualified_target_is_reported_as_candidate_not_a_local_relation() {
    let orders = table(
        "sales",
        "orders",
        "订单表",
        vec![
            col("id", "bigint", true, ""),
            col(
                "partner_ref",
                "bigint",
                false,
                "引用 ext.partners.partner_id",
            ),
        ],
    );
    let catalog = catalog(vec![orders.clone()], vec![ep(&orders, &["id"])]);

    let analysis = infer_relations(&catalog);
    assert!(analysis.relations.is_empty(), "{:?}", analysis.relations);
    let external = Endpoint {
        table: TableId {
            schema: "ext".into(),
            name: "partners".into(),
        },
        columns: vec!["partner_id".into()],
    };
    assert_candidate_to(&analysis, &external);
    let c = analysis
        .candidates
        .iter()
        .find(|c| c.target == external)
        .unwrap();
    assert!(
        c.reason.contains("外部") || c.reason.contains("目录"),
        "{}",
        c.reason
    );
}
