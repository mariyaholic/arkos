//! Compares the USN reader with the walk on one tree. Windows only, and it needs an elevated
//! prompt on an NTFS volume, so it is ignored by default:
//!
//! `cargo test --release --test mft_windows -- --ignored`
//!
//! This test has not been run: the build host for this change has no Windows machine.
#![cfg(windows)]

mod support;

use arkos::scan::mft::mft_into_snapshot;
use arkos::scan::paths::normalize_display_path;
use arkos::scan::walk::{walk_into_snapshot, ScanFilter};
use arkos::volumes::{is_elevated, is_ntfs};
use rusqlite::{params, Connection};
use std::collections::BTreeSet;
use std::path::{Component, Path};
use support::fixture::{generate, Spec};
use support::{temp_dir, Env};

fn snapshot_row(conn: &Connection, root: &str, mode: &str) -> i64 {
    conn.execute(
        "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
         VALUES(?1, ?2, ?3, 1, 'running', 0)",
        params![root, root, mode],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// `(directory relative to the tree, file name, size)` for files under `tree`.
fn files_under(conn: &Connection, snapshot: i64, tree: &str) -> BTreeSet<(String, String, i64)> {
    let prefix = tree.trim_end_matches('\\').to_lowercase();
    let mut stmt = conn
        .prepare(
            "SELECT d.path, f.name, f.size FROM arkos_files f
             JOIN arkos_dirs d ON d.id = f.dir_id WHERE f.snapshot_id = ?1",
        )
        .unwrap();
    let rows = stmt
        .query_map([snapshot], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .unwrap();
    rows.map(|r| r.unwrap())
        .filter(|(dir, _, _)| dir.to_lowercase().starts_with(&prefix))
        .map(|(dir, name, size)| {
            (
                dir[prefix.len()..].to_lowercase(),
                name.to_lowercase(),
                size,
            )
        })
        .collect()
}

#[test]
#[ignore = "needs an elevated prompt on an NTFS volume"]
fn walk_matches_mft_on_the_same_tree() {
    let scratch = temp_dir("mft-tree");
    let tree = std::fs::canonicalize(&scratch).unwrap();
    let tree_display = normalize_display_path(&tree);
    if !is_elevated() || !is_ntfs(&tree) {
        eprintln!("skipped: needs an elevated token and an NTFS volume");
        return;
    }
    generate(
        &tree,
        &Spec {
            files: 400,
            dirs: 40,
            depth: 4,
            seed: 5,
        },
    )
    .unwrap();

    let volume_root = match tree.components().next() {
        Some(Component::Prefix(p)) => {
            format!("{}\\", normalize_display_path(Path::new(p.as_os_str())))
        }
        _ => panic!("tree has no drive prefix: {tree_display}"),
    };

    let mut env = Env::new("mft-compare");
    let walk_id = snapshot_row(&env.conn, &tree_display, "walk");
    walk_into_snapshot(
        &mut env.conn,
        walk_id,
        &tree,
        ScanFilter::default(),
        &mut arkos::progress::Reporter::off(),
    )
    .unwrap();

    let mft_id = snapshot_row(&env.conn, &volume_root, "mft");
    let outcome = mft_into_snapshot(
        &mut env.conn,
        mft_id,
        Path::new(&volume_root),
        ScanFilter::default(),
        &mut arkos::progress::Reporter::off(),
        false,
    )
    .unwrap();
    assert_eq!(
        outcome.mode, "mft",
        "the USN reader should have run: {:?}",
        outcome.note
    );

    let from_walk = files_under(&env.conn, walk_id, &tree_display);
    let from_mft = files_under(&env.conn, mft_id, &tree_display);
    assert_eq!(from_walk.len(), 400);
    assert_eq!(from_walk, from_mft);
    let _ = std::fs::remove_dir_all(&scratch);
}
