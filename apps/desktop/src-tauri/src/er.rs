//! 桌面端 ER 图：目录缓存、选表范围、人工校对与 SVG / 项目文件读写。

use crate::models::{ConnectionParams, HistoryEntry};
use crate::store;
use chrono::Local;
use dbs_core::er::{
    Decision, DecisionAction, Endpoint, ErCatalog, ErDiagram, ErProject, ErSnapshot, ProjectScope,
    TableId,
};
use dbs_core::{DbType, TableInfo};
use dbs_db::er_project_scope;
use dbs_doc::er_file::{ensure_no_path_conflict, StagedErFile};
use dbs_doc::render_er_svg_with_collapsed;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use tauri::{AppHandle, State};

/// 前端传来的表身份。兼容旧调用只给纯表名；同名指向多张表时必须报歧义。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TableRefInput {
    Id(TableIdInput),
    Name(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableIdInput {
    #[serde(default)]
    pub schema: String,
    pub name: String,
}

/// 目录中的一张表：供界面调整成图范围与手工补充关系。
#[derive(Debug, Clone, Serialize)]
pub struct ErTableChoice {
    pub schema: String,
    pub name: String,
    pub comment: String,
    pub columns: Vec<String>,
    /// 完整唯一键；手工补充关系时给出目标字段建议。
    pub keys: Vec<Vec<String>>,
}

/// 一次预览的完整结果。结构与 SVG 来自同一份目录快照。
#[derive(Debug, Clone, Serialize)]
pub struct ErPreview {
    pub session_id: u64,
    pub revision: u64,
    pub scope: ProjectScope,
    pub scope_label: String,
    pub tables: Vec<ErTableChoice>,
    pub selected: Vec<TableId>,
    /// 字段已收起的表；展开状态与导出一致。
    pub collapsed: Vec<TableId>,
    pub diagram: ErDiagram,
    pub project: ErProject,
    pub svg: String,
    pub title: String,
    pub show_all_columns: bool,
}

/// 默认展开全部字段；旧请求缺省时也保持「全部字段」。
fn default_show_all() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErOpenParams {
    pub conn: ConnectionParams,
    pub database: String,
    /// `None` 表示整个目录；空数组表示用户没有勾选任何表。
    #[serde(default)]
    pub selected: Option<Vec<TableRefInput>>,
    #[serde(default)]
    pub project: Option<ErProject>,
    /// 前端单调递增的请求号，用于拒绝晚到的旧请求。
    pub request_id: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_show_all")]
    pub show_all_columns: bool,
    /// 缺省表示全部展开；旧前端不传该字段仍可用。
    #[serde(default)]
    pub collapsed: Vec<TableRefInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErRenderParams {
    pub session_id: u64,
    pub revision: u64,
    pub selected: Vec<TableRefInput>,
    pub project: ErProject,
    #[serde(default = "default_show_all")]
    pub show_all_columns: bool,
    /// `None` 表示沿用上次预览的展开状态（兼容旧前端）。
    #[serde(default)]
    pub collapsed: Option<Vec<TableRefInput>>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErDecisionParams {
    pub session_id: u64,
    pub source: Endpoint,
    pub target: Endpoint,
    pub action: DecisionAction,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErExportParams {
    pub session_id: u64,
    pub revision: u64,
    pub selected: Vec<TableRefInput>,
    pub project: ErProject,
    pub out_path: String,
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_show_all")]
    pub show_all_columns: bool,
    /// 缺省时按上次成功预览的逐表展开状态导出。
    #[serde(default)]
    pub collapsed: Option<Vec<TableRefInput>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErExportResult {
    pub out_path: String,
    pub table_count: usize,
    pub relation_count: usize,
    pub candidate_count: usize,
    pub external_count: usize,
    pub external_candidate_count: usize,
    pub warnings: Vec<String>,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErProjectFileParams {
    pub conn: ConnectionParams,
    pub database: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErWriteProjectParams {
    pub conn: ConnectionParams,
    pub database: String,
    pub path: String,
    pub project: ErProject,
}

struct ErSession {
    id: u64,
    revision: u64,
    scope: ProjectScope,
    // 桌面端缓存 ErSnapshot：勾选切换只重投影，不重复推断；
    // 与 build_diagram / make_decision 共用同一份 project_diagram / reviewed_decision 实现。
    snapshot: ErSnapshot,
    choices: Vec<ErTableChoice>,
    selected: Vec<TableId>,
    collapsed: BTreeSet<TableId>,
    show_all_columns: bool,
    title: String,
    project_signature: String,
}

/// 桌面端只保存最近一次目录快照；切库或重新打开会整体替换，旧会话 id 随即失效。
#[derive(Default)]
pub struct ErState {
    session: Mutex<Option<ErSession>>,
    next_session_id: AtomicU64,
    /// 每次「打开」在读取目录前领取序号，只允许最后开始的那次安装快照。
    open_seq: AtomicU64,
}

fn lock_session(state: &ErState) -> std::sync::MutexGuard<'_, Option<ErSession>> {
    state
        .session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn table_choices(catalog: &ErCatalog) -> Vec<ErTableChoice> {
    let mut tables: Vec<&TableInfo> = catalog.tables.iter().collect();
    tables.sort_by_key(|table| (&table.schema, &table.name));
    tables
        .into_iter()
        .map(|table| {
            let id = TableId::from(table);
            let mut keys: Vec<Vec<String>> = catalog
                .keys
                .iter()
                .filter(|key| key.table == id)
                .map(|key| key.columns.clone())
                .collect();
            keys.sort();
            keys.dedup();
            ErTableChoice {
                schema: table.schema.clone(),
                name: table.name.clone(),
                comment: table.comment.clone(),
                columns: table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                keys,
            }
        })
        .collect()
}

fn all_table_ids(choices: &[ErTableChoice]) -> Vec<TableId> {
    choices
        .iter()
        .map(|choice| TableId {
            schema: choice.schema.clone(),
            name: choice.name.clone(),
        })
        .collect()
}

fn resolve_table_refs(
    choices: &[ErTableChoice],
    requested: &[TableRefInput],
) -> Result<Vec<TableId>, String> {
    let available: Vec<(String, String)> = choices
        .iter()
        .map(|choice| (choice.schema.clone(), choice.name.clone()))
        .collect();
    resolve_table_identities(&available, requested)
}

fn resolve_collapsed(
    choices: &[ErTableChoice],
    requested: &[TableRefInput],
) -> Result<BTreeSet<TableId>, String> {
    if requested.is_empty() {
        return Ok(BTreeSet::new());
    }
    Ok(resolve_table_refs(choices, requested)?
        .into_iter()
        .collect())
}

/// 共用表身份解析：显式 schema 精确匹配；旧纯表名只有唯一指向时才接受。
pub(crate) fn resolve_table_identities(
    available: &[(String, String)],
    requested: &[TableRefInput],
) -> Result<Vec<TableId>, String> {
    if requested.is_empty() {
        return Err("请至少选择一张表".into());
    }
    let mut by_name = std::collections::BTreeMap::<&str, Vec<&(String, String)>>::new();
    for id in available {
        by_name.entry(&id.1).or_default().push(id);
    }
    let mut matched = BTreeSet::new();
    for input in requested {
        let (schema, name) = match input {
            TableRefInput::Id(id) => (Some(id.schema.as_str()), id.name.as_str()),
            TableRefInput::Name(name) => (None, name.as_str()),
        };
        if name.is_empty() {
            return Err("表名不能为空".into());
        }
        let mut candidates = by_name.get(name).cloned().unwrap_or_default();
        if let Some(schema) = schema {
            candidates.retain(|id| id.0 == schema);
        }
        match candidates.as_slice() {
            [] if schema.is_none() => return Err(format!("表不存在：{name}")),
            [] => return Err(format!("表不存在：{}.{name}", schema.unwrap_or_default())),
            [candidate] => {
                matched.insert(TableId {
                    schema: candidate.0.clone(),
                    name: candidate.1.clone(),
                });
            }
            many => {
                let labels = many
                    .iter()
                    .map(|(candidate_schema, candidate)| {
                        if candidate_schema.is_empty() {
                            candidate.clone()
                        } else {
                            format!("{candidate_schema}.{candidate}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("、");
                return Err(format!(
                    "纯表名「{name}」指向多张表（{labels}），请明确 schema 后重试"
                ));
            }
        }
    }
    // 按目录顺序返回，避免前端勾选顺序改变布局。
    Ok(available
        .iter()
        .filter_map(|(schema, name)| {
            let id = TableId {
                schema: schema.clone(),
                name: name.clone(),
            };
            matched.contains(&id).then_some(id)
        })
        .collect())
}

fn prepare_project(
    mut project: ErProject,
    scope: &ProjectScope,
    selected: &[TableId],
) -> Result<ErProject, String> {
    if let Some(current) = &project.scope {
        if current != scope {
            return Err(format!(
                "项目 scope 与当前连接不一致（scope 冲突）：项目 {}，当前 {}。已拒绝使用，避免把修正应用到错误的数据库",
                scope_text(current),
                scope_text(scope)
            ));
        }
    }
    project.scope = Some(scope.clone());
    project.selected = Some(selected.to_vec());
    Ok(project)
}

fn project_signature(project: &ErProject) -> String {
    serde_json::to_string(project).unwrap_or_default()
}

fn scope_text(scope: &ProjectScope) -> String {
    scope.label()
}

fn default_title(database: &str, db_type: DbType) -> String {
    let display = if db_type == DbType::Sqlite {
        Path::new(database)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| database.to_string())
    } else {
        database.to_string()
    };
    if db_type == DbType::MongoDb {
        format!("{display} 集合关系图")
    } else {
        format!("{display} 数据库 ER 图")
    }
}

fn render_preview(
    session: &ErSession,
    revision: u64,
    diagram: ErDiagram,
    project: ErProject,
    title: String,
    show_all_columns: bool,
    svg: String,
) -> ErPreview {
    ErPreview {
        session_id: session.id,
        revision,
        scope: session.scope.clone(),
        scope_label: scope_text(&session.scope),
        tables: session.choices.clone(),
        selected: session.selected.clone(),
        collapsed: session.collapsed.iter().cloned().collect(),
        diagram,
        project,
        svg,
        title,
        show_all_columns,
    }
}

/// 打开 ER 预览：读取一次目录并缓存全域裁决，后续勾选/校对复用该快照。
#[tauri::command]
pub async fn er_open_cmd(
    state: State<'_, ErState>,
    params: ErOpenParams,
) -> Result<ErPreview, String> {
    // 在开始读取目录前领取序号：并发打开时，晚开始的那次才有资格安装快照。
    let opts = super::commands::to_options(&params.conn, &params.database)?;
    let open_seq = state.open_seq.fetch_add(1, Ordering::Relaxed) + 1;
    let scope = er_project_scope(&opts);
    let catalog = dbs_db::load_er_catalog(&opts)
        .await
        .map_err(super::commands::to_msg)?;
    if state.open_seq.load(Ordering::Relaxed) != open_seq {
        return Err("ER 请求已过期，请重新打开预览".into());
    }
    let choices = table_choices(&catalog);
    if choices.is_empty() {
        return Err("数据库中没有可导出的表".into());
    }
    let selected = match params.selected.as_deref() {
        Some(requested) => resolve_table_refs(&choices, requested)?,
        None => all_table_ids(&choices),
    };
    let project = prepare_project(params.project.unwrap_or_default(), &scope, &selected)?;
    let collapsed = resolve_collapsed(&choices, &params.collapsed)?;
    let snapshot = ErSnapshot::new(catalog);
    let diagram = snapshot.diagram(Some(&selected), &project)?;
    // 前端预览保留可见来源的全部候选；SVG 只使用对外交付投影。
    let delivered = snapshot.delivery(Some(&selected), &project)?;
    let title = if params.title.trim().is_empty() {
        default_title(&params.database, opts.db_type)
    } else {
        params.title.trim().to_string()
    };
    let svg = render_er_svg_with_collapsed(&delivered, &title, params.show_all_columns, &collapsed);

    let mut session = lock_session(&state);
    if state.open_seq.load(Ordering::Relaxed) != open_seq {
        return Err("ER 请求已过期，请重新打开预览".into());
    }
    let id = state.next_session_id.fetch_add(1, Ordering::Relaxed) + 1;
    let signature = project_signature(&project);
    let preview = ErPreview {
        session_id: id,
        revision: params.request_id,
        scope: scope.clone(),
        scope_label: scope_text(&scope),
        tables: choices.clone(),
        selected: selected.clone(),
        collapsed: collapsed.iter().cloned().collect(),
        diagram,
        project: project.clone(),
        svg,
        title: title.clone(),
        show_all_columns: params.show_all_columns,
    };
    *session = Some(ErSession {
        id,
        revision: params.request_id,
        scope,
        snapshot,
        choices,
        selected,
        collapsed,
        show_all_columns: params.show_all_columns,
        title,
        project_signature: signature,
    });
    Ok(preview)
}

/// 只调整成图范围/字段范围/项目修正，不重新读取数据库。
#[tauri::command]
pub async fn er_render_cmd(
    state: State<'_, ErState>,
    params: ErRenderParams,
) -> Result<ErPreview, String> {
    let mut guard = lock_session(&state);
    let session = guard
        .as_mut()
        .ok_or_else(|| "ER 预览已失效，请重新打开".to_string())?;
    if session.id != params.session_id {
        return Err("ER 预览已切换，请重新打开".into());
    }
    if params.revision < session.revision {
        return Err("ER 预览请求已过期".into());
    }
    let selected = resolve_table_refs(&session.choices, &params.selected)?;
    let project = prepare_project(params.project, &session.scope, &selected)?;
    let diagram = session.snapshot.diagram(Some(&selected), &project)?;
    let delivered = session.snapshot.delivery(Some(&selected), &project)?;
    let title = match params.title {
        Some(title) if !title.trim().is_empty() => title.trim().to_string(),
        _ => session.title.clone(),
    };
    let collapsed = match &params.collapsed {
        Some(requested) => resolve_collapsed(&session.choices, requested)?,
        None => session.collapsed.clone(),
    };
    let svg = render_er_svg_with_collapsed(&delivered, &title, params.show_all_columns, &collapsed);
    let signature = project_signature(&project);

    session.revision = params.revision;
    session.selected = selected;
    session.collapsed = collapsed;
    session.project_signature = signature;
    session.show_all_columns = params.show_all_columns;
    session.title = title.clone();
    Ok(render_preview(
        session,
        params.revision,
        diagram,
        project,
        title,
        params.show_all_columns,
        svg,
    ))
}

/// 用同一份目录快照复核人工决定；采纳/排除失败时直接返回原因。
#[tauri::command]
pub async fn er_decide_cmd(
    state: State<'_, ErState>,
    params: ErDecisionParams,
) -> Result<Decision, String> {
    let guard = lock_session(&state);
    let session = guard
        .as_ref()
        .ok_or_else(|| "ER 预览已失效，请重新打开".to_string())?;
    if session.id != params.session_id {
        return Err("ER 预览已切换，请重新打开".into());
    }
    session
        .snapshot
        .decision(params.source, params.target, params.action)
}

/// 导出前复核：修订号、选表、项目修正与字段范围必须与最近一次预览一致。
fn validate_export_request(
    session: &ErSession,
    revision: u64,
    selected: &[TableId],
    project_signature: &str,
    show_all_columns: bool,
    collapsed: &BTreeSet<TableId>,
) -> Result<(), String> {
    if session.revision != revision {
        return Err("预览已更新，请等待刷新后再导出".into());
    }
    if selected != session.selected {
        return Err("选表范围已变化，请重新预览后再导出".into());
    }
    if project_signature != session.project_signature {
        return Err("项目修正已变化，请重新预览后再导出".into());
    }
    if show_all_columns != session.show_all_columns {
        return Err("字段显示范围已变化，请重新预览后再导出".into());
    }
    if collapsed != &session.collapsed {
        return Err("字段展开状态已变化，请等待预览更新后再导出".into());
    }
    Ok(())
}

/// 导出当前预览对应的 SVG。修订号、选表与项目修正都必须与最近一次预览一致。
#[tauri::command]
pub async fn er_export_cmd(
    app: AppHandle,
    state: State<'_, ErState>,
    params: ErExportParams,
) -> Result<ErExportResult, String> {
    let started = Instant::now();
    let out = Path::new(&params.out_path);
    if out.as_os_str().is_empty() {
        return Err("请选择 SVG 输出路径".into());
    }

    let (diagram, svg, title, scope, selected) = {
        let guard = lock_session(&state);
        let session = guard
            .as_ref()
            .ok_or_else(|| "ER 预览已失效，请重新打开".to_string())?;
        if session.id != params.session_id {
            return Err("ER 预览已切换，请重新打开".into());
        }
        let selected = resolve_table_refs(&session.choices, &params.selected)?;
        let project = prepare_project(params.project, &session.scope, &selected)?;
        let signature = project_signature(&project);
        let collapsed = match &params.collapsed {
            Some(requested) => resolve_collapsed(&session.choices, requested)?,
            None => session.collapsed.clone(),
        };
        validate_export_request(
            session,
            params.revision,
            &selected,
            &signature,
            params.show_all_columns,
            &collapsed,
        )?;
        if !params.title.trim().is_empty() && params.title.trim() != session.title {
            return Err("标题已变化，请重新预览后再导出".into());
        }
        let title = session.title.clone();
        // 导出与 SVG 预览、CLI 产物共用同一份交付投影。
        let diagram = session.snapshot.delivery(Some(&selected), &project)?;
        let svg =
            render_er_svg_with_collapsed(&diagram, &title, params.show_all_columns, &collapsed);
        (diagram, svg, title, session.scope.clone(), selected)
    };

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败：{e}"))?;
        }
    }
    // 目标文件已存在时追加时间戳，避免静默覆盖上一次导出的结果。
    let target = if out.exists() {
        let stem = out
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("er");
        let ext = out
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("svg");
        let stamp = Local::now().format("%Y%m%d_%H%M%S");
        out.with_file_name(format!("{stem}_{stamp}.{ext}"))
    } else {
        out.to_path_buf()
    };
    if target != out {
        eprintln!("[DBScribe] 目标文件已存在，改写入 {}", target.display());
    }
    protect_source(&scope, out)?;
    StagedErFile::prepare(&target, svg.as_bytes())
        .and_then(StagedErFile::save_new)
        .map_err(|e| format!("写入 SVG 失败：{e:#}"))?;
    let elapsed_ms = started.elapsed().as_millis();
    let result = ErExportResult {
        out_path: target.display().to_string(),
        table_count: diagram.tables.len(),
        relation_count: diagram.relations.len(),
        candidate_count: diagram.candidates.len(),
        external_count: diagram.external_count,
        external_candidate_count: diagram.external_candidate_count,
        warnings: diagram.warnings.clone(),
        elapsed_ms,
    };

    // 与文档导出共用同一份历史；同一路径只保留最新一条。
    let mut app_state = store::load(&app);
    app_state
        .history
        .retain(|entry| entry.out_path != result.out_path);
    app_state.history.insert(
        0,
        HistoryEntry {
            time: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            database: scope.database.clone(),
            format: "svg".into(),
            out_path: result.out_path.clone(),
            table_count: result.table_count,
            elapsed_ms,
        },
    );
    if let Err(err) = store::save(&app, &app_state) {
        eprintln!("[DBScribe] 写入导出历史失败：{err:#}");
    }

    eprintln!(
        "[DBScribe] ER 导出 {} 张表 / {} 条关系（{}），标题「{}」",
        result.table_count,
        result.relation_count,
        selected.len(),
        title
    );
    Ok(result)
}

/// 读取与 CLI 共用的 ErProject JSON；scope 必须与当前连接一致。
#[tauri::command]
pub async fn er_read_project_cmd(params: ErProjectFileParams) -> Result<ErProject, String> {
    let opts = super::commands::to_options(&params.conn, &params.database)?;
    let current = er_project_scope(&opts);
    let text = std::fs::read_to_string(&params.path)
        .map_err(|e| format!("读取项目文件失败：{}：{e}", params.path))?;
    let project: ErProject = serde_json::from_str(&text)
        .map_err(|e| format!("解析项目文件失败：{}：{e}", params.path))?;
    let scope = project.scope.as_ref().ok_or_else(|| {
        format!(
            "项目文件缺少有效 scope：{}；显式使用项目文件时必须绑定当前连接",
            params.path
        )
    })?;
    if scope != &current {
        return Err(format!(
            "项目 scope 与当前连接不一致（scope 冲突）：项目 {}，当前 {}。已拒绝使用，避免把修正应用到错误的数据库",
            scope_text(scope),
            scope_text(&current)
        ));
    }
    Ok(project)
}

/// 保存与 CLI 共用的 ErProject JSON；写入前补齐当前连接的 scope。
#[tauri::command]
pub async fn er_write_project_cmd(params: ErWriteProjectParams) -> Result<ErProject, String> {
    if params.path.trim().is_empty() {
        return Err("请选择项目文件路径".into());
    }
    let opts = super::commands::to_options(&params.conn, &params.database)?;
    let scope = er_project_scope(&opts);
    protect_source(&scope, Path::new(&params.path))?;
    let mut project = params.project;
    if let Some(current) = &project.scope {
        if current != &scope {
            return Err(format!(
                "项目 scope 与当前连接不一致（scope 冲突）：项目 {}，当前 {}。已拒绝保存，避免把修正写到错误的数据库",
                scope_text(current),
                scope_text(&scope)
            ));
        }
    }
    project.scope = Some(scope);
    let path = Path::new(&params.path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
        }
    }
    let text =
        serde_json::to_string_pretty(&project).map_err(|e| format!("序列化项目文件失败：{e}"))?;
    StagedErFile::prepare(path, text.as_bytes())
        .and_then(StagedErFile::save)
        .map_err(|e| format!("写入项目文件失败：{e:#}"))?;
    Ok(project)
}

fn protect_source(scope: &ProjectScope, output: &Path) -> Result<(), String> {
    match scope.db_type {
        DbType::Sqlite => ensure_no_path_conflict(
            ("输出文件", output),
            ("SQLite 源库", Path::new(&scope.database)),
        )
        .map_err(|e| e.to_string()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbs_core::DbType;

    fn choice(schema: &str, name: &str) -> ErTableChoice {
        ErTableChoice {
            schema: schema.into(),
            name: name.into(),
            comment: String::new(),
            columns: vec!["id".into()],
            keys: vec![vec!["id".into()]],
        }
    }

    fn scope(database: &str) -> ProjectScope {
        ProjectScope {
            db_type: DbType::PostgreSql,
            host: "localhost".into(),
            port: 5432,
            database: database.into(),
        }
    }

    #[test]
    fn bare_name_resolves_only_when_unique() {
        let choices = vec![choice("public", "users"), choice("sales", "orders")];
        let ids = resolve_table_refs(&choices, &[TableRefInput::Name("users".into())]).unwrap();
        assert_eq!(
            ids,
            vec![TableId {
                schema: "public".into(),
                name: "users".into()
            }]
        );
    }

    #[test]
    fn bare_name_across_schemas_reports_ambiguity() {
        let choices = vec![choice("public", "users"), choice("sales", "users")];
        let error =
            resolve_table_refs(&choices, &[TableRefInput::Name("users".into())]).unwrap_err();
        assert!(error.contains("public"), "{error}");
        assert!(error.contains("sales"), "{error}");
        assert!(error.contains("schema"), "{error}");
    }

    #[test]
    fn schema_identity_wins_over_bare_name() {
        let choices = vec![choice("public", "users"), choice("sales", "users")];
        let ids = resolve_table_refs(
            &choices,
            &[TableRefInput::Id(TableIdInput {
                schema: "sales".into(),
                name: "users".into(),
            })],
        )
        .unwrap();
        assert_eq!(ids[0].schema, "sales");
    }

    #[test]
    fn empty_selection_is_rejected() {
        let choices = vec![choice("public", "users")];
        assert!(resolve_table_refs(&choices, &[]).is_err());
    }

    #[test]
    fn project_scope_is_bound_and_selected_saved() {
        let selected = vec![TableId {
            schema: "public".into(),
            name: "users".into(),
        }];
        let project = prepare_project(ErProject::default(), &scope("app"), &selected).unwrap();
        assert_eq!(project.scope.as_ref().unwrap().database, "app");
        assert_eq!(project.selected.as_deref(), Some(selected.as_slice()));

        let mismatch = prepare_project(
            ErProject {
                scope: Some(scope("other")),
                ..ErProject::default()
            },
            &scope("app"),
            &selected,
        )
        .unwrap_err();
        assert!(mismatch.contains("scope"), "{mismatch}");
        assert!(mismatch.contains("app"), "{mismatch}");
    }

    #[test]
    fn project_signature_tracks_decisions() {
        let base = ErProject::default();
        let changed = ErProject {
            selected: Some(vec![TableId {
                schema: "public".into(),
                name: "users".into(),
            }]),
            ..ErProject::default()
        };
        assert_ne!(project_signature(&base), project_signature(&changed));
    }

    #[test]
    fn table_choices_keep_schema_columns_and_keys() {
        let public = TableInfo {
            schema: "public".into(),
            name: "users".into(),
            columns: vec![dbs_core::ColumnInfo {
                order: 1,
                name: "id".into(),
                data_type: "bigint".into(),
                length: String::new(),
                is_primary_key: true,
                is_auto_increment: false,
                nullable: false,
                default_value: String::new(),
                comment: String::new(),
            }],
            ..Default::default()
        };
        let sales = TableInfo {
            schema: "sales".into(),
            name: "users".into(),
            columns: vec![dbs_core::ColumnInfo {
                order: 1,
                name: "id".into(),
                data_type: "bigint".into(),
                length: String::new(),
                is_primary_key: true,
                is_auto_increment: false,
                nullable: false,
                default_value: String::new(),
                comment: String::new(),
            }],
            ..Default::default()
        };
        let catalog = ErCatalog {
            tables: vec![sales, public],
            keys: vec![Endpoint {
                table: TableId {
                    schema: "public".into(),
                    name: "users".into(),
                },
                columns: vec!["id".into()],
            }],
            ..Default::default()
        };
        let choices = table_choices(&catalog);
        assert_eq!(choices[0].schema, "public");
        assert_eq!(choices[0].keys, vec![vec!["id".to_string()]]);
        assert_eq!(choices[1].schema, "sales");
        assert!(choices[1].keys.is_empty());
    }

    #[test]
    fn sqlite_scope_labels_strip_windows_prefix() {
        let mut sqlite = scope(r"\\?\C:\data\app.db");
        sqlite.db_type = DbType::Sqlite;
        assert_eq!(scope_text(&sqlite), r"sqlite://C:\data\app.db");
    }

    fn column(name: &str) -> dbs_core::ColumnInfo {
        dbs_core::ColumnInfo {
            order: 1,
            name: name.into(),
            data_type: "integer".into(),
            length: String::new(),
            is_primary_key: false,
            is_auto_increment: false,
            nullable: false,
            default_value: String::new(),
            comment: String::new(),
        }
    }

    fn session_fixture() -> ErSession {
        let left = TableInfo {
            schema: "main".into(),
            name: "left_side".into(),
            columns: vec![column("link")],
            ..Default::default()
        };
        let right = TableInfo {
            schema: "main".into(),
            name: "right_side".into(),
            columns: vec![column("value")],
            ..Default::default()
        };
        let source = Endpoint {
            table: TableId::from(&left),
            columns: vec!["link".into()],
        };
        let target = Endpoint {
            table: TableId::from(&right),
            columns: vec!["value".into()],
        };
        let catalog = ErCatalog {
            tables: vec![left, right],
            keys: vec![target.clone()],
            types: vec![
                dbs_core::er::ErColumnType {
                    table: source.table.clone(),
                    column: "link".into(),
                    signature: "integer".into(),
                },
                dbs_core::er::ErColumnType {
                    table: target.table.clone(),
                    column: "value".into(),
                    signature: "integer".into(),
                },
            ],
            ..Default::default()
        };
        let choices = table_choices(&catalog);
        let selected = vec![source.table.clone(), target.table.clone()];
        let scope = scope("app");
        let project = prepare_project(ErProject::default(), &scope, &selected).unwrap();
        let signature = project_signature(&project);
        ErSession {
            id: 1,
            revision: 7,
            scope,
            snapshot: ErSnapshot::new(catalog),
            choices,
            selected,
            collapsed: BTreeSet::new(),
            show_all_columns: false,
            title: "测试 ER".into(),
            project_signature: signature,
        }
    }

    #[test]
    fn export_requires_the_same_preview_revision_selection_and_project() {
        let session = session_fixture();
        let signature = session.project_signature.clone();
        let selected = session.selected.clone();
        let collapsed = BTreeSet::new();
        assert!(
            validate_export_request(&session, 7, &selected, &signature, false, &collapsed).is_ok()
        );

        let stale = validate_export_request(&session, 6, &selected, &signature, false, &collapsed)
            .unwrap_err();
        assert!(stale.contains("预览已更新"), "{stale}");

        let subset = &selected[..1];
        let changed = validate_export_request(&session, 7, subset, &signature, false, &collapsed)
            .unwrap_err();
        assert!(changed.contains("选表范围"), "{changed}");

        let changed =
            validate_export_request(&session, 7, &selected, "v1-other", false, &collapsed)
                .unwrap_err();
        assert!(changed.contains("项目修正"), "{changed}");

        let changed = validate_export_request(&session, 7, &selected, &signature, true, &collapsed)
            .unwrap_err();
        assert!(changed.contains("字段显示范围"), "{changed}");

        let folded: BTreeSet<TableId> = selected[..1].iter().cloned().collect();
        let changed = validate_export_request(&session, 7, &selected, &signature, false, &folded)
            .unwrap_err();
        assert!(changed.contains("字段展开状态"), "{changed}");
    }

    #[test]
    fn collapsed_refs_keep_full_schema_identity() {
        let choices = vec![choice("public", "users"), choice("sales", "users")];
        let folded = resolve_collapsed(
            &choices,
            &[TableRefInput::Id(TableIdInput {
                schema: "sales".into(),
                name: "users".into(),
            })],
        )
        .unwrap();
        assert_eq!(folded.len(), 1);
        assert!(folded.contains(&TableId {
            schema: "sales".into(),
            name: "users".into()
        }));
        assert_eq!(resolve_collapsed(&choices, &[]).unwrap().len(), 0);
    }
}
