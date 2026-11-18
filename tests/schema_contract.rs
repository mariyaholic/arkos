//! Pins the shape of schema 0001. A column change, a new column or a changed constraint fails
//! here, because DupliCut reads these tables (see docs/schema-contract.md).

mod support;

use support::Env;

/// `(name, declared type, not null, default, primary key position)` per column, in order.
type Col = (&'static str, &'static str, i64, Option<&'static str>, i64);

const SNAPSHOTS: &[Col] = &[
    ("id", "INTEGER", 0, None, 1),
    ("volume", "TEXT", 1, None, 0),
    ("root", "TEXT", 1, None, 0),
    ("label", "TEXT", 0, None, 0),
    ("mode", "TEXT", 1, None, 0),
    ("elevated", "INTEGER", 1, None, 0),
    ("state", "TEXT", 1, None, 0),
    ("started_at", "INTEGER", 1, None, 0),
    ("finished_at", "INTEGER", 0, None, 0),
    ("files", "INTEGER", 1, Some("0"), 0),
    ("dirs", "INTEGER", 1, Some("0"), 0),
    ("bytes", "INTEGER", 1, Some("0"), 0),
    ("denied", "INTEGER", 1, Some("0"), 0),
    ("skipped", "INTEGER", 1, Some("0"), 0),
    ("scan_ms", "INTEGER", 0, None, 0),
];

const DIRS: &[Col] = &[
    ("id", "INTEGER", 0, None, 1),
    ("snapshot_id", "INTEGER", 1, None, 0),
    ("parent_id", "INTEGER", 0, None, 0),
    ("path", "TEXT", 1, None, 0),
    ("name", "TEXT", 1, None, 0),
    ("depth", "INTEGER", 1, None, 0),
    ("mtime", "INTEGER", 0, None, 0),
    ("bytes", "INTEGER", 1, Some("0"), 0),
    ("files", "INTEGER", 1, Some("0"), 0),
];

const FILES: &[Col] = &[
    ("snapshot_id", "INTEGER", 1, None, 1),
    ("dir_id", "INTEGER", 1, None, 2),
    ("name", "TEXT", 1, None, 3),
    ("size", "INTEGER", 1, None, 0),
    ("mtime", "INTEGER", 1, None, 0),
    ("ext", "TEXT", 0, None, 0),
];

fn columns(env: &Env, table: &str) -> Vec<(String, String, i64, Option<String>, i64)> {
    env.conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

fn check(env: &Env, table: &str, golden: &[Col]) {
    let want: Vec<_> = golden
        .iter()
        .map(|(n, t, nn, d, pk)| {
            (
                n.to_string(),
                t.to_string(),
                *nn,
                d.map(str::to_string),
                *pk,
            )
        })
        .collect();
    assert_eq!(
        columns(env, table),
        want,
        "{table} no longer matches schema 0001"
    );
}

#[test]
fn arkos_tables_match_the_0001_column_list() {
    let env = Env::new("schema");
    check(&env, "arkos_snapshots", SNAPSHOTS);
    check(&env, "arkos_dirs", DIRS);
    check(&env, "arkos_files", FILES);
}

#[test]
fn schema_has_exactly_the_0001_objects() {
    let env = Env::new("schema-objects");
    let mut names: Vec<(String, String)> = env
        .conn
        .prepare(
            "SELECT type, name FROM sqlite_master
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    names.sort();
    let got: Vec<String> = names.iter().map(|(t, n)| format!("{t}:{n}")).collect();
    assert_eq!(
        got,
        vec![
            "index:arkos_dirs_bytes",
            "index:arkos_dirs_depth",
            "index:arkos_dirs_path",
            "index:arkos_files_ext",
            "index:arkos_files_mtime",
            "index:arkos_files_size",
            "index:arkos_snapshots_one_running",
            "index:arkos_snapshots_volume_at",
            "table:arkos_dirs",
            "table:arkos_files",
            "table:arkos_snapshots",
        ]
    );
    let version: u32 = env
        .conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
}
