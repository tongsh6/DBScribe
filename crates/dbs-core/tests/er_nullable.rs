//! MySQL 可空唯一键：只在普通 `=` 且键位非空时至多一行的条件下参与既有裁决。
use dbs_core::er::{
    infer_relations, Endpoint, ErCatalog, ErColumnType, EvidenceKind, RelationKind, TableId,
    ViewComparison,
};
use dbs_core::{ColumnInfo, DbType, TableInfo};

fn col(name: &str, signature: &str, nullable: bool, comment: &str) -> ColumnInfo {
    ColumnInfo {
        order: 0,
        name: name.into(),
        data_type: signature.into(),
        length: String::new(),
        is_primary_key: false,
        is_auto_increment: false,
        nullable,
        default_value: String::new(),
        comment: comment.into(),
    }
}

fn table(name: &str, comment: &str, columns: Vec<ColumnInfo>) -> TableInfo {
    TableInfo {
        order: 0,
        schema: "app".into(),
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

fn catalog(
    tables: Vec<TableInfo>,
    keys: Vec<Endpoint>,
    non_null_keys: Vec<Endpoint>,
    views: Vec<ViewComparison>,
) -> ErCatalog {
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
        db_type: Some(DbType::MySql),
        tables,
        keys,
        non_null_keys,
        types,
        views,
        ..Default::default()
    }
}

#[test]
fn matching_text_types_without_collation_do_not_prove_non_null_uniqueness() {
    let target = table("meters", "", vec![col("code", "varchar(32)", true, "")]);
    let source = table(
        "events",
        "",
        vec![col("meter_code", "varchar(32)", false, "")],
    );
    let from = ep(&source, &["meter_code"]);
    let to = ep(&target, &["code"]);
    let catalog = catalog(vec![source, target], vec![], vec![to.clone()], vec![]);
    let result = infer_relations(&catalog);
    assert!(!result
        .relations
        .iter()
        .any(|r| r.source == from && r.target == to));
    assert!(result
        .candidates
        .iter()
        .any(|c| c.source == from && c.target == to));
}

#[test]
fn nullable_key_can_be_inferred_with_non_null_equality_condition() {
    let target = table(
        "customers",
        "客户表",
        vec![
            col("id", "bigint", false, ""),
            col("customer_no", "bigint", true, ""),
        ],
    );
    let source = table(
        "orders",
        "订单表",
        vec![
            col("id", "bigint", false, ""),
            col(
                "customer_ref",
                "bigint",
                false,
                "引用 app.customers.customer_no",
            ),
        ],
    );
    let from = ep(&source, &["customer_ref"]);
    let to = ep(&target, &["customer_no"]);
    let cat = catalog(vec![source, target], vec![], vec![to.clone()], vec![]);

    let analysis = infer_relations(&cat);
    let relation = analysis
        .relations
        .iter()
        .find(|r| r.source == from && r.target == to)
        .expect("明确注释应允许连到非空等值键");
    assert_eq!(relation.kind, RelationKind::Inferred);
    let condition = relation.condition.as_ref().expect("必须绑定可空条件");
    assert!(condition.non_null_equality);
    assert!(condition.required_target.is_empty());
    assert!(
        relation.reason.contains("NULL"),
        "理由必须说明 NULL 不匹配：{}",
        relation.reason
    );
}

#[test]
fn complete_unambiguous_naming_also_works_but_keeps_condition() {
    let target = table(
        "customers",
        "客户表",
        vec![
            col("id", "bigint", false, ""),
            col("customer_no", "bigint", true, ""),
        ],
    );
    let source = table(
        "orders",
        "订单表",
        vec![
            col("id", "bigint", false, ""),
            col("customer_no", "bigint", false, ""),
        ],
    );
    let from = ep(&source, &["customer_no"]);
    let to = ep(&target, &["customer_no"]);
    let cat = catalog(vec![source, target], vec![], vec![to.clone()], vec![]);

    let analysis = infer_relations(&cat);
    let relation = analysis
        .relations
        .iter()
        .find(|r| r.source == from && r.target == to)
        .expect("完整无歧义命名应允许连到非空等值键");
    assert!(relation
        .condition
        .as_ref()
        .is_some_and(|condition| condition.non_null_equality));
}

#[test]
fn ordinary_equality_view_still_supports_the_same_conditional_edge() {
    let target = table(
        "customers",
        "客户表",
        vec![
            col("id", "bigint", false, ""),
            col("customer_no", "bigint", true, ""),
        ],
    );
    let source = table(
        "orders",
        "订单表",
        vec![
            col("id", "bigint", false, ""),
            col("customer_no", "bigint", false, ""),
        ],
    );
    let from = ep(&source, &["customer_no"]);
    let to = ep(&target, &["customer_no"]);
    let cat = catalog(
        vec![source, target],
        vec![],
        vec![to.clone()],
        vec![ViewComparison {
            view: TableId {
                schema: "app".into(),
                name: "order_customer".into(),
            },
            left: from.clone(),
            right: to.clone(),
        }],
    );

    let analysis = infer_relations(&cat);
    let relation = analysis
        .relations
        .iter()
        .find(|r| r.source == from && r.target == to)
        .expect("普通等值视图可作为既有方向证据");
    assert!(relation
        .condition
        .as_ref()
        .is_some_and(|condition| condition.non_null_equality));
}

#[test]
fn null_safe_view_blocks_inference_and_explains_itself() {
    let target = table(
        "customers",
        "客户表",
        vec![
            col("id", "bigint", false, ""),
            col("customer_no", "bigint", true, ""),
        ],
    );
    let source = table(
        "orders",
        "订单表",
        vec![
            col("id", "bigint", false, ""),
            col("customer_no", "bigint", false, ""),
        ],
    );
    let from = ep(&source, &["customer_no"]);
    let to = ep(&target, &["customer_no"]);
    let mut cat = catalog(vec![source, target], vec![], vec![to.clone()], vec![]);
    cat.null_safe_views.push(ViewComparison {
        view: TableId {
            schema: "app".into(),
            name: "order_customer_ns".into(),
        },
        left: from.clone(),
        right: to.clone(),
    });

    let analysis = infer_relations(&cat);
    assert!(
        !analysis
            .relations
            .iter()
            .any(|r| r.source == from && r.target == to),
        "<=> 不能当成 =：{:?}",
        analysis.relations
    );
    let candidate = analysis
        .candidates
        .iter()
        .find(|c| c.source == from && c.target == to)
        .expect("冲突应保留候选");
    assert!(candidate.reason.contains("<=>"), "{}", candidate.reason);
    assert!(candidate
        .condition
        .as_ref()
        .is_some_and(|condition| condition.non_null_equality));
}

#[test]
fn nullable_target_competes_with_full_key_target_in_one_adjudication() {
    let full = table(
        "account",
        "账户表",
        vec![
            col("id", "bigint", false, ""),
            col("account_id", "bigint", false, ""),
        ],
    );
    let nullable = table(
        "accounts",
        "账户表",
        vec![
            col("id", "bigint", false, ""),
            col("account_id", "bigint", true, ""),
        ],
    );
    let source = table(
        "entries",
        "分录表",
        vec![col("account_id", "bigint", false, "")],
    );
    let from = ep(&source, &["account_id"]);
    let full_target = ep(&full, &["account_id"]);
    let nullable_target = ep(&nullable, &["account_id"]);
    let cat = catalog(
        vec![source, full.clone(), nullable.clone()],
        vec![full_target.clone()],
        vec![nullable_target.clone()],
        vec![],
    );

    let analysis = infer_relations(&cat);
    assert!(
        !analysis
            .relations
            .iter()
            .any(|r| r.source == from && r.target == nullable_target),
        "存在全表键竞争时不能自动连到可空键：{:?}",
        analysis.relations
    );
    let full_candidate = analysis
        .candidates
        .iter()
        .find(|c| c.source == from && c.target == full_target)
        .expect("全表键竞争目标必须保留");
    let nullable_candidate = analysis
        .candidates
        .iter()
        .find(|c| c.source == from && c.target == nullable_target)
        .expect("可空键竞争目标必须保留");
    assert!(full_candidate.condition.is_none());
    assert!(nullable_candidate
        .condition
        .as_ref()
        .is_some_and(|condition| condition.non_null_equality));
    assert!(
        nullable_candidate.reason.contains("竞争目标：")
            && nullable_candidate.reason.contains("account"),
        "{}",
        nullable_candidate.reason
    );
}

#[test]
fn mixed_composite_view_blocks_only_when_the_null_safe_pair_matches() {
    let target = table(
        "customers",
        "客户表",
        vec![
            col("tenant_id", "bigint", true, ""),
            col("customer_no", "bigint", true, ""),
        ],
    );
    let source = table(
        "orders",
        "订单表",
        vec![
            col("tenant_id", "bigint", false, ""),
            col("customer_no", "bigint", false, ""),
        ],
    );
    let from = ep(&source, &["tenant_id", "customer_no"]);
    let to = ep(&target, &["tenant_id", "customer_no"]);
    let mut cat = catalog(vec![source, target], vec![], vec![to.clone()], vec![]);
    let view = TableId {
        schema: "app".into(),
        name: "order_customer_mixed".into(),
    };
    cat.views.push(ViewComparison {
        view: view.clone(),
        left: ep(&cat.tables[0], &["tenant_id"]),
        right: ep(&cat.tables[1], &["tenant_id"]),
    });
    cat.null_safe_views.push(ViewComparison {
        view,
        left: ep(&cat.tables[0], &["customer_no"]),
        right: ep(&cat.tables[1], &["customer_no"]),
    });
    // 显式注释给出完整复合键指向；空安全比较只否决普通等值，不否决竞争记录。
    cat.tables[0].columns[1].comment = "引用 `app`.`customers`(`tenant_id`, `customer_no`)".into();

    let analysis = infer_relations(&cat);
    assert!(
        !analysis
            .relations
            .iter()
            .any(|r| r.source == from && r.target == to),
        "复合键中任一配对列使用 <=> 都不能当普通等值：{:?}",
        analysis.relations
    );
    let candidate = analysis
        .candidates
        .iter()
        .find(|c| c.source == from && c.target == to)
        .expect("混合视图冲突应保留候选");
    assert!(candidate.reason.contains("<=>"), "{}", candidate.reason);
}

#[test]
fn differently_named_view_alone_creates_a_local_conditional_edge_without_migration() {
    let members = table(
        "members",
        "会员表",
        vec![
            col("id", "bigint", false, ""),
            col("member_no", "bigint", true, ""),
        ],
    );
    let activity = table(
        "activity",
        "活动表",
        vec![col("buyer_code", "bigint", false, "")],
    );
    let sibling = table(
        "activity_history",
        "活动历史表",
        vec![col("buyer_code", "bigint", false, "")],
    );
    let from = ep(&activity, &["buyer_code"]);
    let target = ep(&members, &["member_no"]);
    let sibling_from = ep(&sibling, &["buyer_code"]);
    let cat = catalog(
        vec![activity, members, sibling],
        vec![],
        vec![target.clone()],
        vec![ViewComparison {
            view: TableId {
                schema: "app".into(),
                name: "v_activity_member".into(),
            },
            left: target.clone(),
            right: from.clone(),
        }],
    );

    let analysis = infer_relations(&cat);
    let relation = analysis
        .relations
        .iter()
        .find(|r| r.source == from && r.target == target)
        .expect("普通等值视图本身应给出可追溯的局部条件边");
    assert!(relation
        .condition
        .as_ref()
        .is_some_and(|condition| condition.non_null_equality));
    assert_eq!(
        relation
            .condition
            .as_ref()
            .and_then(|condition| condition.evidence_kind),
        Some(EvidenceKind::ViewMapping)
    );
    assert!(relation.reason.contains("v_activity_member"));
    assert!(
        !analysis.candidates.iter().any(|c| c.source == sibling_from),
        "条件视图映射不能成为其他行的角色迁移锚点：{:?}",
        analysis.candidates
    );
}

#[test]
fn conditional_view_mapping_is_skipped_when_the_same_pair_also_has_null_safe_comparison() {
    let members = table(
        "members",
        "会员表",
        vec![
            col("id", "bigint", false, ""),
            col("member_no", "bigint", true, ""),
        ],
    );
    let activity = table(
        "activity",
        "活动表",
        vec![col("buyer_code", "bigint", false, "")],
    );
    let from = ep(&activity, &["buyer_code"]);
    let target = ep(&members, &["member_no"]);
    let comparison = ViewComparison {
        view: TableId {
            schema: "app".into(),
            name: "v_activity_member".into(),
        },
        left: target.clone(),
        right: from.clone(),
    };
    let mut cat = catalog(
        vec![activity, members],
        vec![],
        vec![target.clone()],
        vec![comparison.clone()],
    );
    cat.null_safe_views.push(comparison);

    let analysis = infer_relations(&cat);
    assert!(
        analysis.relations.is_empty() && analysis.candidates.is_empty(),
        "同一列对的 <=> 不能靠自己或同名视图补成普通等值：{:?}",
        analysis
    );
}

#[test]
fn composite_view_keeps_the_original_ordered_column_pairs() {
    let members = table(
        "members",
        "会员表",
        vec![
            col("region", "bigint", true, ""),
            col("member_no", "bigint", true, ""),
        ],
    );
    let activity = table(
        "activity",
        "活动表",
        vec![
            col("area", "bigint", false, ""),
            col("buyer_code", "bigint", false, ""),
        ],
    );
    // 范围列决定在哪个分区内查找，没有必要再带 id/code 后缀。
    let from = ep(&activity, &["area", "buyer_code"]);
    let target = ep(&members, &["region", "member_no"]);
    let cat = catalog(
        vec![activity, members],
        vec![],
        vec![target.clone()],
        vec![ViewComparison {
            view: TableId {
                schema: "app".into(),
                name: "v_activity_member".into(),
            },
            left: target.clone(),
            right: from.clone(),
        }],
    );

    let analysis = infer_relations(&cat);
    let relation = analysis
        .relations
        .iter()
        .find(|r| r.source == from && r.target == target)
        .expect("复合视图必须保留原始有序列对，不能靠同名重建");
    assert!(relation
        .condition
        .as_ref()
        .is_some_and(|condition| condition.non_null_equality));
}

#[test]
fn null_safe_comparison_does_not_block_a_full_non_null_key_relation() {
    let customers = table("customers", "客户表", vec![col("id", "bigint", false, "")]);
    let orders = table(
        "orders",
        "订单表",
        vec![col("customer_id", "bigint", false, "")],
    );
    let from = ep(&orders, &["customer_id"]);
    let to = ep(&customers, &["id"]);
    let mut cat = catalog(vec![orders, customers], vec![to.clone()], vec![], vec![]);
    cat.null_safe_views.push(ViewComparison {
        view: TableId {
            schema: "app".into(),
            name: "v_order_customer_ns".into(),
        },
        left: from.clone(),
        right: to.clone(),
    });

    let analysis = infer_relations(&cat);
    let relation = analysis
        .relations
        .iter()
        .find(|r| r.source == from && r.target == to)
        .expect("<=> 只限制可空目标，不应否定原本可证的全表唯一关系");
    assert!(relation.condition.is_none());
}

#[test]
fn cross_role_competition_cannot_produce_two_winners() {
    let accounts = table(
        "accounts",
        "账户表",
        vec![
            col("id", "bigint", false, ""),
            col("user_no", "bigint", true, ""),
        ],
    );
    let users = table("users", "用户表", vec![col("id", "bigint", false, "")]);
    let source = table(
        "orders",
        "订单表",
        vec![col(
            "user_id",
            "bigint",
            false,
            "采购员编号，引用 app.accounts.user_no",
        )],
    );
    let from = ep(&source, &["user_id"]);
    let full_target = ep(&users, &["id"]);
    let nullable_target = ep(&accounts, &["user_no"]);
    let cat = catalog(
        vec![source, users, accounts],
        vec![full_target.clone()],
        vec![nullable_target.clone()],
        vec![],
    );

    let analysis = infer_relations(&cat);
    let strong = analysis
        .relations
        .iter()
        .find(|r| r.source == from && r.target == nullable_target)
        .expect("单一明确直接引用且对手只有弱命名时，强胜者应保留");
    assert!(strong
        .condition
        .as_ref()
        .is_some_and(|condition| condition.non_null_equality));
    assert!(
        !analysis
            .relations
            .iter()
            .any(|r| r.source == from && r.target == full_target),
        "弱命名不得与直接引用并列成第二条线：{:?}",
        analysis.relations
    );
    let full_candidate = analysis
        .candidates
        .iter()
        .find(|c| c.source == from && c.target == full_target)
        .expect("弱命名竞争目标应保留候选");
    assert!(full_candidate.condition.is_none());
    assert!(
        full_candidate.reason.contains("角色") || full_candidate.reason.contains("竞争"),
        "{}",
        full_candidate.reason
    );
}

#[test]
fn equal_strength_cross_role_targets_are_both_withheld() {
    let accounts = table(
        "accounts",
        "账户表",
        vec![
            col("id", "bigint", false, ""),
            col("user_no", "bigint", true, ""),
        ],
    );
    let users = table("users", "用户表", vec![col("id", "bigint", false, "")]);
    let source = table(
        "orders",
        "订单表",
        vec![col(
            "user_id",
            "bigint",
            false,
            "采购员编号，引用 app.accounts.user_no；用户 引用 app.users.id",
        )],
    );
    let from = ep(&source, &["user_id"]);
    let full_target = ep(&users, &["id"]);
    let nullable_target = ep(&accounts, &["user_no"]);
    let cat = catalog(
        vec![source, users, accounts],
        vec![full_target.clone()],
        vec![nullable_target.clone()],
        vec![],
    );

    let analysis = infer_relations(&cat);
    assert!(
        !analysis.relations.iter().any(|r| r.source == from),
        "同等强度且角色不同时不能静默选一条：{:?}",
        analysis.relations
    );
    for target in [&full_target, &nullable_target] {
        assert!(
            analysis
                .candidates
                .iter()
                .any(|c| c.source == from && &c.target == target),
            "两端都应暂缓为候选：{:?}",
            analysis.candidates
        );
    }
}

#[test]
fn collation_mismatch_rejects_the_automatic_edge() {
    let target = table(
        "customers",
        "客户表",
        vec![
            col("id", "bigint", false, ""),
            col(
                "customer_no",
                "varchar(64) collate utf8mb4_general_ci",
                true,
                "",
            ),
        ],
    );
    let source = table(
        "orders",
        "订单表",
        vec![
            col("id", "bigint", false, ""),
            col("customer_no", "varchar(64) collate utf8mb4_bin", false, ""),
        ],
    );
    let from = ep(&source, &["customer_no"]);
    let to = ep(&target, &["customer_no"]);
    let cat = catalog(vec![source, target], vec![], vec![to.clone()], vec![]);

    let analysis = infer_relations(&cat);
    assert!(!analysis
        .relations
        .iter()
        .any(|r| r.source == from && r.target == to));
    let candidate = analysis
        .candidates
        .iter()
        .find(|c| c.source == from && c.target == to)
        .expect("排序规则不明时应保留候选");
    assert!(
        candidate.reason.contains("排序规则"),
        "{}",
        candidate.reason
    );
}

#[test]
fn composite_nullable_key_missing_one_source_column_is_not_inferred() {
    let target = table(
        "customers",
        "客户表",
        vec![
            col("tenant_id", "bigint", true, ""),
            col("customer_no", "bigint", true, ""),
        ],
    );
    let source = table(
        "orders",
        "订单表",
        vec![
            col("id", "bigint", false, ""),
            col("tenant_id", "bigint", false, "引用 app.customers.tenant_id"),
        ],
    );
    let from = ep(&source, &["tenant_id"]);
    let to = ep(&target, &["tenant_id", "customer_no"]);
    let cat = catalog(vec![source, target], vec![], vec![to.clone()], vec![]);

    let analysis = infer_relations(&cat);
    assert!(!analysis
        .relations
        .iter()
        .any(|r| r.source == from && r.target == to));
    let candidate = analysis
        .candidates
        .iter()
        .find(|c| c.source == from && c.target == to)
        .expect("缺列应保留候选");
    assert!(
        candidate.reason.contains("缺少来源列"),
        "{}",
        candidate.reason
    );
}

#[test]
fn source_itself_being_a_key_blocks_the_automatic_edge() {
    let target = table(
        "customers",
        "客户表",
        vec![
            col("id", "bigint", false, ""),
            col("customer_no", "bigint", true, ""),
        ],
    );
    let source = table(
        "orders",
        "订单表",
        vec![col(
            "customer_no",
            "bigint",
            false,
            "引用 app.customers.customer_no",
        )],
    );
    let from = ep(&source, &["customer_no"]);
    let to = ep(&target, &["customer_no"]);
    let cat = catalog(
        vec![source, target],
        vec![],
        vec![to.clone(), from.clone()],
        vec![],
    );

    let analysis = infer_relations(&cat);
    assert!(!analysis
        .relations
        .iter()
        .any(|r| r.source == from && r.target == to));
    let candidate = analysis
        .candidates
        .iter()
        .find(|c| c.source == from && c.target == to)
        .expect("来源自身是键时应保留复核候选");
    assert!(
        candidate.reason.contains("来源字段组自身"),
        "{}",
        candidate.reason
    );
}
