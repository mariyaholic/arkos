//! Snapshot-to-snapshot comparison across directories and files (§4.6).

use crate::db::map_db_err;
use crate::exit::{Error, Result};
use crate::fmt::join_display;
use rusqlite::{params, Connection};
use serde::Serialize;

/// Change classification for a directory or file between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Grown,
    Shrunk,
}

/// Directory delta entry returned by `arkos diff`.
#[derive(Debug, Serialize)]
pub struct DirDiff {
    pub path: String,
    pub kind: ChangeKind,
    pub before_bytes: i64,
    pub after_bytes: i64,
    pub delta_bytes: i64,
    pub delta_files: i64,
}

/// File delta entry returned by `arkos diff`.
#[derive(Debug, Serialize)]
pub struct FileDiff {
    pub path: String,
    pub kind: ChangeKind,
    pub before_size: i64,
    pub after_size: i64,
    pub delta_bytes: i64,
}

/// Full diff report across two snapshots.
#[derive(Debug, Serialize)]
pub struct DiffReport {
    pub snapshot_a: i64,
    pub snapshot_b: i64,
    pub volume: String,
    pub dirs: Vec<DirDiff>,
    pub files: Vec<FileDiff>,
}

/// Validate that both snapshots exist, belong to the same volume, and reached `complete` state.
pub fn validate_diff_pair(conn: &Connection, snap_a: i64, snap_b: i64) -> Result<String> {
    let fetch = |id: i64| -> Result<(String, String)> {
        conn.query_row(
            "SELECT volume, state FROM arkos_snapshots WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Input(format!("snapshot {id} does not exist"))
            }
            other => map_db_err(other),
        })
    };

    let (vol_a, state_a) = fetch(snap_a)?;
    let (vol_b, state_b) = fetch(snap_b)?;
    if state_a != "complete" {
        return Err(Error::Input(format!(
            "snapshot {snap_a} is '{state_a}' and cannot be diffed"
        )));
    }
    if state_b != "complete" {
        return Err(Error::Input(format!(
            "snapshot {snap_b} is '{state_b}' and cannot be diffed"
        )));
    }
    if vol_a != vol_b {
        return Err(Error::Input(format!(
            "snapshots {snap_a} ({vol_a}) and {snap_b} ({vol_b}) belong to different volumes"
        )));
    }
    Ok(vol_a)
}

/// One entry of the shared ranking: a directory or a file change.
enum Change {
    Dir(DirDiff),
    File(FileDiff),
}

impl Change {
    fn delta(&self) -> i64 {
        match self {
            Change::Dir(d) => d.delta_bytes,
            Change::File(f) => f.delta_bytes,
        }
    }

    fn path(&self) -> &str {
        match self {
            Change::Dir(d) => &d.path,
            Change::File(f) => &f.path,
        }
    }
}

/// Compute the top `top` changes between `snap_a` and `snap_b`.
///
/// Directories and files share one ranking by absolute delta (ties by path). `min_size` filters
/// before the cut, and `dirs` and `files` in the report hold the entries that survived it.
pub fn diff_snapshots(
    conn: &Connection,
    snap_a: i64,
    snap_b: i64,
    min_size: i64,
    top: usize,
) -> Result<DiffReport> {
    let volume = validate_diff_pair(conn, snap_a, snap_b)?;

    let mut dir_stmt = conn
        .prepare(
            "SELECT a.path, a.bytes, a.files, COALESCE(b.bytes, -1), COALESCE(b.files, 0)
             FROM arkos_dirs a
             LEFT JOIN arkos_dirs b ON b.snapshot_id = ?2 AND b.path = a.path
             WHERE a.snapshot_id = ?1
             UNION ALL
             SELECT b.path, -1, 0, b.bytes, b.files
             FROM arkos_dirs b
             WHERE b.snapshot_id = ?2
               AND NOT EXISTS (
                 SELECT 1 FROM arkos_dirs a WHERE a.snapshot_id = ?1 AND a.path = b.path
               )",
        )
        .map_err(map_db_err)?;

    let mut changes: Vec<Change> = Vec::new();
    let dir_rows = dir_stmt
        .query_map(params![snap_a, snap_b], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(map_db_err)?;

    for row in dir_rows {
        let (path, a_bytes, a_files, b_bytes, b_files) = row.map_err(map_db_err)?;
        let (kind, before_bytes, after_bytes, delta_files) = match (a_bytes, b_bytes) {
            (-1, nb) => (ChangeKind::Added, 0, nb, b_files),
            (ob, -1) => (ChangeKind::Removed, ob, 0, -a_files),
            (ob, nb) if nb > ob => (ChangeKind::Grown, ob, nb, b_files - a_files),
            (ob, nb) if nb < ob => (ChangeKind::Shrunk, ob, nb, b_files - a_files),
            _ => continue,
        };
        let delta_bytes = after_bytes - before_bytes;
        if delta_bytes.abs() >= min_size {
            changes.push(Change::Dir(DirDiff {
                path,
                kind,
                before_bytes,
                after_bytes,
                delta_bytes,
                delta_files,
            }));
        }
    }

    // File-level diff joined through directory path.
    let mut file_stmt = conn
        .prepare(
            "SELECT da.path, fa.name, fa.size,
                    COALESCE((
                      SELECT fb.size FROM arkos_files fb
                      JOIN arkos_dirs db ON db.id = fb.dir_id
                      WHERE fb.snapshot_id = ?2 AND db.snapshot_id = ?2 AND db.path = da.path AND fb.name = fa.name
                    ), -1)
             FROM arkos_files fa
             JOIN arkos_dirs da ON da.id = fa.dir_id
             WHERE fa.snapshot_id = ?1
             UNION ALL
             SELECT db.path, fb.name, -1, fb.size
             FROM arkos_files fb
             JOIN arkos_dirs db ON db.id = fb.dir_id
             WHERE fb.snapshot_id = ?2
               AND NOT EXISTS (
                 SELECT 1 FROM arkos_files fa
                 JOIN arkos_dirs da ON da.id = fa.dir_id
                 WHERE fa.snapshot_id = ?1 AND da.snapshot_id = ?1 AND da.path = db.path AND fa.name = fb.name
               )",
        )
        .map_err(map_db_err)?;

    let file_rows = file_stmt
        .query_map(params![snap_a, snap_b], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(map_db_err)?;

    for row in file_rows {
        let (dir_path, name, a_size, b_size) = row.map_err(map_db_err)?;
        let (kind, before_size, after_size) = match (a_size, b_size) {
            (-1, ns) => (ChangeKind::Added, 0, ns),
            (os, -1) => (ChangeKind::Removed, os, 0),
            (os, ns) if ns > os => (ChangeKind::Grown, os, ns),
            (os, ns) if ns < os => (ChangeKind::Shrunk, os, ns),
            _ => continue,
        };
        let delta_bytes = after_size - before_size;
        if delta_bytes.abs() >= min_size {
            changes.push(Change::File(FileDiff {
                path: join_display(&dir_path, &name),
                kind,
                before_size,
                after_size,
                delta_bytes,
            }));
        }
    }

    changes.sort_by(|x, y| {
        y.delta()
            .abs()
            .cmp(&x.delta().abs())
            .then_with(|| x.path().cmp(y.path()))
    });
    changes.truncate(top);

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for change in changes {
        match change {
            Change::Dir(d) => dirs.push(d),
            Change::File(f) => files.push(f),
        }
    }

    Ok(DiffReport {
        snapshot_a: snap_a,
        snapshot_b: snap_b,
        volume,
        dirs,
        files,
    })
}
