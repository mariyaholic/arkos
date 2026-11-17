//! Elevation-gated NTFS MFT accelerator (§5.2) with explicit refusal / unsupported exits.

use crate::exit::{Error, Result};
use crate::progress::Reporter;
use crate::scan::walk::walk_into_snapshot;
use crate::scan::ScanOutcome;
use rusqlite::Connection;
use std::path::Path;

/// Check whether `--mode mft` can execute in the current build and privilege context.
pub fn ensure_mft_ready(root: &Path) -> Result<()> {
    #[cfg(not(all(windows, feature = "mft")))]
    {
        let _ = root;
        Err(Error::Unsupported(
            "mft mode requires a Windows build with the `mft` feature enabled".into(),
        ))
    }
    #[cfg(all(windows, feature = "mft"))]
    {
        use crate::volumes::{is_elevated, is_ntfs};
        if !is_ntfs(root) {
            return Err(Error::Unsupported(format!(
                "mft mode requires an NTFS volume: {}",
                root.display()
            )));
        }
        if !is_elevated() {
            return Err(Error::Refused(
                "--mode mft requires an elevated token on Windows; run unelevated with --mode walk or --mode auto".into(),
            ));
        }
        Ok(())
    }
}

/// Run an elevated NTFS MFT pass when available.
///
/// The returned outcome names the path that ran. Until the USN reader lands this walks and says
/// `walk`, so the snapshot never claims `mft` for a walk.
pub fn mft_into_snapshot(
    conn: &mut Connection,
    snapshot_id: i64,
    root: &Path,
    excludes: &[String],
    progress: &mut Reporter,
) -> Result<ScanOutcome> {
    ensure_mft_ready(root)?;
    let stats = walk_into_snapshot(conn, snapshot_id, root, excludes, progress)?;
    Ok(ScanOutcome {
        stats,
        mode: "walk",
    })
}
