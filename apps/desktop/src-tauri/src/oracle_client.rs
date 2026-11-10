//! Oracle Instant Client 的懒安装命令。
//!
//! 只有用户选择 Oracle 并首次连接时才会触发下载；下载完成后下次直接复用。

use tauri::{AppHandle, Emitter};

/// 确保 Oracle 客户端可用，返回客户端目录。
///
/// 已安装时立即返回；否则下载并解压（通过 `oracle-client-progress` 事件上报进度）。
#[tauri::command]
pub async fn ensure_oracle_client(app: AppHandle) -> Result<String, String> {
    if let Some(dir) = dbs_db::installed_client_dir() {
        return Ok(dir.to_string_lossy().to_string());
    }

    if dbs_db::client_download_url().is_none() {
        return Err(
            "当前平台暂不支持自动下载 Oracle 客户端，请手动安装 Instant Client 后放到应用数据目录"
                .to_string(),
        );
    }

    let app_for_event = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut last_percent = u64::MAX;
        let mut progress = |done: u64, total: u64| {
            let percent = done.saturating_mul(100).checked_div(total).unwrap_or(0);
            // 每 1% 上报一次，避免事件风暴
            if percent != last_percent {
                last_percent = percent;
                let _ = app_for_event.emit(
                    "oracle-client-progress",
                    serde_json::json!({ "done": done, "total": total, "percent": percent }),
                );
            }
        };
        dbs_db::ensure_client(&mut progress)
    })
    .await
    .map_err(|e| format!("下载任务异常退出：{e}"))?
    .map_err(|e| e.to_string())?;

    Ok(result.to_string_lossy().to_string())
}

/// 查询 Oracle 客户端是否已安装（不触发下载）。
#[tauri::command]
pub async fn oracle_client_status() -> Result<bool, String> {
    Ok(dbs_db::installed_client_dir().is_some())
}
