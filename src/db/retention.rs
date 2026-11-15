//! Snapshot retention, abandoned-scan sweeper, and bounded incremental vacuum.

use crate::db::map_db_err;
use crate::exit::Result;
use rusqlite::{params, Connection};

const STALE_RUNNING_AGE_SECS: i64 = 24 * 3600;
const INCREMENTAL_VACUUM_THRESHOLD_BYTES: u64 = 32 * 1024 * 1024;

/// Mark stale `running` snapshots older than 24 hours as `abandoned`.
pub fn sweep_abandoned(conn: &Connection) -> Result<usize> {
    let now = chrono::Utc::now().timestamp();
    sweep_abandoned_at(conn, now)
}

/// Testable variant of [`sweep_abandoned`] with explicit current timestamp.
pub fn sweep_abandoned_at(conn: &Connection, now: i64) -> Result<usize> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='arkos_snapshots')",
            [],
            |r| r.get(0),
        )
        .map_err(map_db_err)?;
    if !exists {
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

/// Delete old completed snapshots beyond `keep`, leaving `running` snapshots untouched.
///
/// Only runs `PRAGMA incremental_vacuum(2048)` when freelist exceeds 32 MiB; never runs full `VACUUM`.
pub fn prune_completed(conn: &Connection, volume_filter: Option<&str>, keep: u32) -> Result<usize> {
    let volumes: Vec<String> = match volume_filter {
        Some(v) => vec![v.to_owned()],
        None => {
            let mut stmt = conn.prepare("SELECT DISTINCT volume FROM arkos_snapshots")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        }
    };

    let mut deleted_total = 0usize;
    for volume in volumes {
        let deleted = conn
            .execute(
                "DELETE FROM arkos_snapshots
                 WHERE id IN (
                   SELECT id FROM arkos_snapshots
                   WHERE volume = ?1 AND state = 'complete'
                   ORDER BY started_at DESC, id DESC
                   LIMIT -1 OFFSET ?2
                 )",
                params![volume, keep as i64],
            )
            .map_err(map_db_err)?;
        deleted_total += deleted;
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
