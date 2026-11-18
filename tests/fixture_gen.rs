mod support;

use support::fixture::{generate, Spec};
use support::temp_dir;

fn count(root: &std::path::Path) -> (usize, usize) {
    let (mut files, mut dirs) = (0, 0);
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                dirs += 1;
                stack.push(e.path());
            } else {
                files += 1;
            }
        }
    }
    (files, dirs)
}

#[test]
fn generator_is_deterministic_and_matches_its_spec() {
    let spec = Spec {
        files: 500,
        dirs: 60,
        depth: 5,
        seed: 42,
    };
    let (a, b, c) = (temp_dir("gen-a"), temp_dir("gen-b"), temp_dir("gen-c"));
    let ma = generate(&a, &spec).unwrap();
    let mb = generate(&b, &spec).unwrap();
    let mc = generate(&c, &Spec { seed: 43, ..spec }).unwrap();
    assert_eq!(ma, mb);
    assert_ne!(ma.hash, mc.hash);
    // `dirs` counts the root, the walk below it does not.
    assert_eq!(count(&a), (500, 59));
    assert_eq!(ma.dirs, 60);
    for d in [a, b, c] {
        let _ = std::fs::remove_dir_all(d);
    }
}
