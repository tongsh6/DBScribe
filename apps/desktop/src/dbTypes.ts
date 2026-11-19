/** 数据库类型的能力描述：列表、字段需求与提示只在这里定义。 */
export interface DbTypeSpec {
  value: string;
  label: string;
  /** 默认端口；0 表示没有端口概念（SQLite）。 */
  port: number;
  /** 是否需要主机 / 端口 / 用户（本地文件库为 false）。 */
  network: boolean;
  /** 「库」字段的标签，未提供时用「数据库」。 */
  databaseLabel?: string;
  /** 是否需要本地文件选择（SQLite）。 */
  databaseFile?: boolean;
  /** host 输入框的提示。 */
  hostPlaceholder?: string;
}

export const DB_TYPES: DbTypeSpec[] = [
  { value: "mysql", label: "MySQL / MariaDB", port: 3306, network: true },
  { value: "postgres", label: "PostgreSQL", port: 5432, network: true },
  {
    value: "sqlite",
    label: "SQLite（本地文件）",
    port: 0,
    network: false,
    databaseLabel: "数据库文件",
    databaseFile: true,
  },
  { value: "sqlserver", label: "SQL Server", port: 1433, network: true },
  {
    value: "oracle",
    label: "Oracle",
    port: 1521,
    network: true,
    databaseLabel: "服务名（service name）",
  },
  { value: "oceanbase", label: "OceanBase（MySQL 兼容）", port: 2881, network: true },
  { value: "dameng", label: "达梦 DM8（需 ODBC 驱动）", port: 5236, network: true },
  {
    value: "mongodb",
    label: "MongoDB",
    port: 27017,
    network: true,
    hostPlaceholder: "主机，或完整连接串 mongodb://…",
  },
];

const DEFAULT_SPEC: DbTypeSpec = { value: "mysql", label: "MySQL", port: 3306, network: true };

/** 按值取类型描述；未知值回退到 MySQL（与后端默认一致）。 */
export function dbTypeSpec(value: string): DbTypeSpec {
  return DB_TYPES.find((spec) => spec.value === value) ?? DEFAULT_SPEC;
}

/** host 是否是完整连接串（MongoDB 支持）。 */
export function isConnectionString(host: string): boolean {
  return /^mongodb(\+srv)?:\/\//i.test(host.trim());
}
