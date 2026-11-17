mod support;

use arkos::db::retention::{plan_prune, prune_completed};
use arkos::fmt::SIZE_NOTE;
use arkos::tree::render_tree_html;
use rusqlite::{params, Connection};
use std::process::Command;
use support::{temp_dir, Env};

fn insert(conn: &Connection, volume: &str, root: &str, state: &str, at: i64) -> i64 {
    conn.execute(
        "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
         VALUES(?1, ?2, 'walk', 0, ?3, ?4)",
        params![volume, root, state, at],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[test]
fn retention_keeps_n_per_volume_and_root() {
    let env = Env::new("retroot");
    let c = &env.conn;
    // Full-volume snapshots, oldest first, then three newer subtree scans of the same volume.
    let full: Vec<i64> = (1..=2)
        .map(|i| insert(c, "C:", "C:\\", "complete", 100 + i))
        .collect();
    let sub: Vec<i64> = (1..=3)
        .map(|i| insert(c, "C:", "C:\\Users", "complete", 200 + i))
        .collect();
    let running = insert(c, "C:", "C:\\Users", "running", 300);

    let plan = plan_prune(c, Some("C:"), 2).unwrap();
    let ids: Vec<i64> = plan.iter().map(|p| p.id).collect();
    // Only the oldest subtree snapshot goes; both full-volume snapshots survive.
    assert_eq!(ids, vec![sub[0]]);

    assert_eq!(prune_completed(c, None, 2).unwrap(), 1);
    for id in full.iter().chain(&sub[1..]).chain(&[running]) {
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM arkos_snapshots WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "snapshot {id} should remain");
    }
}

#[test]
fn failed_and_abandoned_rows_go_only_when_older_than_the_kept_set() {
    let env = Env::new("retfail");
    let c = &env.conn;
    let old_failed = insert(c, "C:", "C:\\", "failed", 10);
    let old_abandoned = insert(c, "C:", "C:\\", "abandoned", 11);
    let _c1 = insert(c, "C:", "C:\\", "complete", 20);
    let _c2 = insert(c, "C:", "C:\\", "complete", 30);
    let new_failed = insert(c, "C:", "C:\\", "failed", 40);
    // A different root with a single old failure and no completed snapshot: nothing to compare.
    let lone_failed = insert(c, "C:", "C:\\Other", "failed", 5);

    let ids: Vec<i64> = plan_prune(c, None, 2)
        .unwrap()
        .iter()
        .map(|p| p.id)
        .collect();
    assert!(ids.contains(&old_failed));
    assert!(ids.contains(&old_abandoned));
    assert!(!ids.contains(&new_failed));
    assert!(!ids.contains(&lone_failed));
    assert_eq!(ids.len(), 2);
}

#[test]
fn html_report_carries_the_size_note() {
    let mut env = Env::new("note");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("a.bin"), b"abc").unwrap();
    let s = env.scan(&tree);
    let html = render_tree_html(&env.conn, s.snapshot_id, 4).unwrap();
    assert!(html.contains(SIZE_NOTE), "note missing");
    assert!(html.contains("Hardlinked files may be counted more than once"));
    assert!(html.contains("allocated size may be smaller"));
}

#[test]
fn scan_text_prints_the_note_and_json_names_the_basis() {
    let root = temp_dir("note-cli");
    let home = root.join("home");
    let tree = root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("x.tar.gz"), b"12345").unwrap();
    let vol = tree.to_string_lossy().into_owned();
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_arkos"))
            .env("ARKOS_HOME", &home)
            .args(["scan", "--volume", &vol, "--mode", "walk"])
            .args(extra)
            .output()
            .unwrap()
    };
    let text = String::from_utf8(run(&[]).stdout).unwrap();
    assert!(text.contains(&format!("note: {SIZE_NOTE}")), "{text}");
    let v: serde_json::Value = serde_json::from_slice(&run(&["--json"]).stdout).unwrap();
    assert_eq!(v["size_basis"], "logical_per_name");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scan_stores_compound_archive_extensions_whole() {
    let mut env = Env::new("ext");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("backup.tar.gz"), b"1").unwrap();
    std::fs::write(tree.join("pack.TAR.ZST"), b"1").unwrap();
    std::fs::write(tree.join("notes.txt"), b"1").unwrap();
    std::fs::write(tree.join(".tar.gz"), b"1").unwrap();
    let s = env.scan(&tree);
    let mut stmt = env
        .conn
        .prepare("SELECT name, ext FROM arkos_files WHERE snapshot_id = ?1 ORDER BY name")
        .unwrap();
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map([s.snapshot_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let ext = |n: &str| rows.iter().find(|r| r.0 == n).unwrap().1.clone();
    assert_eq!(ext("backup.tar.gz").as_deref(), Some("tar.gz"));
    assert_eq!(ext("pack.TAR.ZST").as_deref(), Some("tar.zst"));
    assert_eq!(ext("notes.txt").as_deref(), Some("txt"));
    assert_eq!(ext(".tar.gz").as_deref(), Some("gz"));
}
