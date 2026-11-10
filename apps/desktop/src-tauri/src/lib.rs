mod commands;
mod models;
mod secrets;
mod store;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
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
            commands::clear_history,
            commands::export_document,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
