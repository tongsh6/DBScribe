use dbs_core::er::{Candidate, Endpoint, ErDiagram, Relation, RelationKind, TableId};
use dbs_core::{ColumnInfo, TableInfo};
use std::collections::BTreeSet;

fn column(order: u32, name: &str, ty: &str, pk: bool) -> ColumnInfo {
    ColumnInfo {
        order,
        name: name.into(),
        data_type: ty.into(),
        length: String::new(),
        is_primary_key: pk,
        is_auto_increment: false,
        nullable: !pk,
        default_value: String::new(),
        comment: String::new(),
    }
}

fn table(name: &str, columns: Vec<ColumnInfo>) -> TableInfo {
    TableInfo {
        order: 1,
        schema: "main".into(),
        name: name.into(),
        columns,
        ..Default::default()
    }
}

fn end(table: &TableInfo, columns: &[&str]) -> Endpoint {
    Endpoint {
        table: TableId::from(table),
        columns: columns.iter().map(|c| (*c).to_string()).collect(),
    }
}

#[test]
fn long_chinese_and_xml_special_characters_are_escaped_without_losing_text() {
    let long_prefix = "这是一段很长的中文说明，用来确认渲染器按字符宽度排布而不会截断多字节文本";
    let long_comment = format!("{long_prefix}<&\"'>");
    let mut user = table(
        "用户<&\"'>",
        vec![
            column(1, "id<&\"'>", "integer", true),
            column(2, "note<&\"'>", "varchar", false),
        ],
    );
    user.comment = long_comment;
    let diagram = ErDiagram {
        tables: vec![user],
        ..Default::default()
    };

    let svg = dbs_doc::render_er_svg(&diagram, "标题 <&\"'> 长中文", false);

    assert!(svg.starts_with("<svg"));
    assert!(svg.contains("&lt;"), "尖括号必须转义：{svg}");
    assert!(svg.contains("&amp;"), "与号必须转义：{svg}");
    assert!(svg.contains("&quot;"), "引号必须转义：{svg}");
    assert!(svg.contains("&apos;"), "单引号必须转义：{svg}");
    assert!(!svg.contains("<&"), "不能输出未转义的 XML 特殊字符");
    assert!(
        svg.contains(long_prefix),
        "长中文说明应完整保留（可在 title 中）：{svg}"
    );
}

#[test]
fn non_null_equality_is_visible_in_the_svg_and_survives_json_roundtrips() {
    let readings = table("reading", vec![column(1, "meter_code", "varchar", false)]);
    let meters = table("meter", vec![column(1, "code", "varchar", false)]);
    let legacy = serde_json::json!({
        "source": end(&readings, &["meter_code"]), "target": end(&meters, &["code"]),
        "kind": "inferred", "reason": "字段命名与目标实体和键用途对应"
    });
    let mut conditioned = legacy.clone();
    conditioned["condition"] = serde_json::json!({
        "non_null_equality": true, "required_target": [], "unbound_target_columns": []
    });
    for input in [legacy, conditioned] {
        let relation: Relation = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(&relation).unwrap(), input);
        let diagram = ErDiagram {
            tables: vec![readings.clone(), meters.clone()],
            relations: vec![relation],
            ..Default::default()
        };
        let svg = dbs_doc::render_er_svg(&diagram, "测试", false);
        assert_eq!(svg.contains("非空等值"), input.get("condition").is_some());
    }
}

#[test]
fn default_fields_are_keys_and_relation_fields_and_all_columns_is_opt_in() {
    let customer = table("customer", vec![column(1, "id", "integer", true)]);
    let order = table(
        "order",
        vec![
            column(1, "id", "integer", true),
            column(2, "customer_id", "integer", false),
            column(3, "internal_memo", "varchar", false),
        ],
    );
    let diagram = ErDiagram {
        tables: vec![order.clone(), customer.clone()],
        relations: vec![Relation {
            condition: None,
            source: end(&order, &["customer_id"]),
            target: end(&customer, &["id"]),
            kind: RelationKind::Inferred,
            reason: "字段名完整匹配".into(),
        }],
        ..Default::default()
    };

    let keys_only = dbs_doc::render_er_svg(&diagram, "订单", false);
    assert!(keys_only.contains("customer_id"), "{keys_only}");
    assert!(keys_only.contains("id"), "{keys_only}");
    assert!(
        !keys_only.contains("internal_memo"),
        "默认不应显示无关普通字段：{keys_only}"
    );
    assert!(keys_only.contains("主键 / 唯一键 / 关系字段"));

    let all = dbs_doc::render_er_svg(&diagram, "订单", true);
    assert!(all.contains("internal_memo"), "全部字段模式应显示普通字段");
    assert!(all.contains("全部字段"));
}

#[test]
fn relation_kinds_cycles_dual_roles_isolated_tables_and_candidates_are_distinguishable() {
    let a = table(
        "account",
        vec![
            column(1, "id", "integer", true),
            column(2, "owner_id", "integer", false),
            column(3, "editor_id", "integer", false),
            column(4, "tenant_id", "integer", false),
            column(5, "tenant_kind", "integer", false),
        ],
    );
    let b = table(
        "tenant",
        vec![
            column(1, "id", "integer", true),
            column(2, "kind", "integer", true),
            column(3, "ref_account_id", "integer", false),
        ],
    );
    let c = table("legacy", vec![column(1, "id", "integer", true)]);
    let isolated = table("孤立表", vec![column(1, "id", "integer", true)]);
    let relations = vec![
        Relation {
            condition: None,
            source: end(&a, &["owner_id"]),
            target: end(&b, &["id"]),
            kind: RelationKind::Declared,
            reason: "数据库声明外键 fk_owner".into(),
        },
        Relation {
            condition: None,
            source: end(&a, &["editor_id"]),
            target: end(&b, &["id"]),
            kind: RelationKind::Inferred,
            reason: "字段名完整匹配".into(),
        },
        Relation {
            condition: None,
            source: end(&b, &["ref_account_id"]),
            target: end(&a, &["id"]),
            kind: RelationKind::Inferred,
            reason: "形成环线也不能丢失".into(),
        },
        Relation {
            condition: None,
            source: end(&a, &["tenant_id", "tenant_kind"]),
            target: end(&b, &["id", "kind"]),
            kind: RelationKind::Confirmed,
            reason: "项目内人工确认".into(),
        },
    ];
    let candidates = vec![Candidate {
        condition: None,
        source: end(&a, &["tenant_id"]),
        target: end(&c, &["id"]),
        reason: "存在竞争目标，保留候选".into(),
    }];
    let diagram = ErDiagram {
        tables: vec![a, b, c, isolated],
        relations,
        candidates,
        ..Default::default()
    };

    let svg = dbs_doc::render_er_svg(&diagram, "关系", true);

    for label in ["数据库声明", "系统推断", "人工确认", "候选：1"] {
        assert!(svg.contains(label), "缺少图例或标记：{label}\n{svg}");
    }
    assert!(svg.contains("class=\"edge declared\""), "{svg}");
    assert!(svg.contains("class=\"edge inferred\""), "{svg}");
    assert!(svg.contains("class=\"edge confirmed\""), "{svg}");
    assert!(
        !svg.contains("class=\"edge candidate\""),
        "未决候选不能成为图上的边"
    );
    assert!(
        svg.contains("tenant_id / tenant_kind → id / kind"),
        "复合键必须逐列配对展示：{svg}"
    );
    assert!(
        svg.matches("class=\"edge declared\"").count() >= 1
            && svg.matches("class=\"edge inferred\"").count() >= 2,
        "双角色与环线都应保留各自边：{svg}"
    );
    assert!(svg.contains("孤立表"), "孤立表不能丢失：{svg}");
    assert!(!svg.contains("存在竞争目标"), "候选理由留在预览和报告");

    let again = dbs_doc::render_er_svg(&diagram, "关系", true);
    assert_eq!(svg, again, "相同输入必须得到稳定输出");
}

/// 取出某个节点组内的标记文本（先去掉标签再反转义）。
fn text_of_class(block: &str, class: &str) -> String {
    let needle = format!(r#"class="{class}""#);
    let start = block
        .find(&needle)
        .unwrap_or_else(|| panic!("缺少 class={class}：{block}"));
    let open_end = block[start..].find('>').expect("缺少开始标签结束") + start + 1;
    let close = block[open_end..].find("</text>").expect("缺少 text 结束") + open_end;
    let raw = &block[open_end..close];
    let mut text = String::new();
    let mut in_tag = false;
    for character in raw.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn node_block<'a>(svg: &'a str, schema: &str, table: &str) -> &'a str {
    let needle = format!(r#"<g class="node" data-schema="{schema}" data-table="{table}">"#);
    let start = svg
        .find(&needle)
        .unwrap_or_else(|| panic!("缺少节点 {needle}"));
    let end = svg[start..].find("</g>").expect("节点未闭合") + start;
    &svg[start..end]
}

fn attr(block: &str, needle: &str, name: &str) -> f64 {
    let start = block.find(needle).expect("缺少元素");
    let rest = &block[start..];
    let value_start = rest.find(&format!("{name}=\"")).expect("缺少属性") + name.len() + 2;
    let value_end = rest[value_start..].find('"').expect("属性未闭合") + value_start;
    rest[value_start..value_end].parse().expect("属性不是数字")
}

fn edge_d_end_y(svg: &str) -> f64 {
    let start = svg
        .find(r#"<path class="edge declared""#)
        .expect("缺少声明边");
    let rest = &svg[start..];
    let d_start = rest.find("d=\"").expect("缺少路径") + 3;
    let d_end = rest[d_start..].find('"').expect("路径未闭合") + d_start;
    let d = &rest[d_start..d_end];
    let numbers = d
        .split(['M', 'C', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f64>().expect("路径坐标"))
        .collect::<Vec<_>>();
    *numbers.last().expect("路径没有终点")
}

#[test]
fn node_header_uses_bare_table_name_and_fields_have_separate_columns() {
    let mut orders = table(
        "orders",
        vec![
            column(1, "id", "integer", true),
            column(2, "amount", "decimal", false),
        ],
    );
    orders.schema = "public".into();
    orders.comment = "订单主表说明".into();
    orders.columns[1].length = "12,2".into();
    orders.columns[1].comment = "成交金额说明".into();
    let diagram = ErDiagram {
        tables: vec![orders],
        ..Default::default()
    };

    let svg = dbs_doc::render_er_svg_with_collapsed(&diagram, "订单", true, &BTreeSet::new());

    assert!(svg.contains(r#"data-schema="public""#), "{svg}");
    assert!(svg.contains(r#"data-table="orders""#), "{svg}");
    assert!(svg.contains(r#"class="node-header""#), "{svg}");
    assert!(
        !svg.contains("public.orders"),
        "表头不能带 schema 前缀：{svg}"
    );
    assert_eq!(text_of_class(&svg, "node-title"), "orders");
    assert_eq!(text_of_class(&svg, "table-comment"), "订单主表说明");
    assert!(svg.contains(r#"class="field-marker key""#), "{svg}");
    assert!(svg.contains(">PK</text>"), "{svg}");
    assert!(svg.contains("amount"), "{svg}");
    assert!(
        svg.contains("<tspan") && svg.contains("decimal(12,2)"),
        "类型应完整可见：{svg}"
    );
    assert_eq!(text_of_class(&svg, "field-comment"), "成交金额说明");
    let block = node_block(&svg, "public", "orders");
    assert!(
        !block.contains("<title>"),
        "不允许把卡片各行拼进一个 tooltip：{block}"
    );
    assert!(!svg.contains('…'), "完整文字不允许用省略号：{svg}");
}

#[test]
fn collapsing_a_table_removes_its_fields_and_anchors_edges_to_its_header() {
    let a = table("a", vec![column(1, "id", "integer", true)]);
    let b = table(
        "b",
        vec![
            column(1, "id", "integer", true),
            column(2, "secret_note", "varchar", false),
            column(3, "a_id", "integer", false),
        ],
    );
    let diagram = ErDiagram {
        tables: vec![a.clone(), b.clone()],
        relations: vec![Relation {
            condition: None,
            source: end(&a, &["id"]),
            target: end(&b, &["a_id"]),
            kind: RelationKind::Declared,
            reason: "数据库声明外键".into(),
        }],
        ..Default::default()
    };
    let collapsed = BTreeSet::from([TableId::from(&b)]);

    let svg = dbs_doc::render_er_svg_with_collapsed(&diagram, "折叠", true, &collapsed);

    let b_block = node_block(&svg, "main", "b");
    assert!(b_block.contains(r#"class="node-header""#), "{b_block}");
    assert!(
        !b_block.contains(r#"class="field-name""#),
        "折叠表不能画字段：{b_block}"
    );
    assert!(!b_block.contains("secret_note"), "{b_block}");
    let header_y = attr(b_block, r#"class="node-header""#, "y");
    let header_h = attr(b_block, r#"class="node-header""#, "height");
    let end_y = edge_d_end_y(&svg);
    assert!(
        end_y >= header_y && end_y <= header_y + header_h,
        "连线终点应落在折叠表头：edge={end_y} header={header_y}..{}",
        header_y + header_h
    );

    let a_block = node_block(&svg, "main", "a");
    assert!(
        a_block.contains(r#"class="field-name""#),
        "折叠单表不能影响其他节点：{a_block}"
    );
    assert_eq!(
        svg.matches(r#"<path class="edge declared""#).count(),
        1,
        "{svg}"
    );
    assert!(!svg.contains("<script"), "{svg}");
    assert!(!svg.contains("foreignObject"), "{svg}");
}

#[test]
fn same_table_name_in_different_schemas_keeps_distinct_click_identity() {
    let mut left = table("t", vec![column(1, "id", "integer", true)]);
    left.schema = "s1".into();
    let mut right = table("t", vec![column(1, "id", "integer", true)]);
    right.schema = "s2".into();
    let diagram = ErDiagram {
        tables: vec![left.clone(), right.clone()],
        relations: vec![Relation {
            condition: None,
            source: end(&left, &["id"]),
            target: end(&right, &["id"]),
            kind: RelationKind::Inferred,
            reason: "同名不同 schema".into(),
        }],
        ..Default::default()
    };

    let svg = dbs_doc::render_er_svg(&diagram, "同名表", true);

    assert!(svg.contains(r#"data-schema="s1""#), "{svg}");
    assert!(svg.contains(r#"data-schema="s2""#), "{svg}");
    assert_eq!(svg.matches(r#"data-table="t""#).count(), 2, "{svg}");
    assert_eq!(
        node_block(&svg, "s1", "t")
            .matches(r#"class="node-header""#)
            .count(),
        1
    );
    assert_eq!(
        node_block(&svg, "s2", "t")
            .matches(r#"class="node-header""#)
            .count(),
        1
    );
}

#[test]
fn long_table_name_and_field_comment_remain_visible_full_text() {
    let long_name = "超长中文表名用于验证完整换行显示".repeat(2);
    let long_comment = "这是字段的中文说明文本会被单独排版并完整显示".repeat(2);
    let mut wide = table(
        &long_name,
        vec![
            column(1, "id", "integer", true),
            column(2, "payload_identifier", "varchar", false),
        ],
    );
    wide.schema = "超长模式名".into();
    wide.comment = "表注释独立成段显示在标题下方".into();
    wide.columns[1].comment = long_comment.clone();
    let diagram = ErDiagram {
        tables: vec![wide],
        ..Default::default()
    };

    let svg = dbs_doc::render_er_svg(&diagram, "长文本", true);

    assert_eq!(text_of_class(&svg, "node-title"), long_name);
    assert_eq!(text_of_class(&svg, "field-comment"), long_comment);
    assert!(svg.contains("varchar"), "{svg}");
    assert!(!svg.contains('…'), "长文本只能换行，不能省略：{svg}");
    assert!(!svg.contains("foreignObject"), "{svg}");
}

fn rects_of(svg: &str, needle: &str) -> Vec<(f64, f64, f64, f64)> {
    let mut rects = Vec::new();
    let mut rest = svg;
    while let Some(start) = rest.find(needle) {
        let block = &rest[start..];
        rects.push((
            attr(block, needle, "x"),
            attr(block, needle, "y"),
            attr(block, needle, "width"),
            attr(block, needle, "height"),
        ));
        rest = &block[needle.len()..];
    }
    rects
}

fn overlaps(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

#[test]
fn edge_labels_never_cover_table_name_or_field_text() {
    let tables = (0..4)
        .map(|index| {
            table(
                &format!("node_{index}"),
                vec![
                    column(1, "id", "integer", true),
                    column(2, "node_0_id", "integer", false),
                    column(3, "node_1_id", "integer", false),
                    column(4, "node_2_id", "integer", false),
                ],
            )
        })
        .collect::<Vec<_>>();
    let mut relations = Vec::new();
    for target in 0..3 {
        relations.push(Relation {
            condition: None,
            source: end(&tables[3], &[&format!("node_{target}_id")]),
            target: end(&tables[target], &["id"]),
            kind: RelationKind::Inferred,
            reason: "标签避让".into(),
        });
    }
    let diagram = ErDiagram {
        tables,
        relations,
        ..Default::default()
    };

    let svg = dbs_doc::render_er_svg(&diagram, "标签避让", true);
    let cards = rects_of(&svg, r#"<rect class="node-card""#);
    let labels = rects_of(&svg, r#"<rect class="edge-label-bg""#);
    assert!(!cards.is_empty() && !labels.is_empty(), "{svg}");
    for label in &labels {
        for card in &cards {
            assert!(
                !overlaps(*label, *card),
                "边标签压住了卡片文字区域：label={label:?} card={card:?}\n{svg}"
            );
        }
    }
}

#[test]
fn warnings_and_external_references_are_listed() {
    let diagram = ErDiagram {
        tables: vec![table("only", vec![column(1, "id", "integer", true)])],
        external_count: 2,
        warnings: vec!["人工决定已失效，请复核".into()],
        ..Default::default()
    };
    let svg = dbs_doc::render_er_svg(&diagram, "提示", false);
    assert!(svg.contains("人工决定已失效，请复核"), "{svg}");
    assert!(svg.contains("图外引用"), "{svg}");
    assert!(svg.contains(">2<") || svg.contains("：2"), "{svg}");
}
