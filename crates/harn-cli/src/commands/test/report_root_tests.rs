use super::common_report_root;

#[test]
fn file_target_uses_its_parent_as_the_report_root() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let test_file = temp.path().join("test_one.harn");
    std::fs::write(&test_file, "pipeline test_one(_task) { assert(true) }\n")
        .expect("write test file");

    assert_eq!(
        common_report_root(&[test_file.to_string_lossy().into_owned()]),
        temp.path().canonicalize().expect("canonical tempdir")
    );
}

#[test]
fn multiple_targets_use_their_common_ancestor_as_the_report_root() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let left = temp.path().join("left");
    let right = temp.path().join("right/nested");
    std::fs::create_dir_all(&left).expect("create left suite");
    std::fs::create_dir_all(&right).expect("create right suite");

    assert_eq!(
        common_report_root(&[
            left.to_string_lossy().into_owned(),
            right.to_string_lossy().into_owned(),
        ]),
        temp.path().canonicalize().expect("canonical tempdir")
    );
}
