//! Per-volume advisory lock file under `<home>/locks/volume-<hash12>.lock`.

use crate::exit::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Advisory volume lock held for the lifetime of a mutating scan.
#[derive(Debug)]
pub struct VolumeLock {
    file: File,
    pub path: PathBuf,
}

impl VolumeLock {
    /// Acquire the exclusive advisory lock for `volume`, waiting up to `timeout_ms` before returning `Busy`.
    pub fn acquire(locks_dir: &Path, volume: &str, timeout_ms: u64) -> Result<Self> {
        std::fs::create_dir_all(locks_dir).map_err(|source| Error::Io {
            context: "create locks directory",
            source,
        })?;
        let path = lock_path_for(locks_dir, volume);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| Error::Io {
                context: "open volume lock file",
                source,
            })?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            match try_lock_exclusive(&file) {
                Ok(true) => break,
                Ok(false) => {
                    if Instant::now() >= deadline {
                        let holder = std::fs::read_to_string(&path).unwrap_or_default();
                        let suffix = if holder.trim().is_empty() {
                            String::new()
                        } else {
                            format!(": {}", holder.trim())
                        };
                        return Err(Error::Busy(format!(
                            "volume lock for {volume} is held past {timeout_ms}ms{suffix}"
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(source) => {
                    return Err(Error::Io {
                        context: "lock volume file",
                        source,
                    })
                }
            }
        }

        // Write diagnostic holder metadata to offset 0 (advisory lock covers a high byte range on Windows).
        let diag = serde_json::json!({
            "holder": "arkos",
            "pid": std::process::id(),
            "volume": volume,
            "started_at": chrono::Utc::now().timestamp(),
        });
        let _ = file.set_len(0);
        let _ = file.seek(SeekFrom::Start(0));
        let _ = writeln!(file, "{diag}");
        let _ = file.flush();

        Ok(Self { file, path })
    }
}

impl Drop for VolumeLock {
    fn drop(&mut self) {
        let _ = unlock_exclusive(&self.file);
    }
}

/// Compute the deterministic lock file path `volume-<hex12>.lock` for a volume identifier.
pub fn lock_path_for(locks_dir: &Path, volume: &str) -> PathBuf {
    let hex = fnv64_hex(&volume.to_ascii_uppercase());
    locks_dir.join(format!("volume-{}.lock", &hex[..12]))
}

fn fnv64_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(windows)]
fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // Lock byte at offset 0x4000_0000 so offset 0 stays readable/writable by diagnostic inspection.
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.Anonymous.Anonymous.Offset = 0x4000_0000;
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle() as HANDLE,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Ok(false)
    } else {
        Err(err)
    }
}

#[cfg(windows)]
fn unlock_exclusive(file: &File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.Anonymous.Anonymous.Offset = 0x4000_0000;
    let ok = unsafe { UnlockFileEx(file.as_raw_handle() as HANDLE, 0, 1, 0, &mut overlapped) };
    if ok != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

// On Unix, std's `File::try_lock` takes `flock(LOCK_EX | LOCK_NB)` on the whole file. The lock
// belongs to the open file description, so a second `open` of the same lock file conflicts even
// inside one process, and the kernel releases it when the descriptor closes.
#[cfg(not(windows))]
fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(err)) => Err(err),
    }
}

#[cfg(not(windows))]
fn unlock_exclusive(file: &File) -> std::io::Result<()> {
    file.unlock()
}
