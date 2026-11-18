//! The walk must reach paths longer than 260 characters. Windows only; this test has not been
//! run, because the build host for this change has no Windows machine.
#![cfg(windows)]

mod support;

use support::Env;

#[test]
fn the_walk_reaches_a_path_over_300_characters() {
    let mut env = Env::new("longpath");
    let tree = env.root.join("tree");
    // Each segment is 40 characters, so eight levels put the file past 300 characters.
    let mut deep = tree.clone();
    for i in 0..8 {
        deep.push(format!("segment-{i}-{}", "x".repeat(30)));
    }
    // std adds the `\\?\` prefix to create_dir_all and write for paths past the legacy limit.
    std::fs::create_dir_all(&deep).unwrap();
    let file = deep.join("deep-file.bin");
    std::fs::write(&file, vec![0u8; 1234]).unwrap();
    assert!(
        file.to_string_lossy().len() > 300,
        "the test path is only {} characters",
        file.to_string_lossy().len()
    );

    let report = env.scan(&tree);
    assert_eq!(
        report.files, 1,
        "the file under the long path was not found"
    );
    assert_eq!(report.bytes, 1234);
    assert_eq!(report.dirs, 9);
    assert_eq!(report.denied, 0);

    let stored: String = env
        .conn
        .query_row(
            "SELECT d.path FROM arkos_files f JOIN arkos_dirs d ON d.id = f.dir_id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        stored.len() > 260,
        "stored path is {} characters",
        stored.len()
    );
    assert!(
        !stored.starts_with(r"\\?\"),
        "display path kept the verbatim prefix"
    );
}
