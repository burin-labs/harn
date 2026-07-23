//! Concurrent package-import smoke coverage for the parallel test and strict
//! check command paths. Lease publication/collection ordering is covered by
//! `harn-modules/tests/package_generation_lease.rs` without timing assumptions.

use crate::test_util;

use std::process::Child;

use test_util::package_generation::{
    create_package_generation, package_content_hash, publish_package_generation,
};
use test_util::process::harn_e2e_command;

fn wait_success(child: &mut Child, command: &str) {
    let status = child.wait().unwrap();
    assert!(status.success(), "{command} exited with {status}");
}

#[test]
fn parallel_test_and_strict_check_share_installed_packages() {
    const MODULE: &str = "pub fn answer() -> int { return 42 }\n";
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::write(
        root.join("harn.toml"),
        "[package]\nname = \"consumer\"\n\n[dependencies]\nacme = { path = \"package-src\" }\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("package-src")).unwrap();
    std::fs::write(
        root.join("package-src/harn.toml"),
        "[package]\nname = \"acme\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(root.join("package-src/math.harn"), MODULE).unwrap();
    let lock = format!(
        concat!(
            "version = 4\n\n",
            "[[package]]\n",
            "name = \"acme\"\n",
            "source = \"path+{}\"\n",
            "content_hash = \"{}\"\n",
        ),
        url::Url::from_file_path(root.join("package-src").canonicalize().unwrap()).unwrap(),
        package_content_hash(&root.join("package-src")),
    );
    std::fs::write(root.join("harn.lock"), &lock).unwrap();
    let packages = create_package_generation(root);
    std::fs::create_dir_all(packages.join("acme")).unwrap();
    std::fs::copy(
        root.join("package-src/harn.toml"),
        packages.join("acme/harn.toml"),
    )
    .unwrap();
    std::fs::write(packages.join("acme/math.harn"), MODULE).unwrap();
    publish_package_generation(root, &lock);
    std::fs::create_dir(root.join("tests")).unwrap();
    std::fs::write(
        root.join("tests/test_answer.harn"),
        concat!(
            "import { answer } from \"acme/math\"\n\n",
            "pipeline test_answer(_task) { assert_eq(answer(), 42) }\n",
        ),
    )
    .unwrap();

    let mut tests = harn_e2e_command()
        .current_dir(root)
        .args(["test", "tests", "--parallel"])
        .spawn()
        .unwrap();
    let mut check = harn_e2e_command()
        .current_dir(root)
        .args(["check", "--strict-types", "tests"])
        .spawn()
        .unwrap();

    wait_success(&mut tests, "harn test --parallel");
    wait_success(&mut check, "harn check --strict-types");
}
