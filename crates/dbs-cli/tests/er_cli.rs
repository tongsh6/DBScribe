use dbs_core::DbType;
use dbs_db::{er_project_scope, ConnectionOptions};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    db: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let db = dir.path().join("demo.db");
        let connection = rusqlite::Connection::open(&db).expect("创建 SQLite");
        connection
            .execute_batch(
                r#"
                PRAGMA foreign_keys = ON;
                CREATE TABLE org (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    internal_memo TEXT
                );
                CREATE TABLE app_user (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    org_id INTEGER REFERENCES org(id)
                );
                CREATE TABLE audit_log (
                    id INTEGER PRIMARY KEY,
                    internal_memo TEXT
                );
                "#,
            )
            .expect("建表");
        Self { dir, db }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn text(&self, name: &str) -> String {
        fs::read_to_string(self.path(name)).expect("读取输出文件")
    }

    fn run(&self, args: &[&str]) -> Output {
        run_in(self.dir.path(), args)
    }
}

fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dbs"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("运行 dbs")
}

fn assert_ok(output: &Output) {
    assert!(
        output.status.success(),
        "命令失败\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failed(output: &Output, needle: &str) {
    assert!(!output.status.success(), "命令本应失败但成功了");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(needle),
        "失败信息应包含「{needle}」，实际 stderr：\n{stderr}"
    );
}

#[test]
fn hardlinked_outputs_cannot_overwrite_the_sqlite_input() {
    let fixture = Fixture::new();
    let before = fs::read(&fixture.db).unwrap();
    let alias = fixture.path("alias.svg");
    fs::hard_link(&fixture.db, &alias).unwrap();
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        fixture.db.to_str().unwrap(),
        "--out",
        alias.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert_eq!(fs::read(&fixture.db).unwrap(), before);
}

#[test]
fn er_export_writes_real_svg_and_complete_report() {
    let fixture = Fixture::new();
    let db = fixture.db.to_string_lossy().into_owned();
    let out = fixture.path("er.svg");
    let report = fixture.path("er.md");
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--out",
        out.to_str().unwrap(),
        "--report",
        report.to_str().unwrap(),
    ]);
    assert_ok(&output);

    let svg = fixture.text("er.svg");
    assert!(svg.starts_with("<svg"), "输出必须是 SVG：{svg}");
    assert!(svg.contains("app_user"), "缺少用户表：{svg}");
    assert!(svg.contains("org"), "缺少组织表：{svg}");
    assert!(svg.contains("audit_log"), "孤立表不能丢失：{svg}");
    assert!(svg.contains("数据库声明"), "缺少来源图例：{svg}");
    assert!(
        svg.contains("class=\"edge declared\""),
        "缺少声明外键边：{svg}"
    );
    assert!(svg.contains("internal_memo"), "默认应显示全部字段：{svg}");

    let report_text = fixture.text("er.md");
    assert!(
        report_text.contains("# "),
        "报告应为 Markdown：{report_text}"
    );
    assert!(report_text.contains("数据库声明"), "{report_text}");
    assert!(report_text.contains("理由"), "{report_text}");
    assert!(report_text.contains("候选"), "{report_text}");
    assert!(report_text.contains("app_user"), "{report_text}");
    assert!(report_text.contains("org"), "{report_text}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("导出成功"),
        "缺少成功提示"
    );

    let all = fixture.path("er-all.svg");
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--out",
        all.to_str().unwrap(),
        "--all-columns",
    ]);
    assert_ok(&output);
    assert!(
        fixture.text("er-all.svg").contains("internal_memo"),
        "--all-columns 应显示全部字段"
    );
    let compact = fixture.path("er-key.svg");
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--out",
        compact.to_str().unwrap(),
        "--key-columns",
    ]);
    assert_ok(&output);
    assert!(!fixture.text("er-key.svg").contains("internal_memo"));
}

#[test]
fn partial_export_hides_external_foreign_keys_and_keeps_warnings_consistent() {
    let fixture = Fixture::new();
    let db = fixture.db.to_string_lossy().into_owned();
    let scope = er_project_scope(&ConnectionOptions::new(DbType::Sqlite, "", 0, "", "", &db));
    let project = serde_json::json!({
        "scope": scope,
        "selected": [{"schema": "main", "name": "app_user"}],
        "decisions": [{
            "source": {
                "table": {"schema": "main", "name": "app_user"},
                "columns": ["org_id"]
            },
            "target": {
                "table": {"schema": "main", "name": "org"},
                "columns": ["id"]
            },
            "action": "exclude",
            "basis": "v1-bogus"
        }]
    });
    let project_path = fixture.path("selected-project.json");
    fs::write(
        &project_path,
        serde_json::to_string_pretty(&project).unwrap(),
    )
    .unwrap();

    let json_out = fixture.path("selected.json.svg");
    let json_report = fixture.path("selected.json");
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--project",
        project_path.to_str().unwrap(),
        "--out",
        json_out.to_str().unwrap(),
        "--report",
        json_report.to_str().unwrap(),
    ]);
    assert_ok(&output);

    let report: serde_json::Value =
        serde_json::from_str(&fixture.text("selected.json")).expect("报告必须是 JSON");
    let tables = report["tables"].as_array().expect("tables");
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0]["name"], "app_user");
    assert!(
        tables[0]["foreign_keys"]
            .as_array()
            .expect("foreign_keys")
            .is_empty(),
        "图外外键不能原样交付：{}",
        tables[0]
    );
    assert_eq!(report["candidates"].as_array().unwrap().len(), 0);
    assert!(report["external_count"].as_u64().unwrap() >= 1);
    assert_eq!(report["external_candidate_count"].as_u64().unwrap(), 0);
    let warnings: Vec<String> = report["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|warning| warning.as_str().unwrap().to_string())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("人工决定已失效")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .all(|warning| !warning.contains("org (id)") && !warning.contains("main.org")),
        "失效决定的提示不能回显图外目标：{warnings:?}"
    );

    let svg = fixture.text("selected.json.svg");
    assert!(!svg.contains("class=\"edge candidate\""), "{svg}");
    for warning in &warnings {
        assert!(
            svg.contains(warning),
            "SVG 与 JSON 的提示不一致，缺少：{warning}\n{svg}"
        );
    }

    let md_out = fixture.path("selected.md.svg");
    let md_report = fixture.path("selected.md");
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--project",
        project_path.to_str().unwrap(),
        "--out",
        md_out.to_str().unwrap(),
        "--report",
        md_report.to_str().unwrap(),
    ]);
    assert_ok(&output);
    let markdown = fixture.text("selected.md");
    for warning in &warnings {
        assert!(
            markdown.contains(warning),
            "Markdown 与 JSON 的提示不一致，缺少：{warning}\n{markdown}"
        );
    }
    assert!(
        !markdown.contains("main.org"),
        "Markdown 不能泄露图外表名：{markdown}"
    );
}

#[test]
fn project_selection_and_scope_are_enforced() {
    let fixture = Fixture::new();
    let db = fixture.db.to_string_lossy().into_owned();
    let scope = er_project_scope(&ConnectionOptions::new(DbType::Sqlite, "", 0, "", "", &db));
    let project = serde_json::json!({
        "scope": scope,
        "selected": [{"schema": "main", "name": "org"}],
        "decisions": []
    });
    let project_path = fixture.path("project.json");
    fs::write(
        &project_path,
        serde_json::to_string_pretty(&project).unwrap(),
    )
    .unwrap();

    let out = fixture.path("selected.svg");
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--project",
        project_path.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_ok(&output);
    let svg = fixture.text("selected.svg");
    assert!(svg.contains("org"), "项目勾选的表应导出：{svg}");
    assert!(!svg.contains("app_user"), "未勾选的表不应导出：{svg}");

    let mismatch = serde_json::json!({
        "scope": {
            "db_type": "sqlite",
            "host": "",
            "port": 0,
            "database": fixture.path("other.db").to_string_lossy()
        },
        "selected": null,
        "decisions": []
    });
    let mismatch_path = fixture.path("mismatch.json");
    fs::write(
        &mismatch_path,
        serde_json::to_string_pretty(&mismatch).unwrap(),
    )
    .unwrap();
    let rejected = fixture.path("rejected.svg");
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--project",
        mismatch_path.to_str().unwrap(),
        "--out",
        rejected.to_str().unwrap(),
    ]);
    assert!(!output.status.success(), "scope 不一致必须拒绝");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("scope") || stderr.contains("作用域"),
        "应说明 scope 不一致：{stderr}"
    );
    assert!(!rejected.exists(), "校验失败不能产生输出");

    let null_scope = fixture.path("null-scope.json");
    fs::write(
        &null_scope,
        r#"{"scope":null,"selected":null,"decisions":[]}"#,
    )
    .unwrap();
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--project",
        null_scope.to_str().unwrap(),
        "--out",
        rejected.to_str().unwrap(),
    ]);
    assert_failed(&output, "scope");
    assert!(!rejected.exists());
}

#[test]
fn output_paths_cannot_overwrite_sqlite_source_or_project() {
    let fixture = Fixture::new();
    let db = fixture.db.to_string_lossy().into_owned();
    let original_db = fs::read(&fixture.db).unwrap();

    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--out",
        &db,
    ]);
    assert_failed(&output, "冲突");
    assert_eq!(fs::read(&fixture.db).unwrap(), original_db, "源库被破坏");

    let scope = er_project_scope(&ConnectionOptions::new(DbType::Sqlite, "", 0, "", "", &db));
    let project = serde_json::json!({
        "scope": scope,
        "selected": null,
        "decisions": []
    });
    let project_path = fixture.path("project.json");
    let original_project = serde_json::to_string_pretty(&project).unwrap();
    fs::write(&project_path, &original_project).unwrap();
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--project",
        project_path.to_str().unwrap(),
        "--out",
        project_path.to_str().unwrap(),
    ]);
    assert_failed(&output, "冲突");
    assert_eq!(
        fs::read_to_string(&project_path).unwrap(),
        original_project,
        "项目文件被破坏"
    );

    let same = fixture.path("same.svg");
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--out",
        same.to_str().unwrap(),
        "--report",
        fixture.path("sub/../same.svg").to_str().unwrap(),
    ]);
    assert_failed(&output, "冲突");
    assert!(!same.exists(), "输出与报告等价时不能写任何文件");
    assert_eq!(fs::read(&fixture.db).unwrap(), original_db);
}

#[test]
fn relative_output_aliases_are_detected_before_writing_the_database() {
    let fixture = Fixture::new();
    let db = fixture.db.to_string_lossy().into_owned();
    let original_db = fs::read(&fixture.db).unwrap();
    let output = fixture.run(&[
        "er",
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--out",
        "./nested/../demo.db",
    ]);
    assert_failed(&output, "冲突");
    assert_eq!(fs::read(&fixture.db).unwrap(), original_db, "源库被破坏");
}

#[test]
fn legacy_export_command_still_works() {
    let fixture = Fixture::new();
    let db = fixture.db.to_string_lossy().into_owned();
    let output = fixture.run(&[
        "export",
        "--driver",
        "sqlite",
        "--database",
        &db,
        "--out",
        "legacy.md",
    ]);
    assert_ok(&output);
    let markdown = fixture.text("legacy.md");
    assert!(markdown.starts_with("# "), "{markdown}");
    assert!(markdown.contains("app_user"), "{markdown}");
    assert!(markdown.contains("org_id"), "{markdown}");
}

#[test]
fn er_help_lists_native_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_dbs"))
        .args(["er", "export", "--help"])
        .output()
        .expect("运行 dbs");
    assert_ok(&output);
    let help = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "--database",
        "--out",
        "--report",
        "--project",
        "--all-columns",
    ] {
        assert!(help.contains(expected), "帮助缺少 {expected}：\n{help}");
    }
}
