/** 支持的数据库类型：主界面与连接管理共用，避免两处列表漂移。 */
export const DB_TYPES = [
  { value: "mysql", label: "MySQL / MariaDB", port: 3306 },
  { value: "postgres", label: "PostgreSQL", port: 5432 },
  { value: "sqlite", label: "SQLite（本地文件）", port: 0 },
  { value: "sqlserver", label: "SQL Server", port: 1433 },
  { value: "oracle", label: "Oracle", port: 1521 },
  { value: "oceanbase", label: "OceanBase（MySQL 兼容）", port: 2881 },
  { value: "dameng", label: "达梦 DM8（需 ODBC 驱动）", port: 5236 },
  { value: "mongodb", label: "MongoDB（host 可填连接串）", port: 27017 },
];
