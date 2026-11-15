//! Bounded rolling JSONL log writer (3 x 1 MiB files, never written to SQLite).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_LOG_BYTES: u64 = 1_048_576;
const LOG_KEEP: usize = 3;

/// Bounded rolling file logger under `<home>/logs/arkos.log`.
pub struct Logger {
    path: PathBuf,
}

impl Logger {
    /// Open or prepare a logger pointing at `logs_dir/arkos.log`.
    pub fn new(logs_dir: &Path) -> Self {
        Self {
            path: logs_dir.join("arkos.log"),
        }
    }

    /// Append one structured event line, rotating at 1 MiB across 3 generations.
    pub fn event(&self, kind: &str, message: &str) {
        let _ = self.try_event(kind, message);
    }

    fn try_event(&self, kind: &str, message: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Ok(meta) = fs::metadata(&self.path) {
            if meta.len() >= MAX_LOG_BYTES {
                self.rotate()?;
            }
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::json!({
            "at": chrono::Utc::now().timestamp(),
            "kind": kind,
            "message": message,
        });
        writeln!(f, "{line}")?;
        Ok(())
    }

    fn rotate(&self) -> std::io::Result<()> {
        for idx in (1..LOG_KEEP).rev() {
            let src = self.path.with_extension(format!("log.{idx}"));
            let dst = self.path.with_extension(format!("log.{}", idx + 1));
            if src.exists() {
                let _ = fs::rename(&src, &dst);
            }
        }
        if self.path.exists() {
            let dst = self.path.with_extension("log.1");
            let _ = fs::rename(&self.path, &dst);
        }
        Ok(())
    }
}
