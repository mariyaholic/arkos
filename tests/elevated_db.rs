//! Defensive ledger settings used by elevated runs (`docs/elevated-writes.md`, D4).

mod support;

use arkos::db;
use arkos::exit::Error;
use rusqlite::config::DbConfig;
use support::temp_dir;

#[test]
fn hardened_open_sets_defensive_mode_and_untrusted_schema() {
    let dir = temp_dir("harden-on");
    let conn = db::open_and_migrate_with(&dir.join("shimatsu.sqlite"), &dir, 500, true).unwrap();
    assert!(conn.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE).unwrap());
    let trusted: i64 = conn
        .pragma_query_value(None, "trusted_schema", |r| r.get(0))
        .unwrap();
    assert_eq!(trusted, 0);
}

#[test]
fn ordinary_open_is_unchanged() {
    let dir = temp_dir("harden-off");
    let conn = db::open_and_migrate(&dir.join("shimatsu.sqlite"), &dir, 500).unwrap();
    assert!(!conn.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE).unwrap());
}

#[test]
fn hardened_open_refuses_a_file_that_fails_quick_check() {
    let dir = temp_dir("harden-bad");
    let path = dir.join("shimatsu.sqlite");
    std::fs::write(&path, vec![0x5au8; 8192]).unwrap();
    match db::open_and_migrate_with(&path, &dir, 500, true) {
        Err(Error::Refused(msg)) => assert!(msg.contains("arkos db vacuum"), "{msg}"),
        Err(other) => panic!("expected a refusal, got {other}"),
        Ok(_) => panic!("expected a refusal"),
    }
}

#[test]
fn hardened_open_accepts_a_healthy_ledger_again() {
    let dir = temp_dir("harden-twice");
    let path = dir.join("shimatsu.sqlite");
    drop(db::open_and_migrate(&path, &dir, 500).unwrap());
    db::open_and_migrate_with(&path, &dir, 500, true).unwrap();
}
