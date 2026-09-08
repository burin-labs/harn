//! Whether a child spawned through owner-death containment is actually confined.
//!
//! Separate from `process_tools_e2e` because it asks a different question. That
//! file smoke-tests the process-tool wiring against real subprocesses; this one
//! measures a security boundary, and needs a kernel that can enforce it. It
//! reuses that file's spawn helpers, including the guardian re-exec fixture,
//! because the boundary has to be measured through the same production call
//! shape rather than a bespoke spawn built for the test.

#![cfg(all(unix, target_os = "linux"))]

use harn_vm::VmValue;

use super::process_tools_e2e::{
    call, dict, lock_env, require_dict, require_int, require_str, vlist_str, vstr,
};

/// The environment declaration that this host must enforce Landlock.
///
/// Same name the VM's live-Landlock tests read, so one CI job setting can
/// govern every test that would otherwise skip itself into a green tick.
const REQUIRE_LIVE_LANDLOCK_ENV: &str = "HARN_REQUIRE_LANDLOCK_TESTS";

/// Whether this kernel can enforce Landlock at all.
fn landlock_enforcing() -> bool {
    let version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0_usize,
            1_u32,
        )
    };
    version > 0
}

/// Open the boundary test, or say by name why it is not being measured.
///
/// A host without Landlock cannot answer the question this test asks, and an
/// assertion that passes there would mean the opposite of what it says. But a
/// silent skip is how a boundary stops being tested without anyone noticing, so
/// a runner class that is supposed to confine declares itself through
/// `HARN_REQUIRE_LANDLOCK_TESTS` and the skip becomes a failure there.
fn require_landlock(test: &str) -> bool {
    if landlock_enforcing() {
        return true;
    }
    let modules = std::fs::read_to_string("/sys/kernel/security/lsm")
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|error| format!("<unreadable: {error}>"));
    let required = std::env::var(REQUIRE_LIVE_LANDLOCK_ENV)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "off" | "no")
        })
        .unwrap_or(false);
    assert!(
        !required,
        "[{test}] Landlock is unavailable on this host and \
         {REQUIRE_LIVE_LANDLOCK_ENV} declares this host must enforce it. \
         active security modules: {modules}"
    );
    eprintln!(
        "[{test}] SKIPPED: Landlock unavailable on this host, the guardian's confinement was \
         not exercised. active security modules: {modules}"
    );
    false
}

/// A payload spawned through owner-death containment is confined.
///
/// # The defect
///
/// The guardian re-execs this binary and rebuilds the payload command from a
/// serialized program, args, cwd and environment. This backend installs its
/// confinement from a `pre_exec` callback, and a callback has no representation
/// in that projection, so it was dropped in the handover: every payload spawned
/// through this path ran completely unconfined while the ruleset was built,
/// never entered, and reported as enforced.
///
/// # Why it goes through the tool
///
/// The direct spawn path was already confined and stayed confined throughout,
/// so a test that drives it proves nothing about the bug. Only a spawn that
/// crosses the guardian can, which is why this asks for a background command:
/// that is the request shape that takes owner-death containment.
///
/// # Reading it
///
/// The in-workspace leg is the direction control. A host that refused every
/// spawn would satisfy the outside assertion for a reason that has nothing to
/// do with confinement, and without a leg that must succeed, a guardian that
/// never ran at all would read as a clean denial.
#[test]
fn the_owner_death_guardian_confines_the_payload_it_spawns() {
    use harn_vm::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };

    if !require_landlock("owner-death-guardian-confinement") {
        return;
    }

    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let outside_root = outside.path().canonicalize().expect("canonical outside");
    let inside_file = workspace_root.join("inside.txt");
    let outside_file = outside_root.join("secret.txt");
    std::fs::write(&inside_file, "INSIDE\n").expect("write inside");
    std::fs::write(&outside_file, "OUTSIDE\n").expect("write outside");

    let _env_guard = lock_env();
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace_root.to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });

    let read = |path: &std::path::Path| {
        let mut request = dict();
        request.insert("argv".into(), vlist_str(&["cat", &path.to_string_lossy()]));
        request.insert("cwd".into(), vstr(&workspace_root.to_string_lossy()));
        // Background is what routes the spawn through owner-death containment.
        request.insert("background".into(), VmValue::Bool(true));
        let start = require_dict(call("hostlib_tools_run_command", request).expect("run_command"));
        let mut wait = dict();
        wait.insert("handle_id".into(), vstr(&require_str(&start, "handle_id")));
        wait.insert("timeout_ms".into(), VmValue::Int(20_000));
        require_dict(call("hostlib_tools_wait_command", wait).expect("wait_command"))
    };

    let inside = read(&inside_file);
    let outside_result = read(&outside_file);
    pop_execution_policy();

    assert_eq!(
        require_int(&inside, "exit_code"),
        0,
        "the in-workspace read must succeed, or nothing here measured confinement: {inside:?}"
    );
    assert_ne!(
        require_int(&outside_result, "exit_code"),
        0,
        "a payload spawned through the guardian must not read outside the workspace: \
         {outside_result:?}"
    );
}
