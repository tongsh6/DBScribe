use dbs_core::DbType;
use dbs_db::{load_er_catalog, ConnectionOptions};

#[tokio::test]
async fn a_competing_column_pair_in_a_view_invalidates_junction_naming() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pairing.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("
        CREATE TABLE Sensor(SensorId INTEGER PRIMARY KEY);
        CREATE TABLE Gauge(GaugeId INTEGER PRIMARY KEY);
        CREATE TABLE Legacy(Id INTEGER NOT NULL, Version INTEGER NOT NULL, PRIMARY KEY(Id, Version));
        CREATE TABLE Assignment(SensorId INTEGER NOT NULL, GaugeId INTEGER NOT NULL,
            Version INTEGER NOT NULL, PRIMARY KEY(SensorId, GaugeId));
        CREATE VIEW current_assignments AS SELECT a.SensorId FROM Assignment a JOIN Legacy l
            ON a.GaugeId = l.Id AND a.Version = l.Version;
    ").unwrap();
    drop(db);
    let opts = ConnectionOptions::new(DbType::Sqlite, "", 0, "", "", path.to_string_lossy());
    let mut catalog = load_er_catalog(&opts).await.unwrap();
    assert_eq!(catalog.views.len(), 1);
    assert_eq!(catalog.views[0].left.columns.len(), 2);
    let sensor_link = |catalog: &dbs_core::er::ErCatalog| {
        dbs_core::er::infer_relations(catalog)
            .relations
            .iter()
            .any(|r| {
                r.source.table.name == "Assignment"
                    && r.source.columns == ["SensorId"]
                    && r.target.table.name == "Sensor"
            })
    };
    assert!(
        !sensor_link(&catalog),
        "复合比较中的另一指向使整组命名失去依据"
    );
    catalog.views.clear();
    assert!(sensor_link(&catalog), "移除冲突后整组可按命名裁决");
}

#[tokio::test]
async fn missing_sqlite_input_is_never_created() {
    let path =
        std::env::temp_dir().join(format!("dbscribe-missing-{}-db.sqlite", std::process::id()));
    let opts = ConnectionOptions::new(DbType::Sqlite, "", 0, "", "", path.to_string_lossy());
    assert!(!path.exists());
    assert!(load_er_catalog(&opts).await.is_err());
    assert!(!path.exists());
}

#[tokio::test]
async fn reads_ordered_keys_and_unconditional_view_mappings_from_real_ddl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("structure.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("
        CREATE TABLE items(code TEXT COLLATE NOCASE, tenant INTEGER, optional TEXT, PRIMARY KEY(tenant,code));
        CREATE UNIQUE INDEX partial_key ON items(optional) WHERE optional IS NOT NULL;
        CREATE UNIQUE INDEX expr_key ON items(length(optional));
        CREATE UNIQUE INDEX other_collation ON items(code COLLATE BINARY);
        CREATE TABLE refs(local_code TEXT COLLATE NOCASE, local_tenant INTEGER,
            FOREIGN KEY(local_tenant,local_code) REFERENCES items);
        CREATE VIEW paired AS SELECT r.local_code FROM refs r JOIN items i
            ON i.code=r.local_code AND r.local_tenant=i.tenant;
        CREATE VIEW scoped AS SELECT r.local_code FROM refs r JOIN items i
            ON i.code=r.local_code AND r.local_tenant=i.tenant WHERE i.optional IS NULL;
    ").unwrap();
    drop(db);
    let opts = ConnectionOptions::new(DbType::Sqlite, "", 0, "", "", path.to_string_lossy());
    let catalog = load_er_catalog(&opts).await.unwrap();
    assert_eq!(catalog.keys.len(), 1);
    assert_eq!(catalog.keys[0].columns, ["tenant", "code"]);
    let fk = &catalog
        .tables
        .iter()
        .find(|t| t.name == "refs")
        .unwrap()
        .foreign_keys[0];
    assert_eq!(fk.columns, ["local_tenant", "local_code"]);
    assert_eq!(fk.ref_columns, ["tenant", "code"]);
    assert_eq!(catalog.views.len(), 1);
    let mapping = &catalog.views[0];
    assert_eq!(mapping.view.name, "paired");
    assert_eq!(mapping.left.columns, ["local_tenant", "local_code"]);
    assert_eq!(mapping.right.columns, ["tenant", "code"]);
    assert!(catalog
        .types
        .iter()
        .any(|t| t.column == "code" && t.signature.contains("NOCASE")));
}
