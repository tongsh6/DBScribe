//! Oracle 元数据读取。
//!
//! 通过数据字典视图（`all_tables` / `all_tab_columns` / `all_tab_comments` /
//! `all_col_comments`）读取表与列。Oracle 的「数据库」概念对应连接串里的服务名，
//! 表归属的 schema 取当前登录用户。

use crate::options::ConnectionOptions;
use crate::oracle_client as client;
use crate::remote_er::{self, ErEvidence};
use anyhow::{Context, Result};
use dbs_core::er::ErCatalog;
use dbs_core::{ColumnInfo, ForeignKey, IndexInfo, TableInfo};
use oracle::Connection;

const TABLE_SQL: &str = r#"
SELECT t.table_name,
       NVL(c.comments, ' ')
FROM all_tables t
LEFT JOIN all_tab_comments c
       ON c.owner = t.owner AND c.table_name = t.table_name
WHERE t.owner = :1
ORDER BY t.table_name
"#;

const COLUMN_SQL: &str = r#"
SELECT c.table_name,
       c.column_id,
       c.column_name,
       c.data_type,
       c.data_length,
       c.data_precision,
       c.data_scale,
       c.nullable,
       NVL(c.identity_column, 'NO'),
       c.data_default,
       NVL(cc.comments, ' '),
       CASE WHEN pk.column_name IS NULL THEN 0 ELSE 1 END
FROM all_tab_cols c
LEFT JOIN all_col_comments cc
       ON cc.owner = c.owner
      AND cc.table_name = c.table_name
      AND cc.column_name = c.column_name
LEFT JOIN (
    SELECT cc2.owner, cc2.table_name, cc2.column_name
    FROM all_constraints cst
    JOIN all_cons_columns cc2
      ON cc2.owner = cst.owner AND cc2.constraint_name = cst.constraint_name
    WHERE cst.constraint_type = 'P'
) pk ON pk.owner = c.owner
    AND pk.table_name = c.table_name
    AND pk.column_name = c.column_name
WHERE c.owner = :1
  AND c.hidden_column = 'NO'
ORDER BY c.table_name, c.column_id
"#;

/// 索引：all_indexes + all_ind_columns。
const INDEX_SQL: &str = r#"
SELECT i.table_name,
       i.index_name,
       i.uniqueness,
       ic.column_name
FROM all_indexes i
JOIN all_ind_columns ic
  ON ic.index_owner = i.owner AND ic.index_name = i.index_name
WHERE i.table_owner = :1
ORDER BY i.table_name, i.index_name, ic.column_position
"#;

/// 主键索引名（用于标记 is_primary）。
const PK_INDEX_SQL: &str = r#"
SELECT cc.table_name, c.index_name
FROM all_constraints c
JOIN all_cons_columns cc
  ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name
WHERE c.constraint_type = 'P' AND c.owner = :1
"#;

/// 外键：all_constraints(type='R') 关联被引用约束，并保留目标表真实 owner。
const FK_SQL: &str = r#"
SELECT c.table_name,
       c.constraint_name,
       cc.column_name,
       r.owner AS ref_owner,
       r.table_name AS ref_table,
       rcc.column_name AS ref_column
FROM all_constraints c
JOIN all_cons_columns cc
  ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name
JOIN all_constraints r
  ON r.owner = c.r_owner AND r.constraint_name = c.r_constraint_name
JOIN all_cons_columns rcc
  ON rcc.owner = r.owner AND rcc.constraint_name = r.constraint_name
 AND rcc.position = cc.position
WHERE c.constraint_type = 'R' AND c.owner = :1
ORDER BY c.table_name, c.constraint_name, cc.position
"#;

/// 分区表。
const PARTITION_SQL: &str = r#"
SELECT table_name FROM all_tables WHERE owner = :1 AND partitioned = 'YES'
"#;

fn connect(opts: &ConnectionOptions) -> Result<Connection> {
    client::ensure_initialized();
    // 连接串里的 service name 取自 database 字段
    let conn_string = format!("//{}:{}/{}", opts.host, opts.port, opts.database);
    Connection::connect(&opts.user, &opts.password, &conn_string).with_context(|| {
        format!(
            "连接 Oracle 失败（{}，客户端 {} 缺失时请确认已随程序安装）",
            conn_string,
            client::client_lib_name()
        )
    })
}

/// 与 `load_tables` 保持一致：当前数据字典 owner 取登录用户的大写形式。
fn owner_of(opts: &ConnectionOptions) -> String {
    opts.user.to_uppercase()
}

/// Oracle 用服务名标识数据库，这里返回连接串中的服务名。
pub async fn list_databases(opts: &ConnectionOptions) -> Result<Vec<String>> {
    if opts.database.trim().is_empty() {
        // 没有服务名就没法建立连接；返回空列表，由调用方提示用户填写
        return Ok(Vec::new());
    }
    Ok(vec![opts.database.clone()])
}

/// 读取当前用户 schema 下的全部表及其列信息。
pub async fn load_tables(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    let opts = opts.clone();
    tokio::task::spawn_blocking(move || load_tables_blocking(&opts))
        .await
        .context("Oracle 读取任务异常退出")?
}

/// 读取单张表的列信息（界面预览用）。
pub async fn load_table_columns(opts: &ConnectionOptions, table: &str) -> Result<Vec<ColumnInfo>> {
    let tables = load_tables(opts).await?;
    tables
        .into_iter()
        .find(|t| t.name.eq_ignore_ascii_case(table))
        .map(|t| t.columns)
        .with_context(|| format!("表 {table} 不存在"))
}

fn load_tables_blocking(opts: &ConnectionOptions) -> Result<Vec<TableInfo>> {
    let conn = connect(opts)?;
    // schema = 当前登录用户（Oracle 默认大写）
    let owner = owner_of(opts);

    let table_rows = conn.query(TABLE_SQL, &[&owner]).context("查询表清单失败")?;

    let column_rows = conn
        .query(COLUMN_SQL, &[&owner])
        .context("查询列信息失败")?;

    let mut columns_by_table: std::collections::HashMap<String, Vec<ColumnInfo>> =
        std::collections::HashMap::new();

    for row in column_rows {
        let row = row.context("读取列信息失败")?;
        let table_name: String = row.get(0)?;
        let data_type: String = row.get(3)?;
        let data_length: Option<i64> = row.get(4)?;
        let precision: Option<i64> = row.get(5)?;
        let scale: Option<i64> = row.get(6)?;
        let nullable: String = row.get(7)?;
        let identity: String = row.get(8)?;
        let default_value: Option<String> = row.get(9)?;
        let comment: String = row.get(10)?;
        let is_pk: i64 = row.get(11)?;

        columns_by_table
            .entry(table_name)
            .or_default()
            .push(ColumnInfo {
                order: row.get::<_, i64>(1)? as u32,
                name: row.get(2)?,
                length: format_length(&data_type, data_length, precision, scale),
                data_type,
                is_primary_key: is_pk != 0,
                is_auto_increment: identity.eq_ignore_ascii_case("YES"),
                nullable: nullable.eq_ignore_ascii_case("Y"),
                default_value: default_value.unwrap_or_default().trim().to_string(),
                comment: comment.trim().to_string(),
            });
    }

    // 主键索引名集合（用于标记 is_primary）
    let mut pk_indexes: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for row in conn.query(PK_INDEX_SQL, &[&owner])? {
        let row = row.context("读取主键索引失败")?;
        pk_indexes.insert((row.get(0)?, row.get(1)?));
    }

    // 索引
    let mut indexes_by_table: std::collections::HashMap<String, Vec<IndexInfo>> =
        std::collections::HashMap::new();
    for row in conn.query(INDEX_SQL, &[&owner])? {
        let row = row.context("读取索引失败")?;
        let table_name: String = row.get(0)?;
        let index_name: String = row.get(1)?;
        let uniqueness: String = row.get(2)?;
        let column_name: String = row.get(3)?;
        let is_primary = pk_indexes.contains(&(table_name.clone(), index_name.clone()));

        let list = indexes_by_table.entry(table_name).or_default();
        match list.iter_mut().find(|i| i.name == index_name) {
            Some(existing) => existing.columns.push(column_name),
            None => list.push(IndexInfo {
                name: index_name,
                columns: vec![column_name],
                is_unique: uniqueness.eq_ignore_ascii_case("UNIQUE"),
                is_primary,
            }),
        }
    }

    // 外键
    let mut fks_by_table: std::collections::HashMap<String, Vec<ForeignKey>> =
        std::collections::HashMap::new();
    for row in conn.query(FK_SQL, &[&owner])? {
        let row = row.context("读取外键失败")?;
        let table_name: String = row.get(0)?;
        let constraint: String = row.get(1)?;
        let column_name: String = row.get(2)?;
        let ref_schema: String = row.get(3)?;
        let ref_table: String = row.get(4)?;
        let ref_column: String = row.get(5)?;

        let list = fks_by_table.entry(table_name).or_default();
        match list.iter_mut().find(|f| f.name == constraint) {
            Some(existing) => {
                existing.columns.push(column_name);
                existing.ref_columns.push(ref_column);
            }
            None => list.push(ForeignKey {
                name: constraint,
                columns: vec![column_name],
                ref_schema,
                ref_table,
                ref_columns: vec![ref_column],
            }),
        }
    }

    // 分区表
    let mut partitioned: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in conn.query(PARTITION_SQL, &[&owner])? {
        let row = row.context("读取分区信息失败")?;
        partitioned.insert(row.get(0)?);
    }

    let mut tables = Vec::new();
    for (idx, row) in table_rows.enumerate() {
        let row = row.context("读取表信息失败")?;
        let name: String = row.get(0)?;
        let comment: String = row.get(1)?;
        tables.push(TableInfo {
            order: (idx + 1) as u32,
            schema: owner.clone(),
            name: name.clone(),
            comment: comment.trim().to_string(),
            columns: columns_by_table.remove(&name).unwrap_or_default(),
            indexes: indexes_by_table.remove(&name).unwrap_or_default(),
            foreign_keys: fks_by_table.remove(&name).unwrap_or_default(),
            is_partitioned: partitioned.contains(&name),
            note: String::new(),
        });
    }

    tracing::info!(
        "已读取 {} 张表、{} 个列定义",
        tables.len(),
        tables.iter().map(|t| t.columns.len()).sum::<usize>()
    );
    Ok(tables)
}

/// 把 Oracle 的类型元数据拼成展示用的长度。
///
/// `VARCHAR2(64)` 用 data_length；`NUMBER(10,2)` 用 precision + scale。
fn format_length(
    data_type: &str,
    data_length: Option<i64>,
    precision: Option<i64>,
    scale: Option<i64>,
) -> String {
    match data_type.to_uppercase().as_str() {
        // NCHAR / NVARCHAR2 的 data_length 是字节数（每字符 2 字节），换算成字符数
        "NCHAR" | "NVARCHAR2" => data_length.map(|v| (v / 2).to_string()).unwrap_or_default(),
        "VARCHAR2" | "CHAR" | "RAW" => data_length.map(|v| v.to_string()).unwrap_or_default(),
        "NUMBER" => match (precision, scale) {
            (Some(p), Some(s)) => format!("{p},{s}"),
            (Some(p), None) => p.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// ER 类型签名：保留字符语义、字符集、NUMBER 精度和 TIMESTAMP 小数秒。
const ER_TYPE_SQL: &str = r#"
SELECT c.table_name,
       c.column_name,
       c.data_type,
       c.data_length,
       c.data_precision,
       c.data_scale,
       c.char_length,
       c.char_used,
       c.character_set_name
FROM all_tab_cols c
WHERE c.owner = :1
  AND c.hidden_column = 'NO'
ORDER BY c.table_name, c.column_id
"#;

/// ER 键约束：保留约束状态、验证状态和背后索引的状态。
const ER_CONSTRAINT_SQL: &str = r#"
SELECT c.table_name,
       c.constraint_name,
       c.constraint_type,
       cc.column_name,
       cc.position,
       c.status,
       c.validated,
       i.status AS index_status,
       i.index_type,
       tc.nullable
FROM all_constraints c
JOIN all_cons_columns cc
  ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name
LEFT JOIN all_indexes i
  ON i.owner = COALESCE(c.index_owner, c.owner) AND i.index_name = c.index_name
LEFT JOIN all_tab_cols tc
  ON tc.owner = c.owner
 AND tc.table_name = c.table_name
 AND tc.column_name = cc.column_name
 AND tc.hidden_column = 'NO'
WHERE c.owner = :1
  AND c.constraint_type IN ('P', 'U')
ORDER BY c.table_name, c.constraint_name, cc.position
"#;

/// 独立唯一索引：约束背后的索引不算，避免重复；函数索引、不可用和分区索引
/// 保留状态，由装配器拒绝并说明原因。
const ER_INDEX_SQL: &str = r#"
SELECT i.table_name,
       i.index_name,
       i.status,
       i.index_type,
       i.partitioned,
       ic.column_name,
       ic.column_position,
       tc.nullable
FROM all_indexes i
JOIN all_ind_columns ic
  ON ic.index_owner = i.owner AND ic.index_name = i.index_name
LEFT JOIN all_tab_cols tc
  ON tc.owner = i.table_owner
 AND tc.table_name = i.table_name
 AND tc.column_name = ic.column_name
 AND tc.hidden_column = 'NO'
WHERE i.table_owner = :1
  AND i.uniqueness = 'UNIQUE'
  AND NOT EXISTS (
      SELECT 1
      FROM all_constraints c
      WHERE c.owner = i.table_owner
        AND c.constraint_type IN ('P', 'U')
        AND c.table_name = i.table_name
        AND c.index_name = i.index_name
  )
ORDER BY i.table_name, i.index_name, ic.column_position
"#;

/// 外键状态：禁用的外键仍是声明事实，但不证明引用完整性。
const ER_FK_STATUS_SQL: &str = r#"
SELECT table_name, constraint_name, status, validated
FROM all_constraints
WHERE owner = :1 AND constraint_type = 'R'
ORDER BY table_name, constraint_name
"#;

type OracleTypeRow = remote_er::OracleLikeTypeRow;
type OracleConstraintRow = remote_er::OracleLikeConstraintRow;
type OracleIndexRow = remote_er::OracleLikeIndexRow;
type OracleFkRow = remote_er::OracleLikeFkRow;

/// 读取 ER 专用证据：完整类型签名、约束/唯一索引的有效键及外键状态。
pub(crate) async fn load_er_catalog(opts: &ConnectionOptions) -> Result<ErCatalog> {
    let tables = load_tables(opts).await?;
    let opts = opts.clone();
    let evidence_tables = tables.clone();
    let evidence = tokio::task::spawn_blocking(move || read_er_evidence(&opts, &evidence_tables))
        .await
        .context("Oracle 读取 ER 证据任务异常退出")
        .and_then(|result| result);
    Ok(remote_er::assemble_read(tables, evidence))
}

fn read_er_evidence(opts: &ConnectionOptions, tables: &[TableInfo]) -> Result<ErEvidence> {
    let conn = connect(opts)?;
    let owner = owner_of(opts);

    let mut type_rows = Vec::new();
    for row in conn
        .query(ER_TYPE_SQL, &[&owner])
        .context("查询 ER 类型信息失败")?
    {
        let row = row.context("读取 ER 类型信息失败")?;
        type_rows.push(OracleTypeRow {
            table: row.get(0)?,
            column: row.get(1)?,
            data_type: row.get(2)?,
            data_length: row.get(3)?,
            precision: row.get(4)?,
            scale: row.get(5)?,
            char_length: row.get(6)?,
            char_used: row.get(7)?,
            charset: row.get(8)?,
        });
    }

    let mut constraint_rows = Vec::new();
    for row in conn
        .query(ER_CONSTRAINT_SQL, &[&owner])
        .context("查询 ER 约束信息失败")?
    {
        let row = row.context("读取 ER 约束信息失败")?;
        let nullable: Option<String> = row.get(9)?;
        constraint_rows.push(OracleConstraintRow {
            table: row.get(0)?,
            constraint: row.get(1)?,
            kind: row.get(2)?,
            column: row.get(3)?,
            position: row.get(4)?,
            status: row.get(5)?,
            validated: row.get(6)?,
            index_status: row.get(7)?,
            index_type: row.get(8)?,
            nullable: nullable.map(|value| value.eq_ignore_ascii_case("Y")),
        });
    }

    let mut index_rows = Vec::new();
    for row in conn
        .query(ER_INDEX_SQL, &[&owner])
        .context("查询 ER 唯一索引失败")?
    {
        let row = row.context("读取 ER 唯一索引失败")?;
        let nullable: Option<String> = row.get(7)?;
        index_rows.push(OracleIndexRow {
            table: row.get(0)?,
            index: row.get(1)?,
            status: row.get(2)?,
            index_type: row.get(3)?,
            partitioned: row.get(4)?,
            column: row.get(5)?,
            position: row.get(6)?,
            nullable: nullable.map(|value| value.eq_ignore_ascii_case("Y")),
        });
    }

    let mut fk_rows = Vec::new();
    for row in conn
        .query(ER_FK_STATUS_SQL, &[&owner])
        .context("查询 ER 外键状态失败")?
    {
        let row = row.context("读取 ER 外键状态失败")?;
        fk_rows.push(OracleFkRow {
            table: row.get(0)?,
            constraint: row.get(1)?,
            status: row.get(2)?,
            validated: row.get(3)?,
        });
    }

    Ok(evidence_from_rows(
        &owner,
        tables,
        &type_rows,
        &constraint_rows,
        &index_rows,
        &fk_rows,
    ))
}

fn evidence_from_rows(
    owner: &str,
    tables: &[TableInfo],
    type_rows: &[OracleTypeRow],
    constraint_rows: &[OracleConstraintRow],
    index_rows: &[OracleIndexRow],
    fk_rows: &[OracleFkRow],
) -> ErEvidence {
    remote_er::oracle_like_evidence(
        "Oracle",
        owner,
        tables,
        type_rows,
        constraint_rows,
        index_rows,
        fk_rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_length_by_type() {
        assert_eq!(
            format_length("VARCHAR2", Some(64), None, None),
            "64".to_string()
        );
        assert_eq!(
            format_length("NUMBER", None, Some(10), Some(2)),
            "10,2".to_string()
        );
        assert_eq!(
            format_length("NUMBER", None, Some(38), None),
            "38".to_string()
        );
        assert_eq!(format_length("DATE", Some(7), None, None), "".to_string());
        assert_eq!(format_length("CLOB", None, None, None), "".to_string());
    }

    fn column(name: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            order: 1,
            name: name.into(),
            data_type: "x".into(),
            length: String::new(),
            is_primary_key: false,
            is_auto_increment: false,
            nullable,
            default_value: String::new(),
            comment: String::new(),
        }
    }

    fn item_table() -> TableInfo {
        TableInfo {
            order: 1,
            schema: "APP".into(),
            name: "ITEMS".into(),
            columns: vec![
                column("TENANT_ID", false),
                column("ID", false),
                column("CODE", false),
                column("OPTIONAL", true),
            ],
            ..Default::default()
        }
    }

    fn constraint(
        name: &str,
        kind: &str,
        column: &str,
        position: i64,
        status: &str,
        nullable: Option<bool>,
    ) -> OracleConstraintRow {
        OracleConstraintRow {
            table: "ITEMS".into(),
            constraint: name.into(),
            kind: kind.into(),
            column: column.into(),
            position,
            status: status.into(),
            validated: "VALIDATED".into(),
            index_status: Some("VALID".into()),
            index_type: Some("NORMAL".into()),
            nullable,
        }
    }

    fn unique_index(
        name: &str,
        column: &str,
        index_type: &str,
        partitioned: &str,
        nullable: Option<bool>,
    ) -> OracleIndexRow {
        OracleIndexRow {
            table: "ITEMS".into(),
            index: name.into(),
            status: "VALID".into(),
            index_type: index_type.into(),
            partitioned: partitioned.into(),
            column: column.into(),
            position: 1,
            nullable,
        }
    }

    #[test]
    fn constraint_and_index_status_owner_and_order_are_all_checked() {
        let types = vec![
            OracleTypeRow {
                table: "ITEMS".into(),
                column: "CODE".into(),
                data_type: "VARCHAR2".into(),
                data_length: Some(64),
                precision: None,
                scale: None,
                char_length: Some(64),
                char_used: Some("B".into()),
                charset: Some("AL32UTF8".into()),
            },
            OracleTypeRow {
                table: "ITEMS".into(),
                column: "ID".into(),
                data_type: "NUMBER".into(),
                data_length: None,
                precision: Some(10),
                scale: Some(2),
                char_length: None,
                char_used: None,
                charset: None,
            },
        ];
        let constraints = vec![
            // 故意打乱位置，装配器必须按 POSITION 恢复复合键顺序。
            constraint("PK_ITEMS", "P", "ID", 2, "ENABLED", Some(false)),
            constraint("PK_ITEMS", "P", "TENANT_ID", 1, "ENABLED", Some(false)),
            constraint("UQ_DISABLED", "U", "CODE", 1, "DISABLED", Some(false)),
            constraint("UQ_NULLABLE", "U", "OPTIONAL", 1, "ENABLED", Some(true)),
        ];
        let indexes = vec![
            unique_index("UK_CODE", "CODE", "NORMAL", "NO", Some(false)),
            unique_index(
                "UK_FUNC",
                "CODE",
                "FUNCTION-BASED NORMAL",
                "NO",
                Some(false),
            ),
            unique_index("UK_PART", "CODE", "NORMAL", "YES", Some(false)),
        ];
        let fks = vec![
            OracleFkRow {
                table: "ORDERS".into(),
                constraint: "FK_OK".into(),
                status: "ENABLED".into(),
                validated: "VALIDATED".into(),
            },
            OracleFkRow {
                table: "ORDERS".into(),
                constraint: "FK_OLD".into(),
                status: "DISABLED".into(),
                validated: "NOT VALIDATED".into(),
            },
        ];

        let evidence =
            evidence_from_rows("APP", &[item_table()], &types, &constraints, &indexes, &fks);
        assert_eq!(evidence.keys.len(), 2, "{:?}", evidence.keys);
        assert!(
            evidence
                .keys
                .iter()
                .any(|key| key.columns == ["TENANT_ID", "ID"]),
            "{:?}",
            evidence.keys
        );
        assert!(
            evidence.keys.iter().any(|key| key.columns == ["CODE"]),
            "{:?}",
            evidence.keys
        );
        assert_eq!(
            evidence
                .types
                .iter()
                .find(|ty| ty.column == "CODE")
                .unwrap()
                .signature,
            "VARCHAR2(64 BYTE) CHARACTER SET AL32UTF8"
        );
        for expected in [
            "约束状态为 DISABLED",
            "可为 NULL",
            "不是普通列索引",
            "分区唯一索引",
            "FK_OLD",
        ] {
            assert!(
                evidence
                    .scoped_warnings
                    .iter()
                    .any(|warning| warning.message.contains(expected))
                    || evidence
                        .warnings
                        .iter()
                        .any(|warning| warning.contains(expected)),
                "缺少警告 {expected}：{:?} / {:?}",
                evidence.scoped_warnings,
                evidence.warnings
            );
        }
    }
}
