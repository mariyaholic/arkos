//! Path normalization, `\\?\` long-path support, extension extraction, and glob exclusions.

use std::path::{Path, PathBuf};

/// Convert a path to an extended-length `\\?\` path on Windows when needed.
pub fn to_os_open_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = path.to_string_lossy();
        if s.starts_with(r"\\?\") || s.starts_with(r"\\.\") {
            return path.to_path_buf();
        }
        if s.len() >= 240 {
            if let Some(rest) = s.strip_prefix(r"\\") {
                return PathBuf::from(format!(r"\\?\UNC\{rest}"));
            }
            if path.is_absolute() {
                return PathBuf::from(format!(r"\\?\{s}"));
            }
        }
    }
    path.to_path_buf()
}

/// Strip Windows `\\?\` display prefix so stored `arkos_dirs.path` stays human-readable.
pub fn normalize_display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(local) = raw.strip_prefix(r"\\?\") {
            return local.to_owned();
        }
    }
    raw.into_owned()
}

/// Archive suffixes kept whole, so `backup.tar.gz` groups as `tar.gz` and not `gz`.
const COMPOUND_EXTS: [&str; 7] = [
    "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "tar.lz", "tar.lzma", "tar.z",
];

/// Extract the lowercased extension without a dot (`None` for extensionless names, leading-dot
/// dotfiles and a trailing dot).
///
/// A fixed list of compound archive suffixes is stored whole; every other name keeps the text
/// after its last dot. A name must be longer than the suffix plus its dot to count, so a file
/// named exactly `.tar.gz` is treated as the dotfile it is and falls through to the last dot.
pub fn normalize_ext(file_name: &str) -> Option<String> {
    let lower = file_name.to_ascii_lowercase();
    for suffix in COMPOUND_EXTS {
        if lower.len() > suffix.len() + 1
            && lower.ends_with(suffix)
            && lower.as_bytes()[lower.len() - suffix.len() - 1] == b'.'
        {
            return Some(suffix.to_owned());
        }
    }
    let dot = file_name.rfind('.')?;
    if dot == 0 || dot + 1 >= file_name.len() {
        return None;
    }
    Some(lower[dot + 1..].to_owned())
}

/// Simple case-insensitive `*` / `?` glob matching for `--exclude`.
pub fn matches_any_exclude(name: &str, full_path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pat| {
        if pat.contains('\\') || pat.contains('/') {
            glob_match_ci(pat, full_path)
        } else {
            glob_match_ci(pat, name)
        }
    })
}

fn glob_match_ci(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let t: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_p, mut star_t) = (None, 0usize);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_p = Some(pi);
            pi += 1;
            star_t = ti;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_normalizes_lowercase_and_ignores_dotfiles() {
        assert_eq!(normalize_ext("Photo.JPG").as_deref(), Some("jpg"));
        assert_eq!(normalize_ext("archive.tar.GZ").as_deref(), Some("tar.gz"));
        assert_eq!(normalize_ext("Makefile"), None);
        assert_eq!(normalize_ext(".gitignore"), None);
        assert_eq!(normalize_ext("trailing."), None);
    }

    #[test]
    fn compound_archive_extensions_are_stored_whole() {
        for (name, want) in [
            ("a.tar.gz", "tar.gz"),
            ("a.tar.bz2", "tar.bz2"),
            ("a.TAR.XZ", "tar.xz"),
            ("a.tar.zst", "tar.zst"),
            ("a.tar.lz", "tar.lz"),
            ("a.tar.lzma", "tar.lzma"),
            ("a.tar.z", "tar.z"),
            ("many.dots.in.name.tar.gz", "tar.gz"),
        ] {
            assert_eq!(normalize_ext(name).as_deref(), Some(want), "{name}");
        }
        // Not on the list, or no separating dot, or nothing before the suffix.
        assert_eq!(normalize_ext("a.tar.7z").as_deref(), Some("7z"));
        assert_eq!(normalize_ext("atar.gz").as_deref(), Some("gz"));
        assert_eq!(normalize_ext("a.tgz").as_deref(), Some("tgz"));
        assert_eq!(normalize_ext(".tar.gz").as_deref(), Some("gz"));
        assert_eq!(normalize_ext("tar.gz").as_deref(), Some("gz"));
        assert_eq!(normalize_ext("x.tar.gz.").as_deref(), None);
        assert_eq!(normalize_ext("archive.tar").as_deref(), Some("tar"));
    }

    #[test]
    fn glob_matches_case_insensitively() {
        assert!(matches_any_exclude(
            "node_modules",
            r"C:\proj\node_modules",
            &["node_modules".into()]
        ));
        assert!(matches_any_exclude(
            "foo.tmp",
            r"C:\foo.tmp",
            &["*.TMP".into()]
        ));
        assert!(!matches_any_exclude(
            "main.rs",
            r"C:\src\main.rs",
            &["*.tmp".into()]
        ));
    }
}
