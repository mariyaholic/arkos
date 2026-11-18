//! Elevation-gated NTFS MFT accelerator (§5.2) with explicit refusal / unsupported exits.

use crate::exit::{Error, Result};
use crate::progress::Reporter;
use crate::scan::paths::normalize_display_path;
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

/// The drive letter when `root` is a volume root such as `C:\`, `None` for any subtree.
///
/// USN enumeration covers a whole volume and cannot be scoped to a folder, so only a volume root
/// can use it.
pub fn volume_root_letter(root: &Path) -> Option<char> {
    let s = normalize_display_path(root);
    let mut chars = s.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next() != Some(':') {
        return None;
    }
    match chars.as_str() {
        "" | "\\" | "/" => Some(letter.to_ascii_uppercase()),
        _ => None,
    }
}

/// Run an elevated NTFS MFT pass when available, and say which path ran.
///
/// On a volume root the two-phase USN reader runs and the outcome is `mft`. On a subtree, or when
/// the volume cannot be opened, or when the directory map outgrows its limit, the walk runs
/// instead and the outcome is `walk` with a note for the log. The snapshot records the mode that
/// ran, so `walk` here is the correct and complete answer, only slower.
///
/// With `explicit` set (`--mode mft`) the fallbacks for an unopenable volume and for too many
/// directories become `Error::Unsupported`; only `--mode auto` falls back to the walk there. A
/// subtree root still walks, since USN enumeration cannot be scoped to a folder.
pub fn mft_into_snapshot(
    conn: &mut Connection,
    snapshot_id: i64,
    root: &Path,
    filter: ScanFilter<'_>,
    progress: &mut Reporter,
    explicit: bool,
) -> Result<ScanOutcome> {
    ensure_mft_ready(root)?;
    #[cfg(not(all(windows, feature = "mft")))]
    let _ = explicit;
    #[cfg(all(windows, feature = "mft"))]
    let note: Option<String> = {
        use crate::scan::usn::{scan_with_source, UsnScan, DIR_LIMIT};
        match volume_root_letter(root) {
            None => Some("mft needs a volume root; walked the subtree".to_owned()),
            Some(letter) => {
                match win::Volume::open(letter).and_then(|v| win::root_frn(root).map(|f| (v, f))) {
                    Ok((volume, frn)) => {
                        match scan_with_source(
                            conn,
                            snapshot_id,
                            root,
                            frn,
                            &volume,
                            filter,
                            progress,
                            DIR_LIMIT,
                        )? {
                            UsnScan::Done(done) => {
                                let (stats, diag) = *done;
                                return Ok(ScanOutcome {
                                    stats,
                                    mode: "mft",
                                    note: Some(diag.to_string()),
                                })
                            }
                            UsnScan::TooManyDirs if explicit => {
                                return Err(Error::Unsupported(format!(
                                    "mft-slow: more than {DIR_LIMIT} directories; use --mode auto to walk instead"
                                )))
                            }
                            UsnScan::TooManyDirs => Some(format!(
                                "mft-slow: more than {DIR_LIMIT} directories; walked instead"
                            )),
                        }
                    }
                    Err(e) if explicit => {
                        return Err(Error::Unsupported(format!(
                            "mft-unavailable: cannot open the volume ({e}); use --mode auto to walk instead"
                        )))
                    }
                    Err(e) => Some(format!("mft-unavailable: {e}; walked instead")),
                }
            }
        }
    };
    #[cfg(not(all(windows, feature = "mft")))]
    let note: Option<String> = None;
    let stats = walk_into_snapshot(conn, snapshot_id, root, filter, progress)?;
    Ok(ScanOutcome {
        stats,
        mode: "walk",
        note,
    })
}

/// Windows handle calls behind [`UsnSource`](crate::scan::usn::UsnSource). Type-checked in CI on
/// every platform's cross target, and run only on an elevated Windows host.
#[cfg(all(windows, feature = "mft"))]
mod win {
    use crate::scan::usn::{filetime_to_unix, parse_usn_buffer, FileStat, UsnRecord, UsnSource};
    use std::ffi::c_void;
    use std::io;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_HANDLE_EOF, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileIdType, GetFileInformationByHandle, OpenFileById,
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR,
        FILE_ID_DESCRIPTOR_0, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Ioctl::{FSCTL_ENUM_USN_DATA, MFT_ENUM_DATA_V0};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    /// Output buffer for one `FSCTL_ENUM_USN_DATA` call.
    const BUF_BYTES: usize = 64 * 1024;
    const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error() -> io::Error {
        io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
    }

    fn filetime(ft: &windows_sys::Win32::Foundation::FILETIME) -> i64 {
        filetime_to_unix(((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64)
    }

    /// An open `\\.\X:` volume handle, closed on drop.
    pub struct Volume {
        handle: HANDLE,
    }

    impl Volume {
        pub fn open(letter: char) -> io::Result<Self> {
            let path = wide(&format!("\\\\.\\{letter}:"));
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(last_error());
            }
            Ok(Self { handle })
        }
    }

    impl Drop for Volume {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.handle) };
        }
    }

    /// File reference number of the volume root directory, as USN records report it.
    pub fn root_frn(root: &Path) -> io::Result<u64> {
        let path = wide(&root.to_string_lossy());
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                FILE_READ_ATTRIBUTES,
                SHARE_ALL,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        let err = (ok == 0).then(last_error);
        unsafe { CloseHandle(handle) };
        match err {
            Some(e) => Err(e),
            None => Ok(((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64),
        }
    }

    impl UsnSource for Volume {
        fn enumerate(&self, f: &mut dyn FnMut(UsnRecord) -> bool) -> io::Result<()> {
            let mut input = MFT_ENUM_DATA_V0 {
                StartFileReferenceNumber: 0,
                LowUsn: 0,
                HighUsn: i64::MAX,
            };
            // u64 elements keep the buffer 8-byte aligned, as the records expect.
            let mut buf = vec![0u64; BUF_BYTES / 8];
            loop {
                let mut returned = 0u32;
                let ok = unsafe {
                    DeviceIoControl(
                        self.handle,
                        FSCTL_ENUM_USN_DATA,
                        &input as *const MFT_ENUM_DATA_V0 as *const c_void,
                        std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32,
                        buf.as_mut_ptr() as *mut c_void,
                        BUF_BYTES as u32,
                        &mut returned,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    let code = unsafe { GetLastError() };
                    if code == ERROR_HANDLE_EOF {
                        return Ok(());
                    }
                    return Err(io::Error::from_raw_os_error(code as i32));
                }
                let bytes = unsafe {
                    std::slice::from_raw_parts(buf.as_ptr() as *const u8, returned as usize)
                };
                let (next, stopped) = parse_usn_buffer(bytes, f)?;
                if stopped {
                    return Ok(());
                }
                input.StartFileReferenceNumber = next;
            }
        }

        fn stat(&self, frn: u64) -> io::Result<FileStat> {
            let id = FILE_ID_DESCRIPTOR {
                dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
                Type: FileIdType,
                Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 },
            };
            let handle = unsafe {
                OpenFileById(
                    self.handle,
                    &id,
                    FILE_READ_ATTRIBUTES,
                    SHARE_ALL,
                    std::ptr::null(),
                    FILE_FLAG_BACKUP_SEMANTICS,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(last_error());
            }
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
            let err = (ok == 0).then(last_error);
            unsafe { CloseHandle(handle) };
            if let Some(e) = err {
                return Err(e);
            }
            Ok(FileStat {
                size: (((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64) as i64,
                mtime: filetime(&info.ftLastWriteTime),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_volume_root_can_use_usn() {
        assert_eq!(volume_root_letter(Path::new("C:\\")), Some('C'));
        assert_eq!(volume_root_letter(Path::new("d:")), Some('D'));
        assert_eq!(volume_root_letter(Path::new("C:\\Users")), None);
        assert_eq!(volume_root_letter(Path::new("/tmp")), None);
        assert_eq!(volume_root_letter(Path::new("\\\\server\\share")), None);
    }

    #[test]
    fn readiness_table() {
        assert!(check_ready(true, true, true).is_ok());
        assert!(matches!(
            check_ready(false, true, true),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            check_ready(true, false, true),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            check_ready(true, true, false),
            Err(Error::Refused(_))
        ));
    }
}
