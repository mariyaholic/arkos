//! Scanner trait, mode selection (`auto`, `mft`, `walk`), and snapshot lifecycle.

pub mod mft;
pub mod paths;
pub mod walk;

use crate::config::Config;
use crate::db::map_db_err;
use crate::exit::{Error, Result};
use crate::lock::VolumeLock;
use crate::logging::Logger;
use crate::paths::Paths;
use crate::rollup::rollup_snapshot;
use crate::scan::paths::normalize_display_path;
use crate::volumes::{is_elevated, is_ntfs, resolve_target};
use clap::ValueEnum;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;
use std::time::Instant;

/// Requested scanner mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScanMode {
    Auto,
    Mft,
    Walk,
}

impl ScanMode {
    fn as_resolved(self, root: &Path) -> Result<&'static str> {
        match self {
            Self::Walk => Ok("walk"),
            Self::Mft => {
                mft::ensure_mft_ready(root)?;
                Ok("mft")
            }
            Self::Auto => {
                if cfg!(all(windows, feature = "mft")) && is_elevated() && is_ntfs(root) {
                    Ok("mft")
                } else {
                    Ok("walk")
                }
            }
        }
    }
}

/// Outcome returned after completing a snapshot scan.
#[derive(Debug, Serialize)]
pub struct ScanReport {
    pub snapshot_id: i64,
    pub volume: String,
    pub root: String,
    pub mode: String,
    pub dirs: u64,
    pub files: u64,
    pub bytes: u64,
    pub denied: u64,
    pub skipped: u64,
    pub scan_ms: u64,
    pub rollup_ms: u64,
    pub warning: Option<String>,
}

/// Execute a full snapshot scan against `volume` (and optional `subpath`), returning `ScanReport`.
#[allow(clippy::too_many_arguments)]
pub fn run_scan(
    conn: &mut Connection,
    paths: &Paths,
    config: &Config,
    logger: &Logger,
    volume_arg: &str,
    subpath: Option<&Path>,
    label: Option<&str>,
    mode: ScanMode,
    extra_excludes: &[String],
) -> Result<ScanReport> {
    let target = resolve_target(volume_arg, subpath)?;
    let _lock = VolumeLock::acquire(&paths.locks, &target.volume, config.busy_timeout_ms)?;

    let already_running: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM arkos_snapshots WHERE volume = ?1 AND state = 'running')",
            [&target.volume],
            |r| r.get(0),
        )
        .map_err(map_db_err)?;
    if already_running {
        return Err(Error::Refused(format!(
            "another scan is already running for volume {}",
            target.volume
        )));
    }

    let resolved_mode = mode.as_resolved(&target.root)?;
    let elevated = if is_elevated() { 1i64 } else { 0i64 };
    let started_at = chrono::Utc::now().timestamp();
    let root_str = normalize_display_path(&target.root);

    conn.execute(
        "INSERT INTO arkos_snapshots(volume, root, label, mode, elevated, state, started_at)
         VALUES(?1, ?2, ?3, ?4, ?5, 'running', ?6)",
        params![
            target.volume,
            root_str,
            label,
            resolved_mode,
            elevated,
            started_at
        ],
    )
    .map_err(map_db_err)?;
    let snapshot_id = conn.last_insert_rowid();

    let mut excludes = config.excludes.clone();
    excludes.extend_from_slice(extra_excludes);

    let scan_t0 = Instant::now();
    let walk_res = match resolved_mode {
        "mft" => mft::mft_into_snapshot(conn, snapshot_id, &target.root, &excludes),
        _ => walk::walk_into_snapshot(conn, snapshot_id, &target.root, &excludes),
    };
    let scan_ms = scan_t0.elapsed().as_millis() as u64;

    let stats = match walk_res {
        Ok(s) => s,
        Err(e) => {
            let now = chrono::Utc::now().timestamp();
            let _ = conn.execute(
                "UPDATE arkos_snapshots SET state = 'failed', finished_at = ?1, scan_ms = ?2 WHERE id = ?3",
                params![now, scan_ms as i64, snapshot_id],
            );
            logger.event("scan_failed", &format!("snapshot={snapshot_id} err={e}"));
            return Err(e);
        }
    };

    let rollup_t0 = Instant::now();
    rollup_snapshot(conn, snapshot_id)?;
    let rollup_ms = rollup_t0.elapsed().as_millis() as u64;

    let finished_at = chrono::Utc::now().timestamp();
    conn.execute(
        "UPDATE arkos_snapshots
         SET state = 'complete', finished_at = ?1, files = ?2, dirs = ?3, bytes = ?4,
             denied = ?5, skipped = ?6, scan_ms = ?7
         WHERE id = ?8",
        params![
            finished_at,
            stats.files as i64,
            stats.dirs as i64,
            stats.bytes as i64,
            stats.denied as i64,
            stats.skipped as i64,
            scan_ms as i64,
            snapshot_id,
        ],
    )
    .map_err(map_db_err)?;

    let warning = if stats.denied > 0 {
        Some(format!("{} directories were not readable", stats.denied))
    } else {
        None
    };

    logger.event(
        "scan_complete",
        &format!(
            "snapshot={snapshot_id} volume={} mode={resolved_mode} files={} dirs={} bytes={} denied={} scan_ms={scan_ms} rollup_ms={rollup_ms}",
            target.volume, stats.files, stats.dirs, stats.bytes, stats.denied
        ),
    );

    Ok(ScanReport {
        snapshot_id,
        volume: target.volume,
        root: root_str,
        mode: resolved_mode.to_owned(),
        dirs: stats.dirs,
        files: stats.files,
        bytes: stats.bytes,
        denied: stats.denied,
        skipped: stats.skipped,
        scan_ms,
        rollup_ms,
        warning,
    })
}
