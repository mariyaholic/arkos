//! Top files and directories query against a completed snapshot.

use crate::db::map_db_err;
use crate::exit::{Error, Result};
use crate::fmt::join_display;
use clap::ValueEnum;
use rusqlite::{params, Connection};
use serde::Serialize;

/// Sort dimension for `arkos top`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TopBy {
    Size,
    Mtime,
    Count,
}

/// Top entry row for JSON/table output.
#[derive(Debug, Serialize)]
pub struct TopItem {
    pub path: String,
    pub bytes: i64,
    pub files: Option<i64>,
    pub mtime: Option<i64>,
    pub ext: Option<String>,
}

/// Return top directories or top files inside `snapshot_id`.
pub fn query_top(
    conn: &Connection,
    snapshot_id: i64,
    dirs: bool,
    by: TopBy,
    limit: usize,
) -> Result<Vec<TopItem>> {
    crate::db::read_tx(conn, || query_top_inner(conn, snapshot_id, dirs, by, limit))
}

fn query_top_inner(
    conn: &Connection,
    snapshot_id: i64,
    dirs: bool,
    by: TopBy,
    limit: usize,
) -> Result<Vec<TopItem>> {
    let state: String = conn
        .query_row(
            "SELECT state FROM arkos_snapshots WHERE id = ?1",
            [snapshot_id],
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Input(format!("snapshot {snapshot_id} does not exist"))
            }
            other => map_db_err(other),
        })?;
    if state != "complete" {
        return Err(Error::Input(format!(
            "snapshot {snapshot_id} is '{state}', not 'complete'"
        )));
    }

    if dirs {
        let order = match by {
            TopBy::Size => "bytes DESC, path ASC",
            TopBy::Count => "files DESC, bytes DESC",
            TopBy::Mtime => "mtime ASC, bytes DESC",
        };
        let sql = format!(
            "SELECT path, bytes, files, mtime FROM arkos_dirs WHERE snapshot_id = ?1 ORDER BY {order} LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db_err)?;
        let rows = stmt
            .query_map(params![snapshot_id, limit as i64], |r| {
                Ok(TopItem {
                    path: r.get(0)?,
                    bytes: r.get(1)?,
                    files: Some(r.get(2)?),
                    mtime: r.get(3)?,
                    ext: None,
                })
            })
            .map_err(map_db_err)?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item.map_err(map_db_err)?);
        }
        Ok(out)
    } else {
        let order = match by {
            TopBy::Size | TopBy::Count => "f.size DESC, d.path ASC, f.name ASC",
            TopBy::Mtime => "f.mtime ASC, f.size DESC",
        };
        let sql = format!(
            "SELECT d.path, f.name, f.size, f.mtime, f.ext
             FROM arkos_files f
             JOIN arkos_dirs d ON d.id = f.dir_id
             WHERE f.snapshot_id = ?1
             ORDER BY {order}
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql).map_err(map_db_err)?;
        let rows = stmt
            .query_map(params![snapshot_id, limit as i64], |r| {
                let dir_path: String = r.get(0)?;
                let name: String = r.get(1)?;
                Ok(TopItem {
                    path: join_display(&dir_path, &name),
                    bytes: r.get(2)?,
                    files: None,
                    mtime: Some(r.get(3)?),
                    ext: r.get(4)?,
                })
            })
            .map_err(map_db_err)?;
        let mut out = Vec::new();
        for item in rows {
            out.push(item.map_err(map_db_err)?);
        }
        Ok(out)
    }
}
