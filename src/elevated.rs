//! Guard for elevated runs: pin the data directory chain and check the fixed files.
//!
//! An elevated token reaches the user's data directory, which unelevated code running as the same
//! user can change. When [`crate::volumes::is_elevated`] is true, [`enter`] opens every folder from
//! the data root down to `logs` without `FILE_SHARE_DELETE`, so none of them can be renamed,
//! deleted or turned into a junction while the run lasts. It refuses (exit 4) on a reparse point,
//! on a final path that differs from the expected one, and on a fixed file that is a reparse point
//! or has more than one hard link. It never falls back to another directory. The design is in
//! `docs/elevated-writes.md`.
//!
//! On other systems this module does nothing.

use crate::exit::Result;
use crate::paths::{PathEnvironment, Paths};
use std::path::PathBuf;

/// Handles that keep the checked directories in place. Keep the value alive until the process
/// exits; dropping it releases the pins.
pub struct PinnedDirs {
    active: bool,
    #[cfg(windows)]
    handles: Vec<std::os::windows::io::OwnedHandle>,
}

impl PinnedDirs {
    fn inactive() -> Self {
        Self {
            active: false,
            #[cfg(windows)]
            handles: Vec::new(),
        }
    }

    /// True when the guard ran, so the ledger should be opened with the defensive settings.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Number of directories held open.
    pub fn pinned_count(&self) -> usize {
        #[cfg(windows)]
        {
            self.handles.len()
        }
        #[cfg(not(windows))]
        {
            0
        }
    }
}

/// The directories to pin, outermost first: the data root chain, then `locks` and `logs`.
///
/// With `ARKOS_HOME` the chain starts at that folder. Otherwise on Windows it starts at
/// `%LOCALAPPDATA%` and goes down through `mariyaholic` and `shimatsu`.
pub fn data_chain(paths: &Paths, env: &PathEnvironment) -> Vec<PathBuf> {
    let explicit = env.vars.get("ARKOS_HOME").is_some_and(|v| !v.is_empty());
    let local = env
        .vars
        .get("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let mut chain = match local {
        Some(local) if env.windows && !explicit => {
            let mariyaholic = local.join("mariyaholic");
            let shimatsu = mariyaholic.join("shimatsu");
            vec![local, mariyaholic, shimatsu]
        }
        _ => vec![paths.home.clone()],
    };
    chain.push(paths.locks.clone());
    chain.push(paths.logs.clone());
    chain
}

/// Run the guard when the process is elevated; otherwise do nothing and return an inactive value.
pub fn enter(paths: &Paths, chain: &[PathBuf]) -> Result<PinnedDirs> {
    if !crate::volumes::is_elevated() {
        return Ok(PinnedDirs::inactive());
    }
    enter_forced(paths, chain)
}

/// Run the guard whatever the token is. Used by tests to exercise the Windows code paths without
/// an elevated prompt. On other systems it does nothing.
#[doc(hidden)]
pub fn enter_forced(paths: &Paths, chain: &[PathBuf]) -> Result<PinnedDirs> {
    #[cfg(windows)]
    {
        win::enter(paths, chain)
    }
    #[cfg(not(windows))]
    {
        let _ = (paths, chain);
        Ok(PinnedDirs::inactive())
    }
}

/// Bring a path to a comparable form: no `\\?\` or `\\?\UNC\` prefix, backslashes only, no `.`
/// or empty parts, `..` applied, no trailing separator, lower case.
#[cfg(any(windows, test))]
fn normalize_for_compare(path: &str) -> String {
    let path = path.replace('/', "\\");
    let (prefix, rest) = if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        (r"\\", unc.to_owned())
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        ("", rest.to_owned())
    } else if let Some(rest) = path.strip_prefix(r"\\") {
        (r"\\", rest.to_owned())
    } else {
        ("", path.clone())
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("{prefix}{}", parts.join("\\")).to_lowercase()
}

/// True when `final_path` (from `GetFinalPathNameByHandleW`) names the same place as `expected`,
/// ignoring case, the `\\?\` prefix and a trailing separator.
#[cfg(any(windows, test))]
fn final_path_matches(expected: &str, final_path: &str) -> bool {
    normalize_for_compare(expected) == normalize_for_compare(final_path)
}

#[cfg(windows)]
mod win {
    use super::{final_path_matches, PinnedDirs};
    use crate::exit::{Error, Result};
    use crate::paths::Paths;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateDirectoryW, CreateFileW, FileAttributeTagInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx, GetFinalPathNameByHandleW, GetLongPathNameW,
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING, VOLUME_NAME_DOS,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn io(context: &'static str, source: std::io::Error) -> Error {
        Error::Io { context, source }
    }

    pub(super) fn enter(paths: &Paths, chain: &[PathBuf]) -> Result<PinnedDirs> {
        let mut handles = Vec::with_capacity(chain.len());
        for dir in chain {
            handles.push(pin_dir(dir)?);
        }
        check_fixed_files(paths)?;
        Ok(PinnedDirs {
            active: true,
            handles,
        })
    }

    /// Create `dir` if it is missing, open it without `FILE_SHARE_DELETE` and check it.
    fn pin_dir(dir: &Path) -> Result<OwnedHandle> {
        let name = wide(dir);
        // SAFETY: `name` is a NUL-terminated wide string that outlives the call.
        let created = unsafe { CreateDirectoryW(name.as_ptr(), std::ptr::null()) };
        if created == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
                return Err(io("create arkos directory", err));
            }
        }
        // SAFETY: as above. No `FILE_SHARE_DELETE`: that is what pins the folder.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io("pin arkos directory", std::io::Error::last_os_error()));
        }
        // SAFETY: `raw` is a valid handle that nothing else owns.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let attrs = tag_attributes(&handle)?;
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(Error::Refused(format!(
                "{} is a reparse point (junction or symbolic link); elevated runs refuse to write through one. Remove it, or run Arkos from a prompt that is not elevated",
                dir.display()
            )));
        }
        if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(Error::Refused(format!(
                "{} is not a folder; elevated runs need a real folder there",
                dir.display()
            )));
        }
        let resolved = final_path(&handle)?;
        // A short (8.3) name in the configured path is not a redirect, so compare the long form
        // too. A `subst` or mapped drive still differs and is refused.
        let expected = dir.to_string_lossy();
        let matches = final_path_matches(&expected, &resolved)
            || long_path(dir).is_some_and(|long| final_path_matches(&long, &resolved));
        if !matches {
            return Err(Error::Refused(format!(
                "{} resolves to {resolved}; elevated runs refuse to write through a redirected folder",
                dir.display()
            )));
        }
        Ok(handle)
    }

    /// The long form of `dir` (8.3 parts expanded), or `None` when the call fails.
    fn long_path(dir: &Path) -> Option<String> {
        let name = wide(dir);
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: `name` is NUL-terminated and `buf` holds `buf.len()` elements.
            let n = unsafe { GetLongPathNameW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
                as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                return Some(String::from_utf16_lossy(&buf[..n]));
            }
            buf.resize(n + 1, 0);
        }
    }

    fn tag_attributes(handle: &OwnedHandle) -> Result<u32> {
        use std::os::windows::io::AsRawHandle;
        let mut info = FILE_ATTRIBUTE_TAG_INFO {
            FileAttributes: 0,
            ReparseTag: 0,
        };
        // SAFETY: `info` is a valid FILE_ATTRIBUTE_TAG_INFO of the size passed.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle.as_raw_handle(),
                FileAttributeTagInfo,
                (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io(
                "read directory attributes",
                std::io::Error::last_os_error(),
            ));
        }
        Ok(info.FileAttributes)
    }

    fn final_path(handle: &OwnedHandle) -> Result<String> {
        use std::os::windows::io::AsRawHandle;
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: `buf` holds `buf.len()` writable u16 values.
            let n = unsafe {
                GetFinalPathNameByHandleW(
                    handle.as_raw_handle(),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
                )
            };
            if n == 0 {
                return Err(io(
                    "resolve directory path",
                    std::io::Error::last_os_error(),
                ));
            }
            if (n as usize) < buf.len() {
                return Ok(String::from_utf16_lossy(&buf[..n as usize]));
            }
            buf.resize(n as usize + 1, 0);
        }
    }

    /// Refuse when an existing fixed file is a reparse point or has more than one hard link.
    fn check_file(path: &Path) -> Result<()> {
        let name = wide(path);
        // SAFETY: `name` is a NUL-terminated wide string. Attribute-only access takes no part in
        // sharing checks, and the handle is closed at the end of this function.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            let err = std::io::Error::last_os_error();
            return match err.raw_os_error() {
                Some(c) if c == ERROR_FILE_NOT_FOUND as i32 || c == ERROR_PATH_NOT_FOUND as i32 => {
                    Ok(())
                }
                _ => Err(io("check arkos file", err)),
            };
        }
        // SAFETY: `raw` is a valid handle that nothing else owns.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        use std::os::windows::io::AsRawHandle;
        // SAFETY: BY_HANDLE_FILE_INFORMATION is plain integers, so all-zero is valid.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid out value for this call.
        if unsafe { GetFileInformationByHandle(handle.as_raw_handle(), &mut info) } == 0 {
            return Err(io("check arkos file", std::io::Error::last_os_error()));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(Error::Refused(format!(
                "{} is a reparse point (symbolic link or similar); elevated runs refuse to write through one",
                path.display()
            )));
        }
        if info.nNumberOfLinks > 1 {
            return Err(Error::Refused(format!(
                "{} has {} hard links; elevated runs refuse a file that is also reachable by another name",
                path.display(),
                info.nNumberOfLinks
            )));
        }
        Ok(())
    }

    /// The files Arkos opens by a fixed name: the ledger and its sidecars, the log and its three
    /// rotated copies, and every file in `locks` (volume locks and their `.hb` sidecars, named by
    /// `lock::lock_path_for` and `lock::heartbeat_path`).
    fn check_fixed_files(paths: &Paths) -> Result<()> {
        let mut files: Vec<PathBuf> = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let mut name = paths.db.as_os_str().to_owned();
            name.push(suffix);
            files.push(PathBuf::from(name));
        }
        let log = paths.logs.join("arkos.log");
        files.push(log.clone());
        for idx in 1..=3 {
            files.push(log.with_extension(format!("log.{idx}")));
        }
        match std::fs::read_dir(&paths.locks) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|e| io("list locks directory", e))?;
                    files.push(entry.path());
                }
            }
            Err(e) => return Err(io("list locks directory", e)),
        }
        for file in files {
            check_file(&file)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn matches_ignoring_case_prefix_and_trailing_separator() {
        assert!(final_path_matches(
            r"C:\Users\Me\Data",
            r"\\?\c:\users\me\DATA"
        ));
        assert!(final_path_matches(
            r"C:\Users\Me\Data\",
            r"\\?\C:\Users\Me\Data"
        ));
        assert!(final_path_matches(
            r"C:/Users/Me/Data",
            r"\\?\C:\Users\Me\Data\"
        ));
        assert!(final_path_matches(
            r"C:\Users\Me\.\Data",
            r"C:\Users\Me\Data"
        ));
        assert!(final_path_matches(
            r"C:\Users\x\..\Me\Data",
            r"\\?\C:\Users\Me\Data"
        ));
        assert!(final_path_matches(r"C:\", r"\\?\C:\"));
    }

    #[test]
    fn unc_paths_compare_with_and_without_the_prefix() {
        assert!(final_path_matches(
            r"\\host\share\dir",
            r"\\?\UNC\HOST\share\dir"
        ));
    }

    #[test]
    fn different_places_do_not_match() {
        assert!(!final_path_matches(
            r"C:\Users\Me\Data",
            r"\\?\C:\Windows\System32"
        ));
        assert!(!final_path_matches(
            r"C:\Users\Me\Data",
            r"\\?\D:\Users\Me\Data"
        ));
        assert!(!final_path_matches(r"C:\a\b", r"\\?\C:\a\b\c"));
        assert!(!final_path_matches(r"\\host\share", r"\\?\C:\host\share"));
    }

    fn env_with(vars: &[(&str, &Path)]) -> PathEnvironment {
        PathEnvironment {
            windows: true,
            vars: vars
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.as_os_str().to_owned()))
                .collect(),
        }
    }

    #[test]
    fn chain_starts_at_arkos_home_when_set() {
        let base = std::env::temp_dir();
        let (home, local) = (base.join("h"), base.join("l"));
        let env = env_with(&[("ARKOS_HOME", &home), ("LOCALAPPDATA", &local)]);
        let paths = Paths::resolve(&env).unwrap();
        assert_eq!(
            data_chain(&paths, &env),
            vec![home.clone(), home.join("locks"), home.join("logs")]
        );
    }

    #[test]
    fn chain_goes_down_from_local_app_data() {
        let local = std::env::temp_dir().join("l");
        let env = env_with(&[("LOCALAPPDATA", &local)]);
        let paths = Paths::resolve(&env).unwrap();
        let shimatsu = local.join("mariyaholic").join("shimatsu");
        assert_eq!(
            data_chain(&paths, &env),
            vec![
                local.clone(),
                local.join("mariyaholic"),
                shimatsu.clone(),
                shimatsu.join("locks"),
                shimatsu.join("logs"),
            ]
        );
    }
}
