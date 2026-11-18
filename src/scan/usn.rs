//! USN journal record parsing and the two-phase MFT scan, independent of the operating system.
//!
//! The Windows-only part (`volume.rs`-style handle calls) lives in `mft.rs` behind
//! `cfg(all(windows, feature = "mft"))` and implements [`UsnSource`]. Everything here is plain
//! data handling, so it runs and is tested on any host.

use crate::db::map_db_err;
use crate::exit::{Error, Result};
use crate::progress::Reporter;
use crate::scan::paths::{
    attrs_are_placeholder, matches_any_exclude, normalize_display_path, normalize_ext,
};
use crate::scan::walk::{commit_due, ScanFilter, WalkStats, MAX_LOGGED_NAMES};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::io;
use std::path::Path;

/// More directories than this and the FRN map no longer fits the memory gate; the scan falls
/// back to the walk (logged as `mft-slow`).
pub const DIR_LIMIT: usize = 1_000_000;

const ATTR_DIRECTORY: u32 = 0x0010;
const ATTR_REPARSE_POINT: u32 = 0x0400;

/// Seconds between 1601-01-01 (FILETIME epoch) and 1970-01-01.
const FILETIME_UNIX_DIFF_SECS: i64 = 11_644_473_600;

/// Convert a FILETIME (100 ns ticks since 1601) to Unix seconds.
pub fn filetime_to_unix(ft: u64) -> i64 {
    (ft / 10_000_000) as i64 - FILETIME_UNIX_DIFF_SECS
}

/// One `USN_RECORD_V2` from `FSCTL_ENUM_USN_DATA`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsnRecord {
    pub frn: u64,
    pub parent: u64,
    /// Update sequence number of the record.
    pub usn: i64,
    pub attrs: u32,
    /// USN record time as a FILETIME. This is the time of the last change, not of the last write.
    pub timestamp: u64,
    /// The name, in lossy form when it was not valid UTF-16.
    pub name: String,
    pub valid_unicode: bool,
}

impl UsnRecord {
    pub fn is_dir(&self) -> bool {
        self.attrs & ATTR_DIRECTORY != 0
    }

    pub fn is_reparse(&self) -> bool {
        self.attrs & ATTR_REPARSE_POINT != 0
    }
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Parse one `FSCTL_ENUM_USN_DATA` output buffer: an 8-byte next-FRN followed by records.
///
/// Calls `f` for every version 2 record and returns the FRN to resume from, plus whether `f`
/// asked to stop. Records of other versions are skipped by length, and a malformed length ends
/// the buffer.
pub fn parse_usn_buffer(
    buf: &[u8],
    f: &mut dyn FnMut(UsnRecord) -> bool,
) -> io::Result<(u64, bool)> {
    if buf.len() < 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "USN buffer shorter than its header",
        ));
    }
    let next = u64_at(buf, 0);
    let mut off = 8usize;
    while off + 60 <= buf.len() {
        let len = u32_at(buf, off) as usize;
        if len < 60 || off + len > buf.len() {
            break;
        }
        if u16_at(buf, off + 4) == 2 {
            let name_len = u16_at(buf, off + 56) as usize;
            let name_off = u16_at(buf, off + 58) as usize;
            if name_off + name_len <= len && name_len & 1 == 0 {
                let units: Vec<u16> = buf[off + name_off..off + name_off + name_len]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c: &[u8; 2]| u16::from_le_bytes(*c))
                    .collect();
                let (name, valid_unicode) = match String::from_utf16(&units) {
                    Ok(s) => (s, true),
                    Err(_) => (String::from_utf16_lossy(&units), false),
                };
                let keep_going = f(UsnRecord {
                    frn: u64_at(buf, off + 8),
                    parent: u64_at(buf, off + 16),
                    usn: u64_at(buf, off + 24) as i64,
                    attrs: u32_at(buf, off + 52),
                    timestamp: u64_at(buf, off + 32),
                    name,
                    valid_unicode,
                });
                if !keep_going {
                    return Ok((next, true));
                }
            }
        }
        off += len;
    }
    Ok((next, false))
}

/// Size and last-write time of one file, read through its handle.
#[derive(Debug, Clone, Copy)]
pub struct FileStat {
    pub size: i64,
    pub mtime: i64,
}

/// What the scan needs from a volume: enumeration and per-file stat by FRN.
pub trait UsnSource {
    /// Visit every record on the volume, in FRN order. The visitor returns `false` to stop early.
    fn enumerate(&self, f: &mut dyn FnMut(UsnRecord) -> bool) -> io::Result<()>;
    /// Logical size and last-write time of the file with this FRN. USN records carry neither.
    fn stat(&self, frn: u64) -> io::Result<FileStat>;
}

/// Result of an MFT scan attempt.
#[derive(Debug)]
pub enum UsnScan {
    Done(Box<(WalkStats, MftDiag)>),
    /// The directory map passed the limit; nothing was written and the caller should walk.
    TooManyDirs,
}

/// What the MFT reader saw, for the scan log of an experimental run.
#[derive(Debug, Default, Clone)]
pub struct MftDiag {
    /// Lowest and highest USN among the records seen in phase 1, with the range asked for.
    pub usn_min: Option<i64>,
    pub usn_max: Option<i64>,
    pub records_phase1: u64,
    pub records_phase2: u64,
    pub dirs: u64,
    /// Directories whose parent chain does not reach the root, and a sample of the parent FRNs.
    pub orphan_dirs: u64,
    pub orphan_parents: Vec<u64>,
    /// Files whose parent directory did not exist after phase 1.
    pub late_files: u64,
    pub phase1_ms: u64,
    pub resolve_ms: u64,
    pub dirs_ms: u64,
    pub phase2_ms: u64,
    pub finalize_ms: u64,
}

/// Most orphan parent FRNs kept for the log.
const MAX_ORPHAN_SAMPLE: usize = 10;

impl std::fmt::Display for MftDiag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let usn = match (self.usn_min, self.usn_max) {
            (Some(lo), Some(hi)) => format!("{lo}..{hi}"),
            _ => "none".to_owned(),
        };
        let orphans: Vec<String> = self
            .orphan_parents
            .iter()
            .map(|p| format!("{p:#x}"))
            .collect();
        write!(
            f,
            "mft_diag usn_seen={usn} usn_asked=0..max records_phase1={} records_phase2={} dirs={} \
             orphan_dirs={} orphan_parent_frns=[{}] late_files={} phase1_ms={} resolve_ms={} \
             dirs_ms={} phase2_ms={} finalize_ms={}",
            self.records_phase1,
            self.records_phase2,
            self.dirs,
            self.orphan_dirs,
            orphans.join(","),
            self.late_files,
            self.phase1_ms,
            self.resolve_ms,
            self.dirs_ms,
            self.phase2_ms,
            self.finalize_ms
        )
    }
}

#[derive(Debug)]
struct DirNode {
    parent: u64,
    name: String,
}

#[derive(Debug, Clone)]
enum Res {
    Dir {
        path: String,
        depth: i32,
    },
    /// Matched an exclude, was not valid Unicode, or sits below one that did.
    Excluded,
    /// The parent chain does not reach the root.
    Orphan,
}

fn join_win(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

fn is_excluded(filter: &ScanFilter<'_>, name: &str, path: &str) -> bool {
    filter.excludes_exact(Path::new(path)) || matches_any_exclude(name, path, filter.globs)
}

fn note_non_unicode(stats: &mut WalkStats, parent_path: &str, name: &str) {
    stats.skipped += 1;
    stats.non_unicode_total += 1;
    if stats.non_unicode.len() < MAX_LOGGED_NAMES {
        stats.non_unicode.push(join_win(parent_path, name));
    }
}

/// Commits every 50,000 writes or 2 seconds and starts the next transaction.
struct Batch<'a> {
    conn: &'a Connection,
    ops: usize,
    since: std::time::Instant,
}

impl Batch<'_> {
    fn begin(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN").map_err(map_db_err)
    }

    fn wrote(&mut self) -> Result<()> {
        self.ops += 1;
        if commit_due(self.ops, self.since) {
            self.ops = 0;
            self.since = std::time::Instant::now();
            self.conn
                .execute_batch("COMMIT; BEGIN")
                .map_err(map_db_err)?;
        }
        Ok(())
    }

    fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT").map_err(map_db_err)
    }
}

/// Run both phases against `src` and write the rows for `snapshot_id`.
///
/// Phase 1 keeps only directory records, as `FRN -> (parent, name)`, and inserts the directories
/// in depth order. Phase 2 enumerates again and writes each file under its parent's `dir_id`.
/// A file whose parent is missing counts as `denied`, and so does a file that cannot be sized.
/// Reparse points are recorded as size-0 entries and never followed, as in the walk. Directory
/// totals are written once at the end, because files arrive in FRN order and not by directory.
///
/// On error the open transaction is rolled back and the caller marks the snapshot failed.
#[allow(clippy::too_many_arguments)]
pub fn scan_with_source(
    conn: &Connection,
    snapshot_id: i64,
    root: &Path,
    root_frn: u64,
    src: &dyn UsnSource,
    filter: ScanFilter<'_>,
    progress: &mut Reporter,
    dir_limit: usize,
) -> Result<UsnScan> {
    let mut batch = Batch {
        conn,
        ops: 0,
        since: std::time::Instant::now(),
    };
    let res = run(
        &mut batch,
        snapshot_id,
        root,
        root_frn,
        src,
        filter,
        progress,
        dir_limit,
    );
    if res.is_err() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    res
}

#[allow(clippy::too_many_arguments)]
fn run(
    batch: &mut Batch<'_>,
    snapshot_id: i64,
    root: &Path,
    root_frn: u64,
    src: &dyn UsnSource,
    filter: ScanFilter<'_>,
    progress: &mut Reporter,
    dir_limit: usize,
) -> Result<UsnScan> {
    let enum_err = |source| Error::Io {
        context: "enumerate MFT",
        source,
    };
    let conn = batch.conn;
    let mut stats = WalkStats::default();

    // Phase 1: directories only.
    let mut nodes: HashMap<u64, DirNode> = HashMap::new();
    let mut bad_names: Vec<(u64, u64, String)> = Vec::new();
    let mut too_many = false;
    let mut diag = MftDiag::default();
    let t_phase1 = std::time::Instant::now();
    src.enumerate(&mut |rec| {
        diag.records_phase1 += 1;
        diag.usn_min = Some(diag.usn_min.map_or(rec.usn, |m| m.min(rec.usn)));
        diag.usn_max = Some(diag.usn_max.map_or(rec.usn, |m| m.max(rec.usn)));
        if !rec.is_dir() || rec.frn == root_frn {
            return true;
        }
        if rec.is_reparse() {
            return true; // recorded as a size-0 entry in phase 2, never followed
        }
        if !rec.valid_unicode {
            bad_names.push((rec.frn, rec.parent, rec.name));
            return true;
        }
        nodes.insert(
            rec.frn,
            DirNode {
                parent: rec.parent,
                name: rec.name,
            },
        );
        if nodes.len() > dir_limit {
            too_many = true;
        }
        !too_many
    })
    .map_err(enum_err)?;
    diag.phase1_ms = t_phase1.elapsed().as_millis() as u64;
    if too_many {
        return Ok(UsnScan::TooManyDirs);
    }
    let t_resolve = std::time::Instant::now();

    // Resolve every directory to a path, counting each excluded or orphaned directory once.
    let root_display = normalize_display_path(root);
    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root_display.clone());
    let mut resolved: HashMap<u64, Res> = HashMap::with_capacity(nodes.len());
    let mut frns: Vec<u64> = nodes.keys().copied().collect();
    frns.sort_unstable();
    for &start in &frns {
        if resolved.contains_key(&start) {
            continue;
        }
        let mut chain = vec![start];
        let mut base = loop {
            let top = *chain.last().expect("chain is never empty");
            let parent = nodes[&top].parent;
            if parent == root_frn {
                break Res::Dir {
                    path: root_display.clone(),
                    depth: 0,
                };
            }
            if let Some(r) = resolved.get(&parent) {
                break r.clone();
            }
            if !nodes.contains_key(&parent) || chain.len() > nodes.len() {
                stats.denied += 1; // the top of this chain has no parent we know of
                diag.orphan_dirs += 1;
                if diag.orphan_parents.len() < MAX_ORPHAN_SAMPLE {
                    diag.orphan_parents.push(parent);
                }
                break Res::Orphan;
            }
            chain.push(parent);
        };
        for &frn in chain.iter().rev() {
            let node = &nodes[&frn];
            base = match &base {
                Res::Dir { path, depth } => {
                    let child = join_win(path, &node.name);
                    if is_excluded(&filter, &node.name, &child) {
                        stats.skipped += 1;
                        Res::Excluded
                    } else {
                        Res::Dir {
                            path: child,
                            depth: depth + 1,
                        }
                    }
                }
                other => other.clone(),
            };
            resolved.insert(frn, base.clone());
        }
    }
    for (frn, parent, name) in &bad_names {
        let parent_path = match resolved.get(parent) {
            Some(Res::Dir { path, .. }) => path.as_str(),
            _ if *parent == root_frn => root_display.as_str(),
            _ => "",
        };
        note_non_unicode(&mut stats, parent_path, name);
        resolved.insert(*frn, Res::Excluded);
    }

    diag.resolve_ms = t_resolve.elapsed().as_millis() as u64;
    let t_dirs = std::time::Instant::now();

    // Insert directories in depth order so every parent exists before its children.
    let mut dirs: Vec<(u64, &str, i32)> = resolved
        .iter()
        .filter_map(|(frn, r)| match r {
            Res::Dir { path, depth } => Some((*frn, path.as_str(), *depth)),
            _ => None,
        })
        .collect();
    dirs.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.1.cmp(b.1)));

    let root_mtime = std::fs::metadata(root)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);

    batch.begin()?;
    let mut dir_ids: HashMap<u64, i64> = HashMap::with_capacity(dirs.len() + 1);
    conn.execute(
        "INSERT INTO arkos_dirs(snapshot_id, parent_id, path, name, depth, mtime, bytes, files)
         VALUES(?1, NULL, ?2, ?3, 0, ?4, 0, 0)",
        params![snapshot_id, root_display, root_name, root_mtime],
    )
    .map_err(map_db_err)?;
    dir_ids.insert(root_frn, conn.last_insert_rowid());
    stats.dirs += 1;
    for &(frn, path, depth) in &dirs {
        let node = &nodes[&frn];
        let parent_id = dir_ids.get(&node.parent).copied();
        conn.prepare_cached(
            "INSERT INTO arkos_dirs(snapshot_id, parent_id, path, name, depth, mtime, bytes, files)
             VALUES(?1, ?2, ?3, ?4, ?5, NULL, 0, 0)",
        )
        .map_err(map_db_err)?
        .execute(params![snapshot_id, parent_id, path, node.name, depth])
        .map_err(map_db_err)?;
        dir_ids.insert(frn, conn.last_insert_rowid());
        stats.dirs += 1;
        progress.tick(stats.files, stats.dirs, stats.bytes);
        batch.wrote()?;
    }
    let dir_paths: HashMap<u64, &str> = dirs
        .iter()
        .map(|&(frn, path, _)| (frn, path))
        .chain(std::iter::once((root_frn, root_display.as_str())))
        .collect();

    diag.dirs = stats.dirs;
    diag.dirs_ms = t_dirs.elapsed().as_millis() as u64;
    let t_phase2 = std::time::Instant::now();

    // Phase 2: files.
    let mut totals: HashMap<i64, (i64, i64)> = HashMap::new();
    let mut db_err: Option<Error> = None;
    src.enumerate(&mut |rec| {
        diag.records_phase2 += 1;
        if rec.frn == root_frn || (rec.is_dir() && !rec.is_reparse()) {
            return true;
        }
        let Some(&dir_id) = dir_ids.get(&rec.parent) else {
            // Below an excluded directory the entry is silent; any other missing parent is
            // a directory created after phase 1 and counts as denied.
            if !matches!(resolved.get(&rec.parent), Some(Res::Excluded)) {
                stats.denied += 1;
                diag.late_files += 1;
            }
            return true;
        };
        let parent_path = dir_paths.get(&rec.parent).copied().unwrap_or("");
        if !rec.valid_unicode {
            note_non_unicode(&mut stats, parent_path, &rec.name);
            return true;
        }
        let full = join_win(parent_path, &rec.name);
        if is_excluded(&filter, &rec.name, &full) {
            stats.skipped += 1;
            return true;
        }
        // A cloud placeholder is a reparse point that is still an ordinary file with a size.
        let placeholder = attrs_are_placeholder(rec.attrs);
        let (size, mtime, ext) = if rec.is_reparse() && !placeholder {
            (0, filetime_to_unix(rec.timestamp), None)
        } else {
            match src.stat(rec.frn) {
                Ok(s) => (s.size, s.mtime, normalize_ext(&rec.name)),
                Err(_) => {
                    stats.denied += 1;
                    return true;
                }
            }
        };
        let write = conn
            .prepare_cached(
                "INSERT INTO arkos_files(snapshot_id, dir_id, name, size, mtime, ext)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .and_then(|mut st| st.execute(params![snapshot_id, dir_id, rec.name, size, mtime, ext]))
            .map_err(map_db_err)
            .and_then(|_| batch.wrote());
        if let Err(e) = write {
            db_err = Some(e);
            return false;
        }
        if placeholder {
            stats.placeholders += 1;
        }
        let t = totals.entry(dir_id).or_insert((0, 0));
        t.0 += size;
        t.1 += 1;
        stats.files += 1;
        stats.bytes += size as u64;
        progress.tick(stats.files, stats.dirs, stats.bytes);
        true
    })
    .map_err(enum_err)?;
    if let Some(e) = db_err {
        return Err(e);
    }
    diag.phase2_ms = t_phase2.elapsed().as_millis() as u64;
    let t_finalize = std::time::Instant::now();

    for (dir_id, (bytes, files)) in &totals {
        conn.prepare_cached("UPDATE arkos_dirs SET bytes = ?1, files = ?2 WHERE id = ?3")
            .map_err(map_db_err)?
            .execute(params![bytes, files, dir_id])
            .map_err(map_db_err)?;
    }
    batch.commit()?;
    diag.finalize_ms = t_finalize.elapsed().as_millis() as u64;

    stats.non_unicode.sort();
    Ok(UsnScan::Done(Box::new((stats, diag))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate;

    const ROOT: u64 = 5;

    fn record_bytes(frn: u64, parent: u64, attrs: u32, name: &[u16]) -> Vec<u8> {
        let name_bytes = name.len() * 2;
        let len = (60 + name_bytes).div_ceil(8) * 8;
        let mut b = vec![0u8; len];
        b[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        b[8..16].copy_from_slice(&frn.to_le_bytes());
        b[16..24].copy_from_slice(&parent.to_le_bytes());
        b[32..40].copy_from_slice(&132_000_000_000_000_000u64.to_le_bytes());
        b[52..56].copy_from_slice(&attrs.to_le_bytes());
        b[56..58].copy_from_slice(&(name_bytes as u16).to_le_bytes());
        b[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (i, u) in name.iter().enumerate() {
            b[60 + i * 2..62 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        b
    }

    #[test]
    fn parses_records_and_resume_frn() {
        let mut buf = 777u64.to_le_bytes().to_vec();
        let n: Vec<u16> = "a.txt".encode_utf16().collect();
        buf.extend(record_bytes(10, ROOT, 0x20, &n));
        let bad = [0x0062u16, 0xD800, 0x0063]; // unpaired surrogate
        buf.extend(record_bytes(11, ROOT, 0x10, &bad));
        // A version 3 record is skipped by length.
        let mut v3 = record_bytes(12, ROOT, 0x20, &n);
        v3[4..6].copy_from_slice(&3u16.to_le_bytes());
        buf.extend(v3);

        let mut seen = Vec::new();
        let (next, stopped) = parse_usn_buffer(&buf, &mut |r| {
            seen.push(r);
            true
        })
        .unwrap();
        assert_eq!(next, 777);
        assert!(!stopped);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].name, "a.txt");
        assert!(seen[0].valid_unicode && !seen[0].is_dir());
        assert_eq!((seen[0].frn, seen[0].parent), (10, ROOT));
        assert!(seen[1].is_dir() && !seen[1].valid_unicode);
        assert_eq!(seen[1].name, "b\u{fffd}c");

        let (_, stopped) = parse_usn_buffer(&buf, &mut |_| false).unwrap();
        assert!(stopped);
        assert!(parse_usn_buffer(&[0u8; 4], &mut |_| true).is_err());
    }

    #[test]
    fn filetime_converts_to_unix_seconds() {
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), 0);
        assert_eq!(
            filetime_to_unix(116_444_736_000_000_000 + 10_000_000 * 86_400),
            86_400
        );
    }

    struct Fake {
        records: Vec<UsnRecord>,
        sizes: HashMap<u64, i64>,
    }

    impl UsnSource for Fake {
        fn enumerate(&self, f: &mut dyn FnMut(UsnRecord) -> bool) -> io::Result<()> {
            for r in &self.records {
                if !f(r.clone()) {
                    break;
                }
            }
            Ok(())
        }

        fn stat(&self, frn: u64) -> io::Result<FileStat> {
            match self.sizes.get(&frn) {
                Some(&size) => Ok(FileStat {
                    size,
                    mtime: 1_700_000_000,
                }),
                None => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            }
        }
    }

    fn rec(frn: u64, parent: u64, attrs: u32, name: &str) -> UsnRecord {
        UsnRecord {
            frn,
            parent,
            usn: frn as i64,
            attrs,
            timestamp: 132_000_000_000_000_000,
            name: name.to_owned(),
            valid_unicode: true,
        }
    }

    fn fake() -> Fake {
        let dir = 0x10;
        let file = 0x20;
        let reparse_dir = 0x10 | 0x400;
        let mut bad = rec(30, 10, file, "b\u{fffd}d");
        bad.valid_unicode = false;
        Fake {
            records: vec![
                rec(10, ROOT, dir, "a"),
                rec(11, 10, dir, "b"),
                rec(12, ROOT, dir, "node_modules"),
                rec(13, 12, dir, "x"),
                rec(14, ROOT, reparse_dir, "link"),
                rec(15, 99, dir, "lost-dir"),
                rec(16, 15, dir, "below-lost"),
                rec(20, 10, file, "f1.txt"),
                rec(21, 11, file, "g.tar.GZ"),
                rec(22, 13, file, "hidden.bin"),
                rec(23, 50, file, "late.txt"),
                rec(24, 10, file, "cannot-stat"),
                bad,
                rec(26, 10, file, "skip.tmp"),
                rec(27, 16, file, "in-lost.txt"),
            ],
            sizes: HashMap::from([(20, 100), (21, 200), (22, 999), (26, 1), (27, 5)]),
        }
    }

    fn setup() -> (Connection, i64) {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
             VALUES('C:', 'C:\\', 'mft', 1, 'running', 0)",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        (conn, id)
    }

    fn run_fake(globs: &[String], limit: usize) -> (Connection, i64, UsnScan) {
        let (conn, id) = setup();
        let src = fake();
        let filter = ScanFilter {
            globs,
            exact_dirs: &[],
        };
        let out = scan_with_source(
            &conn,
            id,
            Path::new("C:\\"),
            ROOT,
            &src,
            filter,
            &mut Reporter::off(),
            limit,
        )
        .unwrap();
        (conn, id, out)
    }

    #[test]
    fn two_phase_scan_writes_dirs_files_and_counts() {
        let globs = vec!["node_modules".to_owned(), "*.tmp".to_owned()];
        let (conn, id, out) = run_fake(&globs, DIR_LIMIT);
        let UsnScan::Done(done) = out else {
            panic!("expected a finished scan");
        };
        let (stats, diag) = *done;

        let dirs: Vec<(String, i32, Option<i64>)> = conn
            .prepare("SELECT path, depth, parent_id FROM arkos_dirs WHERE snapshot_id = ?1 ORDER BY depth, path")
            .unwrap()
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        let paths: Vec<&str> = dirs.iter().map(|d| d.0.as_str()).collect();
        assert_eq!(paths, vec!["C:\\", "C:\\a", "C:\\a\\b"]);
        assert!(dirs[0].2.is_none() && dirs[1].2.is_some() && dirs[2].2.is_some());

        let mut files: Vec<(String, i64, Option<String>)> = conn
            .prepare(
                "SELECT d.path || '|' || f.name, f.size, f.ext FROM arkos_files f
                 JOIN arkos_dirs d ON d.id = f.dir_id ORDER BY 1",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec![
                (
                    "C:\\a\\b|g.tar.GZ".to_owned(),
                    200,
                    Some("tar.gz".to_owned())
                ),
                ("C:\\a|f1.txt".to_owned(), 100, Some("txt".to_owned())),
                ("C:\\|link".to_owned(), 0, None),
            ]
        );
        let a: (i64, i64) = conn
            .query_row(
                "SELECT bytes, files FROM arkos_dirs WHERE path = 'C:\\a'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(a, (100, 1));

        assert_eq!((stats.dirs, stats.files, stats.bytes), (3, 3, 300));
        // denied: the orphan directory, late.txt (unknown parent), cannot-stat, and in-lost.txt.
        assert_eq!(stats.denied, 4);
        // skipped: node_modules (once, not its children), the non-Unicode name, and skip.tmp.
        assert_eq!(stats.skipped, 3);
        assert_eq!(stats.non_unicode, vec!["C:\\a\\b\u{fffd}d".to_owned()]);

        assert_eq!(diag.records_phase1, 15);
        assert_eq!(diag.records_phase2, 15);
        assert_eq!((diag.usn_min, diag.usn_max), (Some(10), Some(30)));
        assert_eq!(diag.orphan_dirs, 1);
        assert_eq!(diag.orphan_parents, vec![99]);
        assert_eq!(diag.late_files, 2);
        let line = diag.to_string();
        assert!(line.contains("orphan_parent_frns=[0x63]"), "{line}");
        assert!(line.contains("usn_seen=10..30"), "{line}");
    }

    #[test]
    fn a_cloud_placeholder_keeps_its_logical_size_and_is_counted() {
        let (conn, id) = setup();
        let file = 0x20;
        let src = Fake {
            records: vec![
                rec(10, ROOT, 0x10, "docs"),
                // Reparse point with RECALL_ON_DATA_ACCESS: a OneDrive style placeholder.
                rec(20, 10, file | 0x400 | 0x40_0000, "online.docx"),
                // A plain reparse point is still a size-0 entry.
                rec(21, 10, file | 0x400, "link.lnk"),
            ],
            sizes: HashMap::from([(20, 4096), (21, 77)]),
        };
        let out = scan_with_source(
            &conn,
            id,
            Path::new("C:\\"),
            ROOT,
            &src,
            ScanFilter::default(),
            &mut Reporter::off(),
            DIR_LIMIT,
        )
        .unwrap();
        let UsnScan::Done(done) = out else {
            panic!("expected a finished scan");
        };
        let (stats, _) = *done;
        assert_eq!((stats.files, stats.bytes, stats.placeholders), (2, 4096, 1));
        let ext: Option<String> = conn
            .query_row(
                "SELECT ext FROM arkos_files WHERE name = 'online.docx'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ext.as_deref(), Some("docx"));
    }

    #[test]
    fn too_many_directories_writes_nothing_and_asks_for_the_walk() {
        let (conn, id, out) = run_fake(&[], 3);
        assert!(matches!(out, UsnScan::TooManyDirs));
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM arkos_dirs WHERE snapshot_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_failed_insert_rolls_back_the_transaction() {
        let (conn, id) = setup();
        conn.execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON arkos_files
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
        let err = scan_with_source(
            &conn,
            id,
            Path::new("C:\\"),
            ROOT,
            &fake(),
            ScanFilter::default(),
            &mut Reporter::off(),
            DIR_LIMIT,
        );
        assert!(err.is_err());
        // The connection is usable and holds no half-open transaction.
        conn.execute_batch("BEGIN; COMMIT;").unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM arkos_dirs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
