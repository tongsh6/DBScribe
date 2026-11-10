//! DBScribe 命令行入口。

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dbs_core::{DatabaseInfo, DbType, OutputFormat, SheetMode};
use dbs_db::{list_databases, load_tables, ConnectionOptions};
use dbs_doc::{export, ExportOptions};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "dbs",
    version,
    about = "DBScribe —— 跨平台数据库结构文档工具",
    long_about = "读取数据库表结构，生成结构清晰的设计文档。\n\
                  支持 Excel / Markdown / HTML / JSON / XML；多 Sheet 导出会自动处理工作表命名冲突，任何表都不会丢失。"
)]
struct Cli {
    /// 输出详细日志
    #[arg(long, short, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 导出一个数据库的结构文档
    Export(ExportArgs),
    /// 列出连接下可导出的数据库
    Databases(DatabasesArgs),
}

/// 数据库连接参数（各子命令共用）。
#[derive(Parser, Debug, Clone)]
struct ConnectionArgs {
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
}

impl ConnectionArgs {
    fn db_type(&self) -> Result<DbType> {
        self.driver
            .parse()
            .with_context(|| format!("无法识别的 --driver：{}", self.driver))
    }

    fn default_port(&self, db_type: DbType) -> u16 {
        self.port.unwrap_or(match db_type {
            DbType::MySql => 3306,
            DbType::PostgreSql => 5432,
            DbType::SqlServer => 1433,
            DbType::Oracle => 1521,
            DbType::Sqlite => 0,
        })
    }

    fn to_options(&self, database: &str) -> Result<(DbType, ConnectionOptions)> {
        let db_type = self.db_type()?;
        let opts = ConnectionOptions::new(
            db_type,
            self.host.clone(),
            self.default_port(db_type),
            self.user.clone(),
            self.password.clone(),
            database,
        );
        Ok((db_type, opts))
    }
}

#[derive(Parser, Debug)]
struct ExportArgs {
    #[command(flatten)]
    conn: ConnectionArgs,

    /// 要导出的数据库名
    #[arg(long)]
    database: String,

    /// 输出文件路径；扩展名可推断格式
    #[arg(long, short)]
    out: PathBuf,

    /// 文档标题，缺省为「{数据库名}数据库设计文档」
    #[arg(long)]
    title: Option<String>,

    /// 工作表模式（仅 Excel 生效）：multi=每个表一个 Sheet（默认），single=所有表放一个 Sheet
    #[arg(long, default_value = "multi")]
    sheet_mode: String,

    /// 输出格式：excel / markdown / html / json / xml；缺省按 --out 扩展名推断
    #[arg(long)]
    format: Option<String>,
}

#[derive(Parser, Debug)]
struct DatabasesArgs {
    #[command(flatten)]
    conn: ConnectionArgs,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    if let Err(err) = run(cli).await {
        eprintln!("失败：{err:#}");
        std::process::exit(1);
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

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Export(args) => run_export(args).await,
        Command::Databases(args) => run_databases(args).await,
    }
}

async fn run_databases(args: DatabasesArgs) -> Result<()> {
    let (db_type, opts) = args.conn.to_options("")?;
    tracing::info!("连接 {}", opts.describe());
    let databases = list_databases(&opts).await?;
    if databases.is_empty() {
        println!("没有可导出的数据库（{}）", db_type);
        return Ok(());
    }
    println!("共 {} 个数据库：", databases.len());
    for name in databases {
        println!("  {name}");
    }
    Ok(())
}

async fn run_export(args: ExportArgs) -> Result<()> {
    let mode: SheetMode = args
        .sheet_mode
        .parse()
        .with_context(|| format!("无法识别的 --sheet-mode：{}", args.sheet_mode))?;

    let format = resolve_format(&args)?;

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

    let (db_type, opts) = args.conn.to_options(&args.database)?;
    tracing::info!("连接 {}", opts.describe());
    let tables = load_tables(&opts).await?;
    if tables.is_empty() {
        anyhow::bail!("数据库 {} 中没有任何基础表", args.database);
    }

    let options = ExportOptions::new(
        title,
        mode,
        DatabaseInfo::new(db_type, args.database.clone()),
        format,
    );
    let report = export(&tables, &options, &out, &mut |_, _| {})?;

    println!("导出成功：{}", report.out_path.display());
    if report.sheet_count > 0 {
        println!(
            "  表数量：{}    工作表：{}    模式：{}",
            report.table_count,
            report.sheet_count,
            match mode {
                SheetMode::Single => "单 Sheet",
                SheetMode::Multi => "每表一个 Sheet",
            }
        );
    } else {
        println!("  表数量：{}    格式：{}", report.table_count, format);
    }
    if !report.renamed.is_empty() {
        println!(
            "  有 {} 个工作表因名称超长/非法/重复而被自动改名：",
            report.renamed.len()
        );
        for (original, actual) in &report.renamed {
            println!("    {original}  ->  {actual}");
        }
    }
    Ok(())
}

/// 解析输出格式：显式 `--format` 优先，其次按 `--out` 扩展名推断，最后兜底 Excel。
fn resolve_format(args: &ExportArgs) -> Result<OutputFormat> {
    if let Some(raw) = &args.format {
        return raw
            .parse()
            .with_context(|| format!("无法识别的 --format：{raw}"));
    }
    Ok(args
        .out
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(OutputFormat::from_extension)
        .unwrap_or_default())
}
