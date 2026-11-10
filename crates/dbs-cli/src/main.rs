//! DBScribe 命令行入口。

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dbs_core::{DatabaseInfo, DbType, SheetMode};
use dbs_db::{load_tables, MySqlOptions};
use dbs_doc::{export_excel, ExcelExportOptions};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "dbs",
    version,
    about = "DBScribe —— 跨平台数据库结构文档工具",
    long_about = "读取数据库表结构，生成结构清晰的设计文档。\n\
                  多 Sheet 模式下会自动处理工作表命名冲突，任何表都不会丢失。"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 导出一个数据库的结构文档
    Export(ExportArgs),
}

#[derive(Parser, Debug)]
struct ExportArgs {
    /// 数据库类型：mysql / postgres / sqlserver / sqlite / oracle
    #[arg(long, default_value = "mysql")]
    driver: String,

    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// 端口，缺省按数据库类型取默认值（mysql=3306）
    #[arg(long)]
    port: Option<u16>,

    #[arg(long, default_value = "root")]
    user: String,

    /// 密码。建议用环境变量 DBS_PASSWORD 传入，避免出现在命令历史里
    #[arg(long, env = "DBS_PASSWORD", default_value = "", hide_env_values = true)]
    password: String,

    /// 要导出的数据库名
    #[arg(long)]
    database: String,

    /// 输出文件路径（.xlsx）
    #[arg(long, short)]
    out: PathBuf,

    /// 文档标题，缺省为「{数据库名}数据库设计文档」
    #[arg(long)]
    title: Option<String>,

    /// 工作表模式：multi=每个表一个 Sheet（默认），single=所有表放一个 Sheet
    #[arg(long, default_value = "multi")]
    sheet_mode: String,

    /// 详细日志
    #[arg(long, short)]
    verbose: bool,
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("导出失败：{err:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Export(args) => run_export(args),
    }
}

fn init_logging(verbose: bool) {
    let level = if verbose { "debug" } else { "info" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!("dbs={level},dbs_db={level}"))
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn run_export(args: ExportArgs) -> Result<()> {
    init_logging(args.verbose);

    let db_type: DbType = args
        .driver
        .parse()
        .with_context(|| format!("无法识别的 --driver：{}", args.driver))?;
    let mode: SheetMode = args
        .sheet_mode
        .parse()
        .with_context(|| format!("无法识别的 --sheet-mode：{}", args.sheet_mode))?;

    let out = args.out.clone();
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建输出目录失败：{}", parent.display()))?;
        }
    }

    let title = args
        .title
        .clone()
        .unwrap_or_else(|| format!("{}数据库设计文档", args.database));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("初始化异步运行时失败")?;

    let tables = runtime.block_on(async {
        match db_type {
            DbType::MySql => {
                let opts = MySqlOptions {
                    host: args.host.clone(),
                    port: args.port.unwrap_or(3306),
                    user: args.user.clone(),
                    password: args.password.clone(),
                    database: args.database.clone(),
                };
                tracing::info!("连接 {}", opts.describe());
                load_tables(&opts).await
            }
            other => anyhow::bail!("暂不支持的数据库类型：{other}（当前已实现 mysql）"),
        }
    })?;

    if tables.is_empty() {
        anyhow::bail!("数据库 {} 中没有任何基础表", args.database);
    }

    let opts = ExcelExportOptions::new(
        title,
        mode,
        DatabaseInfo::new(db_type, args.database.clone()),
    );
    let report = export_excel(&tables, &opts, &out)?;

    println!("导出成功：{}", report.out_path.display());
    println!(
        "  表数量：{}    工作表：{}    模式：{}",
        report.table_count,
        report.sheet_count,
        match mode {
            SheetMode::Single => "单 Sheet",
            SheetMode::Multi => "每表一个 Sheet",
        }
    );
    if !report.renamed.is_empty() {
        println!(
            "  有 {} 个工作表因名称超长/非法/重复而被自动改名（原版会直接导出失败）：",
            report.renamed.len()
        );
        for (original, actual) in &report.renamed {
            println!("    {original}  ->  {actual}");
        }
    }
    Ok(())
}
