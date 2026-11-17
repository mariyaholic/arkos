//! Elevation-gated NTFS MFT accelerator (§5.2) with explicit refusal / unsupported exits.

use crate::exit::{Error, Result};
use crate::progress::Reporter;
use crate::scan::walk::{walk_into_snapshot, ScanFilter};
use crate::scan::ScanOutcome;
use rusqlite::Connection;
use std::path::Path;

/// Decide whether `--mode mft` can run, given what the build and the host offer.
///
/// Pure so the resolution table is testable without a Windows host.
pub fn check_ready(built: bool, ntfs: bool, elevated: bool) -> Result<()> {
    if !built {
        return Err(Error::Unsupported(
            "mft mode requires a Windows build with the `mft` feature enabled".into(),
        ));
    }
    if !ntfs {
        return Err(Error::Unsupported(
            "mft mode requires an NTFS volume (FAT32, exFAT and ReFS are walked instead)".into(),
        ));
    }
    if !elevated {
        return Err(Error::Refused(
            "--mode mft requires an elevated token on Windows; run unelevated with --mode walk or --mode auto".into(),
        ));
    }
    Ok(())
}

/// Check whether `--mode mft` can execute in the current build and privilege context.
pub fn ensure_mft_ready(root: &Path) -> Result<()> {
    #[cfg(not(all(windows, feature = "mft")))]
    {
        let _ = root;
        check_ready(false, false, false)
    }
    #[cfg(all(windows, feature = "mft"))]
    {
        use crate::volumes::{is_elevated, is_ntfs};
        check_ready(true, is_ntfs(root), is_elevated())
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
    filter: ScanFilter<'_>,
    progress: &mut Reporter,
) -> Result<ScanOutcome> {
    ensure_mft_ready(root)?;
    let stats = walk_into_snapshot(conn, snapshot_id, root, filter, progress)?;
    Ok(ScanOutcome {
        stats,
        mode: "walk",
    })
}
