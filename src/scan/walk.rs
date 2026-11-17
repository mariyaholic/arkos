//! Bounded-worker iterative directory walker streaming batches into SQLite.

use crate::db::map_db_err;
use crate::exit::{Error, Result};
use crate::progress::Reporter;
use crate::scan::paths::{
    matches_any_exclude, normalize_display_path, normalize_ext, to_os_open_path,
};
use rusqlite::{params, Connection};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::UNIX_EPOCH;

const WORKER_COUNT: usize = 3;
pub(crate) const BATCH_COMMIT_ENTRIES: usize = 1_000;

/// Most non-Unicode names kept for the scan log; the rest are only counted.
pub(crate) const MAX_LOGGED_NAMES: usize = 50;

/// Summary returned after streaming scan rows for a snapshot.
#[derive(Debug, Default, Clone)]
pub struct WalkStats {
    pub dirs: u64,
    pub files: u64,
    pub bytes: u64,
    pub denied: u64,
    pub skipped: u64,
    /// Lossy forms of entries skipped because their names are not valid Unicode (capped).
    pub non_unicode: Vec<String>,
}

/// What the walk leaves out: name or path globs, and exact directories.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanFilter<'a> {
    pub globs: &'a [String],
    pub exact_dirs: &'a [PathBuf],
}

impl ScanFilter<'_> {
    pub(crate) fn excludes_exact(&self, path: &Path) -> bool {
        self.exact_dirs.iter().any(|p| same_path(p, path))
    }
}

#[cfg(windows)]
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

#[cfg(not(windows))]
fn same_path(a: &Path, b: &Path) -> bool {
    a == b
}

#[derive(Debug)]
struct WorkItem {
    token: u64,
    parent_token: Option<u64>,
    path: PathBuf,
    name: String,
    depth: i32,
}

#[derive(Debug)]
struct FileEntry {
    name: String,
    size: i64,
    mtime: i64,
    ext: Option<String>,
}

#[derive(Debug)]
enum WriterMsg {
    DirBatch {
        token: u64,
        parent_token: Option<u64>,
        path: String,
        name: String,
        depth: i32,
        mtime: Option<i64>,
        files: Vec<FileEntry>,
    },
    Error(Error),
}

struct WorkQueue {
    queue: Mutex<VecDeque<WorkItem>>,
    active: AtomicUsize,
    done: AtomicBool,
    cv: Condvar,
}

impl WorkQueue {
    fn new(root: WorkItem) -> Self {
        let mut q = VecDeque::new();
        q.push_back(root);
        Self {
            queue: Mutex::new(q),
            active: AtomicUsize::new(0),
            done: AtomicBool::new(false),
            cv: Condvar::new(),
        }
    }

    fn push(&self, item: WorkItem) {
        let mut guard = self.queue.lock().expect("work queue lock");
        guard.push_back(item);
        self.cv.notify_one();
    }

    fn pop(&self) -> Option<WorkItem> {
        let mut guard = self.queue.lock().expect("work queue lock");
        loop {
            if let Some(item) = guard.pop_front() {
                self.active.fetch_add(1, Ordering::AcqRel);
                return Some(item);
            }
            if self.done.load(Ordering::Acquire) || self.active.load(Ordering::Acquire) == 0 {
                self.done.store(true, Ordering::Release);
                self.cv.notify_all();
                return None;
            }
            guard = self.cv.wait(guard).expect("work queue wait");
        }
    }

    fn finish_one(&self) {
        let prev = self.active.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            let guard = self.queue.lock().expect("work queue lock");
            if guard.is_empty() {
                self.done.store(true, Ordering::Release);
                self.cv.notify_all();
            }
        }
    }
}

/// Walk `root` using 3 bounded worker threads and stream entries into `conn` under `snapshot_id`.
pub fn walk_into_snapshot(
    conn: &mut Connection,
    snapshot_id: i64,
    root: &Path,
    filter: ScanFilter<'_>,
    progress: &mut Reporter,
) -> Result<WalkStats> {
    let root_meta = fs::symlink_metadata(to_os_open_path(root)).map_err(|source| Error::Io {
        context: "stat scan root",
        source,
    })?;
    let root_display = normalize_display_path(root);
    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root_display.clone());
    let _ = root_meta;

    let queue = Arc::new(WorkQueue::new(WorkItem {
        token: 1,
        parent_token: None,
        path: root.to_path_buf(),
        name: root_name,
        depth: 0,
    }));
    let next_token = Arc::new(AtomicU64::new(2));
    let denied = Arc::new(AtomicU64::new(0));
    let skipped = Arc::new(AtomicU64::new(0));
    let non_unicode = Arc::new(Mutex::new(Vec::<String>::new()));
    let (tx, rx) = mpsc::sync_channel::<WriterMsg>(64);

    let mut stats = WalkStats::default();
    let mut writer_err: Option<Error> = None;

    std::thread::scope(|scope| {
        for _ in 0..WORKER_COUNT {
            let q = Arc::clone(&queue);
            let nt = Arc::clone(&next_token);
            let den = Arc::clone(&denied);
            let skp = Arc::clone(&skipped);
            let nu = Arc::clone(&non_unicode);
            let wtx = tx.clone();
            scope.spawn(move || {
                while let Some(item) = q.pop() {
                    process_dir(&item, filter, &q, &nt, &den, &skp, &nu, &wtx);
                    q.finish_one();
                }
            });
        }
        drop(tx);

        match consume_and_insert(conn, snapshot_id, rx, progress) {
            Ok((dirs, files, bytes)) => {
                stats.dirs = dirs;
                stats.files = files;
                stats.bytes = bytes;
            }
            Err(e) => {
                queue.done.store(true, Ordering::Release);
                queue.cv.notify_all();
                writer_err = Some(e);
            }
        }
    });

    if let Some(err) = writer_err {
        return Err(err);
    }

    stats.denied = denied.load(Ordering::Acquire);
    stats.skipped = skipped.load(Ordering::Acquire);
    stats.non_unicode = non_unicode.lock().expect("non-unicode list").clone();
    stats.non_unicode.sort();
    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
fn process_dir(
    item: &WorkItem,
    filter: ScanFilter<'_>,
    queue: &WorkQueue,
    next_token: &AtomicU64,
    denied: &AtomicU64,
    skipped: &AtomicU64,
    non_unicode: &Mutex<Vec<String>>,
    tx: &mpsc::SyncSender<WriterMsg>,
) {
    let open_path = to_os_open_path(&item.path);
    let mtime = fs::symlink_metadata(&open_path)
        .ok()
        .and_then(|m| modified_epoch(&m));
    let read_dir = match fs::read_dir(&open_path) {
        Ok(rd) => rd,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
            ) =>
        {
            denied.fetch_add(1, Ordering::Relaxed);
            // Still record the directory node itself so hierarchy stays valid if parent exists.
            let _ = tx.send(WriterMsg::DirBatch {
                token: item.token,
                parent_token: item.parent_token,
                path: normalize_display_path(&item.path),
                name: item.name.clone(),
                depth: item.depth,
                mtime,
                files: Vec::new(),
            });
            return;
        }
        Err(source) => {
            let _ = tx.send(WriterMsg::Error(Error::Io {
                context: "read directory",
                source,
            }));
            return;
        }
    };

    let mut files = Vec::new();
    for entry_res in read_dir {
        let entry = match entry_res {
            Ok(e) => e,
            Err(_) => {
                denied.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        // A name that is not valid Unicode cannot be stored faithfully, and a lossy form could
        // collide with a real name under the (snapshot_id, dir_id, name) key. Skip and count it.
        let entry_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(raw) => {
                skipped.fetch_add(1, Ordering::Relaxed);
                let mut names = non_unicode.lock().expect("non-unicode list");
                if names.len() < MAX_LOGGED_NAMES {
                    names.push(format!(
                        "{}{}{}",
                        normalize_display_path(&item.path),
                        std::path::MAIN_SEPARATOR,
                        raw.to_string_lossy()
                    ));
                }
                continue;
            }
        };
        let child_path = item.path.join(&entry_name);
        let child_display = normalize_display_path(&child_path);

        if filter.excludes_exact(&child_path)
            || matches_any_exclude(&entry_name, &child_display, filter.globs)
        {
            skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        let meta = match fs::symlink_metadata(to_os_open_path(&child_path)) {
            Ok(m) => m,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
                ) =>
            {
                denied.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            Err(_) => {
                denied.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        if is_reparse_or_symlink(&meta) {
            // Policy (§5.3): record reparse points as size-0 entries with ext=NULL, never follow.
            files.push(FileEntry {
                name: entry_name,
                size: 0,
                mtime: modified_epoch(&meta).unwrap_or(0),
                ext: None,
            });
            continue;
        }

        if meta.is_dir() {
            let token = next_token.fetch_add(1, Ordering::Relaxed);
            queue.push(WorkItem {
                token,
                parent_token: Some(item.token),
                path: child_path,
                name: entry_name,
                depth: item.depth + 1,
            });
        } else {
            let ext = normalize_ext(&entry_name);
            files.push(FileEntry {
                name: entry_name,
                size: meta.len() as i64,
                mtime: modified_epoch(&meta).unwrap_or(0),
                ext,
            });
        }
    }

    let _ = tx.send(WriterMsg::DirBatch {
        token: item.token,
        parent_token: item.parent_token,
        path: normalize_display_path(&item.path),
        name: item.name.clone(),
        depth: item.depth,
        mtime,
        files,
    });
}

fn consume_and_insert(
    conn: &mut Connection,
    snapshot_id: i64,
    rx: mpsc::Receiver<WriterMsg>,
    progress: &mut Reporter,
) -> Result<(u64, u64, u64)> {
    let mut dirs = 0u64;
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut token_to_id: HashMap<u64, i64> = HashMap::new();
    let mut pending: HashMap<u64, Vec<PendingDir>> = HashMap::new();

    let mut tx = conn.transaction().map_err(map_db_err)?;
    let mut ops_in_tx = 0usize;

    for msg in rx {
        let dir = match msg {
            WriterMsg::Error(e) => return Err(e),
            WriterMsg::DirBatch {
                token,
                parent_token,
                path,
                name,
                depth,
                mtime,
                files: f_list,
            } => PendingDir {
                token,
                parent_token,
                path,
                name,
                depth,
                mtime,
                files: f_list,
            },
        };

        let ready = match dir.parent_token {
            None => true,
            Some(pt) => token_to_id.contains_key(&pt),
        };
        if !ready {
            if let Some(pt) = dir.parent_token {
                pending.entry(pt).or_default().push(dir);
            }
            continue;
        }

        let mut stack = vec![dir];
        while let Some(node) = stack.pop() {
            let parent_id = node
                .parent_token
                .and_then(|pt| token_to_id.get(&pt).copied());
            let mut dir_bytes = 0i64;
            let mut dir_files = 0i64;
            for f in &node.files {
                dir_bytes += f.size;
                dir_files += 1;
            }

            tx.execute(
                "INSERT INTO arkos_dirs(snapshot_id, parent_id, path, name, depth, mtime, bytes, files)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    snapshot_id,
                    parent_id,
                    node.path,
                    node.name,
                    node.depth,
                    node.mtime,
                    dir_bytes,
                    dir_files
                ],
            )
            .map_err(map_db_err)?;
            let dir_id = tx.last_insert_rowid();
            token_to_id.insert(node.token, dir_id);
            dirs += 1;
            ops_in_tx += 1;
            progress.tick(files, dirs, bytes);

            if !node.files.is_empty() {
                let mut stmt = tx
                    .prepare_cached(
                        "INSERT INTO arkos_files(snapshot_id, dir_id, name, size, mtime, ext)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    )
                    .map_err(map_db_err)?;
                for f in &node.files {
                    stmt.execute(params![snapshot_id, dir_id, f.name, f.size, f.mtime, f.ext])
                        .map_err(map_db_err)?;
                    files += 1;
                    bytes += f.size as u64;
                    ops_in_tx += 1;
                }
            }

            if let Some(children) = pending.remove(&node.token) {
                for ch in children.into_iter().rev() {
                    stack.push(ch);
                }
            }

            if ops_in_tx >= BATCH_COMMIT_ENTRIES {
                tx.commit().map_err(map_db_err)?;
                tx = conn.transaction().map_err(map_db_err)?;
                ops_in_tx = 0;
            }
        }
    }

    tx.commit().map_err(map_db_err)?;
    Ok((dirs, files, bytes))
}

struct PendingDir {
    token: u64,
    parent_token: Option<u64>,
    path: String,
    name: String,
    depth: i32,
    mtime: Option<i64>,
    files: Vec<FileEntry>,
}

fn modified_epoch(meta: &Metadata) -> Option<i64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

fn is_reparse_or_symlink(meta: &Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}
