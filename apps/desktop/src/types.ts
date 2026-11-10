/** 前后端共享的数据结构（与 src-tauri/src/models.rs 对应）。 */

export interface ConnectionParams {
  /** mysql / postgres */
  db_type: string;
  host: string;
  port: number;
  user: string;
  password: string;
}

/** 已保存的数据库连接（密码存于系统凭据管理器，不在此结构中） */
export interface SavedConnection {
  id: string;
  name: string;
  db_type: string;
  host: string;
  port: number;
  user: string;
  /** 是否已在系统凭据存储中保存了密码 */
  has_password: boolean;
}

export interface TableSummary {
  name: string;
  comment: string;
  column_count: number;
}

/** 列详情（表结构预览） */
export interface ColumnView {
  order: number;
  name: string;
  data_type: string;
  length: string;
  is_primary_key: boolean;
  is_auto_increment: boolean;
  nullable: boolean;
  default_value: string;
  comment: string;
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

/** 界面偏好：记住上次使用的导出设置 */
export interface Preferences {
  format: string | null;
  sheet_mode: string | null;
  out_dir: string | null;
}

export interface AppState {
  connections: SavedConnection[];
  active_connection: string | null;
  history: HistoryEntry[];
  preferences: Preferences;
}

export interface ExportPayload {
  database: string;
  title: string;
  format: string;
  sheet_mode: string;
  out_path: string;
  tables: string[] | null;
}
