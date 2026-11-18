//! Golden check: after a scripted set of changes, `diff` names the expected top five directories
//! in the expected order.

mod support;

use arkos::diff::{diff_snapshots, ChangeKind};
use std::path::Path;
use support::fixture::{at, generate, write_sparse, Spec};
use support::Env;

const MIB: u64 = 1024 * 1024;

fn rel(path: &str, root: &Path) -> String {
    let p = Path::new(path);
    match p.strip_prefix(root) {
        Ok(r) if r.as_os_str().is_empty() => ".".to_owned(),
        Ok(r) => r
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path.to_owned(),
    }
}

#[test]
fn diff_names_the_expected_top_five_directories() {
    let mut env = Env::new("golden");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    generate(
        &tree,
        &Spec {
            files: 300,
            dirs: 40,
            depth: 4,
            seed: 7,
        },
    )
    .unwrap();

    // Known directories next to the generated `dNNNNN` ones.
    write_sparse(&at(&tree, "proj/cache/blob.bin"), 5 * MIB).unwrap();
    write_sparse(&at(&tree, "proj/media/keep.bin"), MIB).unwrap();
    write_sparse(&at(&tree, "old/archive.bin"), 30 * MIB).unwrap();
    write_sparse(&at(&tree, "logs/big.log"), 10 * MIB).unwrap();
    let a = env.scan(&tree);

    write_sparse(&at(&tree, "proj/media/clip.bin"), 40 * MIB).unwrap(); // +40
    write_sparse(&at(&tree, "proj/cache/blob.bin"), 25 * MIB).unwrap(); // +20
    std::fs::remove_file(at(&tree, "old/archive.bin")).unwrap(); // -30
    write_sparse(&at(&tree, "logs/big.log"), 2 * MIB).unwrap(); // -8
    write_sparse(&at(&tree, "fresh/new.bin"), 6 * MIB).unwrap(); // +6, new dir
    let b = env.scan(&tree);

    let report = diff_snapshots(&env.conn, a.snapshot_id, b.snapshot_id, 0, 1000).unwrap();
    let dirs: Vec<(String, i64)> = report
        .dirs
        .iter()
        .map(|d| (rel(&d.path, &tree), d.delta_bytes))
        .collect();
    let mib = |n: i64| n * MIB as i64;
    let top5: Vec<(String, i64)> = dirs.iter().take(5).cloned().collect();
    assert_eq!(
        top5,
        vec![
            ("proj".to_owned(), mib(60)),
            ("proj/media".to_owned(), mib(40)),
            ("old".to_owned(), -mib(30)),
            (".".to_owned(), mib(28)),
            ("proj/cache".to_owned(), mib(20)),
        ]
    );
    assert_eq!(dirs[5], ("logs".to_owned(), -mib(8)));
    assert_eq!(dirs[6], ("fresh".to_owned(), mib(6)));
    let fresh = report
        .dirs
        .iter()
        .find(|d| rel(&d.path, &tree) == "fresh")
        .unwrap();
    assert_eq!(fresh.kind, ChangeKind::Added);
}
