//! Does a hermetic session profile actually close every child environment?
//!
//! `security::hermetic_env` documents `resolve_env` as "the *single* code path
//! that builds a child environment", and the hermetic profile's contract is
//! that no credential crosses into a spawned child. That contract is only as
//! strong as its weakest spawn seam, so this probes each process builtin
//! directly with a secret-shaped variable set in the parent environment.
//!
//! Every probe spawns the hermetic `harn-test-echo-env` helper binary directly,
//! so the same resolver and sandbox-funnel coverage runs on every target.

mod support;

use harn_vm::security::session_grants::SessionProfile;

const SECRET: &str = "HARN_PROBE_FAKE_API_KEY";
const SECRET_VALUE: &str = "sk-probe-must-not-cross";

/// Baseline: the governed seam (`exec`, via `process_command_config`) must not
/// leak. If this fails the probe itself is wrong, not the runtime.
#[test]
fn exec_does_not_leak_the_secret() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    let out = support::logged_hermetic(&format!(
        r#"const r = exec({}, "{}")
log(r.stdout == "" ? "CLOSED" : "LEAKED:" + r.stdout)"#,
        support::harn_quote(&support::process_helper()),
        SECRET,
    ))
    .expect("exec result");
    assert_eq!(out, vec!["CLOSED".to_string()], "governed seam leaked");
}

#[test]
fn exec_opts_does_not_leak_the_secret() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    let out = support::logged_hermetic(&format!(
        r#"const r = exec_opts([{}, "{}"], {{}})
log(r.stdout == "" ? "CLOSED" : "LEAKED:" + r.stdout)"#,
        support::harn_quote(&support::process_helper()),
        SECRET,
    ))
    .expect("exec_opts result");
    assert_eq!(out, vec!["CLOSED".to_string()], "exec_opts leaked");
}

#[test]
fn spawn_captured_does_not_leak_the_secret() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    let out = support::logged_hermetic(&format!(
        r#"const r = spawn_captured({{ cmd: {}, args: ["{}"] }})
log(r.stdout == "" ? "CLOSED" : "LEAKED:" + r.stdout)"#,
        support::harn_quote(&support::process_helper()),
        SECRET,
    ))
    .expect("spawn_captured result");
    assert_eq!(out, vec!["CLOSED".to_string()], "spawn_captured leaked");
}

/// `process.exec` is the host op behind the agent's own shell tool — the
/// highest-value seam of all, since that is where model-authored commands run.
#[test]
fn host_process_exec_does_not_leak_the_secret() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    let out = support::logged_hermetic(&format!(
        r#"const r = host_call("process.exec", {{ mode: "argv", argv: [{}, "{}"] }})
log(r.stdout == "" ? "CLOSED" : "LEAKED:" + r.stdout)"#,
        support::harn_quote(&support::process_helper()),
        SECRET,
    ))
    .expect("host process result");
    assert_eq!(
        out,
        vec!["CLOSED".to_string()],
        "process.exec host op leaked"
    );
}

/// The funnel itself. `harn-hostlib`'s `prepare_command` — the spawner behind
/// the agent's own `run_command` tool — builds its child through
/// `std_command_for`, as do several orchestration seams. Pinning the funnel
/// directly covers all of them, including callers outside this crate that a
/// Harn-level probe in `harn-vm` cannot reach.
#[test]
fn std_command_for_returns_a_closed_command() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    harn_vm::reset_thread_local_state();
    harn_vm::stdlib::process::set_session_profile(Some(SessionProfile::hermetic()));
    let mut command = harn_vm::process_sandbox::std_command_for(
        &support::process_helper(),
        &[SECRET.to_string()],
    )
    .expect("build command");
    let out = command.output().expect("spawn");
    harn_vm::stdlib::process::set_session_profile(None);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "std_command_for handed the child an open environment"
    );
}
