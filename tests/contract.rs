use arkos::config::Config;
use arkos::db::{self, retention};
use arkos::diff::{diff_snapshots, ChangeKind};
use arkos::exit::{Error, Exit};
use arkos::lock::VolumeLock;
use arkos::logging::Logger;
use arkos::paths::Paths;
use arkos::rollup::ROLLUP_SELECT_SQL;
use arkos::scan::{run_scan, ScanMode};
use arkos::tree::render_tree_html;
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

fn temp_dir(name: &str) -> PathBuf {
    let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("arkos-contract-{}-{name}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn migration_0001_idempotent_and_foreign_keys_cascade_on_prune() {
    let root = temp_dir("migrate");
    let paths = Paths::from_home(root.join("home"));
    paths.ensure_dirs().unwrap();
    let mut conn = db::open_and_migrate(&paths.db, &paths.locks, 1000).unwrap();
    let v = db::migrate(&conn).unwrap();
    assert_eq!(v, 1);

    let tree = root.join("tree");
    std::fs::create_dir_all(tree.join("sub")).unwrap();
    std::fs::write(tree.join("a.txt"), b"hello").unwrap();
    std::fs::write(tree.join("sub/b.bin"), b"0123456789").unwrap();

    let cfg = Config::default();
    let log = Logger::new(&paths.logs);
    let r = run_scan(
        &mut conn,
        &paths,
        &cfg,
        &log,
        &tree.to_string_lossy(),
        None,
        None,
        ScanMode::Walk,
        &[],
    )
    .unwrap();
    assert_eq!(r.files, 2);
    assert_eq!(r.bytes, 15);

    let pruned = retention::prune_completed(&conn, None, 0).unwrap();
    assert_eq!(pruned, 1);
    let dirs_left: i64 = conn
        .query_row("SELECT count(*) FROM arkos_dirs", [], |r| r.get(0))
        .unwrap();
    let files_left: i64 = conn
        .query_row("SELECT count(*) FROM arkos_files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(dirs_left, 0);
    assert_eq!(files_left, 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn newer_user_version_is_4_and_no_write() {
    let root = temp_dir("newer");
    let db_path = root.join("shimatsu.sqlite");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "user_version", 99u32).unwrap();
        conn.execute("CREATE TABLE future_tbl(x INTEGER)", [])
            .unwrap();
    }
    let err = db::open_and_migrate(&db_path, &root.join("locks"), 500).unwrap_err();
    assert_eq!(err.exit(), Exit::Refused);

    let check = Connection::open(&db_path).unwrap();
    let ver: u32 = check
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(ver, 99);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn duplicut_0002_does_not_touch_arkos_tables() {
    let root = temp_dir("0002");
    let paths = Paths::from_home(root.join("home"));
    paths.ensure_dirs().unwrap();
    let conn = db::open_and_migrate(&paths.db, &paths.locks, 500).unwrap();
    conn.execute_batch(
        "CREATE TABLE duplicut_groups(id INTEGER PRIMARY KEY, size INTEGER);
         PRAGMA user_version = 2;",
    )
    .unwrap();
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='arkos_snapshots')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(exists);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn lock_wait_times_out_as_6_and_second_scan_is_4() {
    let root = temp_dir("lock");
    let paths = Paths::from_home(root.join("home"));
    paths.ensure_dirs().unwrap();

    let held = VolumeLock::acquire(&paths.locks, "C:", 500).unwrap();
    let err = VolumeLock::acquire(&paths.locks, "C:", 80).unwrap_err();
    assert_eq!(err.exit(), Exit::Busy);
    // Different volume acquires without waiting.
    let _other = VolumeLock::acquire(&paths.locks, "D:", 80).unwrap();
    drop(held);

    let mut conn = db::open_and_migrate(&paths.db, &paths.locks, 500).unwrap();
    let tree = root.join("vol");
    std::fs::create_dir_all(&tree).unwrap();
    let vol_key = format!("dir:{}", tree.to_string_lossy());
    conn.execute(
        "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
         VALUES(?1, ?2, 'walk', 0, 'running', ?3)",
        params![
            vol_key,
            tree.to_string_lossy().as_ref(),
            chrono::Utc::now().timestamp()
        ],
    )
    .unwrap();

    let cfg = Config::default();
    let log = Logger::new(&paths.logs);
    let scan_err = run_scan(
        &mut conn,
        &paths,
        &cfg,
        &log,
        &tree.to_string_lossy(),
        None,
        None,
        ScanMode::Walk,
        &[],
    )
    .unwrap_err();
    assert_eq!(scan_err.exit(), Exit::Refused);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dir_bytes_equal_sum_of_children_and_depth_pass_is_single_scan() {
    let root = temp_dir("rollup");
    let paths = Paths::from_home(root.join("home"));
    paths.ensure_dirs().unwrap();
    let mut conn = db::open_and_migrate(&paths.db, &paths.locks, 1000).unwrap();

    let tree = root.join("tree");
    std::fs::create_dir_all(tree.join("a/b/c")).unwrap();
    std::fs::write(tree.join("top.txt"), vec![b'x'; 10]).unwrap();
    std::fs::write(tree.join("a/mid.txt"), vec![b'y'; 25]).unwrap();
    std::fs::write(tree.join("a/b/c/deep.txt"), vec![b'z'; 65]).unwrap();

    let cfg = Config::default();
    let log = Logger::new(&paths.logs);
    let rep = run_scan(
        &mut conn,
        &paths,
        &cfg,
        &log,
        &tree.to_string_lossy(),
        None,
        None,
        ScanMode::Walk,
        &[],
    )
    .unwrap();
    assert_eq!(rep.bytes, 100);
    assert_eq!(rep.files, 3);

    let (root_bytes, root_files): (i64, i64) = conn
        .query_row(
            "SELECT bytes, files FROM arkos_dirs WHERE snapshot_id = ?1 AND depth = 0",
            [rep.snapshot_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(root_bytes, 100);
    assert_eq!(root_files, 3);

    let mut plan_stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {ROLLUP_SELECT_SQL}"))
        .unwrap();
    let plan: Vec<String> = plan_stmt
        .query_map([rep.snapshot_id], |r| r.get::<_, String>(3))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let joined = plan.join(" | ");
    assert!(
        joined.contains("arkos_dirs_depth") && !joined.contains("TEMP B-TREE"),
        "unexpected query plan: {joined}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn diff_detects_grow_shrink_new_deleted_and_refuses_failed() {
    let root = temp_dir("diff");
    let paths = Paths::from_home(root.join("home"));
    paths.ensure_dirs().unwrap();
    let mut conn = db::open_and_migrate(&paths.db, &paths.locks, 1000).unwrap();

    let tree = root.join("tree");
    std::fs::create_dir_all(tree.join("grow")).unwrap();
    std::fs::create_dir_all(tree.join("shrink")).unwrap();
    std::fs::create_dir_all(tree.join("gone")).unwrap();
    std::fs::write(tree.join("grow/f.bin"), vec![1u8; 10]).unwrap();
    std::fs::write(tree.join("shrink/f.bin"), vec![1u8; 100]).unwrap();
    std::fs::write(tree.join("gone/f.bin"), vec![1u8; 40]).unwrap();

    let cfg = Config::default();
    let log = Logger::new(&paths.logs);
    let s1 = run_scan(
        &mut conn,
        &paths,
        &cfg,
        &log,
        &tree.to_string_lossy(),
        None,
        None,
        ScanMode::Walk,
        &[],
    )
    .unwrap();

    std::fs::write(tree.join("grow/f.bin"), vec![1u8; 60]).unwrap();
    std::fs::write(tree.join("shrink/f.bin"), vec![1u8; 20]).unwrap();
    std::fs::remove_dir_all(tree.join("gone")).unwrap();
    std::fs::create_dir_all(tree.join("added")).unwrap();
    std::fs::write(tree.join("added/n.bin"), vec![1u8; 30]).unwrap();

    let s2 = run_scan(
        &mut conn,
        &paths,
        &cfg,
        &log,
        &tree.to_string_lossy(),
        None,
        None,
        ScanMode::Walk,
        &[],
    )
    .unwrap();

    let d = diff_snapshots(&conn, s1.snapshot_id, s2.snapshot_id, 0, 50).unwrap();
    assert!(d
        .dirs
        .iter()
        .any(|x| x.path.ends_with("grow") && x.kind == ChangeKind::Grown && x.delta_bytes == 50));
    assert!(d.dirs.iter().any(|x| x.path.ends_with("shrink")
        && x.kind == ChangeKind::Shrunk
        && x.delta_bytes == -80));
    assert!(d.dirs.iter().any(|x| x.path.ends_with("gone")
        && x.kind == ChangeKind::Removed
        && x.delta_bytes == -40));
    assert!(d
        .dirs
        .iter()
        .any(|x| x.path.ends_with("added") && x.kind == ChangeKind::Added && x.delta_bytes == 30));

    // Interrupted / stale scan marked abandoned on next open and refused on diff with exit 3.
    let stale_at = chrono::Utc::now().timestamp() - (25 * 3600);
    conn.execute(
        "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
         VALUES(?1, ?2, 'walk', 0, 'running', ?3)",
        params![s1.volume, tree.to_string_lossy().as_ref(), stale_at],
    )
    .unwrap();
    let bad_id = conn.last_insert_rowid();
    drop(conn);

    let conn2 = db::open_and_migrate(&paths.db, &paths.locks, 1000).unwrap();
    let state: String = conn2
        .query_row(
            "SELECT state FROM arkos_snapshots WHERE id = ?1",
            [bad_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "abandoned");

    let diff_err = diff_snapshots(&conn2, s1.snapshot_id, bad_id, 0, 10).unwrap_err();
    assert_eq!(diff_err.exit(), Exit::Input);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn retention_keeps_n_and_html_escapes_script_names() {
    let root = temp_dir("ret_html");
    let paths = Paths::from_home(root.join("home"));
    paths.ensure_dirs().unwrap();
    let mut conn = db::open_and_migrate(&paths.db, &paths.locks, 1000).unwrap();

    for i in 1..=5i64 {
        conn.execute(
            "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
             VALUES('Z:', 'Z:\\', 'walk', 0, 'complete', ?1)",
            [1000 + i],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
         VALUES('Z:', 'Z:\\', 'walk', 0, 'running', 2000)",
        [],
    )
    .unwrap();

    let deleted = retention::prune_completed(&conn, Some("Z:"), 2).unwrap();
    assert_eq!(deleted, 3);
    let remaining_complete: i64 = conn
        .query_row(
            "SELECT count(*) FROM arkos_snapshots WHERE volume = 'Z:' AND state = 'complete'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let remaining_running: i64 = conn
        .query_row(
            "SELECT count(*) FROM arkos_snapshots WHERE volume = 'Z:' AND state = 'running'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining_complete, 2);
    assert_eq!(remaining_running, 1);

    let tree = root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("a & 'b'.txt"), b"x").unwrap();
    let cfg = Config::default();
    let log = Logger::new(&paths.logs);
    let s = run_scan(
        &mut conn,
        &paths,
        &cfg,
        &log,
        &tree.to_string_lossy(),
        None,
        None,
        ScanMode::Walk,
        &[],
    )
    .unwrap();
    let dir_id: i64 = conn
        .query_row(
            "SELECT id FROM arkos_dirs WHERE snapshot_id = ?1 LIMIT 1",
            [s.snapshot_id],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO arkos_files(snapshot_id, dir_id, name, size, mtime, ext)
         VALUES(?1, ?2, '<script>alert(1)</script>.txt', 42, 1000, 'txt')",
        params![s.snapshot_id, dir_id],
    )
    .unwrap();
    let html = render_tree_html(&conn, s.snapshot_id, 4).unwrap();
    assert!(html.contains("&amp; &#39;b&#39;.txt"));
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;.txt"));
    assert!(!html.contains("<script>alert(1)</script>"));
    let _ = Error::Cli(String::new());
    let _ = std::fs::remove_dir_all(&root);
}
