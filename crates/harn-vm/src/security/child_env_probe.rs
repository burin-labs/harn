//! Test support: a granted session and three planted engine variables, for
//! asserting what a spawned child can see.
//!
//! The session grants one variable to the whole session and one to Harn's own
//! process only, and leaves a third unadmitted. A child built under it must
//! report the session grant as set and the other two as unset; the same child
//! built with no policy installed reports all three as set, which is the
//! control that shows the probe can see a planted variable at all.

use super::session_environment::{
    EnvironmentPolicyKind, GrantAudience, GrantSourceSpec, GrantSpec, SessionEnvironment,
};
use crate::llm::test_env::ScopedEnvVar;

/// Set in the engine environment and never admitted by the policy.
pub(crate) const UNADMITTED: &str = "PROBE_CHILD_UNADMITTED";
/// Granted to Harn's own process only.
pub(crate) const IN_PROCESS: &str = "PROBE_CHILD_IN_PROCESS";
/// Granted to the whole session.
pub(crate) const SESSION: &str = "PROBE_CHILD_SESSION";
/// Every probed name, in report order.
pub(crate) const NAMES: [&str; 3] = [UNADMITTED, IN_PROCESS, SESSION];

/// Holds the planted variables (and the environment lock) for its lifetime.
pub(crate) struct PlantedEngineEnvironment {
    _vars: [ScopedEnvVar; 3],
}

pub(crate) fn plant() -> PlantedEngineEnvironment {
    PlantedEngineEnvironment {
        _vars: [
            ScopedEnvVar::set(UNADMITTED, "engine-only"),
            ScopedEnvVar::set(IN_PROCESS, "in-process-only"),
            ScopedEnvVar::set(SESSION, "session-wide"),
        ],
    }
}

/// The granted session described in the module docs, launched against the
/// planted engine environment.
pub(crate) fn granted_session() -> SessionEnvironment {
    let grant = |name: &str, var: &str, audience| GrantSpec {
        name: name.to_string(),
        source: GrantSourceSpec::Env {
            var: var.to_string(),
        },
        expose_as_env: Some(var.to_string()),
        for_command: None,
        expose_to: audience,
    };
    SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![
            grant("in_process", IN_PROCESS, GrantAudience::InProcess),
            grant("session", SESSION, GrantAudience::Session),
        ],
        &|name| std::env::var(name).ok(),
    )
    .expect("the probe session launches")
}

/// The report a child prints when every name in `set` is present and the
/// rest are absent: `NAME=set,NAME=unset,...` in [`NAMES`] order.
pub(crate) fn expected_report(set: &[&str]) -> String {
    NAMES
        .iter()
        .map(|name| {
            let state = if set.contains(name) { "set" } else { "unset" };
            format!("{name}={state}")
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A child command printing the same environment report as the MCP probe.
/// Python is already required by the cross-platform stdio transport tests.
pub(crate) fn report_command() -> (String, Vec<String>) {
    (
        "python3".to_string(),
        vec![
            "-c".to_string(),
            format!("import os; print({}, end='')", python_report()),
        ],
    )
}

/// A Python expression evaluating to the report for the child's environment.
pub(crate) fn python_report() -> String {
    let names = NAMES
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("','.join(n + '=' + ('set' if n in os.environ else 'unset') for n in [{names}])")
}

/// Installs a session environment on this thread and clears it on drop.
pub(crate) struct InstalledSession;

impl InstalledSession {
    pub(crate) fn install(environment: Option<SessionEnvironment>) -> Self {
        crate::stdlib::process::set_session_environment(environment);
        Self
    }
}

impl Drop for InstalledSession {
    fn drop(&mut self) {
        crate::stdlib::process::set_session_environment(None);
    }
}
