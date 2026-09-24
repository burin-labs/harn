//! harn#8477: the process host must build a tool child's environment from the
//! session environment, and must refuse an inheriting spawn when none exists.
//!
//! # What was wrong
//!
//! An inheriting spawn handed the child the calling process's whole
//! environment and removed only names matching a denylist. The denylist is a
//! list of explicit names, a set of prefixes, and seven suffixes, so every
//! credential named outside those reached a subprocess running a
//! model-authored command. A granted session policy did not change that,
//! because nothing consulted it here.
//!
//! # Why these assertions and not the obvious ones
//!
//! Asserting that a credential-shaped name is absent from the child passes on
//! the OLD code: the denylist does remove a great many of them. The decoy
//! below is therefore named so the denylist does NOT catch it, which is the
//! only shape of assertion that can tell the policy apart from the denylist.
//! The direction control matters just as much: a host that scrubbed the child
//! down to nothing would satisfy every absence assertion here for a reason
//! that has nothing to do with the policy, and would break every tool.

#![cfg(unix)]

use std::sync::Arc;

use harn_hostlib::tools::ToolsCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
use harn_vm::security::{EnvironmentPolicyKind, GrantSourceSpec, GrantSpec, SessionEnvironment};
use harn_vm::VmValue;

/// Credential-shaped, and deliberately spelled so the denylist misses it: the
/// suffix list carries `_CREDENTIALS` and not `_CREDENTIAL`. On the old code
/// this name reached the child. It is the difference between measuring the
/// policy and measuring the denylist.
const DECOY: &str = "HARN_PROBE_FAKE_CREDENTIAL";
/// Credential-shaped AND caught by the denylist. Present so the two layers can
/// be told apart, and so the granted case below can prove the denylist does
/// not overrule a declaration.
const DENYLISTED: &str = "HARN_PROBE_FAKE_API_KEY";
/// Literals. Nothing secret enters this file even if an assertion fails and
/// the name is printed.
const PROBE_VALUE: &str = "probe-must-not-cross-8477";

fn value(text: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(text))
}

fn call(request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    let entry = registry
        .find("hostlib_tools_run_command")
        .expect("run_command builtin must be registered");
    (entry.handler)(&[VmValue::dict(request)])
}

/// Report the child's environment variable NAMES. Names only: a probe that
/// printed values would write the very thing this issue is about into a test
/// log. `cut` rather than a substitution, so a value containing a newline
/// cannot contribute a line that looks like a name.
fn name_probe_request(cwd: &str) -> harn_vm::value::DictMap {
    let mut request = harn_vm::value::DictMap::new();
    request.insert(
        "argv".into(),
        VmValue::List(Arc::new(
            ["sh", "-c", "env | cut -d= -f1 | sort"]
                .into_iter()
                .map(value)
                .collect(),
        )),
    );
    request.insert("cwd".into(), value(cwd));
    request
}

fn child_names(response: &VmValue) -> Vec<String> {
    let VmValue::Dict(dict) = response else {
        panic!("run_command must answer with a dict, got {response:?}");
    };
    let Some(VmValue::String(stdout)) = dict.get("stdout") else {
        panic!("run_command response carried no stdout: {dict:?}");
    };
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Install a session environment for the duration of a test and clear it
/// afterward, including on the panicking path. A leaked policy would make the
/// next test in this binary pass or fail for a reason it never declared.
struct InstalledEnvironment;

impl InstalledEnvironment {
    fn granted(grants: Vec<GrantSpec>) -> Self {
        let environment =
            SessionEnvironment::launch(EnvironmentPolicyKind::Granted, grants, &|name| {
                std::env::var(name).ok()
            })
            .expect("the granted policy must launch");
        harn_vm::stdlib::process::set_session_environment(Some(environment));
        Self
    }
}

impl Drop for InstalledEnvironment {
    fn drop(&mut self) {
        harn_vm::stdlib::process::set_session_environment(None);
    }
}

fn env_grant(name: &str) -> GrantSpec {
    GrantSpec {
        name: name.to_string(),
        source: GrantSourceSpec::Env {
            var: name.to_string(),
        },
        expose_as_env: Some(name.to_string()),
        for_command: None,
    }
}

/// Set both probe variables in the calling process, restoring whatever was
/// there before. They stand in for the operator's unrelated credentials.
struct ProbeVars {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl ProbeVars {
    fn set() -> Self {
        let previous = [DECOY, DENYLISTED]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect();
        // SAFETY: the shared lock in `process_tools_e2e` serializes every
        // environment-mutating test in this binary, and both names are
        // restored on drop.
        unsafe {
            std::env::set_var(DECOY, PROBE_VALUE);
            std::env::set_var(DENYLISTED, PROBE_VALUE);
        }
        Self { previous }
    }
}

impl Drop for ProbeVars {
    fn drop(&mut self) {
        for (name, previous) in &self.previous {
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

/// A regression guard, and stated as one rather than as proof of this change.
///
/// It passes with this change reverted, because the closing step in
/// `std_command_for` already bounds the child whenever a policy is installed
/// on the spawning task. That is the point: the allowlist path was never
/// broken, it was simply not reached. What this test guards is that the path
/// keeps being reached, so a future seam cannot quietly stop closing the
/// environment. The two assertions this change actually owns are below.
#[test]
fn a_granted_session_environment_bounds_the_child() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let _probes = ProbeVars::set();
    let workspace = tempfile::tempdir().expect("workspace");
    let _installed = InstalledEnvironment::granted(Vec::new());

    let cwd = workspace.path().to_string_lossy().into_owned();
    let response = call(name_probe_request(&cwd)).expect("the probe must run");
    let names = child_names(&response);

    // Liveness first. An empty read would satisfy every absence assertion
    // below for the one reason that disqualifies them.
    assert!(
        names.contains(&"PATH".to_string()),
        "the child reported no PATH, so nothing here was measured; it held {} names: {names:?}",
        names.len(),
    );
    assert!(
        !names.contains(&DECOY.to_string()),
        "the child inherited {DECOY}, which the policy never granted and the \
         denylist does not catch; it held {} names: {names:?}",
        names.len(),
    );
    assert!(
        !names.contains(&DENYLISTED.to_string()),
        "the child inherited {DENYLISTED}; it held {} names: {names:?}",
        names.len(),
    );
}

/// The direction control. A name the session grants must still arrive, and the
/// denylist must not overrule the declaration even when the name matches it.
/// Without this, scrubbing the child to nothing would look like a pass.
#[test]
fn a_granted_name_reaches_the_child_even_when_the_denylist_matches_it() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let _probes = ProbeVars::set();
    let workspace = tempfile::tempdir().expect("workspace");
    let _installed = InstalledEnvironment::granted(vec![env_grant(DENYLISTED)]);

    let cwd = workspace.path().to_string_lossy().into_owned();
    let response = call(name_probe_request(&cwd)).expect("the probe must run");
    let names = child_names(&response);

    assert!(
        names.contains(&"PATH".to_string()),
        "the child reported no PATH, so nothing here was measured; it held {} names: {names:?}",
        names.len(),
    );
    assert!(
        names.contains(&DENYLISTED.to_string()),
        "the session granted {DENYLISTED} and the child did not receive it, so a \
         name-shaped guess overruled an explicit declaration; it held {} names: {names:?}",
        names.len(),
    );
    assert!(
        !names.contains(&DECOY.to_string()),
        "granting one name must not open the rest; the child still holds {DECOY}",
    );
}

/// Absence must not read as permission. With no policy installed the closing
/// step is a no-op, so an inheriting spawn would hand over the calling
/// process's own environment. It refuses instead, and the refusal names the
/// mode so the caller can see which of its options produced it.
#[test]
fn a_missing_session_environment_refuses_an_inheriting_spawn() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let workspace = tempfile::tempdir().expect("workspace");
    harn_vm::stdlib::process::set_session_environment(None);

    let cwd = workspace.path().to_string_lossy().into_owned();
    let error = call(name_probe_request(&cwd))
        .expect_err("an inheriting spawn with no session environment must refuse");

    let message = error.to_string();
    assert!(
        message.contains("environment policy missing"),
        "the refusal must say what is missing, got: {message}"
    );
    assert!(
        message.contains("inherit_clean"),
        "the refusal must name the mode that produced it, got: {message}"
    );
}

/// The explicit escape stays open: a caller that supplies the child's whole
/// environment is not inheriting anything, so it needs no policy behind it.
/// Without this, the refusal above would read as "the host cannot spawn".
#[test]
fn an_explicit_replace_spawn_still_runs_without_a_session_environment() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let workspace = tempfile::tempdir().expect("workspace");
    harn_vm::stdlib::process::set_session_environment(None);

    let cwd = workspace.path().to_string_lossy().into_owned();
    let mut request = name_probe_request(&cwd);
    let mut env = harn_vm::value::DictMap::new();
    env.insert("PATH".into(), value("/usr/bin:/bin"));
    request.insert("env".into(), VmValue::dict(env));
    request.insert("env_mode".into(), value("replace"));

    let response = call(request).expect("an explicit replace spawn must still run");
    let names = child_names(&response);
    assert!(
        names.contains(&"PATH".to_string()),
        "the replace spawn lost the environment it was handed; it held {} names: {names:?}",
        names.len(),
    );
}

/// A child spawned through the process-owner guardian gets the session's
/// environment, not the launcher's.
///
/// # The defect
///
/// A background command crosses a re-exec: the guardian rebuilds the payload
/// from a serialized program, argv, and environment. The payload command had
/// been cleared and rebuilt from the session's resolved set, but the request
/// said whether to clear from the requested env MODE, which for an inheriting
/// mode was "no". The guardian then inherited its own environment behind the
/// explicit entries, and every name the session never declared reached the
/// child. The foreground spawn never crosses the handover, which is why the
/// test above stayed green through it.
///
/// # Reading it
///
/// `background: true` is what routes the spawn through the guardian. PATH is
/// the liveness leg: an empty read would satisfy the absence assertion for a
/// reason that has nothing to do with the policy.
#[test]
fn a_child_spawned_through_the_owner_death_guardian_gets_only_the_session_environment() {
    use super::process_tools_e2e::{call as tool_call, require_dict, require_str};

    let _env_guard = super::process_tools_e2e::lock_env();
    let _probes = ProbeVars::set();
    let workspace = tempfile::tempdir().expect("workspace");
    let _installed = InstalledEnvironment::granted(Vec::new());

    let cwd = workspace.path().to_string_lossy().into_owned();
    let mut request = name_probe_request(&cwd);
    request.insert("background".into(), VmValue::Bool(true));
    let started = require_dict(
        tool_call("hostlib_tools_run_command", request).expect("the background probe must start"),
    );
    let mut wait = harn_vm::value::DictMap::new();
    wait.insert(
        "handle_id".into(),
        value(&require_str(&started, "handle_id")),
    );
    wait.insert("timeout_ms".into(), VmValue::Int(20_000));
    let waited = require_dict(
        tool_call("hostlib_tools_wait_command", wait).expect("the background probe must finish"),
    );
    let names: Vec<String> = require_str(&waited, "stdout")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    assert!(
        names.contains(&"PATH".to_string()),
        "the guardian child reported no PATH, so nothing here was measured; it held {} names: \
         {names:?} (wait response: {waited:?})",
        names.len(),
    );
    assert!(
        !names.contains(&DECOY.to_string()),
        "the guardian child inherited {DECOY}, which the session never granted; it held {} \
         names: {names:?}",
        names.len(),
    );
}
