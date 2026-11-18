mod support;

use arkos::tree::render_tree_html;
use support::fixture::{generate, Spec};
use support::Env;

#[test]
fn report_has_a_nested_map_with_hover_titles_and_no_scripts() {
    let mut env = Env::new("treemap");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    generate(
        &tree,
        &Spec {
            files: 2000,
            dirs: 150,
            depth: 5,
            seed: 11,
        },
    )
    .unwrap();
    // A name that must stay escaped everywhere it appears in the map. Windows does not allow
    // `<`, `>` or `"` in file names, so it gets the escapable characters it does allow.
    #[cfg(not(windows))]
    let (name, escaped) = ("a<b>&\"q\"", "a&lt;b&gt;&amp;&quot;q&quot;");
    #[cfg(windows)]
    let (name, escaped) = ("a&b'q", "a&amp;b&#39;q");
    let nasty = tree.join(name);
    std::fs::create_dir_all(nasty.join("inner")).unwrap();
    support::fixture::write_sparse(&nasty.join("inner/big.bin"), 64 * 1024 * 1024).unwrap();
    let s = env.scan(&tree);

    let html = render_tree_html(&env.conn, s.snapshot_id, 4).unwrap();
    let svg_start = html.find("<svg").unwrap();
    let svg_end = html.find("</svg>").unwrap();
    let svg = &html[svg_start..svg_end];

    // Nested: more rectangles than top-level folders, with directory titles below depth 1.
    let rects = svg.matches("<rect ").count();
    let top_level = std::fs::read_dir(&tree).unwrap().count();
    assert!(
        rects > top_level,
        "{rects} rects for {top_level} top-level entries"
    );
    assert_eq!(
        svg.matches("<rect ").count(),
        svg.matches("<title>").count()
    );
    assert!(svg.contains("inner"), "a depth-2 folder should be drawn");
    assert!(svg.contains("files here"));

    // Escaping and policy.
    assert!(svg.contains(escaped), "escaped name {escaped} missing");
    assert!(!html.contains("<script"));
    assert!(!html.contains("http://") && !html.contains("https://"));
    assert!(!svg.contains("lining") && !svg.contains("wine"));
    assert!(html.contains("viewBox=\"0 0 1000 600\""));
}

#[test]
fn map_scales_with_depth_and_stays_bounded() {
    let mut env = Env::new("treemap-depth");
    let tree = env.root.join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    generate(
        &tree,
        &Spec {
            files: 3000,
            dirs: 400,
            depth: 6,
            seed: 3,
        },
    )
    .unwrap();
    let s = env.scan(&tree);
    let count = |depth| {
        let html = render_tree_html(&env.conn, s.snapshot_id, depth).unwrap();
        html.matches("<rect ").count()
    };
    let (d1, d2, d6) = (count(1), count(2), count(6));
    assert!(d1 < d2 && d2 < d6, "{d1} {d2} {d6}");
    // The 4 by 4 floor caps the count by area: 1000 * 600 / 16.
    assert!(d6 <= 37_500);
}
