mod support;

use arkos::config::Config;
use arkos::db::retention::{plan_prune, prune_completed};
use arkos::diff::diff_snapshots;
use arkos::logging::Logger;
use arkos::scan::{run_scan, ScanMode};
use arkos::top::{query_top, TopBy};
use std::process::Command;
use support::{temp_dir, Env};

fn arkos(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arkos"))
        .env("ARKOS_HOME", home)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn auto_mode_records_the_path_that_ran() {
    let mut env = Env::new("mode");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("a"), b"1").unwrap();
    let cfg = Config::default();
    let log = Logger::new(&env.paths.logs);
    let rep = run_scan(
        &mut env.conn,
        &env.paths,
        &cfg,
        &log,
        &tree.to_string_lossy(),
        None,
        None,
        ScanMode::Auto,
        &[],
    )
    .unwrap();
    let stored: String = env
        .conn
        .query_row(
            "SELECT mode FROM arkos_snapshots WHERE id = ?1",
            [rep.snapshot_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rep.mode, "walk");
    assert_eq!(stored, "walk");
}

#[test]
fn diff_top_is_one_ranked_list_across_dirs_and_files() {
    let mut env = Env::new("difftop");
    let tree = env.root.join("tree");
    for d in ["a", "b", "c"] {
        std::fs::create_dir_all(tree.join(d)).unwrap();
        std::fs::write(tree.join(d).join("f.bin"), vec![0u8; 10]).unwrap();
    }
    let s1 = env.scan(&tree);
    for (d, n) in [("a", 1000), ("b", 2000), ("c", 3000)] {
        std::fs::write(tree.join(d).join("f.bin"), vec![0u8; n]).unwrap();
    }
    let s2 = env.scan(&tree);
    let all = diff_snapshots(&env.conn, s1.snapshot_id, s2.snapshot_id, 0, 100).unwrap();
    assert!(all.dirs.len() + all.files.len() > 4);
    for top in [1usize, 3, 4] {
        let r = diff_snapshots(&env.conn, s1.snapshot_id, s2.snapshot_id, 0, top).unwrap();
        assert_eq!(r.dirs.len() + r.files.len(), top, "top={top}");
    }
    // The kept entries are the three largest absolute deltas of the full list.
    let deltas = |r: &arkos::diff::DiffReport| -> Vec<i64> {
        let mut v: Vec<i64> = r
            .dirs
            .iter()
            .map(|d| d.delta_bytes.abs())
            .chain(r.files.iter().map(|f| f.delta_bytes.abs()))
            .collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        v
    };
    let r = diff_snapshots(&env.conn, s1.snapshot_id, s2.snapshot_id, 0, 3).unwrap();
    assert_eq!(deltas(&r), deltas(&all)[..3].to_vec());
}

#[test]
fn file_paths_use_the_platform_separator() {
    let mut env = Env::new("sep");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(tree.join("sub")).unwrap();
    std::fs::write(tree.join("sub").join("f.bin"), vec![0u8; 5]).unwrap();
    let s = env.scan(&tree);
    let items = query_top(&env.conn, s.snapshot_id, false, TopBy::Size, 5).unwrap();
    let expected = tree.join("sub").join("f.bin");
    assert_eq!(items[0].path, expected.to_string_lossy());
    if cfg!(unix) {
        assert!(!items[0].path.contains('\\'));
    }
}

#[test]
fn plan_matches_what_prune_deletes() {
    let env = Env::new("plan");
    for i in 1..=5i64 {
        env.conn
            .execute(
                "INSERT INTO arkos_snapshots(volume, root, mode, elevated, state, started_at)
                 VALUES('Z:', 'Z:\\', 'walk', 0, 'complete', ?1)",
                [1000 + i],
            )
            .unwrap();
    }
    let plan = plan_prune(&env.conn, Some("Z:"), 2).unwrap();
    let ids: Vec<i64> = plan.iter().map(|c| c.id).collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(prune_completed(&env.conn, Some("Z:"), 2).unwrap(), 3);
    for id in ids {
        let n: i64 = env
            .conn
            .query_row(
                "SELECT count(*) FROM arkos_snapshots WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }
}

#[test]
fn prune_cli_dry_run_yes_and_neither() {
    let root = temp_dir("prune-cli");
    let home = root.join("home");
    let tree = root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("f"), b"x").unwrap();
    let vol = tree.to_string_lossy().into_owned();
    for _ in 0..3 {
        let out = arkos(&home, &["scan", "--volume", &vol, "--mode", "walk"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let count = || -> usize {
        let out = arkos(&home, &["list", "--json"]);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v.as_array().unwrap().len()
    };
    assert_eq!(count(), 3);

    let out = arkos(&home, &["prune", "--keep", "1", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("would delete 2"));
    assert_eq!(count(), 3);

    let out = arkos(&home, &["prune", "--keep", "1"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("would delete 2"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    assert_eq!(count(), 3);

    let out = arkos(&home, &["prune", "--keep", "1", "--yes"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(count(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn text_output_uses_human_sizes_and_json_keeps_raw_bytes() {
    let root = temp_dir("human");
    let home = root.join("home");
    let tree = root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("big.bin"), vec![0u8; 3 * 1024 * 1024]).unwrap();
    let vol = tree.to_string_lossy().into_owned();

    let out = arkos(&home, &["scan", "--volume", &vol, "--mode", "walk"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("size=3.0 MiB"), "{text}");
    assert!(!text.contains("3145728"));

    let out = arkos(
        &home,
        &["scan", "--volume", &vol, "--mode", "walk", "--json"],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["bytes"], 3 * 1024 * 1024);

    let out = arkos(&home, &["top", "--snapshot", "1"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("3.0 MiB"));
    let out = arkos(&home, &["top", "--snapshot", "1", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v[0]["bytes"], 3 * 1024 * 1024);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn mode_mft_needs_the_experimental_flag() {
    let env = Env::new("mftflag");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    let vol = tree.to_string_lossy().into_owned();

    let out = arkos(
        &env.paths.home,
        &["scan", "--volume", &vol, "--mode", "mft"],
    );
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--experimental"), "{err}");
    // A refused flag combination must not create a snapshot.
    let list = arkos(&env.paths.home, &["list", "--json"]);
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "[]");

    // With the flag the request gets past the gate, prints the warning, and then fails for the
    // platform: this build cannot read the MFT here (exit 5, unsupported).
    let out = arkos(
        &env.paths.home,
        &["scan", "--volume", &vol, "--mode", "mft", "--experimental"],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("experimental and untested"), "{err}");
    if !cfg!(windows) {
        assert_eq!(out.status.code(), Some(5), "{err}");
    }
}

#[test]
fn scan_output_reports_placeholders() {
    let env = Env::new("placeholders");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("a"), b"1").unwrap();
    let vol = tree.to_string_lossy().into_owned();

    let json = arkos(&env.paths.home, &["scan", "--volume", &vol, "--json"]);
    let text = String::from_utf8_lossy(&json.stdout);
    assert!(text.contains("\"placeholders\": 0"), "{text}");
    let plain = arkos(&env.paths.home, &["scan", "--volume", &vol]);
    let text = String::from_utf8_lossy(&plain.stdout);
    assert!(text.contains("placeholders=0"), "{text}");
}
