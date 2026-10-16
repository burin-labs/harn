//! Host capability expectations shared by CLI unit and integration product tests.

use harn_vm as vm;

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../harn-vm/tests/support/sandbox_host.rs"
));

/// Called only after the real command path ran. Returning true means its exact
/// unsupported-host refusal was asserted, never that execution was skipped.
pub fn assert_unavailable_worktree_refusal(outcome: &crate::commands::run::RunOutcome) -> bool {
    let Some(refusal) = unavailable_worktree_sandbox() else {
        return false;
    };
    assert_ne!(
        outcome.exit_code, 0,
        "unsupported host must refuse the command"
    );
    assert!(
        outcome.stderr.contains(&refusal.to_string()),
        "{}",
        outcome.stderr
    );
    assert!(
        outcome.stdout.is_empty(),
        "refused command produced output: {}",
        outcome.stdout
    );
    true
}
