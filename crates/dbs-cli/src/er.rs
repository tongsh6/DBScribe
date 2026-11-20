//! 独立 ER 图与关系报告的命令行入口。
use super::ConnectionArgs;
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use dbs_core::er::{build_delivery_diagram, ErProject, ProjectScope};
use dbs_core::DbType;
use dbs_db::er_project_scope;
use dbs_doc::er_file::{ensure_no_path_conflict, StagedErFile};
use dbs_doc::{render_er_report, render_er_svg};
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
pub(super) struct ErArgs {
    #[command(subcommand)]
    command: ErCommand,
}

#[derive(Subcommand, Debug)]
enum ErCommand {
    /// 从数据库目录推断关系并导出离线 SVG
    Export(ErExportArgs),
}

/// ER 图导出参数。
#[derive(Parser, Debug)]
struct ErExportArgs {
    #[command(flatten)]
    conn: ConnectionArgs,

    /// 要读取的数据库名；SQLite 时填 .db 文件路径
    #[arg(long)]
    database: String,

    /// SVG 输出路径
    #[arg(long, short)]
    out: PathBuf,

    /// 可选：写入来源、理由与候选报告（.json 为结构化结果，其余为 Markdown）
    #[arg(long)]
    report: Option<PathBuf>,

    /// 可选：项目修正文件（JSON）；显式读取时 scope 必须与当前连接一致
    #[arg(long)]
    project: Option<PathBuf>,

    /// 显示全部字段（默认）
    #[arg(long = "all-columns", conflicts_with = "key_columns")]
    _all_columns: bool,

    /// 只显示主键、唯一键与关系字段
    #[arg(long)]
    key_columns: bool,

    /// 图标题，缺省为「{数据库名} 数据库 ER 图」
    #[arg(long)]
    title: Option<String>,
}

pub(super) async fn run(args: ErArgs) -> Result<()> {
    match args.command {
        ErCommand::Export(args) => export(args).await,
    }
}

async fn export(args: ErExportArgs) -> Result<()> {
    let (db_type, opts) = args.conn.to_options(&args.database)?;

    // 先做纯路径校验，任何读写之前就拒绝会覆盖源库/项目文件的组合。
    validate_er_paths(&args, db_type)?;

    let scope = er_project_scope(&opts);
    let project = match &args.project {
        Some(path) => read_er_project(path, &scope)?,
        None => ErProject::default(),
    };

    tracing::info!("读取 ER 目录 {}", opts.describe());
    let catalog = dbs_db::load_er_catalog(&opts)
        .await
        .with_context(|| format!("读取 ER 目录失败：{}", opts.describe()))?;
    let diagram = build_delivery_diagram(&catalog, None, &project)
        .map_err(|message| anyhow::anyhow!(message))?;
    if diagram.tables.is_empty() {
        bail!("数据库 {} 中没有可导出的表", args.database);
    }

    let display_name = if db_type == DbType::Sqlite {
        Path::new(&args.database)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| args.database.clone())
    } else {
        args.database.clone()
    };
    let title = args.title.clone().unwrap_or_else(|| {
        if db_type == DbType::MongoDb {
            format!("{display_name} 集合关系图")
        } else {
            format!("{display_name} 数据库 ER 图")
        }
    });
    let show_all_columns = !args.key_columns;
    let svg = render_er_svg(&diagram, &title, show_all_columns);
    let image = StagedErFile::prepare(&args.out, svg.as_bytes())?;
    let report = if let Some(path) = &args.report {
        let content = if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            serde_json::to_string_pretty(&diagram)?
        } else {
            render_er_report(&diagram, &title, &scope, show_all_columns)
        };
        Some(StagedErFile::prepare(path, content.as_bytes())?)
    } else {
        None
    };
    if let Some(report) = report {
        report.save()?;
    }
    image.save()?;

    println!("导出成功：{}", args.out.display());
    println!(
        "  实体数量：{}    关系：{}    候选：{}    图外引用：{}",
        diagram.tables.len(),
        diagram.relations.len(),
        diagram.candidates.len(),
        diagram.external_count
    );
    if let Some(report_path) = &args.report {
        println!("  报告：{}", report_path.display());
    }
    for warning in &diagram.warnings {
        println!("  提示：{warning}");
    }
    Ok(())
}

fn read_er_project(path: &Path, current: &ProjectScope) -> Result<ErProject> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取项目文件失败：{}", path.display()))?;
    let project: ErProject = serde_json::from_str(&text)
        .with_context(|| format!("解析项目文件失败：{}", path.display()))?;
    let scope = project.scope.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "项目文件缺少有效 scope：{}；显式使用项目文件时必须绑定当前连接",
            path.display()
        )
    })?;
    if scope != current {
        bail!(
            "项目 scope 与当前连接不一致（scope 冲突）：项目 {}，当前 {}。已拒绝使用，避免把修正应用到错误的数据库",
            scope_text(scope),
            scope_text(current)
        );
    }
    Ok(project)
}

fn scope_text(scope: &ProjectScope) -> String {
    scope.label()
}

fn validate_er_paths(args: &ErExportArgs, db_type: DbType) -> Result<()> {
    let mut outputs: Vec<(&str, &Path)> = vec![("输出 SVG", args.out.as_path())];
    if let Some(report) = &args.report {
        outputs.push(("关系报告", report.as_path()));
    }
    let mut protected: Vec<(&str, &Path)> = Vec::new();
    if db_type == DbType::Sqlite {
        protected.push(("SQLite 源库", Path::new(&args.database)));
    }
    if let Some(project) = &args.project {
        protected.push(("项目文件", project.as_path()));
    }

    for first in 0..outputs.len() {
        for second in first + 1..outputs.len() {
            ensure_no_path_conflict(outputs[first], outputs[second])?;
        }
        for source in &protected {
            ensure_no_path_conflict(outputs[first], *source)?;
        }
    }
    for first in 0..protected.len() {
        for second in first + 1..protected.len() {
            ensure_no_path_conflict(protected[first], protected[second])?;
        }
    }
    Ok(())
}
