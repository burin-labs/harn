//! Does an inherited session environment hand a child the parent's `PATH`
//! unchanged, all the way through a real spawn?
//!
//! # The claim
//!
//! `EnvironmentPolicyKind::Inherited` promises the child sees what the parent
//! sees. `PATH` is the one variable where that promise is easy to break
//! silently: an allowlist that rebuilds the value, or normalizes its key's
//! case, produces an environment that still *looks* correct and still carries
//! a `PATH`, but resolves programs differently from the parent. On Windows the
//! operating system treats environment keys case-insensitively while a
//! Rust-side map does not, so a rebuilt entry can sit beside the inherited one
//! (harn#7993).
//!
//! # Why this shape
//!
//! An earlier version of this probe shelled out to the platform command
//! interpreter to ask whether a well-known program resolved on `PATH`, once in
//! the parent and once in the child. It had three problems. It spawned a
//! shell, which the repository's test-pattern gate forbids because a shell
//! makes the probe depend on the host's command language. It returned early
//! when that program was absent, so on a machine without it the test asserted
//! nothing while still reporting success. And it was gated to one platform, so
//! the other platform had no coverage of the same seam at all.
//!
//! This version spawns the hermetic helper binary instead. The helper is built
//! from this workspace, so it exists on every machine and every target, and it
//! reports the child's own view of a variable by name. That turns the claim
//! into a direct comparison: the value `PATH` holds inside a child launched
//! from the resolved environment must be byte-for-byte the value the parent
//! holds. There is nothing to skip and no host shell in the path.

use std::collections::BTreeMap;

use harn_vm::security::session_environment::SessionEnvironment;
use harn_vm::security::EnvironmentPolicyKind;

use crate::support;

fn no_env(_: &str) -> Option<String> {
    None
}

/// Build the child environment an inherited session would produce.
fn inherited_child_environment() -> BTreeMap<String, String> {
    let environment = SessionEnvironment::launch(EnvironmentPolicyKind::Inherited, vec![], &no_env)
        .expect("an inherited launch never fails");
    let resolve_secret = |_: &str, _: &str| -> Option<String> { None };
    harn_vm::security::resolve_env(&environment, &no_env, &resolve_secret)
        .expect("an inherited resolve_env never fails")
}

/// The map must carry exactly one `PATH`-shaped key, and its value must equal
/// the parent's. Two keys differing only in case is the harn#7993 shape: both
/// are present, the map looks healthy, and which one the operating system uses
/// is not something this process decides.
#[test]
fn an_inherited_environment_carries_one_path_equal_to_the_parents() {
    let env = inherited_child_environment();
    let path_entries: Vec<(&String, &String)> = env
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .collect();
    assert_eq!(
        path_entries.len(),
        1,
        "exactly one PATH-shaped key must reach the child, got {:?}",
        path_entries.iter().map(|(key, _)| key).collect::<Vec<_>>(),
    );
    let parent_path = std::env::var("PATH").expect("this process has a PATH to compare against");
    assert_eq!(
        path_entries[0].1, &parent_path,
        "an inherited child's PATH must equal the parent's PATH byte for byte",
    );
}

/// The assertion above reads the map this process built. This one reads what a
/// real child process actually received, which is the only view that proves
/// the value survived the spawn rather than the construction.
#[test]
fn a_child_spawned_from_an_inherited_environment_sees_the_parents_path() {
    let env = inherited_child_environment();
    let parent_path = std::env::var("PATH").expect("this process has a PATH to compare against");

    let output = std::process::Command::new(support::process_helper())
        .args(["--env", "PATH"])
        .env_clear()
        .envs(&env)
        .output()
        .expect("the hermetic helper must spawn");

    assert!(
        output.status.success(),
        "the helper exited {:?}; stderr={:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    let child_path = String::from_utf8(output.stdout).expect("the helper writes UTF-8");
    // Liveness before the comparison. An empty read would satisfy nothing, but
    // it would also not look like a failure of the claim under test, and the
    // difference matters when this test goes red.
    assert!(
        !child_path.is_empty(),
        "the child reported an empty PATH, so the comparison below measures nothing",
    );
    assert_eq!(
        child_path, parent_path,
        "a child launched from an inherited environment resolved a different PATH \
         from its parent's",
    );
}
