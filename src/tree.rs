//! Static self-contained HTML + SVG treemap renderer (§3, zero network/JS dependencies).
//!
//! Styling follows the estate canon (`design/uika-canonical.css`, ADR-0011): Doloris-face night
//! grounds, gold as the one accent, red lining for warnings, canon font stacks with system
//! fallbacks and no webfont request.

use crate::db::map_db_err;
use crate::exit::{Error, Result};
pub use crate::fmt::format_size;
use crate::fmt::{join_display, SIZE_NOTE};
use crate::treemap::{self, DirRow};
use rusqlite::{params, Connection};
use std::fmt::Write;
use std::path::Path;

/// Canon tokens inlined into every report, so the file opens offline with no external sheet.
const STYLE: &str = r#":root{
--bg-deepest:#060509;--bg-ground:#0b0913;--bg-panel:#14101c;--bg-panel-2:#1a1424;
--line:#2e2440;--line-subtle:#221a30;--line-strong:#3c2f52;
--gold:#bb9955;--gold-bright:#d8b878;--gold-dim:#7d6738;--gold-glow:rgba(187,153,85,.16);
--lavender:#a497d6;--platinum:#e9e4da;--marigold:#e08d3c;--lining:#a2434d;--lining-dim:rgba(162,67,77,.16);
--sage:#6e9c7e;--wine:#881144;--text:#d8d4ce;--muted:#97908f;--slate:#6d6676;
--font-display:"Cinzel","Palatino Linotype","Times New Roman",serif;
--font-body:"Outfit","Segoe UI",system-ui,sans-serif;
--font-mono:"JetBrains Mono","Cascadia Mono",Consolas,ui-monospace,monospace;
--radius-sm:4px;--radius-md:6px;--radius-lg:12px;color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;background:var(--bg-ground);color:var(--text);font:16px/1.5 var(--font-body)}
main{max-width:1100px;margin:0 auto;padding:32px 16px 48px}
.kicker{margin:0;font:500 .75rem var(--font-mono);letter-spacing:.08em;text-transform:uppercase;color:var(--gold)}
h1{margin:.25rem 0 .5rem;font:600 clamp(1.4rem,3.4vw,2.1rem) var(--font-display);color:var(--platinum);overflow-wrap:anywhere}
h2{margin:2.5rem 0 .75rem;font:600 1rem var(--font-body);color:var(--platinum);letter-spacing:.02em}
.meta{display:flex;flex-wrap:wrap;gap:8px 24px;margin:0;padding:0;list-style:none;font:.8rem var(--font-mono);color:var(--muted)}
.meta b{color:var(--gold-bright);font-weight:500}
.note{margin:.75rem 0 0;color:var(--muted);font-size:.8rem}
.warn{margin:1rem 0 0;padding:8px 12px;border-left:3px solid var(--lining);background:var(--lining-dim);color:var(--platinum);border-radius:var(--radius-sm)}
.panel{margin-top:1.5rem;padding:16px;background:var(--bg-panel);border:1px solid var(--line);border-radius:var(--radius-lg)}
svg{display:block;width:100%;height:auto}
svg rect{stroke:var(--bg-panel);stroke-width:1}
.c0{fill:var(--gold)}.c1{fill:var(--lavender)}.c2{fill:var(--sage)}.c3{fill:var(--marigold)}.c4{fill:var(--gold-dim)}.c5{fill:var(--platinum)}.cf{fill:var(--slate)}
.l1{fill-opacity:.88}.l2{fill-opacity:.74}.l3{fill-opacity:.62}.l4{fill-opacity:.52}.l5{fill-opacity:.44}
svg rect.f{fill-opacity:.26}
.hd{font:10px var(--font-mono);fill:var(--platinum);stroke:var(--bg-deepest);stroke-width:2.5px;paint-order:stroke;pointer-events:none}
.legend{display:grid;grid-template-columns:repeat(auto-fill,minmax(220px,1fr));gap:6px 16px;margin:12px 0 0;padding:0;list-style:none;font-size:.85rem}
.legend li{display:flex;align-items:baseline;gap:8px;min-width:0}
.sw{flex:none;width:10px;height:10px;border-radius:2px}
.legend .p{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--font-mono)}
.legend .n{color:var(--muted);font:.8rem var(--font-mono);font-variant-numeric:tabular-nums}
.scroll{overflow-x:auto;border:1px solid var(--line);border-radius:var(--radius-md)}
table{width:100%;border-collapse:collapse;font-size:.85rem}
th{position:sticky;top:0;background:var(--bg-panel-2);color:var(--muted);font:500 .72rem var(--font-mono);text-transform:uppercase;letter-spacing:.06em;text-align:left}
th,td{padding:7px 12px;border-bottom:1px solid var(--line-subtle)}
tbody tr:hover{background:var(--bg-panel)}
td.path{font-family:var(--font-mono);overflow-wrap:anywhere}
td.num,th.num{text-align:right;font-family:var(--font-mono);font-variant-numeric:tabular-nums;white-space:nowrap}
td.dim{color:var(--slate)}
"#;

/// Fill ramp for top-level folders, drawn from canon roles. The map uses the matching `.c0` to
/// `.c5` classes; the legend swatches use these values. Red lining is left out on purpose:
/// it is reserved for warnings.
const FILLS: [&str; 6] = [
    "var(--gold)",
    "var(--lavender)",
    "var(--sage)",
    "var(--marigold)",
    "var(--gold-dim)",
    "var(--platinum)",
];

/// Escape HTML special characters so untrusted filenames cannot inject markup or scripts.
pub fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            // Bidi controls could reorder the line and make a folder name look like another
            // path, so they are shown as visible text instead of being passed through.
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' => {
                out.push_str(&format!("&lt;U+{:04X}&gt;", ch as u32));
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Show `path` relative to the snapshot root (`.` for the root itself); paths outside the root
/// come back unchanged.
pub fn relative(path: &str, root: &str) -> String {
    match path.strip_prefix(root.trim_end_matches(['\\', '/'])) {
        Some(rest) if rest.is_empty() || rest.starts_with(['\\', '/']) => {
            let rest = rest.trim_start_matches(['\\', '/']);
            if rest.is_empty() {
                ".".to_string()
            } else {
                rest.to_string()
            }
        }
        _ => path.to_string(),
    }
}

/// Render a static HTML + SVG report for `snapshot_id` up to `max_depth`.
pub fn render_tree_html(conn: &Connection, snapshot_id: i64, max_depth: i32) -> Result<String> {
    crate::db::read_tx(conn, || {
        render_tree_html_inner(conn, snapshot_id, max_depth)
    })
}

fn render_tree_html_inner(conn: &Connection, snapshot_id: i64, max_depth: i32) -> Result<String> {
    let (volume, root, total_bytes, denied): (String, String, i64, i64) = conn
        .query_row(
            "SELECT volume, root, bytes, denied FROM arkos_snapshots WHERE id = ?1 AND state = 'complete'",
            [snapshot_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::Input(format!("completed snapshot {snapshot_id} not found"))
            }
            other => map_db_err(other),
        })?;

    let mut stmt = conn
        .prepare(
            "SELECT path, name, depth, bytes, files
             FROM arkos_dirs
             WHERE snapshot_id = ?1 AND depth <= ?2
             ORDER BY bytes DESC, depth ASC, path ASC
             LIMIT 200",
        )
        .map_err(map_db_err)?;

    let rows: Vec<(String, String, i32, i64, i64)> = stmt
        .query_map(params![snapshot_id, max_depth], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(map_db_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_db_err)?;

    // Also include largest leaf files up to 50 rows so tricky file names appear and stay escaped.
    let mut fstmt = conn
        .prepare(
            "SELECT d.path, f.name, f.size
             FROM arkos_files f
             JOIN arkos_dirs d ON d.id = f.dir_id
             WHERE f.snapshot_id = ?1
             ORDER BY f.size DESC, f.name ASC
             LIMIT 50",
        )
        .map_err(map_db_err)?;
    let files: Vec<(String, String, i64)> = fstmt
        .query_map([snapshot_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(map_db_err)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(map_db_err)?;

    let total_files: i64 = rows.iter().filter(|r| r.2 == 0).map(|r| r.4).sum();
    let vol = html_escape(&volume);
    let root_e = html_escape(&root);

    let mut html = String::with_capacity(16384);
    writeln!(
        html,
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"color-scheme\" content=\"dark\">"
    )
    .ok();
    writeln!(html, "<title>Arkos · {vol} · #{snapshot_id}</title>").ok();
    writeln!(html, "<style>{STYLE}</style></head><body><main>").ok();
    writeln!(
        html,
        "<p class=\"kicker\">Arkos · snapshot #{snapshot_id}</p>"
    )
    .ok();
    writeln!(html, "<h1>{vol}</h1>").ok();
    writeln!(
        html,
        "<ul class=\"meta\"><li>root <b>{root_e}</b></li><li>size <b>{}</b></li>",
        format_size(total_bytes)
    )
    .ok();
    if total_files > 0 {
        writeln!(html, "<li>files <b>{total_files}</b></li>").ok();
    }
    writeln!(html, "</ul>").ok();
    if denied > 0 {
        writeln!(
            html,
            "<p class=\"warn\">{denied} directories were not readable.</p>"
        )
        .ok();
    }

    writeln!(html, "<p class=\"note\">{}</p>", html_escape(SIZE_NOTE)).ok();

    // Nested squarified treemap over every directory down to `max_depth`.
    let dir_rows: Vec<DirRow> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, parent_id, path, name, depth, bytes
                 FROM arkos_dirs WHERE snapshot_id = ?1 AND depth <= ?2
                 ORDER BY depth ASC, id ASC",
            )
            .map_err(map_db_err)?;
        let mapped = stmt
            .query_map(params![snapshot_id, max_depth], |r| {
                Ok(DirRow {
                    id: r.get(0)?,
                    parent_id: r.get(1)?,
                    path: r.get(2)?,
                    name: r.get(3)?,
                    depth: r.get(4)?,
                    bytes: r.get(5)?,
                })
            })
            .map_err(map_db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_db_err)?;
        mapped
    };
    let map = treemap::render(&dir_rows, max_depth);
    writeln!(html, "<h2>Treemap</h2><section class=\"panel\">").ok();
    let denom = total_bytes.max(1) as f64;
    match &map {
        Some(map) => {
            writeln!(
                html,
                "<svg viewBox=\"0 0 {} {}\" role=\"img\" aria-label=\"Nested treemap of folder sizes\">",
                treemap::VIEW_W,
                treemap::VIEW_H
            )
            .ok();
            html.push_str(&map.svg);
            writeln!(html, "</svg><ul class=\"legend\">").ok();
            for (idx, color) in map.top.iter().take(12) {
                let row = &dir_rows[*idx];
                let pct = row.bytes as f64 * 100.0 / denom;
                writeln!(
                    html,
                    "<li><span class=\"sw\" style=\"background:{}\"></span><span class=\"p\" title=\"{}\">{}</span><span class=\"n\">{} · {pct:.1}%</span></li>",
                    FILLS[*color % FILLS.len()],
                    html_escape(&row.path),
                    html_escape(&row.name),
                    format_size(row.bytes)
                )
                .ok();
            }
        }
        None => {
            writeln!(html, "<p class=\"note\">Nothing to draw: the snapshot holds no bytes.</p><ul class=\"legend\">").ok();
        }
    }
    writeln!(html, "</ul></section>").ok();

    writeln!(
        html,
        "<h2>Folders · depth ≤ {max_depth}</h2><div class=\"scroll\"><table>\
         <thead><tr><th class=\"num\">Depth</th><th>Path</th><th class=\"num\">Size</th><th class=\"num\">Files</th></tr></thead><tbody>"
    )
    .ok();
    for (path, _, depth, bytes, count) in &rows {
        writeln!(
            html,
            "<tr><td class=\"num dim\">{depth}</td><td class=\"path\" title=\"{}\">{}</td><td class=\"num\" title=\"{bytes} B\">{}</td><td class=\"num\">{count}</td></tr>",
            html_escape(path),
            html_escape(&relative(path, &root)),
            format_size(*bytes)
        )
        .ok();
    }
    writeln!(
        html,
        "</tbody></table></div><h2>Largest files</h2><div class=\"scroll\"><table>\
         <thead><tr><th>File</th><th class=\"num\">Size</th></tr></thead><tbody>"
    )
    .ok();
    for (dir_path, name, size) in &files {
        let full = join_display(dir_path, name);
        writeln!(
            html,
            "<tr><td class=\"path\" title=\"{}\">{}</td><td class=\"num\" title=\"{size} B\">{}</td></tr>",
            html_escape(&full),
            html_escape(&relative(&full, &root)),
            format_size(*size)
        )
        .ok();
    }
    writeln!(html, "</tbody></table></div></main></body></html>").ok();
    Ok(html)
}

/// Write treemap HTML to `out_path`, refusing to overwrite an existing file (exit 4).
pub fn write_tree_file(
    conn: &Connection,
    snapshot_id: i64,
    out_path: &Path,
    max_depth: i32,
) -> Result<()> {
    let html = render_tree_html(conn, snapshot_id, max_depth)?;
    if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|source| Error::Io {
            context: "create tree output directory",
            source,
        })?;
    }
    write_new_file(out_path, html.as_bytes(), "write treemap html")
}

/// Create `path` and write `bytes`, refusing to replace an existing file (exit 4). The create is
/// atomic (`create_new`), so two writers cannot both pass an existence check and then overwrite.
pub fn write_new_file(path: &Path, bytes: &[u8], context: &'static str) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                Error::Refused(format!("output file already exists: {}", path.display()))
            } else {
                Error::Io { context, source }
            }
        })?;
    file.write_all(bytes)
        .map_err(|source| Error::Io { context, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escape_shows_bidi_controls_as_visible_text() {
        assert_eq!(html_escape("a\u{202E}b"), "a&lt;U+202E&gt;b");
        assert_eq!(
            html_escape("\u{2066}x\u{2069}"),
            "&lt;U+2066&gt;x&lt;U+2069&gt;"
        );
        assert_eq!(
            html_escape("\u{200E}\u{200F}"),
            "&lt;U+200E&gt;&lt;U+200F&gt;"
        );
        assert_eq!(
            html_escape("\u{202A}\u{202D}"),
            "&lt;U+202A&gt;&lt;U+202D&gt;"
        );
        assert_eq!(html_escape("plain <b> é"), "plain &lt;b&gt; é");
    }

    #[test]
    fn relative_strips_the_root_only_at_a_separator() {
        assert_eq!(relative(r"C:\a\b", r"C:\a"), "b");
        assert_eq!(relative(r"C:\", r"C:\"), ".");
        assert_eq!(relative(r"C:\a", r"C:\a\"), ".");
        assert_eq!(relative(r"C:\ab\c", r"C:\a"), r"C:\ab\c");
        assert_eq!(relative(r"C:\x\y.txt", r"C:\"), r"x\y.txt");
    }

    #[test]
    fn style_carries_canon_tokens_and_no_network() {
        assert!(STYLE.contains("--gold:#bb9955"));
        assert!(STYLE.contains("--bg-ground:#0b0913"));
        assert!(!STYLE.contains("http"));
        assert!(!FILLS.contains(&"var(--lining)"));
        assert!(!FILLS.contains(&"var(--wine)"));
        // Only the warning block may use the red lining; the map classes never do.
        let map_css = &STYLE[STYLE.find(".c0{").unwrap()..STYLE.find(".hd{").unwrap()];
        assert!(!map_css.contains("lining") && !map_css.contains("wine"));
    }
}
