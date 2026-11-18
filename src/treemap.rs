//! Nested squarified treemap (Bruls, Huizing and van Wijk) rendered as inline SVG.
//!
//! The layout is pure: it takes directory rows and returns rectangles, so it is testable without
//! a database. Every label and title goes through `html_escape`, and the output has no script and
//! no external reference.

use crate::fmt::format_size;
use crate::tree::html_escape;
use std::collections::HashMap;
use std::fmt::Write;

/// Width and height of the SVG `viewBox`.
pub const VIEW_W: f64 = 1000.0;
pub const VIEW_H: f64 = 600.0;
/// Rectangles smaller than this in either direction are not emitted; their bytes stay in the parent.
pub const MIN_SIDE: f64 = 4.0;

const HEADER_H: f64 = 14.0;
const PAD: f64 = 1.5;
const LABEL_MIN_W: f64 = 36.0;
const LABEL_MIN_H: f64 = 22.0;
const CHAR_W: f64 = 6.0;
const COLOR_CLASSES: usize = 6;

/// One directory row, as stored (bytes include everything below it).
#[derive(Debug, Clone)]
pub struct DirRow {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub path: String,
    pub name: String,
    pub depth: i32,
    pub bytes: i64,
}

/// An axis-aligned rectangle in `viewBox` units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Result of laying out and drawing a map.
pub struct Rendered {
    pub svg: String,
    /// Rectangles emitted, headers and file blocks included.
    pub rects: usize,
    /// Top-level folders in drawn order: `(row index, colour class)`.
    pub top: Vec<(usize, usize)>,
}

/// Squarify `areas` (descending, all positive) into `rect`, returning one rectangle per area.
///
/// The areas must already be scaled so that they sum to the rectangle's area.
pub fn squarify(areas: &[f64], rect: Rect) -> Vec<Rect> {
    let mut out = Vec::with_capacity(areas.len());
    let mut rest = rect;
    let mut i = 0;
    while i < areas.len() {
        let short = rest.w.min(rest.h);
        if short <= 0.0 {
            // No room left; give the remaining items empty rectangles at the corner.
            out.extend(std::iter::repeat_n(
                Rect {
                    x: rest.x,
                    y: rest.y,
                    w: 0.0,
                    h: 0.0,
                },
                areas.len() - i,
            ));
            break;
        }
        let worst = |sum: f64, max: f64, min: f64| -> f64 {
            let s2 = sum * sum;
            let side2 = short * short;
            (side2 * max / s2).max(s2 / (side2 * min))
        };
        let mut end = i + 1;
        let mut sum = areas[i];
        let mut best = worst(sum, areas[i], areas[i]);
        while end < areas.len() {
            let next_sum = sum + areas[end];
            let candidate = worst(next_sum, areas[i], areas[end]);
            if candidate > best {
                break;
            }
            best = candidate;
            sum = next_sum;
            end += 1;
        }
        if rest.w >= rest.h {
            // A column on the left, items stacked top to bottom.
            let col_w = sum / rest.h;
            let mut y = rest.y;
            for &a in &areas[i..end] {
                let h = a / col_w;
                out.push(Rect {
                    x: rest.x,
                    y,
                    w: col_w,
                    h,
                });
                y += h;
            }
            rest.x += col_w;
            rest.w -= col_w;
        } else {
            // A row along the top, items laid left to right.
            let row_h = sum / rest.w;
            let mut x = rest.x;
            for &a in &areas[i..end] {
                let w = a / row_h;
                out.push(Rect {
                    x,
                    y: rest.y,
                    w,
                    h: row_h,
                });
                x += w;
            }
            rest.y += row_h;
            rest.h -= row_h;
        }
        i = end;
    }
    out
}

enum Item {
    Dir(usize),
    Files(i64),
}

struct Ctx<'a> {
    rows: &'a [DirRow],
    children: HashMap<i64, Vec<usize>>,
    max_depth: i32,
    svg: String,
    rects: usize,
}

impl Ctx<'_> {
    /// Children of `idx` as items sorted by size, with the direct-file remainder as one block.
    fn items(&self, idx: usize) -> Vec<(i64, Item)> {
        let row = &self.rows[idx];
        let mut items: Vec<(i64, Item)> = Vec::new();
        let mut child_sum = 0i64;
        if let Some(kids) = self.children.get(&row.id) {
            for &k in kids {
                child_sum += self.rows[k].bytes.max(0);
                if self.rows[k].bytes > 0 {
                    items.push((self.rows[k].bytes, Item::Dir(k)));
                }
            }
        }
        let files_here = row.bytes - child_sum;
        if files_here > 0 {
            items.push((files_here, Item::Files(files_here)));
        }
        items.sort_by(|a, b| {
            b.0.cmp(&a.0).then_with(|| match (&a.1, &b.1) {
                (Item::Dir(x), Item::Dir(y)) => self.rows[*x].path.cmp(&self.rows[*y].path),
                (Item::Dir(_), Item::Files(_)) => std::cmp::Ordering::Less,
                (Item::Files(_), Item::Dir(_)) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            })
        });
        items
    }

    fn tile(&self, items: &[(i64, Item)], inner: Rect) -> Vec<Rect> {
        let total: i64 = items.iter().map(|i| i.0).sum();
        if total <= 0 || inner.w <= 0.0 || inner.h <= 0.0 {
            return Vec::new();
        }
        let scale = inner.w * inner.h / total as f64;
        let areas: Vec<f64> = items.iter().map(|i| i.0 as f64 * scale).collect();
        squarify(&areas, inner)
    }

    fn emit_rect(&mut self, r: Rect, class: &str, title: &str) {
        self.rects += 1;
        let _ = writeln!(
            self.svg,
            "<rect class=\"{class}\" x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\"><title>{}</title></rect>",
            r.x,
            r.y,
            r.w,
            r.h,
            html_escape(title)
        );
    }

    fn emit_label(&mut self, r: Rect, name: &str) {
        let max_chars = ((r.w - 6.0) / CHAR_W).floor().max(1.0) as usize;
        let count = name.chars().count();
        let text: String = if count > max_chars {
            let mut t: String = name.chars().take(max_chars.saturating_sub(1)).collect();
            t.push('…');
            t
        } else {
            name.to_owned()
        };
        let _ = writeln!(
            self.svg,
            "<text class=\"hd\" x=\"{:.1}\" y=\"{:.1}\">{}</text>",
            r.x + 3.0,
            r.y + 10.5,
            html_escape(&text)
        );
    }

    /// Draw directory `idx` in `r`, then its children inside it.
    fn place(&mut self, idx: usize, r: Rect, level: i32, color: usize) {
        let row = &self.rows[idx];
        let title = format!("{} · {}", row.path, format_size(row.bytes));
        let name = row.name.clone();
        let class = format!("c{color} l{}", level.min(5));
        self.emit_rect(r, &class, &title);
        let labelled = r.w >= LABEL_MIN_W && r.h >= LABEL_MIN_H;
        if labelled {
            self.emit_label(r, &name);
        }
        if level >= self.max_depth {
            return;
        }
        let items = self.items(idx);
        if items.is_empty() {
            return;
        }
        let top = if labelled { HEADER_H } else { PAD };
        let inner = Rect {
            x: r.x + PAD,
            y: r.y + top,
            w: r.w - 2.0 * PAD,
            h: r.h - top - PAD,
        };
        let rects = self.tile(&items, inner);
        for ((_, item), cr) in items.into_iter().zip(rects) {
            if cr.w < MIN_SIDE || cr.h < MIN_SIDE {
                continue;
            }
            match item {
                Item::Dir(k) => self.place(k, cr, level + 1, color),
                Item::Files(bytes) => {
                    let title = format!(
                        "{} · files here · {}",
                        self.rows[idx].path,
                        format_size(bytes)
                    );
                    let class = format!("c{color} f l{}", (level + 1).min(5));
                    self.emit_rect(cr, &class, &title);
                }
            }
        }
    }
}

/// Lay out and draw the map for `rows` (all directories down to `max_depth`).
///
/// The row with `depth == 0` is the root and is not drawn as a rectangle; its children fill the
/// whole view. Returns `None` when the snapshot has no root or nothing with bytes.
pub fn render(rows: &[DirRow], max_depth: i32) -> Option<Rendered> {
    let root = rows.iter().position(|r| r.depth == 0)?;
    let mut children: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if let Some(p) = r.parent_id {
            children.entry(p).or_default().push(i);
        }
    }
    let mut ctx = Ctx {
        rows,
        children,
        max_depth,
        svg: String::with_capacity(64 * 1024),
        rects: 0,
    };
    let items = ctx.items(root);
    if items.is_empty() {
        return None;
    }
    let view = Rect {
        x: 0.0,
        y: 0.0,
        w: VIEW_W,
        h: VIEW_H,
    };
    let rects = ctx.tile(&items, view);
    let mut top = Vec::new();
    let mut next_color = 0usize;
    for ((_, item), r) in items.into_iter().zip(rects) {
        // Colours go by rank among folders, so the legend and the map agree even when a folder
        // is too small to draw.
        match item {
            Item::Dir(k) => {
                let color = next_color % COLOR_CLASSES;
                next_color += 1;
                if r.w >= MIN_SIDE && r.h >= MIN_SIDE {
                    top.push((k, color));
                    ctx.place(k, r, 1, color);
                }
            }
            Item::Files(bytes) => {
                if r.w >= MIN_SIDE && r.h >= MIN_SIDE {
                    let title = format!(
                        "{} · files here · {}",
                        ctx.rows[root].path,
                        format_size(bytes)
                    );
                    ctx.emit_rect(r, "cf f l1", &title);
                }
            }
        }
    }
    Some(Rendered {
        svg: ctx.svg,
        rects: ctx.rects,
        top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(r: &Rect) -> f64 {
        r.w * r.h
    }

    #[test]
    fn squarify_tiles_the_rectangle_without_overlap() {
        let bounds = Rect {
            x: 10.0,
            y: 20.0,
            w: 600.0,
            h: 400.0,
        };
        let weights = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let total: f64 = weights.iter().sum();
        let areas: Vec<f64> = weights
            .iter()
            .map(|w| w / total * bounds.w * bounds.h)
            .collect();
        let rects = squarify(&areas, bounds);
        assert_eq!(rects.len(), areas.len());
        let sum: f64 = rects.iter().map(area).sum();
        assert!((sum - bounds.w * bounds.h).abs() < 1e-6);
        for (r, a) in rects.iter().zip(&areas) {
            assert!((area(r) - a).abs() < 1e-6);
            assert!(r.x >= bounds.x - 1e-9 && r.y >= bounds.y - 1e-9);
            assert!(r.x + r.w <= bounds.x + bounds.w + 1e-6);
            assert!(r.y + r.h <= bounds.y + bounds.h + 1e-6);
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
                let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
                assert!(ox <= 1e-6 || oy <= 1e-6, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn squarify_keeps_aspect_ratios_moderate() {
        // The classic example from the paper: 6, 6, 4, 3, 2, 2, 1 in a 6 by 4 box.
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: 6.0,
            h: 4.0,
        };
        let rects = squarify(&[6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0], bounds);
        for r in &rects {
            let ratio = (r.w / r.h).max(r.h / r.w);
            assert!(ratio < 3.0, "{r:?} is too thin");
        }
        // A plain slice-and-dice layout would give the first item a 1:4 or worse strip.
        assert_eq!(rects[0].w, 3.0);
        assert_eq!(rects[0].h, 2.0);
    }

    fn row(id: i64, parent: Option<i64>, path: &str, depth: i32, bytes: i64) -> DirRow {
        DirRow {
            id,
            parent_id: parent,
            name: path.rsplit('/').next().unwrap().to_owned(),
            path: path.to_owned(),
            depth,
            bytes,
        }
    }

    #[test]
    fn nested_layout_draws_children_and_files_blocks() {
        let rows = vec![
            row(1, None, "/r", 0, 1000),
            row(2, Some(1), "/r/a", 1, 600),
            row(3, Some(1), "/r/b", 1, 300),
            row(4, Some(2), "/r/a/x", 2, 400),
        ];
        // /r has 100 bytes of its own files; /r/a has 200 of its own beside /r/a/x.
        let out = render(&rows, 4).unwrap();
        assert_eq!(out.top.len(), 2);
        assert!(out.svg.contains("/r/a/x"));
        assert!(out.svg.contains("files here"));
        // Depth 1 limits the map to top-level folders (plus the root file block).
        let flat = render(&rows, 1).unwrap();
        assert!(!flat.svg.contains("/r/a/x"));
        assert!(flat.rects < out.rects);
    }

    #[test]
    fn tiny_rectangles_are_not_emitted() {
        let mut rows = vec![
            row(1, None, "/r", 0, 1_000_000),
            row(2, Some(1), "/r/big", 1, 999_000),
        ];
        rows.push(row(3, Some(1), "/r/tiny", 1, 1));
        let out = render(&rows, 4).unwrap();
        assert!(out.svg.contains("/r/big"));
        assert!(!out.svg.contains("/r/tiny"));
    }

    #[test]
    fn titles_and_labels_are_escaped() {
        let rows = vec![
            row(1, None, "/r", 0, 100),
            row(2, Some(1), "/r/<script>alert(1)</script>", 1, 100),
            row(3, Some(2), "/r/<script>alert(1)</script>/\"q\"", 2, 40),
        ];
        let out = render(&rows, 4).unwrap();
        assert!(!out.svg.contains("<script"));
        assert!(out.svg.contains("&lt;script&gt;"));
        assert!(out.svg.contains("&quot;q&quot;"));
    }

    #[test]
    fn empty_snapshot_has_no_map() {
        assert!(render(&[row(1, None, "/r", 0, 0)], 4).is_none());
        assert!(render(&[], 4).is_none());
    }
}
