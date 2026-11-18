//! Volume normalization, filesystem type checks, and elevation detection.

use crate::exit::{Error, Result};
use crate::scan::paths::normalize_display_path;
use std::path::{Path, PathBuf};

/// Normalized scan target describing volume key (`C:`) and concrete root path.
#[derive(Debug, Clone)]
pub struct ScanTarget {
    pub volume: String,
    pub root: PathBuf,
}

/// Normalize `--volume` and optional `--path` into a canonical scan target.
pub fn resolve_target(volume: &str, subpath: Option<&Path>) -> Result<ScanTarget> {
    let trimmed = volume.trim();
    if trimmed.is_empty() {
        return Err(Error::Cli("--volume must not be empty".into()));
    }

    let (norm_vol, base_root) = parse_volume(trimmed)?;
    let root = match subpath {
        Some(sub) => {
            if sub.is_absolute() {
                sub.to_path_buf()
            } else {
                base_root.join(sub)
            }
        }
        None => base_root,
    };

    if !root.exists() {
        return Err(Error::Input(format!(
            "scan root does not exist: {}",
            root.display()
        )));
    }
    if !root.is_dir() {
        return Err(Error::Input(format!(
            "scan root is not a directory: {}",
            root.display()
        )));
    }
    if is_network_path(&root) {
        return Err(Error::Unsupported(format!(
            "lock.unsupported_fs: network share not supported: {}",
            root.display()
        )));
    }

    Ok(ScanTarget {
        volume: norm_vol,
        root,
    })
}

/// The canonical volume key for a `--volume` argument, shared by `scan`, `list --volume` and
/// `prune --volume` so the same input always names the same ledger rows.
///
/// A drive letter becomes `C:`, a volume GUID path keeps its form, and a directory becomes
/// `dir:<absolute path>` using the canonical path (without the `\\?\` prefix on Windows) when the
/// directory exists, or the lexically cleaned absolute path when it no longer does. A stored key
/// (`dir:/x`) is accepted as input too.
pub fn volume_key(input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(Error::Cli("--volume must not be empty".into()));
    }
    parse_volume(trimmed).map(|(key, _)| key)
}

fn parse_volume(input: &str) -> Result<(String, PathBuf)> {
    let cleaned = input.trim_end_matches(['\\', '/']);
    let bytes = cleaned.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let letter = (bytes[0] as char).to_ascii_uppercase();
        let vol = format!("{letter}:");
        let root = PathBuf::from(format!("{letter}:\\"));
        return Ok((vol, root));
    }
    if input.starts_with(r"\\?\Volume{") {
        let vol = cleaned.to_owned();
        let root = PathBuf::from(format!("{vol}\\"));
        return Ok((vol, root));
    }
    let path_text = input.strip_prefix("dir:").unwrap_or(input);
    let p = PathBuf::from(path_text);
    if let Ok(canon) = std::fs::canonicalize(&p) {
        if canon.is_dir() {
            let root = PathBuf::from(normalize_display_path(&canon));
            return Ok((format!("dir:{}", root.to_string_lossy()), root));
        }
    }
    if p.is_absolute() {
        let root = lexical_clean(&p);
        return Ok((format!("dir:{}", root.to_string_lossy()), root));
    }
    Err(Error::Cli(format!(
        "unsupported --volume format (expected e.g. C: or an existing directory): {input}"
    )))
}

/// Drop `.` parts and trailing separators without touching the filesystem.
fn lexical_clean(p: &Path) -> PathBuf {
    p.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect()
}

fn is_network_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with(r"\\") && !s.starts_with(r"\\?\") && !s.starts_with(r"\\.\")
}

/// Return true when the current process is elevated on Windows.
pub fn is_elevated() -> bool {
    #[cfg(all(windows, feature = "mft"))]
    {
        win_is_elevated()
    }
    #[cfg(not(all(windows, feature = "mft")))]
    {
        false
    }
}

/// Return true when `root` appears to reside on an NTFS volume.
pub fn is_ntfs(root: &Path) -> bool {
    #[cfg(all(windows, feature = "mft"))]
    {
        win_is_ntfs(root)
    }
    #[cfg(not(all(windows, feature = "mft")))]
    {
        let _ = root;
        false
    }
}

#[cfg(all(windows, feature = "mft"))]
fn win_is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation: TOKEN_ELEVATION = std::mem::zeroed();
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(all(windows, feature = "mft"))]
fn win_is_ntfs(root: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;

    let Some(first) = root.components().next() else {
        return false;
    };
    let mut vol_str = first.as_os_str().to_string_lossy().into_owned();
    if !vol_str.ends_with('\\') {
        vol_str.push('\\');
    }
    let wide: Vec<u16> = std::ffi::OsStr::new(&vol_str)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut fs_name = [0u16; 32];
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return false;
    }
    let end = fs_name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name.len());
    String::from_utf16_lossy(&fs_name[..end]).eq_ignore_ascii_case("NTFS")
}
