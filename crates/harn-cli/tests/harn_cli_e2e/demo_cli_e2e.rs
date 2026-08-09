//! Real-binary coverage for `harn demo` stdio dispatch.
//!
//! The main `demo_cli` target stays in-process and fast. This separate E2E
//! target owns the captured-stdio contract that requires the executable.

use crate::test_util;

use test_util::process::harn_e2e_command;

#[test]
fn bare_demo_with_captured_stdio_matches_explicit_list() {
    let cwd = tempfile::tempdir().expect("create isolated demo cwd");
    let run = |args: &[&str]| {
        harn_e2e_command()
            .args(args)
            .current_dir(cwd.path())
            .output()
            .expect("run harn demo")
    };

    let bare = run(&["demo"]);
    let explicit = run(&["demo", "--list"]);

    assert_eq!(
        (bare.status.code(), &bare.stdout, &bare.stderr),
        (Some(0), &explicit.stdout, &explicit.stderr)
    );
    assert!(!cwd.path().join(".harn-runs").exists());
}
