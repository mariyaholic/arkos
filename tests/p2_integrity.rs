mod support;

use arkos::config::Config;
use arkos::db;
use arkos::diff::{diff_snapshots, ChangeKind};
use arkos::exit::Exit;
use arkos::lock::VolumeLock;
use arkos::logging::Logger;
use arkos::scan::{resolve_mode, run_scan, ScanMode};
use rusqlite::params;
use support::Env;

fn state_of(conn: &rusqlite::Connection, id: i64) -> String {
    conn.query_row(
        "SELECT state FROM arkos_snapshots WHERE id = ?1",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

fn insert_running(conn: &rusqlite::Connection, volume: &str) -> i64 {
    conn.execute(
        "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
         VALUES(?1, 'x', 'walk', 0, 'running', ?2)",
        params![volume, chrono::Utc::now().timestamp()],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[test]
fn dead_scan_is_abandoned_on_next_open_but_a_live_one_is_kept() {
    let env = Env::new("sweep");
    let dead = insert_running(&env.conn, "dir:/dead");
    let live = insert_running(&env.conn, "dir:/live");
    let held = VolumeLock::acquire(&env.paths.locks, "dir:/live", 500).unwrap();

    let conn2 = db::open_and_migrate(&env.paths.db, 1000).unwrap();
    assert_eq!(state_of(&conn2, dead), "abandoned");
    assert_eq!(state_of(&conn2, live), "running");

    drop(held);
    drop(conn2);
    let conn3 = db::open_and_migrate(&env.paths.db, 1000).unwrap();
    assert_eq!(state_of(&conn3, live), "abandoned");
    let finished: Option<i64> = conn3
        .query_row(
            "SELECT finished_at FROM arkos_snapshots WHERE id = ?1",
            [live],
            |r| r.get(0),
        )
        .unwrap();
    assert!(finished.is_some());
}

#[test]
fn scan_excludes_the_arkos_data_directory_inside_its_root() {
    let mut env = Env::new("datadir");
    std::fs::write(env.root.join("keep.txt"), b"hello").unwrap();
    std::fs::create_dir_all(env.root.join("docs")).unwrap();
    std::fs::write(env.root.join("docs/a.txt"), b"abc").unwrap();
    // The ledger, its WAL and the log live in `<root>/home`; the scan root is `<root>`.
    Logger::new(&env.paths.logs).event("probe", "seed the log directory");
    let root = env.root.clone();
    let rep = env.scan(&root);
    let home = env.paths.home.to_string_lossy().into_owned();

    let rows: Vec<String> = env
        .conn
        .prepare("SELECT path FROM arkos_dirs WHERE snapshot_id = ?1")
        .unwrap()
        .query_map([rep.snapshot_id], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        rows.iter().all(|p| !p.starts_with(&home)),
        "data directory leaked into the snapshot: {rows:?}"
    );
    assert_eq!(rep.files, 2);
    assert_eq!(rep.bytes, 8);
    assert!(rep.skipped >= 1);
    let log = std::fs::read_to_string(env.paths.logs.join("arkos.log")).unwrap();
    assert!(log.contains("excluded_data_dir"));
}

fn big_tree(env: &Env) -> std::path::PathBuf {
    let tree = env.root.join("big");
    for d in 0..6 {
        let dir = tree.join(format!("d{d}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..400 {
            std::fs::write(dir.join(format!("f{f}.txt")), b"x").unwrap();
        }
    }
    tree
}

fn run_walk(env: &mut Env, tree: &std::path::Path) -> arkos::exit::Result<arkos::scan::ScanReport> {
    let cfg = Config::default();
    let log = Logger::new(&env.paths.logs);
    run_scan(
        &mut env.conn,
        &env.paths,
        &cfg,
        &log,
        &tree.to_string_lossy(),
        None,
        None,
        ScanMode::Walk,
        &[],
    )
}

const FAIL_AT_1500: &str = "CREATE TRIGGER fail_mid_scan BEFORE INSERT ON arkos_files
     WHEN (SELECT count(*) FROM arkos_files WHERE snapshot_id = NEW.snapshot_id) >= 1500
     BEGIN SELECT RAISE(ABORT, 'disk full'); END;";

#[test]
fn database_error_mid_scan_leaves_failed_and_no_partial_rows() {
    let mut env = Env::new("dbfail");
    let tree = big_tree(&env);
    env.conn.execute_batch(FAIL_AT_1500).unwrap();
    let err = run_walk(&mut env, &tree).unwrap_err();
    assert_eq!(err.exit(), Exit::Io);

    let (state, id): (String, i64) = env
        .conn
        .query_row("SELECT state, id FROM arkos_snapshots", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(state, "failed");
    for table in ["arkos_files", "arkos_dirs"] {
        let n: i64 = env
            .conn
            .query_row(
                &format!("SELECT count(*) FROM {table} WHERE snapshot_id = ?1"),
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "{table} kept partial rows");
    }
}

#[test]
fn failed_cleanup_leaves_running_and_the_next_open_abandons_it() {
    let mut env = Env::new("dbfail2");
    let tree = big_tree(&env);
    env.conn.execute_batch(FAIL_AT_1500).unwrap();
    env.conn
        .execute_batch(
            "CREATE TRIGGER no_cleanup BEFORE DELETE ON arkos_files
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
    run_walk(&mut env, &tree).unwrap_err();
    let (state, id): (String, i64) = env
        .conn
        .query_row("SELECT state, id FROM arkos_snapshots", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(state, "running");
    let partial: i64 = env
        .conn
        .query_row("SELECT count(*) FROM arkos_files", [], |r| r.get(0))
        .unwrap();
    assert!(partial > 0, "the fixture should have committed a batch");

    let conn2 = db::open_and_migrate(&env.paths.db, 1000).unwrap();
    assert_eq!(state_of(&conn2, id), "abandoned");
}

#[cfg(unix)]
#[test]
fn non_unicode_names_are_skipped_counted_and_logged() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let mut env = Env::new("nonutf8");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("ok.txt"), b"1234").unwrap();
    let bad = tree.join(OsStr::from_bytes(b"bad-\xff-name.txt"));
    std::fs::write(&bad, b"zz").unwrap();
    let bad_dir = tree.join(OsStr::from_bytes(b"dir-\xfe"));
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(bad_dir.join("inner.txt"), b"q").unwrap();

    let rep = env.scan(&tree);
    assert_eq!(rep.files, 1);
    assert_eq!(rep.bytes, 4);
    assert_eq!(rep.dirs, 1);
    assert_eq!(rep.skipped, 2);
    let log = std::fs::read_to_string(env.paths.logs.join("arkos.log")).unwrap();
    assert!(log.contains("skipped_non_unicode"));
    assert!(log.contains("bad-\u{fffd}-name.txt"), "{log}");
}

#[test]
fn case_only_rename_shows_as_one_removed_and_one_added() {
    let mut env = Env::new("case");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(tree.join("Foo")).unwrap();
    std::fs::write(tree.join("Foo/Report.txt"), vec![0u8; 100]).unwrap();
    let a = env.scan(&tree);
    std::fs::rename(tree.join("Foo/Report.txt"), tree.join("Foo/report.txt")).unwrap();
    std::fs::rename(tree.join("Foo"), tree.join("foo")).unwrap();
    let b = env.scan(&tree);

    let d = diff_snapshots(&env.conn, a.snapshot_id, b.snapshot_id, 0, 50).unwrap();
    let kinds = |k: ChangeKind, name: &str| {
        d.dirs
            .iter()
            .filter(|x| x.kind == k && x.path.ends_with(name))
            .count()
    };
    assert_eq!(kinds(ChangeKind::Removed, "Foo"), 1);
    assert_eq!(kinds(ChangeKind::Added, "foo"), 1);
    assert_eq!(
        d.files
            .iter()
            .filter(|f| f.kind == ChangeKind::Removed && f.path.ends_with("Report.txt"))
            .count(),
        1
    );
    assert_eq!(
        d.files
            .iter()
            .filter(|f| f.kind == ChangeKind::Added && f.path.ends_with("report.txt"))
            .count(),
        1
    );
}

#[test]
fn mode_resolution_table() {
    // (mode, built, elevated, ntfs) -> Ok(mode) or exit code
    let ok = |m, b, e, n| resolve_mode(m, b, e, n).map_err(|e| e.exit());
    // auto: mft only when everything lines up, walk otherwise (FAT32/exFAT/ReFS are not NTFS).
    assert_eq!(ok(ScanMode::Auto, true, true, true), Ok("mft"));
    assert_eq!(ok(ScanMode::Auto, true, true, false), Ok("walk"));
    assert_eq!(ok(ScanMode::Auto, true, false, true), Ok("walk"));
    assert_eq!(ok(ScanMode::Auto, false, true, true), Ok("walk"));
    // walk always walks.
    assert_eq!(ok(ScanMode::Walk, true, true, true), Ok("walk"));
    // mft: wrong build or filesystem is exit 5, no elevation is exit 4.
    assert_eq!(ok(ScanMode::Mft, true, true, true), Ok("mft"));
    assert_eq!(ok(ScanMode::Mft, true, true, false), Err(Exit::Unsupported));
    assert_eq!(ok(ScanMode::Mft, false, true, true), Err(Exit::Unsupported));
    assert_eq!(ok(ScanMode::Mft, true, false, true), Err(Exit::Refused));
}
