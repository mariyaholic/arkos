//! Per-volume advisory lock file under `<home>/locks/volume-<hash12>.lock`.

use crate::exit::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Most times `acquire` reopens the lock file after finding that it was replaced under the lock.
const MAX_IDENTITY_RETRIES: usize = 5;

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
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut file = None;
        // A holder of an unlinked lock file does not conflict with a new file at the same path.
        // After each successful lock, check that the handle still is the file at the path.
        for _ in 0..MAX_IDENTITY_RETRIES {
            let candidate = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&path)
                .map_err(|source| Error::Io {
                    context: "open volume lock file",
                    source,
                })?;
            loop {
                match try_lock_exclusive(&candidate) {
                    Ok(true) => break,
                    Ok(false) => {
                        if Instant::now() >= deadline {
                            return Err(Error::Busy(busy_message(&path, volume, timeout_ms)));
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
            if same_file_as_path(&candidate, &path) {
                file = Some(candidate);
                break;
            }
            let _ = unlock_exclusive(&candidate);
        }
        let Some(mut file) = file else {
            return Err(Error::Busy(format!(
                "volume lock for {volume} kept changing identity; the lock file was replaced {MAX_IDENTITY_RETRIES} times"
            )));
        };

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

        let mut heartbeat = Heartbeat::new(&path);
        heartbeat.beat_now(0);
        Ok(Self { file, path })
    }

    /// A heartbeat writer for the scan that holds this lock. The sidecar is removed when the
    /// lock is dropped.
    pub fn heartbeat(&self) -> Heartbeat {
        Heartbeat::new(&self.path)
    }
}

/// A scan whose last beat is older than this many seconds counts as stalled.
pub const STALL_SECS: i64 = 120;

/// Smallest gap between two beat writes.
const BEAT_INTERVAL: Duration = Duration::from_secs(5);

/// Sidecar path `<lock file>.hb` next to a lock file.
pub fn heartbeat_path(lock_path: &Path) -> PathBuf {
    let mut name = lock_path.as_os_str().to_owned();
    name.push(".hb");
    PathBuf::from(name)
}

/// What the sidecar says about the scan that holds a lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatInfo {
    pub pid: u32,
    pub started: i64,
    pub beat: i64,
    pub files: u64,
}

impl HeartbeatInfo {
    /// Seconds since the last beat when that is past [`STALL_SECS`], else `None`.
    pub fn stalled_for(&self, now: i64) -> Option<i64> {
        let age = now - self.beat;
        (age > STALL_SECS).then_some(age)
    }

    fn parse(text: &str) -> Option<Self> {
        let (mut pid, mut started, mut beat, mut files) = (None, None, None, None);
        for part in text.split_whitespace() {
            let (k, v) = part.split_once('=')?;
            match k {
                "pid" => pid = v.parse().ok(),
                "started" => started = v.parse().ok(),
                "beat" => beat = v.parse().ok(),
                "files" => files = v.parse().ok(),
                _ => {}
            }
        }
        Some(Self {
            pid: pid?,
            started: started?,
            beat: beat?,
            files: files?,
        })
    }
}

/// Read the sidecar for the lock file at `lock_path`. A missing, partial or unparsable file
/// reads as `None` ("unknown"). Only trust the result while the lock is held.
pub fn read_heartbeat(lock_path: &Path) -> Option<HeartbeatInfo> {
    let text = std::fs::read_to_string(heartbeat_path(lock_path)).ok()?;
    HeartbeatInfo::parse(&text)
}

/// Heartbeat for a volume as `status` sees it, from the locks directory.
pub fn read_heartbeat_for(locks_dir: &Path, volume: &str) -> Option<HeartbeatInfo> {
    read_heartbeat(&lock_path_for(locks_dir, volume))
}

/// Writes the sidecar from the scan's own loop, at most every 5 seconds. There is no timer
/// thread: a thread would keep beating while the walk is stuck.
#[derive(Debug)]
pub struct Heartbeat {
    path: PathBuf,
    started: i64,
    last: Option<Instant>,
}

impl Heartbeat {
    fn new(lock_path: &Path) -> Self {
        Self {
            path: heartbeat_path(lock_path),
            started: chrono::Utc::now().timestamp(),
            last: None,
        }
    }

    /// Write a beat when 5 seconds have passed since the last one.
    pub fn beat(&mut self, files: u64) {
        if let Some(last) = self.last {
            if last.elapsed() < BEAT_INTERVAL {
                return;
            }
        }
        self.beat_now(files);
    }

    fn beat_now(&mut self, files: u64) {
        self.last = Some(Instant::now());
        let line = format!(
            "pid={} started={} beat={} files={}\n",
            std::process::id(),
            self.started,
            chrono::Utc::now().timestamp(),
            files
        );
        // Rewritten in place; a reader that catches it half written treats it as unknown.
        let _ = std::fs::write(&self.path, line);
    }
}

fn busy_message(path: &Path, volume: &str, timeout_ms: u64) -> String {
    let mut msg = format!("volume lock for {volume} is held past {timeout_ms}ms");
    if let Some(hb) = read_heartbeat(path) {
        msg.push_str(&format!(" by pid {}", hb.pid));
        if let Some(age) = hb.stalled_for(chrono::Utc::now().timestamp()) {
            msg.push_str(&format!(
                "; that scan has not progressed for {age}s and looks stalled; end process {} if it is hung",
                hb.pid
            ));
        }
    } else {
        let holder = std::fs::read_to_string(path).unwrap_or_default();
        if !holder.trim().is_empty() {
            msg.push_str(&format!(": {}", holder.trim()));
        }
    }
    msg
}

impl VolumeLock {
    /// True when nobody holds the lock for `volume`. Tries the lock without waiting and releases
    /// it at once; it creates nothing and never writes to the lock file.
    pub fn is_free(locks_dir: &Path, volume: &str) -> Result<bool> {
        let path = lock_path_for(locks_dir, volume);
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => f,
            // No lock file means no scan ever took this lock.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(source) => {
                return Err(Error::Io {
                    context: "open volume lock file",
                    source,
                })
            }
        };
        match try_lock_exclusive(&file) {
            Ok(true) => {
                let _ = unlock_exclusive(&file);
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(source) => Err(Error::Io {
                context: "lock volume file",
                source,
            }),
        }
    }
}

impl Drop for VolumeLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(heartbeat_path(&self.path));
        let _ = unlock_exclusive(&self.file);
    }
}

/// Compute the deterministic lock file path `volume-<hex12>.lock` for a volume identifier.
pub fn lock_path_for(locks_dir: &Path, volume: &str) -> PathBuf {
    let hex = fnv64_hex(&lock_key(volume));
    locks_dir.join(format!("volume-{}.lock", &hex[..12]))
}

/// Case-fold drive letters and volume GUID paths, which Windows treats as case-insensitive. A
/// `dir:` key folds only on Windows; on Unix `/data/A` and `/data/a` are different directories.
fn lock_key(volume: &str) -> String {
    let is_drive = volume.len() == 2 && volume.ends_with(':');
    let is_guid = volume.starts_with(r"\\?\Volume{");
    if is_drive || is_guid || (cfg!(windows) && volume.starts_with("dir:")) {
        volume.to_ascii_uppercase()
    } else {
        volume.to_owned()
    }
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

/// True when the open handle and the file now at `path` are the same file.
#[cfg(not(windows))]
fn same_file_as_path(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(windows)]
fn same_file_as_path(file: &File, path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    // Share read, write and delete so this open never blocks another holder or a delete.
    const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    let Ok(fresh) = OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .open(path)
    else {
        return false;
    };
    match (file_identity(file), file_identity(&fresh)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Volume serial number and file index from `GetFileInformationByHandle`.
#[cfg(windows)]
fn file_identity(file: &File) -> Option<(u32, u32, u32)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
    (ok != 0).then_some((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}
