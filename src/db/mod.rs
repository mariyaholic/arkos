//! Ledger opening, migration 0001, and user_version discipline for `shimatsu.sqlite`.

pub mod retention;

use crate::exit::{Error, Result};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Highest `PRAGMA user_version` known to Arkos M0.
pub const KNOWN_MAX_VERSION: u32 = 1;

const MIGRATION_0001: &str = include_str!("0001.sql");

/// Open `shimatsu.sqlite` with explicit read-only vs read-write policy.
pub fn open(path: &Path, busy_timeout_ms: u64, writable: bool) -> Result<Connection> {
    if writable {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Io {
                context: "create ledger directory",
                source,
            })?;
        }
    }
    let flags = if writable {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let conn = Connection::open_with_flags(path, flags).map_err(map_db_err)?;
    conn.busy_timeout(Duration::from_millis(busy_timeout_ms))
        .map_err(map_db_err)?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(map_db_err)?;

    let user_version: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(map_db_err)?;
    if user_version > KNOWN_MAX_VERSION {
        return Err(Error::Refused(format!(
            "ledger.newer: database user_version {user_version} exceeds supported {KNOWN_MAX_VERSION}"
        )));
    }

    if writable {
        let _ = conn.pragma_update(None, "auto_vacuum", "INCREMENTAL");
        // Switching the journal mode ignores the busy handler, so retry within the timeout.
        let deadline = std::time::Instant::now() + Duration::from_millis(busy_timeout_ms);
        let mode: String = loop {
            match conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0)) {
                Ok(m) => break m,
                Err(e) => match map_db_err(e) {
                    Error::Busy(msg) => {
                        if std::time::Instant::now() >= deadline {
                            return Err(Error::Busy(msg));
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    other => return Err(other),
                },
            }
        };
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::Io {
                context: "enable WAL mode",
                source: std::io::Error::other(format!("expected WAL journal mode, got {mode}")),
            });
        }
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(map_db_err)?;
        conn.pragma_update(None, "wal_autocheckpoint", 256)
            .map_err(map_db_err)?;
    } else {
        conn.pragma_update(None, "query_only", true)
            .map_err(map_db_err)?;
    }

    conn.execute_batch("PRAGMA cache_size=-4096; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;")
        .map_err(map_db_err)?;
    Ok(conn)
}

/// Page cache during a scan, in KiB (32 MiB). Keeps the index pages of a large insert resident.
const SCAN_CACHE_KIB: i64 = 32 * 1024;
/// Page cache outside a scan, in KiB (4 MiB).
const IDLE_CACHE_KIB: i64 = 4 * 1024;
/// WAL auto-checkpoint outside a scan, in pages.
const IDLE_WAL_AUTOCHECKPOINT: i64 = 256;

/// Switch a writable connection to scan tuning: a 32 MiB page cache and no automatic WAL
/// checkpoint, so the scan's transactions do not stop to checkpoint. Pair with [`end_bulk_write`].
pub fn begin_bulk_write(conn: &Connection) -> Result<()> {
    conn.execute_batch(&format!(
        "PRAGMA cache_size=-{SCAN_CACHE_KIB}; PRAGMA wal_autocheckpoint=0;"
    ))
    .map_err(map_db_err)
}

/// Undo [`begin_bulk_write`]: restore the defaults and fold the WAL back into the database file.
/// The checkpoint is best effort; a reader that blocks it leaves the WAL for a later checkpoint.
pub fn end_bulk_write(conn: &Connection) {
    let _ = conn.execute_batch(&format!(
        "PRAGMA cache_size=-{IDLE_CACHE_KIB}; PRAGMA wal_autocheckpoint={IDLE_WAL_AUTOCHECKPOINT};"
    ));
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
}

/// Open for read-write and ensure migration 0001 is applied, then sweep abandoned scans.
///
/// `locks_dir` is where volume locks live (`Paths::locks`); the sweep checks it to tell a live
/// scan from a dead one. Used by the verbs that write: `scan`, `prune`, `db migrate`, `db vacuum`.
pub fn open_and_migrate(path: &Path, locks_dir: &Path, busy_timeout_ms: u64) -> Result<Connection> {
    let conn = open(path, busy_timeout_ms, true)?;
    migrate(&conn)?;
    retention::sweep_abandoned_with_locks(&conn, locks_dir, chrono::Utc::now().timestamp())?;
    Ok(conn)
}

/// Open an existing ledger read-only for the verbs that only read. It never migrates, sweeps,
/// creates a file or takes the write lock. Returns `None` when there is no ledger yet (no file, or
/// a file with no `arkos_*` tables and `user_version` 0), which callers treat as an empty ledger.
pub fn open_read(path: &Path, busy_timeout_ms: u64) -> Result<Option<Connection>> {
    if !path.exists() {
        return Ok(None);
    }
    let conn = open(path, busy_timeout_ms, false)?;
    let ver: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(map_db_err)?;
    if ver == 0 {
        if arkos_tables_exist(&conn)? {
            return Err(inconsistent());
        }
        return Ok(None);
    }
    Ok(Some(conn))
}

fn arkos_tables_exist(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name LIKE 'arkos\\_%' ESCAPE '\\')",
        [],
        |r| r.get(0),
    )
    .map_err(map_db_err)
}

fn inconsistent() -> Error {
    Error::Refused("ledger.inconsistent: tables present but user_version is 0".into())
}

/// Apply Arkos schema 0001. A ledger that is already at the known version is left alone without
/// taking the write lock; otherwise the check and the script run inside `BEGIN IMMEDIATE`.
pub fn migrate(conn: &Connection) -> Result<u32> {
    let ver: u32 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(map_db_err)?;
    if ver > KNOWN_MAX_VERSION {
        return Err(Error::Refused(format!(
            "ledger.newer: database user_version {ver} exceeds supported {KNOWN_MAX_VERSION}"
        )));
    }
    if ver == KNOWN_MAX_VERSION {
        return Ok(KNOWN_MAX_VERSION);
    }
    conn.execute_batch("BEGIN IMMEDIATE;").map_err(map_db_err)?;
    let res = (|| -> Result<u32> {
        let ver: u32 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(map_db_err)?;
        if ver > KNOWN_MAX_VERSION {
            return Err(Error::Refused(format!(
                "ledger.newer: database user_version {ver} exceeds supported {KNOWN_MAX_VERSION}"
            )));
        }
        if ver == 0 {
            if arkos_tables_exist(conn)? {
                return Err(inconsistent());
            }
            conn.execute_batch(MIGRATION_0001).map_err(map_db_err)?;
            conn.pragma_update(None, "user_version", KNOWN_MAX_VERSION)
                .map_err(map_db_err)?;
        }
        Ok(KNOWN_MAX_VERSION)
    })();
    match res {
        Ok(v) => {
            conn.execute_batch("COMMIT;").map_err(map_db_err)?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

/// Run `f` inside one deferred read transaction, so every statement in it sees one snapshot of
/// the ledger even if another process commits (for example a prune) in between.
pub fn read_tx<T>(conn: &Connection, f: impl FnOnce() -> Result<T>) -> Result<T> {
    if !conn.is_autocommit() {
        return f();
    }
    conn.execute_batch("BEGIN DEFERRED;").map_err(map_db_err)?;
    let out = f();
    let _ = conn.execute_batch("COMMIT;");
    out
}

/// Map SQLite busy/locked errors to `Error::Busy` (exit 6).
pub(crate) fn map_db_err(err: rusqlite::Error) -> Error {
    match &err {
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            Error::Busy(format!("database busy: {err}"))
        }
        _ => Error::Db(err),
    }
}

/// Ledger summary returned by `arkos db info`.
#[derive(Debug, Serialize)]
pub struct DbInfo {
    pub path: Option<String>,
    pub user_version: u32,
    pub sqlite_version: String,
    pub db_bytes: u64,
    pub wal_bytes: u64,
    pub page_size: u64,
    pub page_count: u64,
    pub freelist_count: u64,
    pub counts: BTreeMap<String, i64>,
}

/// Collect table counts and file size metadata.
pub fn info(conn: &Connection) -> Result<DbInfo> {
    let mut counts = BTreeMap::new();
    for table in ["arkos_snapshots", "arkos_dirs", "arkos_files"] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |r| r.get(0),
        )?;
        if exists {
            let n: i64 =
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            counts.insert(table.to_owned(), n);
        }
    }
    let path = conn.path().filter(|s| !s.is_empty()).map(str::to_owned);
    let file_size = |p: &str| -> u64 { std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) };
    let (db_bytes, wal_bytes) = match path.as_deref() {
        Some(p) => (file_size(p), file_size(&format!("{p}-wal"))),
        None => (0, 0),
    };
    Ok(DbInfo {
        path,
        user_version: conn.pragma_query_value(None, "user_version", |r| r.get(0))?,
        sqlite_version: conn.query_row("SELECT sqlite_version()", [], |r| r.get(0))?,
        db_bytes,
        wal_bytes,
        page_size: conn.pragma_query_value(None, "page_size", |r| r.get(0))?,
        page_count: conn.pragma_query_value(None, "page_count", |r| r.get(0))?,
        freelist_count: conn.pragma_query_value(None, "freelist_count", |r| r.get(0))?,
        counts,
    })
}

/// Run explicit `VACUUM` (`arkos db vacuum`), returning reclaimed bytes.
pub fn vacuum(conn: &Connection) -> Result<i64> {
    let before: i64 = conn
        .path()
        .filter(|s| !s.is_empty())
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    conn.execute_batch("VACUUM;").map_err(map_db_err)?;
    let after: i64 = conn
        .path()
        .filter(|s| !s.is_empty())
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    Ok((before - after).max(0))
}
