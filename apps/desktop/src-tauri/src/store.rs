//! 本地持久化：连接列表与导出历史。
//!
//! 存放于系统配置目录下的 `store.json`；**不保存密码**。
//! 读取失败一律回退到空状态，避免因配置损坏导致应用起不来。

use crate::models::AppState;
use anyhow::{Context, Result};
use std::path::PathBuf;
use tauri::Manager;

const STORE_FILE: &str = "store.json";
/// 历史记录最多保留条数。
const MAX_HISTORY: usize = 30;

fn store_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .context("无法定位应用配置目录")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("创建配置目录失败：{}", dir.display()))?;
    Ok(dir.join(STORE_FILE))
}

/// 读取状态；任何异常都回退到默认值。
pub fn load(app: &tauri::AppHandle) -> AppState {
    let result = (|| -> Result<AppState> {
        let path = store_path(app)?;
        if !path.exists() {
            return Ok(AppState::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取配置失败：{}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("解析配置失败：{}", path.display()))
    })();

    match result {
        Ok(state) => state,
        Err(err) => {
            eprintln!("[DBScribe] 读取本地配置失败，使用默认值：{err:#}");
            AppState::default()
        }
    }
}

/// 写入状态；历史记录超出上限时保留最近的。
pub fn save(app: &tauri::AppHandle, state: &AppState) -> Result<()> {
    let path = store_path(app)?;
    let mut trimmed = state.clone();
    if trimmed.history.len() > MAX_HISTORY {
        trimmed.history.truncate(MAX_HISTORY);
    }
    let text = serde_json::to_string_pretty(&trimmed).context("序列化配置失败")?;
    std::fs::write(&path, text).with_context(|| format!("写入配置失败：{}", path.display()))?;
    Ok(())
}
