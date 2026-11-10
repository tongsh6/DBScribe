/** 前后端共享的数据结构（与 src-tauri/src/models.rs 对应）。 */

export interface ConnectionParams {
  host: string;
  port: number;
  user: string;
  password: string;
}

/** 已保存的数据库连接（不含密码） */
export interface SavedConnection {
  id: string;
  name: string;
  host: string;
  port: number;
  user: string;
}

export interface TableSummary {
  name: string;
  comment: string;
  column_count: number;
}

export interface RenamedSheet {
  original: string;
  actual: string;
}

export interface ExportResult {
  out_path: string;
  table_count: number;
  sheet_count: number;
  renamed: RenamedSheet[];
  elapsed_ms: number;
}

export interface HistoryEntry {
  time: string;
  database: string;
  format: string;
  out_path: string;
  table_count: number;
  elapsed_ms: number;
}

export interface AppState {
  connections: SavedConnection[];
  active_connection: string | null;
  history: HistoryEntry[];
}

export interface ExportPayload {
  database: string;
  title: string;
  format: string;
  sheet_mode: string;
  out_path: string;
  tables: string[] | null;
}
