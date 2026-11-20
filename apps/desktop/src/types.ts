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
  /** 库名 / SQLite 文件路径 / Oracle 服务名；旧配置为空 */
  database: string;
  /** 是否已在系统凭据存储中保存了密码 */
  has_password: boolean;
}

export interface TableSummary {
  /** schema / owner；MySQL 为库名，SQLite 为 main，其他方言为 owner。 */
  schema: string;
  name: string;
  comment: string;
  column_count: number;
}

/** 表的真实身份：同名表用 schema 区分。 */
export interface TableIdentity {
  schema: string;
  name: string;
}

export interface Endpoint {
  table: TableIdentity;
  columns: string[];
}

export type RelationKind = "declared" | "inferred" | "confirmed" | "lookup";

export interface KeyCondition {
  non_null_equality?: boolean;
  evidence_kind?: "naming" | "view" | "view_mapping" | "migration" | "business" | "direct";
  required_target: { column: string; value: string | boolean | number }[];
  unbound_target_columns: string[];
}

export interface Relation {
  source: Endpoint;
  target: Endpoint;
  kind: RelationKind;
  reason: string;
  condition?: KeyCondition;
}

export interface Candidate {
  source: Endpoint;
  target: Endpoint;
  reason: string;
  condition?: KeyCondition;
}

export type DecisionAction = "confirm" | "exclude";

export interface Decision {
  source: Endpoint;
  target: Endpoint;
  action: DecisionAction;
  basis: string;
}

export interface ProjectScope {
  db_type: string;
  host: string;
  port: number;
  database: string;
}

/** 与 CLI 共用的 ER 项目修正文件。 */
export interface ErProject {
  scope: ProjectScope | null;
  selected: TableIdentity[] | null;
  decisions: Decision[];
}

export interface ErTableChoice extends TableIdentity {
  comment: string;
  columns: string[];
  /** 完整唯一键，手工补充关系时作为目标字段建议。 */
  keys: string[][];
}

export interface ErDiagram {
  tables: unknown[];
  relations: Relation[];
  candidates: Candidate[];
  external_count: number;
  external_candidate_count: number;
  warnings: string[];
}

export interface ErPreview {
  session_id: number;
  revision: number;
  scope: ProjectScope;
  scope_label: string;
  tables: ErTableChoice[];
  selected: TableIdentity[];
  /** 字段已收起的表；与导出保持同一状态。 */
  collapsed: TableIdentity[];
  diagram: ErDiagram;
  project: ErProject;
  svg: string;
  title: string;
  show_all_columns: boolean;
}

export interface ErExportResult {
  out_path: string;
  table_count: number;
  relation_count: number;
  candidate_count: number;
  external_count: number;
  external_candidate_count: number;
  warnings: string[];
  elapsed_ms: number;
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
  tables: TableIdentity[] | null;
}
