//! Standard probe document (§2) and registry `status --probe` check.

use crate::db::map_db_err;
use crate::exit::{Exit, Result};
use rusqlite::Connection;
use serde::Serialize;

/// Compiled feature matrix exposed in `arkos probe`.
#[derive(Debug, Serialize)]
pub struct CompiledFeatures {
    pub mft: bool,
    pub panel: bool,
    pub ipc: bool,
}

/// Budget ceilings declared by `docs/specs/arkos-m0.md` §2 & §6.
#[derive(Debug, Serialize)]
pub struct Budgets {
    pub mft_1m_ms: u64,
    pub walk_files_per_s: u64,
    pub peak_bytes: u64,
    pub db_bytes_per_1m_snapshot: u64,
    pub diff_1m_ms: u64,
    pub binary_bytes: u64,
}

/// Measured slots (`null` until populated by a real benchmark run).
#[derive(Debug, Serialize)]
pub struct Measured {
    pub mft_1m_ms: Option<u64>,
    pub walk_files_per_s: Option<u64>,
    pub peak_bytes: Option<u64>,
    pub db_bytes_per_1m_snapshot: Option<u64>,
    pub diff_1m_ms: Option<u64>,
    pub binary_bytes: Option<u64>,
    pub fixture: &'static str,
    pub at: Option<String>,
    pub method: &'static str,
}

/// Complete probe payload returned by `arkos probe`.
#[derive(Debug, Serialize)]
pub struct ProbeDoc {
    pub v: u32,
    pub app: &'static str,
    pub version: &'static str,
    pub class: &'static str,
    pub features: Vec<&'static str>,
    pub compiled: CompiledFeatures,
    pub budgets: Budgets,
    pub measured: Measured,
}

/// Build the probe document reflecting compile-time features accurately.
pub fn build_probe_doc() -> ProbeDoc {
    let has_mft = cfg!(all(windows, feature = "mft"));
    let mut features = vec!["walk"];
    if has_mft {
        features.push("mft");
    }
    if cfg!(feature = "ipc") {
        features.push("ipc");
    }
    ProbeDoc {
        v: 1,
        app: "arkos",
        version: env!("CARGO_PKG_VERSION"),
        class: "on-demand",
        features,
        compiled: CompiledFeatures {
            mft: has_mft,
            panel: false,
            ipc: cfg!(feature = "ipc"),
        },
        budgets: Budgets {
            mft_1m_ms: 10_000,
            walk_files_per_s: 200_000,
            peak_bytes: 60_000_000,
            db_bytes_per_1m_snapshot: 75_000_000,
            diff_1m_ms: 2_000,
            binary_bytes: 8_000_000,
        },
        measured: Measured {
            mft_1m_ms: None,
            walk_files_per_s: None,
            peak_bytes: None,
            db_bytes_per_1m_snapshot: None,
            diff_1m_ms: None,
            binary_bytes: None,
            fixture: "gen_tree --files 1000000 --dirs 120000 --depth 12",
            at: None,
            method: "scripts/measure.ps1",
        },
    }
}

/// Registry probe evaluation (`arkos status --probe`):
/// - exits 6 when a scan is running
/// - exits 3 when the latest completed scan had denied directories
/// - exits 0 otherwise (last scan completed cleanly or no scans yet)
pub fn status_probe_exit(conn: &Connection) -> Result<Exit> {
    let running: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM arkos_snapshots WHERE state = 'running')",
            [],
            |r| r.get(0),
        )
        .map_err(map_db_err)?;
    probe_after_running(conn, running)
}

/// Like [`status_probe_exit`], for a read-only ledger that was not swept: a `running` row counts
/// only when its volume lock is held (or, if the lock cannot be probed, it is under 24 hours old).
pub fn status_probe_exit_with_locks(
    conn: &Connection,
    locks_dir: &std::path::Path,
) -> Result<Exit> {
    let now = chrono::Utc::now().timestamp();
    let mut stmt = conn
        .prepare("SELECT volume, started_at FROM arkos_snapshots WHERE state = 'running'")
        .map_err(map_db_err)?;
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(map_db_err)?
        .collect::<std::result::Result<_, _>>()
        .map_err(map_db_err)?;
    let running = rows.iter().any(|(volume, started_at)| {
        match crate::lock::VolumeLock::is_free(locks_dir, volume) {
            Ok(free) => !free,
            Err(_) => *started_at >= now - 24 * 3600,
        }
    });
    probe_after_running(conn, running)
}

/// One `running` snapshot as `arkos status` shows it.
#[derive(Debug, Serialize)]
pub struct RunningScan {
    pub id: i64,
    pub volume: String,
    /// `running`, or `stalled (N s)` when the holder's last beat is older than 120 seconds.
    pub health: String,
}

/// The `running` snapshots whose volume lock is held, with their heartbeat health. A row whose
/// lock is free belongs to a dead scan, and a stale sidecar is ignored.
pub fn running_scans(conn: &Connection, locks_dir: &std::path::Path) -> Result<Vec<RunningScan>> {
    let now = chrono::Utc::now().timestamp();
    let mut stmt = conn
        .prepare("SELECT id, volume FROM arkos_snapshots WHERE state = 'running' ORDER BY id")
        .map_err(map_db_err)?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(map_db_err)?
        .collect::<std::result::Result<_, _>>()
        .map_err(map_db_err)?;
    let mut out = Vec::new();
    for (id, volume) in rows {
        if matches!(
            crate::lock::VolumeLock::is_free(locks_dir, &volume),
            Ok(true)
        ) {
            continue;
        }
        let health = match crate::lock::read_heartbeat_for(locks_dir, &volume)
            .and_then(|hb| hb.stalled_for(now))
        {
            Some(age) => format!("stalled ({age} s)"),
            None => "running".to_owned(),
        };
        out.push(RunningScan { id, volume, health });
    }
    Ok(out)
}

fn probe_after_running(conn: &Connection, running: bool) -> Result<Exit> {
    if running {
        return Ok(Exit::Busy);
    }

    let denied: Option<i64> = conn
        .query_row(
            "SELECT denied FROM arkos_snapshots WHERE state = 'complete' ORDER BY started_at DESC, id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    if denied.unwrap_or(0) > 0 {
        return Ok(Exit::Input);
    }
    Ok(Exit::Ok)
}

#[cfg(test)]
mod tests {
    use super::build_probe_doc;

    #[test]
    fn probe_reports_walk_only_when_built_without_windows_mft() {
        let doc = build_probe_doc();
        assert!(doc.features.contains(&"walk"));
        if !cfg!(all(windows, feature = "mft")) {
            assert_eq!(doc.features, vec!["walk"]);
            assert!(!doc.compiled.mft);
        } else {
            assert!(doc.features.contains(&"mft"));
            assert!(doc.compiled.mft);
        }
    }
}
