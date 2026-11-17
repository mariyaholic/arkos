//! Deterministic fixture tree generator.
//!
//! Shared by `examples/gen_tree.rs` and the integration tests. The same `(files, dirs, depth,
//! seed)` always produces the same tree and the same manifest hash. File sizes are set with
//! `File::set_len`, so the tree is sparse and fast to create.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

/// Shape of a generated tree.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    pub files: usize,
    pub dirs: usize,
    pub depth: usize,
    pub seed: u64,
}

/// Totals and content hash of a generated tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub files: usize,
    pub dirs: usize,
    pub bytes: u64,
    /// FNV-1a 64 over the sorted `relative/path\tsize\n` list, with `/` separators.
    pub hash: u64,
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Mix the seed so that small seeds do not start in a low-entropy state.
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }

    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const EXTS: [&str; 8] = ["txt", "log", "bin", "jpg", "dll", "json", "md", "tmp"];

/// Create the tree under `root` (which must exist) and return its manifest.
pub fn generate(root: &Path, spec: &Spec) -> std::io::Result<Manifest> {
    let mut rng = Rng::new(spec.seed);
    let max_depth = spec.depth.max(1);

    // Directory 0 is the root itself; later directories pick an earlier parent that is not too deep.
    let mut dir_rel: Vec<String> = vec![String::new()];
    let mut dir_depth: Vec<usize> = vec![0];
    let mut dir_total = 1usize;
    while dir_rel.len() < spec.dirs.max(1) {
        let mut parent = rng.below(dir_rel.len());
        let mut guard = 0;
        while dir_depth[parent] >= max_depth && guard < 64 {
            parent = rng.below(dir_rel.len());
            guard += 1;
        }
        if dir_depth[parent] >= max_depth {
            parent = 0;
        }
        let name = format!("d{:05}", dir_rel.len());
        let rel = if dir_rel[parent].is_empty() {
            name
        } else {
            format!("{}/{name}", dir_rel[parent])
        };
        fs::create_dir_all(root.join(&rel))?;
        dir_depth.push(dir_depth[parent] + 1);
        dir_rel.push(rel);
        dir_total += 1;
    }

    let mut entries: Vec<(String, u64)> = Vec::with_capacity(spec.files);
    let mut bytes = 0u64;
    for i in 0..spec.files {
        let dir = &dir_rel[rng.below(dir_rel.len())];
        let ext = EXTS[rng.below(EXTS.len())];
        let size = match rng.below(100) {
            0 => rng.next() % (8 * 1024 * 1024),
            1..=20 => rng.next() % (256 * 1024),
            _ => rng.next() % 16 * 1024,
        };
        let name = format!("f{i:07}.{ext}");
        let rel = if dir.is_empty() {
            name
        } else {
            format!("{dir}/{name}")
        };
        let f = File::create(root.join(&rel))?;
        f.set_len(size)?;
        bytes += size;
        entries.push((rel, size));
    }

    entries.sort();
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for (rel, size) in &entries {
        for b in format!("{rel}\t{size}\n").bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    Ok(Manifest {
        files: spec.files,
        dirs: dir_total,
        bytes,
        hash,
    })
}

/// Write a sparse file of `len` bytes at `path`, creating parent directories.
pub fn write_sparse(path: &Path, len: u64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    File::create(path)?.set_len(len)
}

/// Join `rel` (with `/` separators) onto `root`.
pub fn at(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |p, part| p.join(part))
}
