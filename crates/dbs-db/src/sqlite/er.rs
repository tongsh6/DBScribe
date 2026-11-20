use super::load_tables_from;
use crate::ConnectionOptions;
use anyhow::{Context, Result};
use dbs_core::er::{Endpoint, ErCatalog, ErColumnType, TableId};
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;

mod views;

pub async fn load(opts: &ConnectionOptions) -> Result<ErCatalog> {
    let path = opts.database.clone();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("只读打开 SQLite 文件失败：{path}"))?;
        let tx = conn.unchecked_transaction()?;
        let catalog = read(&tx)?;
        tx.commit()?;
        Ok(catalog)
    })
    .await
    .context("读取 SQLite ER 元数据的任务异常退出")?
}

fn read(conn: &Connection) -> Result<ErCatalog> {
    let mut catalog = ErCatalog {
        tables: load_tables_from(conn)?,
        ..Default::default()
    };
    let mut primary_keys = BTreeMap::new();
    for table in &catalog.tables {
        let id = TableId::from(table);
        let primary: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk")?
            .query_map([&table.name], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        primary_keys.insert(
            table.name.to_ascii_lowercase(),
            (table.name.clone(), primary.clone()),
        );
        let mut collations = BTreeMap::new();
        for column in &table.columns {
            let (declared, collation, ..) =
                conn.column_metadata(Some("main"), table.name.as_str(), column.name.as_str())?;
            let ty = declared
                .and_then(|s| s.to_str().ok())
                .unwrap_or("")
                .trim()
                .to_ascii_uppercase();
            let coll = collation
                .and_then(|s| s.to_str().ok())
                .unwrap_or("")
                .to_ascii_uppercase();
            if !ty.is_empty() && !coll.is_empty() {
                collations.insert(column.name.clone(), coll.clone());
                // 声明精度、长度和比较规则都纳入签名。
                let ty = match ty.as_str() {
                    "INT" => "INTEGER",
                    _ => &ty,
                };
                let signature = format!("{ty} COLLATE {coll}");
                catalog.types.push(ErColumnType {
                    table: id.clone(),
                    column: column.name.clone(),
                    signature,
                });
            }
        }
        let indexes = conn
            .prepare("SELECT name,origin,partial FROM pragma_index_list(?1) WHERE [unique]=1")?
            .query_map([&table.name], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !primary.is_empty() && !indexes.iter().any(|(_, origin, _)| origin == "pk") {
            // INTEGER PRIMARY KEY 使用 rowid，没有独立主键索引。
            catalog.keys.push(Endpoint {
                table: id.clone(),
                columns: primary,
            });
        }
        for (name, _, partial) in indexes {
            if partial {
                continue;
            }
            let parts = conn
                .prepare(
                    "SELECT name,coll FROM pragma_index_xinfo(?1) WHERE [key]=1 ORDER BY seqno",
                )?
                .query_map([name], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let columns: Option<Vec<_>> = parts
                .into_iter()
                .map(|(name, coll)| {
                    let name = name?;
                    let default = collations.get(&name)?;
                    coll?.eq_ignore_ascii_case(default).then_some(name)
                })
                .collect();
            if let Some(columns) = columns.filter(|c| !c.is_empty()) {
                catalog.keys.push(Endpoint {
                    table: id.clone(),
                    columns,
                });
            }
        }
    }
    for table in &mut catalog.tables {
        for fk in &mut table.foreign_keys {
            if let Some((actual, pk)) = primary_keys.get(&fk.ref_table.to_ascii_lowercase()) {
                fk.ref_table = actual.clone();
                if fk.ref_columns.iter().all(String::is_empty) && fk.columns.len() == pk.len() {
                    fk.ref_columns.clone_from(pk);
                }
            }
        }
    }
    catalog.keys.sort();
    catalog.keys.dedup();
    let mut stmt =
        conn.prepare("SELECT name,sql FROM main.sqlite_schema WHERE type='view' ORDER BY name")?;
    let definitions =
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in definitions {
        let (name, ddl) = row?;
        if let Some(view) = views::comparison(&catalog, &name, &ddl) {
            catalog.views.push(view);
        }
    }
    Ok(catalog)
}
