#![allow(dead_code)]

//! Helpers shared by the integration tests.

pub mod fixture;

use arkos::config::Config;
use arkos::db;
use arkos::logging::Logger;
use arkos::paths::Paths;
use arkos::scan::{run_scan, ScanMode, ScanReport};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// A fresh scratch directory under the system temp folder.
pub fn temp_dir(name: &str) -> PathBuf {
    let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("arkos-test-{}-{name}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    long_form(dir)
}

/// The same folder with 8.3 short names expanded. The Windows runner's temp folder sits under
/// `RUNNER~1`, while a scan stores the long form (`runneradmin`), so tests that compare stored
/// paths with their own would differ. `canonicalize` expands the names; its `\\?\` prefix is
/// dropped again so paths keep the ordinary drive form.
fn long_form(dir: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return dir;
    }
    match std::fs::canonicalize(&dir) {
        Ok(full) => {
            let text = full.to_string_lossy();
            match text.strip_prefix(r"\\?\") {
                Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
                _ => dir,
            }
        }
        Err(_) => dir,
    }
}

/// A scratch Arkos home with an open, migrated ledger.
pub struct Env {
    pub root: PathBuf,
    pub paths: Paths,
    pub conn: Connection,
}

impl Env {
    pub fn new(name: &str) -> Self {
        let root = temp_dir(name);
        let paths = Paths::from_home(root.join("home"));
        paths.ensure_dirs().unwrap();
        let conn = db::open_and_migrate(&paths.db, &paths.locks, 1000).unwrap();
        Self { root, paths, conn }
    }

    /// Walk-scan `tree` and return the report.
    pub fn scan(&mut self, tree: &Path) -> ScanReport {
        self.scan_with(tree, &[])
    }

    pub fn scan_with(&mut self, tree: &Path, excludes: &[String]) -> ScanReport {
        let cfg = Config::default();
        let log = Logger::new(&self.paths.logs);
        run_scan(
            &mut self.conn,
            &self.paths,
            &cfg,
            &log,
            &tree.to_string_lossy(),
            None,
            None,
            ScanMode::Walk,
            excludes,
        )
        .unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
