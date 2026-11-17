//! Scanner trait, mode selection (`auto`, `mft`, `walk`), and snapshot lifecycle.

pub mod mft;
pub mod paths;
pub mod usn;
pub mod walk;

use crate::config::Config;
use crate::db::map_db_err;
use crate::exit::{Error, Result};
use crate::lock::VolumeLock;
use crate::logging::Logger;
use crate::paths::Paths;
use crate::progress::Reporter;
use crate::rollup::rollup_snapshot;
use crate::scan::paths::normalize_display_path;
use crate::scan::walk::{ScanFilter, WalkStats};
use crate::volumes::{is_elevated, is_ntfs, resolve_target};
use clap::ValueEnum;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::{Path, PathBuf};
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
        let built = cfg!(all(windows, feature = "mft"));
        match self {
            // The host probes only run where they can answer; elsewhere they are false anyway.
            Self::Walk => resolve_mode(self, built, false, false),
            _ => resolve_mode(self, built, is_elevated(), is_ntfs(root)),
        }
    }
}

/// Resolve the requested mode against what the build and the volume offer.
///
/// `auto` picks `mft` only for an elevated token on NTFS in a build with the `mft` feature, and
/// walks everywhere else (FAT32, exFAT, ReFS, network shares). `mft` fails with exit 5 when the
/// build or filesystem cannot do it and exit 4 when the token is not elevated.
pub fn resolve_mode(
    mode: ScanMode,
    built: bool,
    elevated: bool,
    ntfs: bool,
) -> Result<&'static str> {
    match mode {
        ScanMode::Walk => Ok("walk"),
        ScanMode::Mft => {
            mft::check_ready(built, ntfs, elevated)?;
            Ok("mft")
        }
        ScanMode::Auto => Ok(if built && elevated && ntfs {
            "mft"
        } else {
            "walk"
        }),
    }
}

/// What a scanner returns: the totals and the path that actually ran (`mft` or `walk`).
#[derive(Debug)]
pub struct ScanOutcome {
    pub stats: WalkStats,
    pub mode: &'static str,
    /// Set when the reader fell back to the walk or skipped a faster path, for the log.
    pub note: Option<String>,
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
    pub size_basis: &'static str,
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
    run_scan_with_progress(
        conn,
        paths,
        config,
        logger,
        volume_arg,
        subpath,
        label,
        mode,
        extra_excludes,
        &mut Reporter::off(),
    )
}

/// Same as [`run_scan`], reporting progress through `progress`.
#[allow(clippy::too_many_arguments)]
pub fn run_scan_with_progress(
    conn: &mut Connection,
    paths: &Paths,
    config: &Config,
    logger: &Logger,
    volume_arg: &str,
    subpath: Option<&Path>,
    label: Option<&str>,
    mode: ScanMode,
    extra_excludes: &[String],
    progress: &mut Reporter,
) -> Result<ScanReport> {
    let target = resolve_target(volume_arg, subpath)?;
    let _lock = VolumeLock::acquire(&paths.locks, &target.volume, config.busy_timeout_ms)?;

    // Holding the volume lock proves no live scan owns this volume, so any `running` row for it
    // belongs to a dead scan (for example one that died after this process swept at open).
    let dead: Vec<i64> = {
        let mut stmt = conn
            .prepare("SELECT id FROM arkos_snapshots WHERE volume = ?1 AND state = 'running'")
            .map_err(map_db_err)?;
        let ids = stmt
            .query_map([&target.volume], |r| r.get(0))
            .map_err(map_db_err)?
            .collect::<std::result::Result<Vec<i64>, _>>()
            .map_err(map_db_err)?;
        ids
    };
    for id in dead {
        conn.execute(
            "UPDATE arkos_snapshots SET state = 'abandoned', finished_at = ?1
             WHERE id = ?2 AND state = 'running'",
            params![chrono::Utc::now().timestamp(), id],
        )
        .map_err(map_db_err)?;
        logger.event(
            "abandoned_dead_scan",
            &format!(
                "snapshot={id} volume={} found running under the volume lock",
                target.volume
            ),
        );
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
    let exact_dirs: Vec<PathBuf> = data_dir_exclusion(&target.root, &paths.home)
        .into_iter()
        .collect();
    if let Some(dir) = exact_dirs.first() {
        logger.event(
            "excluded_data_dir",
            &format!("snapshot={snapshot_id} path={}", dir.display()),
        );
    }
    let filter = ScanFilter {
        globs: &excludes,
        exact_dirs: &exact_dirs,
    };

    let scan_t0 = Instant::now();
    let scan_res = match resolved_mode {
        "mft" => mft::mft_into_snapshot(
            conn,
            snapshot_id,
            &target.root,
            filter,
            progress,
            mode == ScanMode::Mft,
        ),
        _ => walk::walk_into_snapshot(conn, snapshot_id, &target.root, filter, progress).map(
            |stats| ScanOutcome {
                stats,
                mode: "walk",
                note: None,
            },
        ),
    };
    progress.finish();
    let scan_ms = scan_t0.elapsed().as_millis() as u64;

    let ScanOutcome {
        stats,
        mode: ran_mode,
        note,
    } = match scan_res {
        Ok(o) => o,
        Err(e) => {
            fail_snapshot(conn, snapshot_id, scan_ms);
            logger.event("scan_failed", &format!("snapshot={snapshot_id} err={e}"));
            return Err(e);
        }
    };

    if let Some(note) = &note {
        logger.event("scan_note", &format!("snapshot={snapshot_id} {note}"));
    }
    if stats.non_unicode_total > 0 {
        logger.event(
            "skipped_non_unicode",
            &format!(
                "snapshot={snapshot_id} names={} total={}",
                stats.non_unicode.join(" | "),
                stats.non_unicode_total
            ),
        );
    }

    let rollup_t0 = Instant::now();
    if let Err(e) = rollup_snapshot(conn, snapshot_id) {
        fail_snapshot(conn, snapshot_id, scan_ms);
        logger.event("scan_failed", &format!("snapshot={snapshot_id} err={e}"));
        return Err(e);
    }
    let rollup_ms = rollup_t0.elapsed().as_millis() as u64;

    let finished_at = chrono::Utc::now().timestamp();
    let completed = conn.execute(
        "UPDATE arkos_snapshots
         SET state = 'complete', finished_at = ?1, files = ?2, dirs = ?3, bytes = ?4,
             denied = ?5, skipped = ?6, scan_ms = ?7, mode = ?8
         WHERE id = ?9 AND state = 'running'",
        params![
            finished_at,
            stats.files as i64,
            stats.dirs as i64,
            stats.bytes as i64,
            stats.denied as i64,
            stats.skipped as i64,
            scan_ms as i64,
            ran_mode,
            snapshot_id,
        ],
    );
    match completed {
        Ok(1) => {}
        Ok(_) => {
            // Another process moved the row out of `running` (a sweep, then maybe a prune). The
            // row is not ours to rewrite or clean up.
            let e = Error::Refused(format!(
                "snapshot {snapshot_id} left the running state during the scan"
            ));
            logger.event("scan_failed", &format!("snapshot={snapshot_id} err={e}"));
            return Err(e);
        }
        Err(e) => {
            let e = map_db_err(e);
            fail_snapshot(conn, snapshot_id, scan_ms);
            logger.event("scan_failed", &format!("snapshot={snapshot_id} err={e}"));
            return Err(e);
        }
    }

    let warning = if stats.denied > 0 {
        Some(format!("{} directories were not readable", stats.denied))
    } else {
        None
    };

    logger.event(
        "scan_complete",
        &format!(
            "snapshot={snapshot_id} volume={} mode={ran_mode} files={} dirs={} bytes={} denied={} scan_ms={scan_ms} rollup_ms={rollup_ms}",
            target.volume, stats.files, stats.dirs, stats.bytes, stats.denied
        ),
    );

    Ok(ScanReport {
        snapshot_id,
        volume: target.volume,
        root: root_str,
        mode: ran_mode.to_owned(),
        dirs: stats.dirs,
        files: stats.files,
        bytes: stats.bytes,
        denied: stats.denied,
        skipped: stats.skipped,
        scan_ms,
        rollup_ms,
        warning,
        size_basis: crate::fmt::SIZE_BASIS,
    })
}

/// Directory to leave out when the scan root contains Arkos's own data directory.
///
/// Returns the data directory's path as the walk will see it (under `root`), or `None` when the
/// data directory is outside the root or is the root itself. This keeps the ledger, its WAL and
/// the log out of the snapshot.
pub fn data_dir_exclusion(root: &Path, home: &Path) -> Option<PathBuf> {
    let root_c = std::fs::canonicalize(root).ok()?;
    let home_c = std::fs::canonicalize(home).ok()?;
    let rel = home_c.strip_prefix(&root_c).ok()?;
    if rel.as_os_str().is_empty() {
        return None;
    }
    Some(root.join(rel))
}

/// Mark a failed scan: delete its partial rows and set `failed` in one transaction.
///
/// If the cleanup itself fails (for example the disk is full) the snapshot stays `running`, and
/// the lock-aware sweep marks it `abandoned` on the next open.
fn fail_snapshot(conn: &mut Connection, snapshot_id: i64, scan_ms: u64) {
    let now = chrono::Utc::now().timestamp();
    let cleanup = (|| -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM arkos_files WHERE snapshot_id = ?1",
            [snapshot_id],
        )?;
        tx.execute(
            "DELETE FROM arkos_dirs WHERE snapshot_id = ?1",
            [snapshot_id],
        )?;
        tx.execute(
            "UPDATE arkos_snapshots SET state = 'failed', finished_at = ?1, scan_ms = ?2
             WHERE id = ?3",
            params![now, scan_ms as i64, snapshot_id],
        )?;
        tx.commit()
    })();
    let _ = cleanup;
}
