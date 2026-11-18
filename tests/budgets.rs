//! Budget gates from spec §6. They are `#[ignore]` so a normal `cargo test` stays fast; run them
//! with `cargo test --release -- --ignored`. The throughput gate only asserts in release builds.

mod support;

use arkos::diff::diff_snapshots;
use std::time::Instant;
use support::fixture::{generate, Spec};
use support::Env;

const FILES: usize = 100_000;

fn fixture(env: &Env) -> std::path::PathBuf {
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    generate(
        &tree,
        &Spec {
            files: FILES,
            dirs: 12_000,
            depth: 12,
            seed: 1,
        },
    )
    .unwrap();
    tree
}

#[test]
#[ignore = "budget gate: run with --release -- --ignored"]
fn walk_throughput_on_100k() {
    let mut env = Env::new("budget-walk");
    let tree = fixture(&env);
    let t = Instant::now();
    let rep = env.scan(&tree);
    let secs = t.elapsed().as_secs_f64();
    let rate = rep.files as f64 / secs;
    eprintln!(
        "walk: {} files in {secs:.2}s = {rate:.0} files/s",
        rep.files
    );
    assert_eq!(rep.files as usize, FILES);
    if cfg!(debug_assertions) {
        eprintln!("debug build: throughput gate skipped");
    } else {
        assert!(
            rate >= 150_000.0,
            "walk throughput {rate:.0} files/s is under 150,000"
        );
    }
}

#[test]
#[ignore = "budget gate: run with --release -- --ignored"]
fn diff_100k_under_2s() {
    let mut env = Env::new("budget-diff");
    let tree = fixture(&env);
    let a = env.scan(&tree);
    // Change about one file in a hundred so the diff has real work to rank.
    let mut changed = 0;
    for entry in std::fs::read_dir(&tree).unwrap().flatten() {
        if entry.path().is_dir() {
            continue;
        }
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(entry.path())
            .unwrap();
        f.set_len(1 << 20).unwrap();
        changed += 1;
    }
    eprintln!("changed {changed} top-level files");
    let b = env.scan(&tree);
    let t = Instant::now();
    let r = diff_snapshots(&env.conn, a.snapshot_id, b.snapshot_id, 0, 50).unwrap();
    let ms = t.elapsed().as_millis();
    eprintln!(
        "diff: {ms} ms, {} dirs {} files",
        r.dirs.len(),
        r.files.len()
    );
    if !cfg!(debug_assertions) {
        assert!(ms < 2_000, "diff took {ms} ms");
    }
}
