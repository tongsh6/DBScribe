//! 复合主键成员的常规命名：只在整组结构、类型与语义都成立时放行同名键。
//!
//! 正例都从 `infer_relations` 的正常入口进入，不直接调用内部函数；反例覆盖缺对应、
//! 成员自身唯一、跨 schema、否定、重复目标、类型不足与仅复合唯一键等保守边界。
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

/// 典型联结表的两端目标；按需改 depot 的 schema 或类型。
fn ops_targets(depot_schema: &str, depot_type: &str) -> (TableInfo, TableInfo) {
    (
        table(
            depot_schema,
            "depots",
            "仓储节点表",
            vec![col("depot_id", depot_type, true, "")],
        ),
        table(
            "ops",
            "carriers",
            "承运商表",
            vec![col("carrier_id", "uuid", true, "")],
        ),
    )
}

fn foreign_key(
    name: &str,
    columns: &[&str],
    target: &TableInfo,
    ref_columns: &[&str],
) -> ForeignKey {
    ForeignKey {
        name: name.into(),
        columns: columns.iter().map(|c| (*c).to_string()).collect(),
        ref_schema: target.schema.clone(),
        ref_table: target.name.clone(),
        ref_columns: ref_columns.iter().map(|c| (*c).to_string()).collect(),
    }
}

fn targets_of(analysis: &Analysis, source: &Endpoint) -> Vec<Endpoint> {
    analysis
        .relations
        .iter()
        .filter(|r| &r.source == source)
        .map(|r| r.target.clone())
        .collect()
}

fn assert_no_relation_from(analysis: &Analysis, source: &Endpoint) {
    assert!(
        analysis.relations.iter().all(|r| &r.source != source),
        "来源 {} 不应自动连线：{:?}",
        source.table.label(),
        analysis.relations
    );
}

/// 只禁止本次特例相关的常规命名边；已声明外键、视图映射等独立路径须原样保留。
fn assert_no_conventional_naming_from(analysis: &Analysis, source: &Endpoint) {
    assert!(
        analysis
            .relations
            .iter()
            .all(|r| &r.source != source || !r.reason.contains("常规命名")),
        "来源 {} 不应凭常规命名自动连线：{:?}",
        source.table.label(),
        analysis.relations
    );
}

/// 典型联结表：两个成员分别指向独立目标表的单列唯一键，目标键列名与成员同名。
fn junction_fixture() -> (TableInfo, TableInfo, TableInfo, ErCatalog) {
    let depots = table(
        "ops",
        "depots",
        "仓储节点表",
        vec![col("depot_id", "uuid", true, "")],
    );
    let carriers = table(
        "ops",
        "carriers",
        "承运商表",
        vec![col("carrier_id", "uuid", true, "")],
    );
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("depot_id", "uuid", true, ""),
            col("carrier_id", "uuid", true, ""),
            col("distance_km", "integer", false, ""),
        ],
    );
    let catalog = catalog(
        vec![depots.clone(), carriers.clone(), routes.clone()],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );
    (depots, carriers, routes, catalog)
}

#[test]
fn junction_members_reach_distinct_single_column_keys_by_conventional_naming() {
    let (depots, carriers, routes, catalog) = junction_fixture();

    let analysis = infer_relations(&catalog);

    let depot_source = ep(&routes, &["depot_id"]);
    let carrier_source = ep(&routes, &["carrier_id"]);
    assert_eq!(
        targets_of(&analysis, &depot_source),
        vec![ep(&depots, &["depot_id"])],
        "{:?}",
        analysis.relations
    );
    assert_eq!(
        targets_of(&analysis, &carrier_source),
        vec![ep(&carriers, &["carrier_id"])],
        "{:?}",
        analysis.relations
    );
    assert!(analysis
        .relations
        .iter()
        .all(|r| r.kind == RelationKind::Inferred
            && (r.reason.contains("命名") || r.reason.contains("常规"))));
}

#[test]
fn reordered_columns_and_members_keep_the_same_outcome() {
    let depots = table(
        "ops",
        "depots",
        "仓储节点表",
        vec![col("depot_id", "uuid", true, "")],
    );
    let carriers = table(
        "ops",
        "carriers",
        "承运商表",
        vec![col("carrier_id", "uuid", true, "")],
    );
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("carrier_id", "uuid", true, ""),
            col("depot_id", "uuid", true, ""),
        ],
    );
    let catalog = catalog(
        vec![depots.clone(), carriers.clone(), routes.clone()],
        vec![
            ep(&routes, &["depot_id", "carrier_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&depots, &["depot_id"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_eq!(
        targets_of(&analysis, &ep(&routes, &["depot_id"])),
        vec![ep(&depots, &["depot_id"])],
        "{:?}",
        analysis.relations
    );
    assert_eq!(
        targets_of(&analysis, &ep(&routes, &["carrier_id"])),
        vec![ep(&carriers, &["carrier_id"])],
        "{:?}",
        analysis.relations
    );
}

#[test]
fn renamed_but_consistent_business_codes_still_connect() {
    let assets = table(
        "store",
        "assets",
        "资产主档",
        vec![col("asset_code", "varchar", true, "")],
    );
    let bays = table(
        "store",
        "bays",
        "库位主档",
        vec![col("bay_code", "varchar", true, "")],
    );
    let placements = table(
        "store",
        "placements",
        "存放记录",
        vec![
            col("asset_code", "varchar", true, ""),
            col("bay_code", "varchar", true, ""),
        ],
    );
    let catalog = catalog(
        vec![assets.clone(), bays.clone(), placements.clone()],
        vec![
            ep(&assets, &["asset_code"]),
            ep(&bays, &["bay_code"]),
            ep(&placements, &["asset_code", "bay_code"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_eq!(
        targets_of(&analysis, &ep(&placements, &["asset_code"])),
        vec![ep(&assets, &["asset_code"])],
        "{:?}",
        analysis.relations
    );
    assert_eq!(
        targets_of(&analysis, &ep(&placements, &["bay_code"])),
        vec![ep(&bays, &["bay_code"])],
        "{:?}",
        analysis.relations
    );
}

#[test]
fn a_member_without_its_own_naming_target_keeps_the_whole_group_withheld() {
    let depots = table(
        "ops",
        "depots",
        "仓储节点表",
        vec![col("depot_id", "uuid", true, "")],
    );
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("depot_id", "uuid", true, ""),
            col("slot_no", "integer", true, ""),
        ],
    );
    let catalog = catalog(
        vec![depots.clone(), routes.clone()],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&routes, &["depot_id", "slot_no"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&routes, &["depot_id"]));
    assert!(
        analysis.candidates.iter().all(|c| {
            !(c.source == ep(&routes, &["depot_id"]) && c.target == ep(&depots, &["depot_id"]))
        }),
        "缺对应时应保持保守，不凭结构召回：{:?}",
        analysis.candidates
    );
}

#[test]
fn a_member_that_is_itself_unique_keeps_the_whole_group_withheld() {
    let (depots, carriers) = ops_targets("ops", "uuid");
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("depot_id", "uuid", true, ""),
            col("carrier_id", "uuid", true, ""),
        ],
    );
    let catalog = catalog(
        vec![depots.clone(), carriers.clone(), routes.clone()],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&routes, &["depot_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&routes, &["depot_id"]));
    assert_no_relation_from(&analysis, &ep(&routes, &["carrier_id"]));
}

#[test]
fn members_in_another_schema_keep_the_whole_group_withheld() {
    let (depots, carriers) = ops_targets("archive", "uuid");
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("depot_id", "uuid", true, ""),
            col("carrier_id", "uuid", true, ""),
        ],
    );
    let catalog = catalog(
        vec![depots.clone(), carriers.clone(), routes.clone()],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&routes, &["depot_id"]));
    assert_no_relation_from(&analysis, &ep(&routes, &["carrier_id"]));
}

#[test]
fn a_negated_member_keeps_the_whole_group_withheld() {
    let (depots, carriers) = ops_targets("ops", "uuid");
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("depot_id", "uuid", true, ""),
            col("carrier_id", "uuid", true, "不引用承运商"),
        ],
    );
    let catalog = catalog(
        vec![depots.clone(), carriers.clone(), routes.clone()],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&routes, &["depot_id"]));
    assert_no_relation_from(&analysis, &ep(&routes, &["carrier_id"]));
}

#[test]
fn a_scoped_member_keeps_the_whole_group_withheld() {
    let (depots, carriers) = ops_targets("ops", "uuid");
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("depot_id", "uuid", true, ""),
            col("carrier_id", "uuid", true, "历史承运商编号"),
        ],
    );
    let catalog = catalog(
        vec![depots.clone(), carriers.clone(), routes.clone()],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&routes, &["depot_id"]));
    assert_no_relation_from(&analysis, &ep(&routes, &["carrier_id"]));
}

#[test]
fn an_incompatible_type_keeps_the_whole_group_withheld() {
    let (depots, carriers) = ops_targets("ops", "bigint");
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("depot_id", "varchar", true, ""),
            col("carrier_id", "uuid", true, ""),
        ],
    );
    let catalog = catalog(
        vec![depots.clone(), carriers.clone(), routes.clone()],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&routes, &["depot_id"]));
    // 另一成员自身仍可按既有命名裁决，但整组不能借它来确认同名列。
    assert_no_relation_from(&analysis, &ep(&routes, &["carrier_id"]));
}

#[test]
fn an_unknown_type_signature_keeps_the_whole_group_withheld() {
    let (depots, carriers) = ops_targets("ops", "uuid");
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("depot_id", "uuid", true, ""),
            col("carrier_id", "uuid", true, ""),
        ],
    );
    let mut catalog = catalog(
        vec![depots.clone(), carriers.clone(), routes.clone()],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );
    let carriers_id = TableId::from(&carriers);
    catalog
        .types
        .retain(|ty| !(ty.table == carriers_id && ty.column == "carrier_id"));

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&routes, &["depot_id"]));
    assert_no_relation_from(&analysis, &ep(&routes, &["carrier_id"]));
}

#[test]
fn repeated_target_entity_keeps_only_the_unblocked_member_out_of_the_group() {
    let assets = table(
        "store",
        "assets",
        "资产主档",
        vec![col("asset_code", "varchar", true, "")],
    );
    let bays = table(
        "store",
        "bays",
        "库位主档",
        vec![col("bay_code", "varchar", true, "")],
    );
    let placements = table(
        "store",
        "placements",
        "存放记录",
        vec![
            col("asset_code", "varchar", true, ""),
            col("asset_no", "varchar", true, ""),
            col("bay_code", "varchar", true, ""),
        ],
    );
    let catalog = catalog(
        vec![assets.clone(), bays.clone(), placements.clone()],
        vec![
            ep(&assets, &["asset_code"]),
            ep(&bays, &["bay_code"]),
            ep(&placements, &["asset_code", "asset_no", "bay_code"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    // `asset_code` 与 `asset_no` 都落到同一实体，整组不放行；未受同名列抑制的
    // `asset_no` 仍走自身常规命名，不被结构规则额外提权或抹除。
    assert_no_relation_from(&analysis, &ep(&placements, &["asset_code"]));
    assert_no_relation_from(&analysis, &ep(&placements, &["bay_code"]));
}

#[test]
fn a_composite_unique_key_that_is_not_the_primary_key_stays_withheld() {
    let depots = table(
        "ops",
        "depots",
        "仓储节点表",
        vec![col("depot_id", "uuid", true, "")],
    );
    let carriers = table(
        "ops",
        "carriers",
        "承运商表",
        vec![col("carrier_id", "uuid", true, "")],
    );
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("route_id", "uuid", true, ""),
            col("depot_id", "uuid", false, ""),
            col("carrier_id", "uuid", false, ""),
        ],
    );
    let catalog = catalog(
        vec![depots.clone(), carriers.clone(), routes.clone()],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&routes, &["route_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&routes, &["depot_id"]));
    assert_no_relation_from(&analysis, &ep(&routes, &["carrier_id"]));
}

#[test]
fn document_catalogs_never_treat_indexes_as_composite_primary_keys() {
    let nodes = table("", "nodes", "", vec![col("node_key", "objectId", true, "")]);
    let units = table("", "units", "", vec![col("unit_key", "objectId", true, "")]);
    // 文档目录即便带有看似多列主键的目录信息，也不能据此替代真实复合主键。
    let links = table(
        "",
        "links",
        "",
        vec![
            col("node_key", "objectId", true, ""),
            col("unit_key", "objectId", true, ""),
        ],
    );
    let mut catalog = catalog(
        vec![nodes.clone(), units.clone(), links.clone()],
        vec![
            ep(&nodes, &["node_key"]),
            ep(&units, &["unit_key"]),
            ep(&links, &["node_key", "unit_key"]),
        ],
    );
    catalog.db_type = Some(dbs_core::DbType::MongoDb);

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&links, &["node_key"]));
    assert_no_relation_from(&analysis, &ep(&links, &["unit_key"]));
}

#[test]
fn ambiguous_same_name_entities_keep_the_whole_group_withheld() {
    let (depots, carriers) = ops_targets("ops", "uuid");
    // 表名变体同样落到「仓储节点」实体，形成真实的命名歧义。
    let depot_info = table(
        "ops",
        "depot_info",
        "仓储节点信息表",
        vec![col("depot_id", "uuid", true, "")],
    );
    let routes = table(
        "ops",
        "routes",
        "线路配置表",
        vec![
            col("depot_id", "uuid", true, ""),
            col("carrier_id", "uuid", true, ""),
        ],
    );
    let catalog = catalog(
        vec![
            depots.clone(),
            depot_info.clone(),
            carriers.clone(),
            routes.clone(),
        ],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&depot_info, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_no_relation_from(&analysis, &ep(&routes, &["depot_id"]));
    assert_no_relation_from(&analysis, &ep(&routes, &["carrier_id"]));
}

/// 同一成员已被显式外键指向别的目标时，整组结构证据被反驳，兄弟成员不能借用。
#[test]
fn a_member_declared_against_another_target_keeps_the_whole_group_withheld() {
    let (depots, carriers, mut routes, _) = junction_fixture();
    let legacy = table(
        "ops",
        "legacy_depots",
        "遗留仓储台账",
        vec![col("legacy_depot_id", "uuid", true, "")],
    );
    routes.foreign_keys.push(foreign_key(
        "fk_routes_depot_legacy",
        &["depot_id"],
        &legacy,
        &["legacy_depot_id"],
    ));
    let catalog = catalog(
        vec![
            depots.clone(),
            carriers.clone(),
            legacy.clone(),
            routes.clone(),
        ],
        vec![
            ep(&depots, &["depot_id"]),
            ep(&carriers, &["carrier_id"]),
            ep(&legacy, &["legacy_depot_id"]),
            ep(&routes, &["depot_id", "carrier_id"]),
        ],
    );

    let analysis = infer_relations(&catalog);

    assert_no_conventional_naming_from(&analysis, &ep(&routes, &["depot_id"]));
    assert_no_conventional_naming_from(&analysis, &ep(&routes, &["carrier_id"]));
    // 已声明外键边本身必须保留，不受整组特例回退影响。
    assert!(
        analysis
            .relations
            .iter()
            .any(|r| r.kind == RelationKind::Declared
                && r.source == ep(&routes, &["depot_id"])
                && r.target == ep(&legacy, &["legacy_depot_id"])),
        "{:?}",
        analysis.relations
    );
}

/// 同一成员已被视图等值到别的端点时，整组也不能借兄弟成员证明同名键。
#[test]
fn a_member_compared_with_another_target_by_a_view_keeps_the_whole_group_withheld() {
    let (_, _, routes, mut catalog) = junction_fixture();
    let legacy = table(
        "ops",
        "legacy_depots",
        "遗留仓储台账",
        vec![col("legacy_depot_id", "uuid", true, "")],
    );
    catalog.tables.push(legacy.clone());
    catalog.keys.push(ep(&legacy, &["legacy_depot_id"]));
    catalog
        .types
        .extend(legacy.columns.iter().map(|c| ErColumnType {
            table: TableId::from(&legacy),
            column: c.name.clone(),
            signature: c.data_type.clone(),
        }));
    catalog.views.push(ViewComparison {
        view: TableId {
            schema: "ops".into(),
            name: "v_depot_legacy".into(),
        },
        left: ep(&routes, &["depot_id"]),
        right: ep(&legacy, &["legacy_depot_id"]),
    });

    let analysis = infer_relations(&catalog);

    // 视图自身的映射边指向 legacy，属于原始证据，必须保留；只回退整组常规命名。
    assert_no_conventional_naming_from(&analysis, &ep(&routes, &["depot_id"]));
    assert_no_conventional_naming_from(&analysis, &ep(&routes, &["carrier_id"]));
    assert!(
        analysis
            .relations
            .iter()
            .any(|r| r.source == ep(&routes, &["depot_id"])
                && r.target == ep(&legacy, &["legacy_depot_id"])
                && r.reason.contains("视图")),
        "{:?}",
        analysis.relations
    );
}

/// 放行理由要说明整组成员分别落到不同实体键，且不把同组两次命名当作独立佐证。
#[test]
fn the_relaxed_reason_names_the_whole_group_instead_of_two_independent_namings() {
    let (_, _, routes, catalog) = junction_fixture();

    let analysis = infer_relations(&catalog);
    let relation = analysis
        .relations
        .iter()
        .find(|r| r.source == ep(&routes, &["depot_id"]))
        .expect("整组结构成立时应保留 depot 成员");

    assert!(
        relation.reason.contains("同组复合主键"),
        "{}",
        relation.reason
    );
    assert!(
        relation.reason.contains("carrier_id") && relation.reason.contains("carriers"),
        "{}",
        relation.reason
    );
    assert!(
        relation.reason.contains("不互为独立证据"),
        "{}",
        relation.reason
    );
}

/// 非复合主键成员的普通命名理由保持原样，不泄漏特例说明。
#[test]
fn ordinary_naming_reason_stays_unchanged() {
    let customers = table(
        "sales",
        "customers",
        "客户主档",
        vec![col("customer_id", "uuid", true, "")],
    );
    let orders = table(
        "sales",
        "orders",
        "订单表",
        vec![
            col("order_id", "uuid", true, ""),
            col("customer_id", "uuid", false, ""),
        ],
    );
    let catalog = catalog(
        vec![customers.clone(), orders.clone()],
        vec![ep(&customers, &["customer_id"]), ep(&orders, &["order_id"])],
    );

    let analysis = infer_relations(&catalog);
    let relation = analysis
        .relations
        .iter()
        .find(|r| r.source == ep(&orders, &["customer_id"]))
        .expect("普通命名应照常生效");

    assert!(
        !relation.reason.contains("同组复合主键"),
        "{}",
        relation.reason
    );
    assert!(relation.reason.contains("命名"), "{}", relation.reason);
}
