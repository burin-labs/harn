//! Does an isolated session environment actually close every child environment?
//!
//! `security::environment_policy` documents `resolve_env` as "the *single* code path
//! that builds a child environment", and the isolated policy's contract is
//! that no credential crosses into a spawned child. That contract is only as
//! strong as its weakest spawn seam, so this probes each process builtin
//! directly with a secret-shaped variable set in the parent environment.
//!
//! Every probe spawns the `harn-test-echo-env` helper binary directly,
//! so the same resolver and sandbox-funnel coverage runs on every target.

use crate::support;

use harn_vm::security::session_environment::SessionEnvironment;

/// The canary every probe below plants in the launcher.
///
/// It deliberately matches nothing in `is_sensitive_env_name`: no provider
/// prefix, and none of the `_API_KEY` / `_TOKEN` / `_SECRET` / `_KEY` /
/// `_PASSWORD` / `_PASSWD` / `_CREDENTIALS` suffixes. It used to be
/// `HARN_PROBE_FAKE_API_KEY`, which the denylist strips whatever the session
/// policy does, so every assertion in this file was satisfied by the backstop
/// and would have passed with the policy never applied at all. A canary whose
/// name the denylist matches measures the denylist.
const SECRET: &str = "HARN_PROBE_SENSITIVE_HANDLE";
const SECRET_VALUE: &str = "sk-probe-must-not-cross";

#[test]
fn harness_env_uses_the_same_isolated_environment() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    let out = support::logged_isolated(&format!(
        r#"fn main(harness: Harness) {{
  harness.stdio.log(harness.env.get("{SECRET}") == nil ? "CLOSED" : "LEAKED")
}}"#,
    ))
    .expect("harness.env result");
    assert_eq!(out, vec!["CLOSED".to_string()]);
}

/// Baseline: the governed seam (`exec`, via `process_command_config`) must not
/// leak. If this fails the probe itself is wrong, not the runtime.
#[test]
fn exec_does_not_leak_the_secret() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    let out = support::logged_isolated(&format!(
        r#"fn main(harness: Harness) {{
  const r = harness.process.exec({}, "{}")
  harness.stdio.log(r.stdout == "" ? "CLOSED" : "LEAKED:" + r.stdout)
}}"#,
        support::harn_quote(&support::process_helper()),
        SECRET,
    ))
    .expect("exec result");
    assert_eq!(out, vec!["CLOSED".to_string()], "governed seam leaked");
}

#[test]
fn process_run_with_options_does_not_leak_the_secret() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    let out = support::logged_isolated(&format!(
        r#"fn main(harness: Harness) {{
  const r = harness.process.run({{program: {}, args: ["{}"], env: {{}}}})
  harness.stdio.log(r.stdout == "" ? "CLOSED" : "LEAKED:" + r.stdout)
}}"#,
        support::harn_quote(&support::process_helper()),
        SECRET,
    ))
    .expect("process.run options result");
    assert_eq!(
        out,
        vec!["CLOSED".to_string()],
        "process.run options leaked"
    );
}

#[test]
fn harness_process_run_does_not_leak_the_secret() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    let out = support::logged_isolated(&format!(
        r#"fn main(harness: Harness) {{
  const r = harness.process.run({{ program: {}, args: ["{}"] }})
  harness.stdio.log(r.stdout == "" ? "CLOSED" : "LEAKED:" + r.stdout)
}}"#,
        support::harn_quote(&support::process_helper()),
        SECRET,
    ))
    .expect("harness.process.run result");
    assert_eq!(
        out,
        vec!["CLOSED".to_string()],
        "harness.process.run leaked"
    );
}

/// The variadic `HarnessProcess.exec` adapter uses the same governed process
/// funnel as structured `run`; pin both public shapes against environment
/// leakage.
#[test]
fn harness_process_exec_does_not_leak_the_secret() {
    let _secret = support::EnvironmentGuard::set(SECRET, SECRET_VALUE);
    let out = support::logged_isolated(&format!(
        r#"fn main(harness: Harness) {{
  const r = harness.process.exec({}, "{}")
  harness.stdio.log(r.stdout == "" ? "CLOSED" : "LEAKED:" + r.stdout)
}}"#,
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
    harn_vm::stdlib::process::set_session_environment(Some(SessionEnvironment::isolated()));
    let mut command = harn_vm::process_sandbox::std_command_for(
        &support::process_helper(),
        &[SECRET.to_string()],
    )
    .expect("build command");
    let out = command.output().expect("spawn");
    harn_vm::stdlib::process::set_session_environment(None);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "",
        "std_command_for handed the child an open environment"
    );
}

/// A name the denylist cannot match, so only the policy can withhold it.
///
/// Every other probe in this file uses `HARN_PROBE_FAKE_API_KEY`, which ends in
/// `_API_KEY` and is therefore stripped by `is_sensitive_env_name` whatever the
/// session policy does. Those probes pass on the denylist alone and cannot see
/// a policy that is not applied at all.
const UNDECLARED: &str = "HARN_PROBE_UNDECLARED_HANDLE";
const UNDECLARED_VALUE: &str = "probe-must-not-cross";

/// A granted session closes the child as tightly as an isolated one.
///
/// The funnel test above pins `isolated`. `granted` is the policy an embedder
/// installs when it wants the allowlist plus a named set, and it is the one a
/// host uses for a run that executes model-authored tool calls, so it is the
/// policy whose failure matters most. Nothing pinned it.
///
/// The grant list is deliberately empty. A granted session with no grants is
/// the allowlist and nothing else, so it must behave exactly like an isolated
/// one, and any difference is the policy kind rather than a grant's resolution.
///
/// Both halves run in one spawn. The undeclared name must be absent, which is
/// the invariant. `PATH` must be present, which is the direction control: a
/// child handed an empty environment, or a helper that never ran, withholds
/// every name for a reason that has nothing to do with the policy and would
/// satisfy the first half on its own.
#[test]
fn std_command_for_closes_a_granted_session_against_an_undeclared_name() {
    let _undeclared = support::EnvironmentGuard::set(UNDECLARED, UNDECLARED_VALUE);
    harn_vm::reset_thread_local_state();
    let environment = SessionEnvironment::launch(
        harn_vm::security::EnvironmentPolicyKind::Granted,
        Vec::new(),
        &|name| std::env::var(name).ok(),
    )
    .expect("a granted policy with no grants must launch");
    harn_vm::stdlib::process::set_session_environment(Some(environment));
    let mut command = harn_vm::process_sandbox::std_command_for(
        &support::process_helper(),
        &[
            "--env".to_string(),
            UNDECLARED.to_string(),
            "PATH".to_string(),
        ],
    )
    .expect("build command");
    let out = command.output().expect("spawn");
    harn_vm::stdlib::process::set_session_environment(None);
    let observed = String::from_utf8_lossy(&out.stdout).to_string();
    let (undeclared, path) = observed.split_once('|').unwrap_or((observed.as_str(), ""));

    assert!(
        !path.is_empty(),
        "PATH did not reach the child, so this run proves nothing about the \
         undeclared name; the child reported {observed:?}"
    );
    assert!(
        undeclared.is_empty(),
        "a granted session handed the child a name it never declared; the \
         child reported it as set"
    );
}

/// A name the granted session declares, so the child must keep it.
const DECLARED: &str = "HARN_PROBE_DECLARED_HANDLE";
const DECLARED_VALUE: &str = "probe-may-cross";

/// The same question, with a grant present.
///
/// The grantless case above is not the shape a real embedder installs. A host
/// that wants the allowlist plus a named set declares those names as grants,
/// and that is the configuration a run executing model-authored tool calls
/// uses, so it is the one whose failure matters.
///
/// Only one `EnvironmentGuard` is taken. It holds a process-wide mutex for its
/// lifetime and is not reentrant, so a second `set` on this thread deadlocks
/// before the body runs. The other names are set under that same held lock and
/// restored by hand.
#[test]
fn std_command_for_closes_a_granted_session_that_carries_a_grant() {
    let _lock = support::EnvironmentGuard::set(UNDECLARED, UNDECLARED_VALUE);
    let previous_declared = std::env::var_os(DECLARED);
    std::env::set_var(DECLARED, DECLARED_VALUE);

    harn_vm::reset_thread_local_state();
    let environment = SessionEnvironment::launch(
        harn_vm::security::EnvironmentPolicyKind::Granted,
        vec![harn_vm::security::GrantSpec {
            name: DECLARED.to_string(),
            source: harn_vm::security::GrantSourceSpec::Env {
                var: DECLARED.to_string(),
            },
            expose_as_env: Some(DECLARED.to_string()),
            for_command: None,
        }],
        &|name| std::env::var(name).ok(),
    )
    .expect("a granted policy with an env-sourced grant must launch");
    harn_vm::stdlib::process::set_session_environment(Some(environment));
    let built = harn_vm::process_sandbox::std_command_for(
        &support::process_helper(),
        &[
            "--env".to_string(),
            UNDECLARED.to_string(),
            DECLARED.to_string(),
        ],
    );
    let observed = built.map(|mut command| {
        let out = command.output().expect("spawn");
        String::from_utf8_lossy(&out.stdout).to_string()
    });
    harn_vm::stdlib::process::set_session_environment(None);
    match previous_declared {
        Some(value) => std::env::set_var(DECLARED, value),
        None => std::env::remove_var(DECLARED),
    }

    let observed = observed.expect("building the command must not fail");
    assert_eq!(
        observed,
        format!("|{DECLARED_VALUE}"),
        "a granted session must hand the child the name it granted and nothing \
         it did not; the child reported {observed:?}"
    );
}

/// A granted name that is NOT set in this process, so its grant cannot resolve.
const UNRESOLVABLE: &str = "HARN_PROBE_ABSENT_HANDLE";

/// A grant naming an absent launcher variable is refused, by name.
///
/// The other two probes declare grants that resolve. This declares one whose
/// variable is not set, which is the case where a policy could quietly degrade
/// into admitting nothing and be mistaken for a close.
///
/// It does not happen: the policy refuses to launch, and this asserts that
/// typed refusal rather than reporting the absence of a child as a pass. There
/// is no early return and no branch whose green needs reading in a log. If the
/// runtime ever stops refusing, this goes red here instead of silently becoming
/// a test of nothing.
#[test]
fn a_grant_naming_an_absent_variable_is_refused_by_name() {
    let _lock = support::EnvironmentGuard::set(UNDECLARED, UNDECLARED_VALUE);
    std::env::remove_var(UNRESOLVABLE);
    harn_vm::reset_thread_local_state();

    let refusal = SessionEnvironment::launch(
        harn_vm::security::EnvironmentPolicyKind::Granted,
        vec![harn_vm::security::GrantSpec {
            name: UNRESOLVABLE.to_string(),
            source: harn_vm::security::GrantSourceSpec::Env {
                var: UNRESOLVABLE.to_string(),
            },
            expose_as_env: Some(UNRESOLVABLE.to_string()),
            for_command: None,
        }],
        &|name| std::env::var(name).ok(),
    )
    .expect_err("a grant naming an absent launcher variable must be refused");

    assert!(
        matches!(
            &refusal,
            harn_vm::security::session_environment::EnvironmentPolicyError::MissingEnv {
                name,
                var,
            } if name == UNRESOLVABLE && var == UNRESOLVABLE
        ),
        "the refusal must name the grant and the variable it could not read, \
         so an operator reads which declaration failed; got {refusal:?}"
    );
}
