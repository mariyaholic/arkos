//! Tests for the implicit state machines recorded in `docs/state-machines.md`.
//!
//! Tests without an `#[ignore]` pin behaviour that is correct today. Each `#[ignore = "defect
//! SM-N: ..."]` test fails today and describes the behaviour a fix must produce. Run them with
//! `cargo test --test state_machines -- --ignored`.

mod support;

use arkos::config::Config;
use arkos::db::{self, retention};
use arkos::diff::diff_snapshots;
use arkos::lock::VolumeLock;
use arkos::logging::Logger;
use arkos::progress::Reporter;
use arkos::scan::{run_scan, run_scan_with_progress, ScanMode};
use arkos::top::{query_top, TopBy};
use arkos::tree::render_tree_html;
use arkos::volumes::resolve_target;
use rusqlite::{params, Connection};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::{temp_dir, Env};

/// The volume key a scan of `dir` uses. Tests must not build `dir:` keys by hand: on Windows,
/// canonicalising expands 8.3 short names in the temp path, so a hand-built key names a different
/// volume and a different lock file.
fn dir_key(dir: &std::path::Path) -> String {
    arkos::volumes::volume_key(&dir.to_string_lossy()).unwrap()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn insert_snap(conn: &Connection, volume: &str, root: &str, state: &str, started_at: i64) -> i64 {
    conn.execute(
        "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
         VALUES(?1, ?2, 'walk', 0, ?3, ?4)",
        params![volume, root, state, started_at],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn state_of(conn: &Connection, id: i64) -> String {
    conn.query_row(
        "SELECT state FROM arkos_snapshots WHERE id = ?1",
        [id],
        |r| r.get(0),
    )
    .unwrap()
}

fn small_tree(env: &Env) -> std::path::PathBuf {
    let tree = env.root.join("tree");
    std::fs::create_dir_all(tree.join("sub")).unwrap();
    std::fs::write(tree.join("a.txt"), b"hello").unwrap();
    std::fs::write(tree.join("sub/b.txt"), b"world!").unwrap();
    tree
}

fn scan_walk(env: &mut Env, volume: &str, sub: Option<&Path>) -> arkos::exit::Result<i64> {
    let cfg = Config::default();
    let log = Logger::new(&env.paths.logs);
    run_scan(
        &mut env.conn,
        &env.paths,
        &cfg,
        &log,
        volume,
        sub,
        None,
        ScanMode::Walk,
        &[],
    )
    .map(|r| r.snapshot_id)
}

/// A writer that runs `hook` when the progress reporter prints, or when it clears its line.
struct Hook {
    on_tick: bool,
    hook: Box<dyn FnMut() + Send>,
}

impl Write for Hook {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let is_clear = buf.len() >= 2 && buf.starts_with(b"\r") && buf.ends_with(b"\r");
        if is_clear != self.on_tick {
            (self.hook)();
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run a walk scan with `hook` firing at the first progress tick (`on_tick`, inside the scan's
/// write transaction) or at the progress clear (after the walk, before rollup and completion).
fn scan_with_hook(
    env: &mut Env,
    volume: &str,
    on_tick: bool,
    hook: Box<dyn FnMut() + Send>,
) -> arkos::exit::Result<arkos::scan::ScanReport> {
    let cfg = Config::default();
    let log = Logger::new(&env.paths.logs);
    let mut once = Some(hook);
    let hook: Box<dyn FnMut() + Send> = Box::new(move || {
        if let Some(mut h) = once.take() {
            h();
        }
    });
    let mut reporter = Reporter::to_writer(Box::new(Hook { on_tick, hook }), Duration::ZERO);
    run_scan_with_progress(
        &mut env.conn,
        &env.paths,
        &cfg,
        &log,
        volume,
        None,
        None,
        ScanMode::Walk,
        &[],
        &mut reporter,
    )
}

// ---------------------------------------------------------------------------------------------
// Machines that behave correctly today.
// ---------------------------------------------------------------------------------------------

#[test]
fn only_complete_snapshots_are_accepted_by_diff_top_and_tree() {
    let mut env = Env::new("sm-accept");
    let tree = small_tree(&env);
    let good = env.scan(&tree).snapshot_id;
    let vol = format!("dir:{}", tree.display());
    for state in ["running", "failed", "abandoned"] {
        // `running` is unique per volume, so use a volume of its own for each state.
        let v = format!("{vol}-{state}");
        let id = insert_snap(&env.conn, &v, "x", state, now());
        assert!(
            diff_snapshots(&env.conn, good, id, 0, 10).is_err(),
            "{state}"
        );
        assert!(
            diff_snapshots(&env.conn, id, good, 0, 10).is_err(),
            "{state}"
        );
        assert!(
            query_top(&env.conn, id, true, TopBy::Size, 5).is_err(),
            "{state}"
        );
        assert!(render_tree_html(&env.conn, id, 3).is_err(), "{state}");
    }
}

#[test]
fn prune_never_lists_a_running_snapshot() {
    let env = Env::new("sm-prune-running");
    insert_snap(&env.conn, "dir:/p", "r", "running", 10);
    for i in 0..3 {
        insert_snap(&env.conn, "dir:/p", "r", "complete", 100 + i);
    }
    let plan = retention::plan_prune(&env.conn, None, 1).unwrap();
    assert!(plan.iter().all(|c| c.state != "running"));
    assert_eq!(plan.len(), 2);
}

#[test]
fn prune_partitions_by_volume_and_root() {
    let env = Env::new("sm-prune-roots");
    for i in 0..3 {
        insert_snap(&env.conn, "dir:/p", "/p", "complete", 100 + i);
        insert_snap(&env.conn, "dir:/p", "/p/sub", "complete", 200 + i);
    }
    let plan = retention::plan_prune(&env.conn, None, 2).unwrap();
    // One oldest snapshot goes from each root.
    assert_eq!(plan.len(), 2);
    let roots: Vec<&str> = plan.iter().map(|c| c.root.as_str()).collect();
    assert!(roots.contains(&"/p") && roots.contains(&"/p/sub"));
}

#[test]
fn sweep_keeps_a_row_whose_lock_is_held_and_abandons_the_rest() {
    let env = Env::new("sm-sweep");
    let live = insert_snap(&env.conn, "dir:/live", "r", "running", now());
    let dead = insert_snap(&env.conn, "dir:/dead", "r", "running", now());
    let held = VolumeLock::acquire(&env.paths.locks, "dir:/live", 200).unwrap();
    let n = retention::sweep_abandoned_with_locks(&env.conn, &env.paths.locks, now()).unwrap();
    assert_eq!(n, 1);
    assert_eq!(state_of(&env.conn, live), "running");
    assert_eq!(state_of(&env.conn, dead), "abandoned");
    drop(held);
}

#[test]
fn an_unreadable_lock_file_falls_back_to_the_24_hour_rule() {
    let env = Env::new("sm-fallback");
    // A directory where the lock file should be makes the probe fail with an error that is not
    // "not found", which is the only way the fallback is reached.
    let path = arkos::lock::lock_path_for(&env.paths.locks, "dir:/odd");
    std::fs::create_dir_all(&path).unwrap();
    let fresh = insert_snap(&env.conn, "dir:/odd", "r", "running", now() - 3600);
    retention::sweep_abandoned_with_locks(&env.conn, &env.paths.locks, now()).unwrap();
    assert_eq!(state_of(&env.conn, fresh), "running");
    env.conn
        .execute(
            "UPDATE arkos_snapshots SET started_at = ?1 WHERE id = ?2",
            params![now() - 25 * 3600, fresh],
        )
        .unwrap();
    retention::sweep_abandoned_with_locks(&env.conn, &env.paths.locks, now()).unwrap();
    assert_eq!(state_of(&env.conn, fresh), "abandoned");
}

#[test]
fn a_second_scan_of_a_running_volume_is_refused_and_leaves_the_first_row_alone() {
    let mut env = Env::new("sm-second");
    let tree = small_tree(&env);
    let id = insert_snap(&env.conn, &dir_key(&tree), "r", "running", now());
    let held = VolumeLock::acquire(&env.paths.locks, &dir_key(&tree), 200).unwrap();
    let cfg = Config {
        busy_timeout_ms: 50,
        ..Config::default()
    };
    let log = Logger::new(&env.paths.logs);
    let err = run_scan(
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
    .unwrap_err();
    assert_eq!(err.exit(), arkos::exit::Exit::Busy);
    assert_eq!(state_of(&env.conn, id), "running");
    drop(held);
}

// ---------------------------------------------------------------------------------------------
// Suspected defects. Each fails today.
// ---------------------------------------------------------------------------------------------

#[test]
fn sm1_scan_abandons_a_dead_running_row_it_finds_under_its_own_lock() {
    let mut env = Env::new("sm1");
    let tree = small_tree(&env);
    // The ledger was opened (and swept) before this row appeared, as when another scan dies while
    // this one waits for the lock. Nobody holds the lock now, so the row's owner is gone.
    let dead = insert_snap(
        &env.conn,
        &dir_key(&tree),
        &tree.to_string_lossy(),
        "running",
        now(),
    );
    let res = scan_walk(&mut env, &tree.to_string_lossy(), None);
    assert!(
        res.is_ok(),
        "scan holds the lock, so the row is dead: {res:?}"
    );
    assert_eq!(state_of(&env.conn, dead), "abandoned");
}

#[test]
fn sm2_scan_does_not_report_success_for_a_snapshot_that_vanished() {
    let mut env = Env::new("sm2");
    let tree = small_tree(&env);
    let vol = dir_key(&tree);
    let root = tree.to_string_lossy().into_owned();
    let db_path = env.paths.db.clone();
    let elsewhere = env.root.join("elsewhere");
    let fired = Arc::new(Mutex::new(false));
    let fired2 = fired.clone();
    let hook = Box::new(move || {
        // A sweeper that sees no lock (lock file lost, see SM-4) abandons the live row, and a
        // prune then drops it because a newer complete snapshot exists for the same root.
        let c2 = db::open_and_migrate(&db_path, &elsewhere, 1000).unwrap();
        insert_snap(&c2, &vol, &root, "complete", now() + 1000);
        retention::prune_completed(&c2, None, 1).unwrap();
        *fired2.lock().unwrap() = true;
    });
    let res = scan_with_hook(&mut env, &tree.to_string_lossy(), false, hook);
    assert!(*fired.lock().unwrap(), "hook did not run");
    let err = res.expect_err("the snapshot left the running state, so the scan did not succeed");
    assert_eq!(err.exit(), arkos::exit::Exit::Refused);
    assert!(err.to_string().contains("left the running state"), "{err}");
}

#[test]
fn sm3_a_read_verb_can_open_the_ledger_while_a_scan_is_writing() {
    let mut env = Env::new("sm3");
    let tree = small_tree(&env);
    let (db_path, locks) = (env.paths.db.clone(), env.paths.locks.clone());
    let outcome: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let out2 = outcome.clone();
    let hook = Box::new(move || {
        // Runs at the first progress tick, with the scan's batch transaction open.
        let read = db::open_read(&db_path, 150).map(|c| c.is_some());
        let write = db::open_and_migrate(&db_path, &locks, 150).map(|_| ());
        *out2.lock().unwrap() = Some(match (read, write) {
            (Ok(true), Ok(())) => "ok".to_owned(),
            (r, w) => format!("read {r:?}, write {:?}", w.map_err(|e| e.to_string())),
        });
    });
    scan_with_hook(&mut env, &tree.to_string_lossy(), true, hook).unwrap();
    let got = outcome.lock().unwrap().clone().expect("hook did not run");
    assert_eq!(
        got, "ok",
        "a `list` during a scan failed to open the ledger"
    );
}

#[cfg(unix)]
#[test]
fn sm4_a_waiter_that_locked_an_unlinked_file_retries_on_the_new_one() {
    // The waiter opens the lock file while `first` holds it. The file is then unlinked and a
    // third party locks a new file at the same path. When `first` lets go, the waiter locks the
    // old, unlinked inode, which must not count as holding the volume.
    let env = Env::new("sm4");
    let first = VolumeLock::acquire(&env.paths.locks, "dir:/shared", 200).unwrap();
    let locks = env.paths.locks.clone();
    let waiter =
        std::thread::spawn(move || VolumeLock::acquire(&locks, "dir:/shared", 1500).map(|_| ()));
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::remove_file(&first.path).unwrap();
    let third = VolumeLock::acquire(&env.paths.locks, "dir:/shared", 200).unwrap();
    drop(first);
    let got = waiter.join().unwrap();
    assert!(got.is_err(), "two live holders of one volume lock");
    drop(third);
}

#[cfg(unix)]
#[test]
fn sm5_case_distinct_directories_have_distinct_locks() {
    let env = Env::new("sm5");
    let a = VolumeLock::acquire(&env.paths.locks, "dir:/data/A", 200).unwrap();
    let b = VolumeLock::acquire(&env.paths.locks, "dir:/data/a", 100);
    assert!(b.is_ok(), "{b:?}");
    drop(a);
}

#[test]
fn sm6a_dir_volume_key_ignores_a_trailing_separator() {
    let d = temp_dir("sm6a");
    let plain = d.to_string_lossy().into_owned();
    let slashed = format!("{plain}{}", std::path::MAIN_SEPARATOR);
    let a = resolve_target(&plain, None).unwrap();
    let b = resolve_target(&slashed, None).unwrap();
    assert_eq!(a.volume, b.volume);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn sm6b_dir_volume_key_is_absolute() {
    // `tests` exists relative to the package root, which is the working directory of this test.
    let t = resolve_target("tests", None).unwrap();
    let key = t.volume.strip_prefix("dir:").unwrap();
    assert!(Path::new(key).is_absolute(), "volume key {:?}", t.volume);
}

#[test]
fn sm7_diff_refuses_snapshots_of_different_roots() {
    let mut env = Env::new("sm7");
    let tree = small_tree(&env);
    let full = scan_walk(&mut env, &tree.to_string_lossy(), None).unwrap();
    let sub = scan_walk(&mut env, &tree.to_string_lossy(), Some(Path::new("sub"))).unwrap();
    let res = diff_snapshots(&env.conn, full, sub, 0, 50);
    assert!(res.is_err(), "diff of full vs subtree reported {res:?}");
}

#[test]
fn sm8_list_volume_accepts_the_same_spelling_as_scan() {
    let home = temp_dir("sm8-home");
    let tree = temp_dir("sm8-tree");
    std::fs::write(tree.join("f"), b"x").unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_arkos"))
            .env("ARKOS_HOME", &home)
            .args(args)
            .output()
            .unwrap()
    };
    let v = tree.to_string_lossy().into_owned();
    assert!(run(&["scan", "--volume", &v, "--mode", "walk"])
        .status
        .success());
    let out = run(&["list", "--volume", &v, "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&tree);
}

#[test]
fn sm9_prune_reclaims_abandoned_rows_of_a_root_that_never_completed() {
    let env = Env::new("sm9");
    let old = insert_snap(&env.conn, "dir:/never", "/never", "abandoned", 100);
    let newer = insert_snap(&env.conn, "dir:/never", "/never", "abandoned", 200);
    // The older abandoned scan left partial rows behind.
    env.conn
        .execute(
            "INSERT INTO arkos_dirs(snapshot_id, parent_id, path, name, depth) VALUES(?1, NULL, '/never', 'never', 0)",
            [old],
        )
        .unwrap();
    let plan = retention::plan_prune(&env.conn, None, 1).unwrap();
    let mut ids: Vec<i64> = plan.iter().map(|c| c.id).collect();
    ids.sort();
    // Decision: failed and abandoned rows are always candidates, whatever the complete count.
    assert_eq!(ids, vec![old, newer]);
}

#[test]
fn sm10_open_waits_for_a_writer_during_the_wal_switch() {
    let d = temp_dir("sm10");
    let p = d.join("shimatsu.sqlite");
    let a = Connection::open(&p).unwrap();
    a.execute_batch("CREATE TABLE t(x); BEGIN IMMEDIATE; INSERT INTO t VALUES(1);")
        .unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        a.execute_batch("COMMIT;").unwrap();
    });
    let r = db::open(&p, 3000, true);
    release.join().unwrap();
    assert!(r.is_ok(), "{:?}", r.err().map(|e| e.to_string()));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn sm11_open_refuses_a_ledger_whose_user_version_was_lost() {
    let d = temp_dir("sm11");
    let p = d.join("shimatsu.sqlite");
    {
        // What a restore from `.dump` produces: the tables, without the user_version pragma.
        let c = Connection::open(&p).unwrap();
        c.execute_batch(include_str!("../src/db/0001.sql")).unwrap();
    }
    // Decision: no guessed recovery; refuse with exit 4 and a stable reason.
    let e = db::open_and_migrate(&p, &d.join("locks"), 500).unwrap_err();
    assert_eq!(e.exit(), arkos::exit::Exit::Refused);
    assert!(
        e.to_string()
            .contains("ledger.inconsistent: tables present but user_version is 0"),
        "{e}"
    );
    let e = db::open_read(&p, 500).unwrap_err();
    assert_eq!(e.exit(), arkos::exit::Exit::Refused);
    let _ = std::fs::remove_dir_all(&d);
}

struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn sm12_progress_reports_the_files_of_the_last_directory() {
    let mut env = Env::new("sm12");
    let tree = env.root.join("flat");
    std::fs::create_dir_all(&tree).unwrap();
    for i in 0..50 {
        std::fs::write(tree.join(format!("f{i}")), b"x").unwrap();
    }
    let buf = Arc::new(Mutex::new(Vec::new()));
    let mut reporter = Reporter::to_writer(Box::new(Shared(buf.clone())), Duration::ZERO);
    let cfg = Config::default();
    let log = Logger::new(&env.paths.logs);
    run_scan_with_progress(
        &mut env.conn,
        &env.paths,
        &cfg,
        &log,
        &tree.to_string_lossy(),
        None,
        None,
        ScanMode::Walk,
        &[],
        &mut reporter,
    )
    .unwrap();
    let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(text.contains("scanned 50 files"), "{text:?}");
}

// ---------------------------------------------------------------------------------------------
// Behaviour added with the fixes.
// ---------------------------------------------------------------------------------------------

#[test]
fn s1_read_tx_keeps_one_snapshot_while_another_connection_prunes() {
    let mut env = Env::new("s1");
    let tree = small_tree(&env);
    let id = env.scan(&tree).snapshot_id;
    let reader = db::open_read(&env.paths.db, 1000).unwrap().unwrap();
    let count = |c: &Connection| -> i64 {
        c.query_row(
            "SELECT count(*) FROM arkos_dirs WHERE snapshot_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    let (before, after) = db::read_tx(&reader, || {
        let before = count(&reader);
        // Another connection prunes the snapshot between the two reads.
        retention::prune_completed(&env.conn, None, 0).unwrap();
        Ok((before, count(&reader)))
    })
    .unwrap();
    assert!(before > 0);
    assert_eq!(before, after);
    assert_eq!(
        count(&reader),
        0,
        "a read after the transaction sees the prune"
    );
    assert!(reader.is_autocommit());
}

#[test]
fn lock_keys_fold_case_for_drive_letters_only() {
    let d = temp_dir("lockkey");
    use arkos::lock::lock_path_for;
    assert_eq!(lock_path_for(&d, "c:"), lock_path_for(&d, "C:"));
    if !cfg!(windows) {
        assert_ne!(lock_path_for(&d, "dir:/x/A"), lock_path_for(&d, "dir:/x/a"));
    }
}

#[test]
fn read_verbs_treat_a_missing_ledger_as_empty_or_exit_3() {
    let home = temp_dir("noledger");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_arkos"))
            .env("ARKOS_HOME", &home)
            .args(args)
            .output()
            .unwrap()
    };
    let list = run(&["list", "--json"]);
    assert!(list.status.success());
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "[]");
    assert!(run(&["status", "--probe"]).status.success());
    assert_eq!(run(&["top", "--snapshot", "1"]).status.code(), Some(3));
    assert!(
        !home.join("shimatsu.sqlite").exists(),
        "a read verb must not create the ledger"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn volume_key_accepts_scan_spelling_stored_key_and_missing_dirs() {
    use arkos::volumes::volume_key;
    let d = temp_dir("vkey");
    let plain = d.to_string_lossy().into_owned();
    let key = volume_key(&plain).unwrap();
    assert_eq!(volume_key(&format!("{plain}/")).unwrap(), key);
    assert_eq!(volume_key(&key).unwrap(), key);
    assert_eq!(volume_key("c:").unwrap(), "C:");
    assert_eq!(volume_key("C:\\").unwrap(), "C:");
    let gone = d.join("gone");
    assert_eq!(
        volume_key(&gone.to_string_lossy()).unwrap(),
        format!("dir:{}", gone.display())
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_scan_that_abandons_a_dead_row_stamps_finished_at() {
    let mut env = Env::new("sm1-finished");
    let tree = small_tree(&env);
    let dead = insert_snap(&env.conn, &dir_key(&tree), "r", "running", now());
    scan_walk(&mut env, &tree.to_string_lossy(), None).unwrap();
    let finished: Option<i64> = env
        .conn
        .query_row(
            "SELECT finished_at FROM arkos_snapshots WHERE id = ?1",
            [dead],
            |r| r.get(0),
        )
        .unwrap();
    assert!(finished.is_some());
    let log = std::fs::read_to_string(env.paths.logs.join("arkos.log")).unwrap();
    assert!(log.contains("abandoned_dead_scan"), "{log}");
}
