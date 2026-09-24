//! Runs the process-sandbox conformance contract against the live backend.
//!
//! The contract lives in `harn_vm::process_sandbox::conformance`: which cases
//! exist, what each must observe, and how an observation is judged. This
//! module drives a real child for every case through the process tools, the
//! same call an agent makes, and records what happened. `harn doctor sandbox`
//! prints the report; the conformance test asserts on it.
//!
//! Every case yields a verdict, measured or not, so an empty or partial run
//! cannot read as a clean one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, ProcessSandboxPolicy,
    SandboxProfile,
};
use harn_vm::process_sandbox::conformance::{
    judge, ConformanceCase, Expectation, Observation, SpawnRoute, Verdict,
};
use harn_vm::security::SessionEnvironment;
use harn_vm::VmValue;
use serde::Serialize;

use crate::tools::ToolsCapability;
use crate::{BuiltinRegistry, HostlibCapability};

/// Written into the outside file so a read is judged by content, not by an
/// exit code a missing tool would also produce.
const OUTSIDE_CONTENT: &str = "harn-conformance-outside-content";

/// The whole run: which backend answered and one report per case.
#[derive(Clone, Debug, Serialize)]
pub struct ConformanceReport {
    /// The active backend, as `harn doctor` names it.
    pub backend: String,
    /// The kernel mechanism that backend confines the filesystem with.
    pub filesystem_mechanism: String,
    /// Whether the backend reports it can enforce filesystem confinement on
    /// this host. A case that needs it and ran without it is `not_measured`.
    pub enforcing: bool,
    /// One report per contract case, in contract order.
    pub cases: Vec<CaseReport>,
}

/// One case: what the contract required, what the child did, and the verdict.
#[derive(Clone, Debug, Serialize)]
pub struct CaseReport {
    /// The case id, such as `fs.outside_write_refused`.
    pub case: &'static str,
    /// How the child was spawned.
    pub route: SpawnRoute,
    /// `None` when the case does not exist on this platform.
    pub expected: Option<Observation>,
    /// `None` when the case was not run.
    pub observed: Option<Observation>,
    /// The path or environment names the probe touched.
    pub target: String,
    /// The judgement, flattened so `verdict` is a top-level key.
    #[serde(flatten)]
    pub verdict: Verdict,
    /// Exit status and the last stderr line, for reading a failure.
    pub detail: String,
}

impl ConformanceReport {
    /// Cases whose verdict fails the contract.
    pub fn failing(&self) -> Vec<&CaseReport> {
        self.cases
            .iter()
            .filter(|case| case.verdict.is_failure())
            .collect()
    }

    /// Cases this host could not measure. Never a pass.
    pub fn not_measured(&self) -> Vec<&CaseReport> {
        self.cases
            .iter()
            .filter(|case| matches!(case.verdict, Verdict::NotMeasured { .. }))
            .collect()
    }

    /// How many cases were measured and hold.
    pub fn conforming(&self) -> usize {
        self.cases
            .iter()
            .filter(|case| case.verdict == Verdict::Conforms)
            .count()
    }

    /// One line naming every count and every failing case.
    pub fn summary_line(&self) -> String {
        let failing: Vec<&str> = self.failing().iter().map(|case| case.case).collect();
        let not_measured = self.not_measured().len();
        let conforming = self.conforming();
        format!(
            "harn.sandbox_conformance_summary backend={} cases={} conforms={conforming} \
             not_measured={not_measured} not_applicable={} failed={} failing={failing:?}",
            self.backend,
            self.cases.len(),
            self.cases.len() - conforming - not_measured - failing.len(),
            failing.len(),
        )
    }
}

impl CaseReport {
    /// One receipt line for this case.
    pub fn receipt_line(&self) -> String {
        format!(
            "harn.sandbox_conformance case={} route={:?} expected={:?} observed={:?} verdict={} {}",
            self.case,
            self.route,
            self.expected,
            self.observed,
            serde_json::to_string(&self.verdict).unwrap_or_default(),
            self.detail,
        )
    }
}

/// Run every case against the active backend.
///
/// Pushes each case's policy and an isolated session environment for the
/// duration of that case only. Background cases go through the process-owner
/// guardian, which re-executes the current binary; an embedder whose binary
/// does not own `main` must point the re-exec at its fixture first.
pub fn run_conformance() -> ConformanceReport {
    let backend = harn_vm::process_sandbox::active_backend_name().to_string();
    let enforcing = harn_vm::process_sandbox::active_backend_filesystem_available();
    let cases = ConformanceCase::ALL
        .iter()
        .map(|&case| run_case(case, enforcing))
        .collect();
    ConformanceReport {
        backend,
        filesystem_mechanism: harn_vm::process_sandbox::active_backend_filesystem_mechanism()
            .to_string(),
        enforcing,
        cases,
    }
}

fn run_case(case: ConformanceCase, enforcing: bool) -> CaseReport {
    let layout = match Layout::new() {
        Ok(layout) => layout,
        Err(error) => return broken(case, format!("could not build the case layout: {error}")),
    };
    let policy = case_policy(case, &layout);
    let expectation = case.expectation(&policy);
    let expected = match expectation {
        Expectation::Observe(observation) => Some(observation),
        Expectation::NotApplicable(_) => None,
    };
    let (argv, target) = probe(case, &layout);
    let report = |observed: Option<Observation>, verdict: Verdict, detail: String| CaseReport {
        case: case.id(),
        route: case.route(),
        expected,
        observed,
        target: target.clone(),
        verdict,
        detail,
    };
    if expected.is_none() {
        let verdict = judge(case, expectation, enforcing, Observation::Refused, &target);
        return report(None, verdict, String::new());
    }
    let _scope = CaseScope::enter(policy);
    let child = match spawn(case, &layout, &argv) {
        Ok(child) => child,
        Err(reason) => return report(None, Verdict::ProbeBroken { reason }, String::new()),
    };
    let observed = match observe(case, &child, &target) {
        Ok(observed) => observed,
        Err(Unmeasured::Broken(reason)) => {
            return report(None, Verdict::ProbeBroken { reason }, child.detail)
        }
        Err(Unmeasured::NothingToMeasure(reason)) => {
            return report(None, Verdict::NotMeasured { reason }, child.detail)
        }
    };
    let leaked = match &observed {
        Observed::Env { leaked, .. } if !leaked.is_empty() => leaked.join(","),
        _ => target.clone(),
    };
    let observation = observed.observation();
    let verdict = judge(case, expectation, enforcing, observation, &leaked);
    report(Some(observation), verdict, child.detail)
}

fn broken(case: ConformanceCase, reason: String) -> CaseReport {
    CaseReport {
        case: case.id(),
        route: case.route(),
        expected: None,
        observed: None,
        target: String::new(),
        verdict: Verdict::ProbeBroken { reason },
        detail: String::new(),
    }
}

/// The directories one case runs against.
///
/// Rooted in the home directory, not the system temp directory: the default
/// presets let a backend grant the shared temp tree (macOS grants all of
/// `/tmp` and `/var/folders` under `user_temp`), and an "outside" directory
/// there is not outside every writable root. Canonical, because a backend
/// that grants `/var/...` and a probe that touches `/private/var/...` would be
/// measuring the symlink instead of the policy.
struct Layout {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    outside: PathBuf,
    socket_root: PathBuf,
    /// A directory in the host's shared temp dir, holding a file another
    /// process left there. It is the one part of the layout outside `HOME`.
    shared_temp: tempfile::TempDir,
}

impl Layout {
    fn new() -> std::io::Result<Self> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .ok_or_else(|| std::io::Error::other("no HOME or USERPROFILE to root the layout in"))?;
        let root = tempfile::Builder::new()
            .prefix(".harn-sandbox-conformance-")
            .tempdir_in(home)?;
        // Windows canonicalizes to a `\\?\` path, which `cmd` redirection
        // refuses as invalid; the profile directory has no alias to resolve.
        let base = if cfg!(windows) {
            root.path().to_path_buf()
        } else {
            root.path().canonicalize()?
        };
        let workspace = base.join("workspace");
        let outside = base.join("outside");
        let socket_root = base.join("sockets");
        for dir in [&workspace, &outside, &socket_root] {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(outside.join("secret.txt"), OUTSIDE_CONTENT)?;
        let shared_temp = tempfile::Builder::new()
            .prefix("harn-sandbox-conformance-sibling-")
            .tempdir()?;
        std::fs::write(shared_temp.path().join("secret.txt"), OUTSIDE_CONTENT)?;
        Ok(Self {
            _root: root,
            workspace,
            outside,
            socket_root,
            shared_temp,
        })
    }
}

/// The policy a case runs under. Every case shares the worktree profile and a
/// process-exec ceiling; the socket cases add their roots, and one adds the
/// network grant that must not narrow them.
fn case_policy(case: ConformanceCase, layout: &Layout) -> CapabilityPolicy {
    let mut policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![layout.workspace.display().to_string()],
        side_effect_level: Some("process_exec".to_string()),
        ..CapabilityPolicy::default()
    };
    let socket_roots = vec![layout.socket_root.display().to_string()];
    match case {
        ConformanceCase::UnixSocketBindUnderRoot
        | ConformanceCase::UnixSocketBindOutsideRootRefused => {
            policy.process_sandbox = Box::new(ProcessSandboxPolicy {
                unix_socket_roots: socket_roots,
                ..ProcessSandboxPolicy::default()
            });
        }
        ConformanceCase::UnixSocketBindUnderRootWithNetwork => {
            policy.side_effect_level = Some("network".to_string());
            policy.process_sandbox = Box::new(ProcessSandboxPolicy {
                unix_socket_roots: socket_roots,
                ..ProcessSandboxPolicy::default()
            });
        }
        _ => {}
    }
    policy
}

/// The child's argv, and the path it touches (or what it reads, for the
/// environment cases).
fn probe(case: ConformanceCase, layout: &Layout) -> (Vec<String>, String) {
    let owned = |argv: &[&str]| argv.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    match case {
        ConformanceCase::WorkspaceWriteAdmitted => {
            let target = layout.workspace.join("probe.txt");
            (write_argv(&target), target.display().to_string())
        }
        ConformanceCase::OutsideWriteRefused | ConformanceCase::GuardianOutsideWriteRefused => {
            let target = layout.outside.join("probe.txt");
            (write_argv(&target), target.display().to_string())
        }
        ConformanceCase::OutsideReadRefused => read_probe(&layout.outside.join("secret.txt")),
        ConformanceCase::SiblingTempReadRefused => {
            read_probe(&layout.shared_temp.path().join("secret.txt"))
        }
        ConformanceCase::AtomicReplaceAdmitted => {
            // JavaScript for Automation reaches Foundation on every Mac; the
            // atomic option (1) is the call SwiftPM's build-file writes make.
            let target = layout.workspace.join("atomic.txt");
            let script = format!(
                "ObjC.import('Foundation'); \
                 $.NSString.alloc.initWithUTF8String('probe').dataUsingEncoding(4)\
                 .writeToURLOptionsError($.NSURL.fileURLWithPath({:?}), 1, null)",
                target.display().to_string()
            );
            (
                owned(&["osascript", "-l", "JavaScript", "-e", &script]),
                target.display().to_string(),
            )
        }
        ConformanceCase::SessionTempWriteAdmitted => {
            // The child names the file through its own TMPDIR; the target is
            // where the session temp dir must put it.
            let target =
                harn_vm::process_sandbox::workspace_local_tmpdir(&case_policy(case, layout))
                    .map(|dir| dir.join(SESSION_TEMP_PROBE))
                    .unwrap_or_default();
            let argv = if cfg!(windows) {
                owned(&[
                    "cmd",
                    "/c",
                    &format!("echo probe> \"%TEMP%\\{SESSION_TEMP_PROBE}\""),
                ])
            } else {
                owned(&[
                    "sh",
                    "-c",
                    &format!("touch \"$TMPDIR/{SESSION_TEMP_PROBE}\""),
                ])
            };
            (argv, target.display().to_string())
        }
        ConformanceCase::UndeclaredEnvironmentNameWithheld
        | ConformanceCase::GuardianUndeclaredEnvironmentNameWithheld => {
            let argv = if cfg!(windows) {
                owned(&["cmd", "/c", "set"])
            } else {
                owned(&["env"])
            };
            (argv, "undeclared launcher environment".to_string())
        }
        ConformanceCase::UnixSocketBindUnderRoot
        | ConformanceCase::UnixSocketBindUnderRootWithNetwork => {
            let target = layout.socket_root.join("probe.sock");
            (bind_argv(&target), target.display().to_string())
        }
        ConformanceCase::UnixSocketBindOutsideRootRefused => {
            let target = layout.outside.join("probe.sock");
            (bind_argv(&target), target.display().to_string())
        }
    }
}

const SESSION_TEMP_PROBE: &str = "harn-conformance-session-temp";

fn read_probe(target: &Path) -> (Vec<String>, String) {
    let path = target.display().to_string();
    let argv = if cfg!(windows) {
        vec!["cmd".into(), "/c".into(), "type".into(), path.clone()]
    } else {
        vec!["cat".into(), path.clone()]
    };
    (argv, path)
}

fn write_argv(target: &Path) -> Vec<String> {
    let path = target.display().to_string();
    if cfg!(windows) {
        // Separate arguments: one argument holding its own quotes is escaped
        // by the launcher's quoting and reaches `cmd` as a malformed path.
        vec![
            "cmd".into(),
            "/c".into(),
            "echo".into(),
            "probe>".into(),
            path,
        ]
    } else {
        vec!["touch".into(), path]
    }
}

/// perl is a real binary on every Unix CI image and on macOS, where
/// `/usr/bin/python3` is a shim that needs caches the profile does not grant.
/// On a backend that refuses socket roots the spawn is refused before this runs.
fn bind_argv(target: &Path) -> Vec<String> {
    if cfg!(windows) {
        return vec!["cmd".into(), "/c".into(), "exit 0".into()];
    }
    vec![
        "perl".into(),
        "-MSocket".into(),
        "-e".into(),
        "socket(S, PF_UNIX, SOCK_STREAM, 0) or die \"socket: $!\"; \
         bind(S, sockaddr_un($ARGV[0])) or die \"bind: $!\";"
            .into(),
        target.display().to_string(),
    ]
}

/// The child's captured result. `stdout` stays in memory: for the
/// environment cases it holds values, and nothing here prints it.
struct Child {
    spawn_refused: bool,
    stdout: String,
    detail: String,
}

fn call(name: &str, request: harn_vm::value::DictMap) -> Result<VmValue, String> {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    let entry = registry
        .find(name)
        .ok_or_else(|| format!("{name} is not registered"))?;
    (entry.handler)(&[VmValue::dict(request)]).map_err(|error| error.to_string())
}

fn spawn(case: ConformanceCase, layout: &Layout, argv: &[String]) -> Result<Child, String> {
    let text = |value: &str| VmValue::String(arcstr::ArcStr::from(value));
    let mut request = harn_vm::value::DictMap::new();
    request.insert(
        "argv".into(),
        VmValue::List(Arc::new(argv.iter().map(|arg| text(arg)).collect())),
    );
    request.insert("cwd".into(), text(&layout.workspace.display().to_string()));
    if case.route() == SpawnRoute::Guardian {
        // Background is the request shape that takes the process-owner
        // guardian, the same one auto-backgrounding converts to.
        request.insert("background".into(), VmValue::Bool(true));
    }
    let response = match call("hostlib_tools_run_command", request) {
        Ok(VmValue::Dict(dict)) => dict,
        Ok(other) => return Err(format!("run_command answered {other:?}")),
        Err(error) => {
            return Ok(Child {
                spawn_refused: true,
                stdout: String::new(),
                detail: error,
            })
        }
    };
    let response = if case.route() == SpawnRoute::Guardian {
        let Some(VmValue::String(handle)) = response.get("handle_id") else {
            return Err("a background start returned no handle".to_string());
        };
        let mut wait = harn_vm::value::DictMap::new();
        wait.insert("handle_id".into(), VmValue::String(handle.clone()));
        wait.insert("timeout_ms".into(), VmValue::Int(20_000));
        match call("hostlib_tools_wait_command", wait) {
            Ok(VmValue::Dict(dict)) => dict,
            other => return Err(format!("wait_command answered {other:?}")),
        }
    } else {
        response
    };
    let field = |key: &str| match response.get(key) {
        Some(VmValue::String(text)) => text.to_string(),
        _ => String::new(),
    };
    let exit = match response.get("exit_code") {
        Some(VmValue::Int(code)) => Some(*code),
        _ => None,
    };
    let stderr = field("stderr");
    let stderr_tail = stderr.lines().last().unwrap_or_default();
    Ok(Child {
        spawn_refused: false,
        stdout: field("stdout"),
        detail: format!("exit_code={exit:?} stderr_tail={stderr_tail:?}"),
    })
}

enum Observed {
    Effect(Observation),
    Env { leaked: Vec<String> },
}

impl Observed {
    fn observation(&self) -> Observation {
        match self {
            Self::Effect(observation) => *observation,
            Self::Env { leaked } if leaked.is_empty() => Observation::Refused,
            Self::Env { .. } => Observation::Admitted,
        }
    }
}

enum Unmeasured {
    Broken(String),
    NothingToMeasure(String),
}

/// Read what the child did from its effect rather than its exit code where
/// the effect is observable.
fn observe(case: ConformanceCase, child: &Child, target: &str) -> Result<Observed, Unmeasured> {
    if child.spawn_refused {
        return Ok(Observed::Effect(Observation::SpawnRefused));
    }
    let took_effect = |effect: bool| {
        Observed::Effect(if effect {
            Observation::Admitted
        } else {
            Observation::Refused
        })
    };
    match case {
        ConformanceCase::OutsideReadRefused | ConformanceCase::SiblingTempReadRefused => {
            Ok(took_effect(child.stdout.contains(OUTSIDE_CONTENT)))
        }
        ConformanceCase::UndeclaredEnvironmentNameWithheld
        | ConformanceCase::GuardianUndeclaredEnvironmentNameWithheld => {
            observe_environment(&child.stdout)
        }
        _ => Ok(took_effect(Path::new(target).exists())),
    }
}

/// Which launcher names the session never declared reached the child.
///
/// Every launcher name the isolated session does not admit is checked, and a
/// name counts as leaked only when the child holds it with the launcher's
/// value: the spawn path injects its own values under names such as `TMPDIR`,
/// and those are the runtime's declaration, not a leak. The runtime's
/// fixed-value locale pins are excluded by name, since a launcher that
/// happens to hold the same value would otherwise read as a leak. Values are
/// compared in memory and only names are ever reported.
fn observe_environment(stdout: &str) -> Result<Observed, Unmeasured> {
    let child: BTreeMap<String, String> = stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim().to_string(), value.to_string()))
        .filter(|(name, _)| !name.is_empty())
        .collect();
    // PATH is the liveness leg: an empty read would satisfy the absence
    // check for a reason unrelated to the policy.
    if !child.keys().any(|name| name.eq_ignore_ascii_case("PATH")) {
        return Err(Unmeasured::Broken(
            "the child reported no PATH, so its environment was not read".to_string(),
        ));
    }
    let mut declared = SessionEnvironment::isolated().admitted_environment_names();
    declared.extend(
        harn_vm::process_sandbox::deterministic_message_locale_env()
            .into_iter()
            .map(|(name, _)| name),
    );
    declared.push(harn_vm::process_sandbox::MESSAGE_LOCALE_OVERRIDE_ENV.to_string());
    let undeclared: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| !declared.contains(name))
        .collect();
    if undeclared.is_empty() {
        return Err(Unmeasured::NothingToMeasure(
            "the launcher holds no name the session leaves undeclared, so there was nothing \
             to withhold"
                .to_string(),
        ));
    }
    let leaked = undeclared
        .into_iter()
        .filter(|(name, value)| child.get(name) == Some(value))
        .map(|(name, _)| name)
        .collect();
    Ok(Observed::Env { leaked })
}

/// Pops the case policy and restores the caller's session environment,
/// including when a case panics, so one case cannot decide the next.
struct CaseScope {
    previous_environment: Option<SessionEnvironment>,
}

impl CaseScope {
    fn enter(policy: CapabilityPolicy) -> Self {
        let previous_environment = harn_vm::stdlib::process::current_session_environment();
        push_execution_policy(policy);
        harn_vm::stdlib::process::set_session_environment(Some(SessionEnvironment::isolated()));
        Self {
            previous_environment,
        }
    }
}

impl Drop for CaseScope {
    fn drop(&mut self) {
        harn_vm::stdlib::process::set_session_environment(self.previous_environment.take());
        pop_execution_policy();
    }
}
