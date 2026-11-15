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

/// Extract lowercased extension without dot (`None` for extensionless or leading-dot dotfiles).
pub fn normalize_ext(file_name: &str) -> Option<String> {
    let dot = file_name.rfind('.')?;
    if dot == 0 || dot + 1 >= file_name.len() {
        return None;
    }
    Some(file_name[dot + 1..].to_ascii_lowercase())
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
        assert_eq!(normalize_ext("archive.tar.GZ").as_deref(), Some("gz"));
        assert_eq!(normalize_ext("Makefile"), None);
        assert_eq!(normalize_ext(".gitignore"), None);
        assert_eq!(normalize_ext("trailing."), None);
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
