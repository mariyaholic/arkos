//! Single-pass descending-depth directory rollup (§4.5).

use crate::db::map_db_err;
use crate::exit::Result;
use rusqlite::{params, Connection};
use std::collections::HashMap;

const ROLLUP_BATCH_SIZE: usize = 5_000;

/// SQL query ordered by `(snapshot_id, depth DESC)` avoiding any recursive CTE or temp B-tree.
pub const ROLLUP_SELECT_SQL: &str =
    "SELECT id, parent_id, bytes, files FROM arkos_dirs WHERE snapshot_id = ?1 ORDER BY depth DESC";

/// Propagate child directory totals bottom-up to parents in a single index-ordered pass.
pub fn rollup_snapshot(conn: &mut Connection, snapshot_id: i64) -> Result<()> {
    let rows: Vec<(i64, Option<i64>, i64, i64)> = {
        let mut stmt = conn.prepare(ROLLUP_SELECT_SQL).map_err(map_db_err)?;
        let mapped = stmt
            .query_map([snapshot_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(map_db_err)?;
        let mut out = Vec::new();
        for item in mapped {
            out.push(item.map_err(map_db_err)?);
        }
        out
    };

    // Accumulate child-to-parent deltas cleanly from deepest nodes upward.
    let mut added: HashMap<i64, (i64, i64)> = HashMap::new();
    for &(id, parent_id, self_bytes, self_files) in &rows {
        let (sub_b, sub_f) = added.get(&id).copied().unwrap_or((0, 0));
        if let Some(pid) = parent_id {
            let entry = added.entry(pid).or_insert((0, 0));
            entry.0 += self_bytes + sub_b;
            entry.1 += self_files + sub_f;
        }
    }

    let updates: Vec<(i64, i64, i64)> = added
        .into_iter()
        .filter(|(_, (b, f))| *b != 0 || *f != 0)
        .map(|(id, (b, f))| (id, b, f))
        .collect();

    for chunk in updates.chunks(ROLLUP_BATCH_SIZE) {
        let tx = conn.transaction().map_err(map_db_err)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "UPDATE arkos_dirs SET bytes = bytes + ?2, files = files + ?3 WHERE id = ?1",
                )
                .map_err(map_db_err)?;
            for &(id, add_bytes, add_files) in chunk {
                stmt.execute(params![id, add_bytes, add_files])
                    .map_err(map_db_err)?;
            }
        }
        tx.commit().map_err(map_db_err)?;
    }

    Ok(())
}
