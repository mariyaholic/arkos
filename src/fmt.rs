//! Text formatting shared by the CLI and the HTML report: sizes and display paths.

/// Format a byte count in binary units (`B`, `KiB`, `MiB`, `GiB`, `TiB`).
///
/// Values under 1 KiB print as whole bytes; larger values keep one decimal. Negative values keep
/// their sign, so a shrink reads `-1.4 GiB`.
pub fn format_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes.unsigned_abs() < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value.abs() >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Like [`format_size`] with an explicit `+` on positive values, for deltas.
pub fn format_delta(bytes: i64) -> String {
    if bytes > 0 {
        format!("+{}", format_size(bytes))
    } else {
        format_size(bytes)
    }
}

/// Join a stored directory path and a file name with the platform separator.
///
/// No separator is added when `dir` already ends in one, which is the case for a volume root such
/// as `C:\`. Stored values are never rewritten; this only builds the string shown to the user.
pub fn join_display(dir: &str, name: &str) -> String {
    let ends_in_sep =
        dir.ends_with(std::path::MAIN_SEPARATOR) || (cfg!(windows) && dir.ends_with('/'));
    if ends_in_sep {
        format!("{dir}{name}")
    } else {
        format!("{dir}{}{name}", std::path::MAIN_SEPARATOR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_uses_binary_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MiB");
        assert_eq!(format_size(3 * 1024_i64.pow(4)), "3.0 TiB");
    }

    #[test]
    fn negative_sizes_keep_their_sign() {
        assert_eq!(format_size(-1536), "-1.5 KiB");
        assert_eq!(format_size(-(1024_i64.pow(3) * 3 / 2)), "-1.5 GiB");
        assert_eq!(format_size(-5), "-5 B");
        assert_eq!(format_delta(2048), "+2.0 KiB");
        assert_eq!(format_delta(-2048), "-2.0 KiB");
    }

    #[test]
    fn join_display_uses_the_platform_separator() {
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(join_display("a", "b.txt"), format!("a{sep}b.txt"));
        assert_eq!(
            join_display(&format!("a{sep}"), "b.txt"),
            format!("a{sep}b.txt")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn join_display_uses_slash_on_unix() {
        assert_eq!(join_display("/tmp/x", "f"), "/tmp/x/f");
        assert_eq!(join_display("/", "f"), "/f");
    }
}
