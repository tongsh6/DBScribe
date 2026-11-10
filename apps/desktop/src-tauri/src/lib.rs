mod commands;
mod models;
mod oracle_client;
mod secrets;
mod store;

use models::WindowState;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let Some(window) = app.get_webview_window("main") else {
                return Ok(());
            };

            // 恢复上次的窗口状态；首次启动按屏幕可用空间给出适中尺寸（默认不全屏）
            let state = store::load(&handle);
            match state.preferences.window.clone() {
                Some(ws) => {
                    let _ = window.set_size(tauri::LogicalSize::new(ws.width, ws.height));
                    let _ = window.set_position(tauri::LogicalPosition::new(ws.x, ws.y));
                    if ws.maximized {
                        let _ = window.maximize();
                    }
                }
                None => {
                    if let Ok(Some(monitor)) = window.current_monitor() {
                        let scale = monitor.scale_factor();
                        let logical_w = monitor.size().width as f64 / scale;
                        let logical_h = monitor.size().height as f64 / scale;
                        let width = (logical_w * 0.86).clamp(900.0, 1320.0);
                        let height = (logical_h * 0.86).clamp(600.0, 840.0);
                        let _ = window.set_size(tauri::LogicalSize::new(width, height));
                    }
                    let _ = window.center();
                }
            }

            // 尺寸/位置变化后记下来（800ms 内无新事件才落盘，避免拖动时频繁写文件）
            let last_event = Arc::new(Mutex::new(Instant::now()));
            let app_for_save = handle.clone();
            let window_for_save = window.clone();
            let last_event_clone = Arc::clone(&last_event);

            window.on_window_event(move |event| {
                if !matches!(
                    event,
                    tauri::WindowEvent::Resized(_)
                        | tauri::WindowEvent::Moved(_)
                        | tauri::WindowEvent::CloseRequested { .. }
                ) {
                    return;
                }
                *last_event_clone.lock().unwrap() = Instant::now();

                let app = app_for_save.clone();
                let win = window_for_save.clone();
                let last = Arc::clone(&last_event_clone);

                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    // 期间又有新事件说明还在拖动，交给最后一次去保存
                    if last.lock().unwrap().elapsed() < Duration::from_millis(700) {
                        return;
                    }
                    if let (Ok(size), Ok(scale)) = (win.outer_size(), win.scale_factor()) {
                        let pos = win.outer_position().ok();
                        let maximized = win.is_maximized().unwrap_or(false);
                        let mut state = store::load(&app);
                        state.preferences.window = Some(WindowState {
                            width: size.width as f64 / scale,
                            height: size.height as f64 / scale,
                            x: pos.map(|p| p.x as f64 / scale).unwrap_or(0.0),
                            y: pos.map(|p| p.y as f64 / scale).unwrap_or(0.0),
                            maximized,
                        });
                        let _ = store::save(&app, &state);
                    }
                });
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::test_connection,
            commands::list_databases_cmd,
            commands::list_tables_cmd,
            commands::get_table_columns_cmd,
            commands::load_app_state,
            commands::save_connection,
            commands::delete_connection,
            commands::set_active_connection,
            commands::get_connection_password,
            commands::save_preferences,
            commands::clear_history,
            commands::export_document,
            oracle_client::ensure_oracle_client,
            oracle_client::oracle_client_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
