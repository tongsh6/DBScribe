mod commands;
mod models;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::test_connection,
            commands::list_databases_cmd,
            commands::list_tables_cmd,
            commands::export_document,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
