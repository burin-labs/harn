//! `harn fix --migrate-operator-syntax` on the real binary: source written for
//! the pre-0.11 operator grammar is refused with a targeted diagnostic, the
//! migration rewrites it, and the rewritten program prints what the old
//! grammar meant.

use std::path::{Path, PathBuf};

use crate::test_util::process::harn_e2e_command;

fn temp_root(name: &str) -> PathBuf {
    crate::test_util::cli::fresh_temp_dir("harn-operator-migration", name)
}

fn run(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = harn_e2e_command()
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn harn");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

const LEGACY_SOURCE: &str = "\
pipeline default(harness: Harness) {
  const b: int? = 5
  harness.stdio.println(b ?? 0 + 1)
  let total = 0
  for i in 1 to 3 {
    total = total + i
  }
  harness.stdio.println(total)
  harness.stdio.println(len(0 to 4 exclusive))
}
";

#[test]
fn migration_rewrites_legacy_operators_and_keeps_their_meaning() {
    let dir = temp_root("rewrite");
    let path = dir.join("legacy.harn");
    std::fs::write(&path, LEGACY_SOURCE).unwrap();

    let (_stdout, stderr, code) = run(&dir, &["run", "legacy.harn"]);
    assert_ne!(code, 0, "legacy ranges must not parse: {stderr}");
    assert!(stderr.contains("HARN-PAR-007"), "{stderr}");

    let (stdout, stderr, code) = run(
        &dir,
        &["fix", "--apply", "--migrate-operator-syntax", "legacy.harn"],
    );
    assert_eq!(code, 0, "migration must succeed: {stdout}{stderr}");
    let migrated = std::fs::read_to_string(&path).unwrap();
    assert!(migrated.contains("(b ?? 0) + 1"), "{migrated}");
    assert!(migrated.contains("for i in 1..=3 {"), "{migrated}");
    assert!(migrated.contains("len(0..4)"), "{migrated}");

    let (stdout, stderr, code) = run(&dir, &["run", "legacy.harn"]);
    assert_eq!(code, 0, "migrated program must run: {stderr}");
    // The old grammar read `b ?? 0 + 1` as `(b ?? 0) + 1`.
    assert_eq!(stdout.lines().collect::<Vec<_>>(), ["6", "6", "4"]);

    let (stdout, _stderr, code) = run(
        &dir,
        &["fix", "--apply", "--migrate-operator-syntax", "legacy.harn"],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("changed=0"),
        "a second run must be a no-op: {stdout}"
    );
}
