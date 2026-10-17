use std::fs;

use harn_source_fingerprint_build::fingerprint;

#[test]
fn owned_sources_track_changes_across_layouts_and_line_endings() {
    let workspace = tempfile::tempdir().unwrap();
    let packaged = tempfile::tempdir().unwrap();
    let left = workspace.path().join("crates/component");
    let right = packaged.path().join("component-1.0.0");
    fs::create_dir_all(left.join("src/nested")).unwrap();
    fs::create_dir_all(right.join("src/nested")).unwrap();
    fs::write(left.join("src/lib.rs"), "pub fn compile() {\n}\n").unwrap();
    fs::write(right.join("src/lib.rs"), "pub fn compile() {\r\n}\r\n").unwrap();
    let baseline = fingerprint("component", &left, &["src"]).unwrap();
    assert_eq!(
        baseline,
        fingerprint("component", &right, &["src"]).unwrap()
    );
    assert_ne!(baseline.as_str(), "0".repeat(64));

    fs::write(right.join("presentation.rs"), "unselected").unwrap();
    assert_eq!(
        baseline,
        fingerprint("component", &right, &["src"]).unwrap()
    );
    let source = right.join("src/nested/added.rs");
    fs::write(&source, "pub fn lower() {}\n").unwrap();
    let added = fingerprint("component", &right, &["src"]).unwrap();
    assert_ne!(
        baseline, added,
        "a newly added compiler input must invalidate"
    );
    fs::write(&source, "pub fn changed() {}\n").unwrap();
    assert_ne!(added, fingerprint("component", &right, &["src"]).unwrap());
    fs::remove_file(source).unwrap();
    assert_eq!(
        baseline,
        fingerprint("component", &right, &["src"]).unwrap()
    );
}

#[test]
fn missing_empty_or_foreign_roots_are_refused() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    assert!(fingerprint("component", root.path(), &["src"]).is_err());
    assert!(fingerprint("component", root.path(), &["absent"]).is_err());
    assert!(fingerprint("component", root.path(), &["../foreign"]).is_err());
    assert!(fingerprint("component", root.path(), &[]).is_err());
}
