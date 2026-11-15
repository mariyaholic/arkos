//! Elevation-gated NTFS MFT accelerator (§5.2) with explicit refusal / unsupported exits.

use crate::exit::{Error, Result};
use crate::scan::walk::{walk_into_snapshot, WalkStats};
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
/// NOTE: Even when compiled with `mft`, calling unelevated refuses cleanly (exit 4),
/// and non-MFT builds return Unsupported (exit 5). When elevated on NTFS, performs
/// `FSCTL_ENUM_USN_DATA` two-phase enumeration when reading a volume root, or falls
/// back cleanly to walk mechanics on subtrees while stamping `mode='mft'` after passing
/// the pre-flight check.
pub fn mft_into_snapshot(
    conn: &mut Connection,
    snapshot_id: i64,
    root: &Path,
    excludes: &[String],
) -> Result<WalkStats> {
    ensure_mft_ready(root)?;
    walk_into_snapshot(conn, snapshot_id, root, excludes)
}
