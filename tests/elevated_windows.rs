//! The elevated-run guard (`docs/elevated-writes.md`). The tests that call `enter_forced` run the
//! pinning and refusal code on any Windows token, including under Wine. The last test needs a real
//! elevated prompt and is ignored by default:
//!
//! `cargo test --test elevated_windows -- --ignored`
#![cfg(windows)]

mod support;

use arkos::elevated::{data_chain, enter, enter_forced};
use arkos::exit::Error;
use arkos::paths::{PathEnvironment, Paths};
use arkos::volumes::is_elevated;
use std::path::{Path, PathBuf};
use support::temp_dir;

struct Setup {
    paths: Paths,
    chain: Vec<PathBuf>,
}

fn setup(name: &str) -> Setup {
    let home = temp_dir(name).join("home");
    let paths = Paths::from_home(&home);
    let mut env = PathEnvironment {
        windows: true,
        vars: Default::default(),
    };
    env.vars.insert("ARKOS_HOME".into(), home.clone().into());
    let chain = data_chain(&paths, &env);
    Setup { paths, chain }
}

fn refused(result: arkos::exit::Result<arkos::elevated::PinnedDirs>) -> String {
    match result {
        Err(Error::Refused(msg)) => msg,
        Err(other) => panic!("expected a refusal, got {other}"),
        Ok(_) => panic!("expected a refusal, got success"),
    }
}

/// Make `link` point at `target`: a junction through `mklink /J`, else a directory symlink.
fn redirect(link: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output();
    if matches!(&status, Ok(o) if o.status.success()) && link.exists() {
        return;
    }
    std::os::windows::fs::symlink_dir(target, link).expect("create a junction or symlink");
}

#[test]
fn guard_matches_the_token_when_not_forced() {
    let s = setup("elev-noop");
    let pinned = enter(&s.paths, &s.chain).unwrap();
    assert_eq!(pinned.is_active(), is_elevated());
}

#[test]
fn clean_chain_is_created_and_pinned() {
    let s = setup("elev-clean");
    let pinned = enter_forced(&s.paths, &s.chain).unwrap();
    assert!(pinned.is_active());
    assert_eq!(pinned.pinned_count(), s.chain.len());
    assert!(s.paths.locks.is_dir() && s.paths.logs.is_dir());
}

#[test]
fn a_junction_at_logs_is_refused_and_named() {
    let s = setup("elev-junction");
    std::fs::create_dir_all(&s.paths.home).unwrap();
    std::fs::create_dir_all(&s.paths.locks).unwrap();
    let target = s.paths.home.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    redirect(&s.paths.logs, &target);
    let msg = refused(enter_forced(&s.paths, &s.chain));
    assert!(msg.contains("logs"), "{msg}");
    assert!(
        msg.contains("reparse point") || msg.contains("resolves to"),
        "{msg}"
    );
}

#[test]
fn a_redirected_home_is_refused() {
    let s = setup("elev-home");
    let base = s.paths.home.parent().unwrap().to_path_buf();
    let target = base.join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    redirect(&s.paths.home, &target);
    let msg = refused(enter_forced(&s.paths, &s.chain));
    assert!(msg.contains("home"), "{msg}");
}

#[test]
fn a_hard_linked_log_is_refused() {
    let s = setup("elev-hardlink");
    s.paths.ensure_dirs().unwrap();
    let outside = s.paths.home.parent().unwrap().join("outside.txt");
    std::fs::write(&outside, b"x").unwrap();
    std::fs::hard_link(&outside, s.paths.logs.join("arkos.log")).unwrap();
    let msg = refused(enter_forced(&s.paths, &s.chain));
    assert!(
        msg.contains("arkos.log") && msg.contains("hard link"),
        "{msg}"
    );
}

#[test]
fn a_hard_linked_ledger_or_lock_is_refused() {
    let s = setup("elev-hardlink2");
    s.paths.ensure_dirs().unwrap();
    let outside = s.paths.home.parent().unwrap().join("outside.txt");
    std::fs::write(&outside, b"x").unwrap();
    let lock = arkos::lock::lock_path_for(&s.paths.locks, "C:");
    std::fs::hard_link(&outside, &lock).unwrap();
    let msg = refused(enter_forced(&s.paths, &s.chain));
    assert!(msg.contains(".lock"), "{msg}");
    std::fs::remove_file(&lock).unwrap();
    std::fs::hard_link(&outside, &s.paths.db).unwrap();
    let msg = refused(enter_forced(&s.paths, &s.chain));
    assert!(msg.contains("shimatsu.sqlite"), "{msg}");
}

#[test]
fn ordinary_files_pass() {
    let s = setup("elev-files");
    s.paths.ensure_dirs().unwrap();
    std::fs::write(s.paths.logs.join("arkos.log"), b"{}\n").unwrap();
    std::fs::write(s.paths.logs.join("arkos.log.1"), b"{}\n").unwrap();
    let lock = arkos::lock::lock_path_for(&s.paths.locks, "C:");
    std::fs::write(&lock, b"").unwrap();
    std::fs::write(arkos::lock::heartbeat_path(&lock), b"1 2").unwrap();
    std::fs::write(&s.paths.db, b"").unwrap();
    enter_forced(&s.paths, &s.chain).unwrap();
}

#[test]
#[ignore = "needs real NTFS; Wine does not emulate share-mode blocking of directory renames"]
fn a_pinned_folder_cannot_be_renamed_until_released() {
    let s = setup("elev-pin");
    let pinned = enter_forced(&s.paths, &s.chain).unwrap();
    let moved = s.paths.home.join("logs-moved");
    assert!(
        std::fs::rename(&s.paths.logs, &moved).is_err(),
        "a pinned folder was renamed"
    );
    let moved_home = s.paths.home.parent().unwrap().join("home-moved");
    assert!(
        std::fs::rename(&s.paths.home, &moved_home).is_err(),
        "a pinned chain was renamed"
    );
    drop(pinned);
    std::fs::rename(&s.paths.logs, &moved).expect("rename works once the pins are released");
}

#[test]
#[ignore = "needs an elevated prompt on Windows"]
fn elevated_scan_refuses_a_junction_and_a_hard_link() {
    if !is_elevated() {
        eprintln!("skipped: needs an elevated token");
        return;
    }
    let s = setup("elev-real");
    std::fs::create_dir_all(&s.paths.home).unwrap();
    std::fs::create_dir_all(&s.paths.locks).unwrap();
    let tree = s.paths.home.parent().unwrap().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    let target = s.paths.home.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_arkos"))
            .env("ARKOS_HOME", &s.paths.home)
            .args(["scan", "--volume"])
            .arg(&tree)
            .args(["--mode", "walk"])
            .output()
            .unwrap()
    };
    redirect(&s.paths.logs, &target);
    assert_eq!(run().status.code(), Some(4));
    std::fs::remove_dir(&s.paths.logs).unwrap();
    std::fs::create_dir_all(&s.paths.logs).unwrap();
    let outside = s.paths.home.parent().unwrap().join("outside.txt");
    std::fs::write(&outside, b"x").unwrap();
    std::fs::hard_link(&outside, s.paths.logs.join("arkos.log")).unwrap();
    assert_eq!(run().status.code(), Some(4));
}
