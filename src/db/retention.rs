//! Snapshot retention, abandoned-scan sweeper, and bounded incremental vacuum.

use crate::db::map_db_err;
use crate::exit::{Error, Result};
use crate::lock::VolumeLock;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;

const STALE_RUNNING_AGE_SECS: i64 = 24 * 3600;
const INCREMENTAL_VACUUM_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;

/// Mark stale `running` snapshots older than 24 hours as `abandoned`.
pub fn sweep_abandoned(conn: &Connection) -> Result<usize> {
    let now = chrono::Utc::now().timestamp();
    sweep_abandoned_at(conn, now)
}

fn has_snapshots_table(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='arkos_snapshots')",
        [],
        |r| r.get(0),
    )
    .map_err(map_db_err)
}

/// Testable variant of [`sweep_abandoned`] with explicit current timestamp.
pub fn sweep_abandoned_at(conn: &Connection, now: i64) -> Result<usize> {
    if !has_snapshots_table(conn)? {
        return Ok(0);
    }
    let cutoff = now - STALE_RUNNING_AGE_SECS;
    let changed = conn
        .execute(
            "UPDATE arkos_snapshots SET state = 'abandoned', finished_at = ?1
             WHERE state = 'running' AND started_at < ?2",
            params![now, cutoff],
        )
        .map_err(map_db_err)?;
    Ok(changed)
}

/// Mark `running` snapshots `abandoned` when nothing holds their volume lock.
///
/// A scan holds its volume lock from before it inserts the `running` row until after it sets the
/// final state, and the OS drops the lock when the process dies. So a `running` row whose lock
/// can be taken without waiting belongs to a dead scan. When the lock directory cannot be used,
/// the row falls back to the 24 hour rule.
pub fn sweep_abandoned_with_locks(conn: &Connection, locks_dir: &Path, now: i64) -> Result<usize> {
    if !has_snapshots_table(conn)? {
        return Ok(0);
    }
    let mut stmt = conn
        .prepare("SELECT id, volume, started_at FROM arkos_snapshots WHERE state = 'running'")
        .map_err(map_db_err)?;
    let running: Vec<(i64, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(map_db_err)?
        .collect::<std::result::Result<_, _>>()
        .map_err(map_db_err)?;
    drop(stmt);

    let mut changed = 0usize;
    for (id, volume, started_at) in running {
        let dead = match VolumeLock::acquire(locks_dir, &volume, 0) {
            Ok(lock) => {
                drop(lock);
                true
            }
            Err(Error::Busy(_)) => false,
            Err(_) => started_at < now - STALE_RUNNING_AGE_SECS,
        };
        if dead {
            changed += conn
                .execute(
                    "UPDATE arkos_snapshots SET state = 'abandoned', finished_at = ?1
                     WHERE id = ?2 AND state = 'running'",
                    params![now, id],
                )
                .map_err(map_db_err)?;
        }
    }
    Ok(changed)
}

/// A snapshot that a prune would delete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PruneCandidate {
    pub id: i64,
    pub volume: String,
    pub root: String,
    pub state: String,
    pub started_at: i64,
    pub bytes: i64,
}

/// List the snapshots a prune would delete: completed ones beyond the newest `keep` per volume.
///
/// `running` snapshots are never candidates. [`prune_completed`] deletes exactly this list.
pub fn plan_prune(
    conn: &Connection,
    volume_filter: Option<&str>,
    keep: u32,
) -> Result<Vec<PruneCandidate>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, volume, root, state, started_at, bytes FROM (
               SELECT id, volume, root, state, started_at, bytes,
                      ROW_NUMBER() OVER (
                        PARTITION BY volume ORDER BY started_at DESC, id DESC
                      ) AS rn
               FROM arkos_snapshots
               WHERE state = 'complete' AND (?1 IS NULL OR volume = ?1)
             ) WHERE rn > ?2
             ORDER BY volume, started_at DESC, id DESC",
        )
        .map_err(map_db_err)?;
    let rows = stmt
        .query_map(params![volume_filter, keep as i64], |r| {
            Ok(PruneCandidate {
                id: r.get(0)?,
                volume: r.get(1)?,
                root: r.get(2)?,
                state: r.get(3)?,
                started_at: r.get(4)?,
                bytes: r.get(5)?,
            })
        })
        .map_err(map_db_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_db_err)?);
    }
    Ok(out)
}

/// Delete old completed snapshots beyond `keep`, leaving `running` snapshots untouched.
///
/// Only runs `PRAGMA incremental_vacuum(2048)` when freelist exceeds 32 MiB; never runs full `VACUUM`.
pub fn prune_completed(conn: &Connection, volume_filter: Option<&str>, keep: u32) -> Result<usize> {
    let plan = plan_prune(conn, volume_filter, keep)?;
    let mut deleted_total = 0usize;
    for candidate in &plan {
        deleted_total += conn
            .execute("DELETE FROM arkos_snapshots WHERE id = ?1", [candidate.id])
            .map_err(map_db_err)?;
    }

    if deleted_total > 0 {
        maybe_incremental_vacuum(conn)?;
    }
    Ok(deleted_total)
}

fn maybe_incremental_vacuum(conn: &Connection) -> Result<bool> {
    let page_size: u64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    let freelist_count: u64 = conn.pragma_query_value(None, "freelist_count", |r| r.get(0))?;
    if freelist_count.saturating_mul(page_size) > INCREMENTAL_VACUUM_THRESHOLD_BYTES {
        conn.execute_batch("PRAGMA incremental_vacuum(2048);")
            .map_err(map_db_err)?;
        return Ok(true);
    }
    Ok(false)
}
