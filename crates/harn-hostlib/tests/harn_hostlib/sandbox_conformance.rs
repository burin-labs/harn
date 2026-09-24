//! Runs every sandbox conformance case against the live backend.
//!
//! The contract lives in `harn_vm::process_sandbox::conformance`: which cases
//! exist, what each must observe, and how an observation is judged. This file
//! only drives a real child through the process tools, the same call an agent
//! makes, and records what happened. It runs unchanged on macOS, Linux and
//! Windows, so a backend difference is a named case failure here rather than a
//! red on one platform discovered later.
//!
//! # Reading the output
//!
//! Every case prints one `harn.sandbox_conformance` line, measured or not. The
//! summary line counts each verdict and names every failing case, so an empty
//! or partial run cannot read as a clean one. A host whose backend reports no
//! enforcement produces `not_measured` for the kernel cases, never a pass, and
//! a runner class that declares it must enforce (`HARN_REQUIRE_LANDLOCK_TESTS`)
//! turns any `not_measured` into a failure.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_hostlib::HostlibError;
use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, ProcessSandboxPolicy,
    SandboxProfile,
};
use harn_vm::process_sandbox::conformance::{
    judge, ConformanceCase, Expectation, Observation, SpawnRoute, Verdict,
};
use harn_vm::security::SessionEnvironment;
use harn_vm::VmValue;

/// Present in the launcher's environment and never declared by the session.
const UNDECLARED_NAME: &str = "HARN_CONFORMANCE_UNDECLARED_NAME";
/// Written into the outside file so a read is judged by content, not by an
/// exit code a missing tool would also produce.
const OUTSIDE_CONTENT: &str = "harn-conformance-outside-content";
/// The declaration that this runner class must enforce. Shared with the
/// Linux boundary tests so one CI setting governs every one of them.
const REQUIRE_ENFORCEMENT_ENV: &str = "HARN_REQUIRE_LANDLOCK_TESTS";

#[cfg(unix)]
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    super::process_tools_e2e::lock_env()
}

#[cfg(not(unix))]
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn vstr(text: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(text))
}

/// On Unix the e2e helper also points the guardian re-exec at its fixture, so
/// a background command reaches a real guardian from this test binary.
#[cfg(unix)]
fn call(name: &str, request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    super::process_tools_e2e::call(name, request)
}

#[cfg(not(unix))]
fn call(name: &str, request: harn_vm::value::DictMap) -> Result<VmValue, HostlibError> {
    use harn_hostlib::tools::ToolsCapability;
    use harn_hostlib::{BuiltinRegistry, HostlibCapability};
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    let entry = registry
        .find(name)
        .unwrap_or_else(|| panic!("{name} must be registered"));
    (entry.handler)(&[VmValue::dict(request)])
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
}

impl Layout {
    fn new() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("a home directory to root the conformance layout in");
        let root = tempfile::Builder::new()
            .prefix(".harn-sandbox-conformance-")
            .tempdir_in(home)
            .expect("conformance root");
        let base = root.path().canonicalize().expect("canonical root");
        let workspace = base.join("workspace");
        let outside = base.join("outside");
        let socket_root = base.join("sockets");
        for dir in [&workspace, &outside, &socket_root] {
            std::fs::create_dir_all(dir).expect("create conformance dir");
        }
        std::fs::write(outside.join("secret.txt"), OUTSIDE_CONTENT).expect("outside file");
        Self {
            _root: root,
            workspace,
            outside,
            socket_root,
        }
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

/// The child's argv, and the path or name it touches.
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
        ConformanceCase::OutsideReadRefused => {
            let target = layout.outside.join("secret.txt");
            let path = target.display().to_string();
            let argv = if cfg!(windows) {
                owned(&["cmd", "/c", "type", &path])
            } else {
                owned(&["cat", &path])
            };
            (argv, path)
        }
        ConformanceCase::UndeclaredEnvironmentNameWithheld
        | ConformanceCase::GuardianUndeclaredEnvironmentNameWithheld => {
            let argv = if cfg!(windows) {
                owned(&["cmd", "/c", "set"])
            } else {
                owned(&["env"])
            };
            (argv, UNDECLARED_NAME.to_string())
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

fn write_argv(target: &Path) -> Vec<String> {
    let path = target.display().to_string();
    if cfg!(windows) {
        vec!["cmd".into(), "/c".into(), format!("echo probe> \"{path}\"")]
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

/// What the child did, read from the effect rather than the exit code where
/// the effect is observable.
struct Run {
    observation: Observation,
    detail: String,
}

fn run(case: ConformanceCase, layout: &Layout, argv: &[String], target: &str) -> Run {
    let mut request = harn_vm::value::DictMap::new();
    request.insert(
        "argv".into(),
        VmValue::List(Arc::new(argv.iter().map(|arg| vstr(arg)).collect())),
    );
    request.insert("cwd".into(), vstr(&layout.workspace.display().to_string()));
    if case.route() == SpawnRoute::Guardian {
        // Background is the request shape that takes the process-owner
        // guardian, the same one auto-backgrounding converts to.
        request.insert("background".into(), VmValue::Bool(true));
    }
    let response = match call("hostlib_tools_run_command", request) {
        Ok(VmValue::Dict(dict)) => dict,
        Ok(other) => panic!("[{}] run_command answered {other:?}", case.id()),
        Err(error) => {
            return Run {
                observation: Observation::SpawnRefused,
                detail: error.to_string(),
            }
        }
    };
    let response = if case.route() == SpawnRoute::Guardian {
        let Some(VmValue::String(handle)) = response.get("handle_id") else {
            panic!(
                "[{}] a background start returned no handle: {response:?}",
                case.id()
            );
        };
        let mut wait = harn_vm::value::DictMap::new();
        wait.insert("handle_id".into(), VmValue::String(handle.clone()));
        wait.insert("timeout_ms".into(), VmValue::Int(20_000));
        match call("hostlib_tools_wait_command", wait) {
            Ok(VmValue::Dict(dict)) => dict,
            other => panic!("[{}] wait_command answered {other:?}", case.id()),
        }
    } else {
        response
    };
    let stdout = match response.get("stdout") {
        Some(VmValue::String(text)) => text.to_string(),
        _ => String::new(),
    };
    let exit = match response.get("exit_code") {
        Some(VmValue::Int(code)) => Some(*code),
        _ => None,
    };
    let stderr = match response.get("stderr") {
        Some(VmValue::String(text)) => text.lines().last().unwrap_or_default().to_string(),
        _ => String::new(),
    };
    let detail = format!("exit_code={exit:?} stderr_tail={stderr:?}");
    let observation = match case {
        ConformanceCase::OutsideReadRefused => took_effect(stdout.contains(OUTSIDE_CONTENT)),
        ConformanceCase::UndeclaredEnvironmentNameWithheld
        | ConformanceCase::GuardianUndeclaredEnvironmentNameWithheld => {
            let names = env_names(&stdout);
            // PATH is the liveness leg: an empty read would satisfy the
            // absence below for a reason unrelated to the policy.
            let live = names.iter().any(|name| name.eq_ignore_ascii_case("PATH"));
            assert!(
                live,
                "[{}] the child reported no PATH, so the environment was not measured: \
                 {response:?}",
                case.id()
            );
            took_effect(names.iter().any(|name| name == UNDECLARED_NAME))
        }
        _ => took_effect(Path::new(target).exists()),
    };
    Run {
        observation,
        detail,
    }
}

fn took_effect(effect: bool) -> Observation {
    if effect {
        Observation::Admitted
    } else {
        Observation::Refused
    }
}

fn env_names(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, _)| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Set the undeclared name in the launcher's environment, restoring whatever
/// was there. It stands in for the operator's unrelated credentials.
struct UndeclaredName(Option<std::ffi::OsString>);

impl UndeclaredName {
    fn set() -> Self {
        let previous = std::env::var_os(UNDECLARED_NAME);
        // SAFETY: `lock_env` serializes every environment-mutating test in
        // this binary, and the name is restored on drop.
        unsafe { std::env::set_var(UNDECLARED_NAME, "must-not-reach-the-child") };
        Self(previous)
    }
}

impl Drop for UndeclaredName {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        unsafe {
            match self.0.take() {
                Some(value) => std::env::set_var(UNDECLARED_NAME, value),
                None => std::env::remove_var(UNDECLARED_NAME),
            }
        }
    }
}

/// Pops the case policy and the session environment, including when a case
/// panics, so one case cannot decide the next.
struct CaseScope;

impl CaseScope {
    fn enter(policy: CapabilityPolicy) -> Self {
        push_execution_policy(policy);
        harn_vm::stdlib::process::set_session_environment(Some(SessionEnvironment::isolated()));
        Self
    }
}

impl Drop for CaseScope {
    fn drop(&mut self) {
        harn_vm::stdlib::process::set_session_environment(None);
        pop_execution_policy();
    }
}

fn enforcement_required() -> bool {
    std::env::var(REQUIRE_ENFORCEMENT_ENV)
        .map(|value| {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "off" | "no")
        })
        .unwrap_or(false)
}

#[test]
fn every_sandbox_conformance_case_holds_on_the_active_backend() {
    let _env = lock_env();
    let _undeclared = UndeclaredName::set();
    let backend = harn_vm::process_sandbox::active_backend_name();
    let enforcing = harn_vm::process_sandbox::active_backend_filesystem_available();

    let mut verdicts = Vec::new();
    for &case in ConformanceCase::ALL {
        let layout = Layout::new();
        let policy = case_policy(case, &layout);
        let expectation = case.expectation(&policy);
        let (argv, target) = probe(case, &layout);
        let (verdict, observed, detail) = match expectation {
            Expectation::NotApplicable(_) => (
                judge(case, expectation, enforcing, Observation::Refused, &target),
                None,
                String::new(),
            ),
            Expectation::Observe(_) => {
                let _scope = CaseScope::enter(policy);
                let run = run(case, &layout, &argv, &target);
                (
                    judge(case, expectation, enforcing, run.observation, &target),
                    Some(run.observation),
                    run.detail,
                )
            }
        };
        println!(
            "harn.sandbox_conformance backend={backend} enforcing={enforcing} case={} route={:?} \
             expected={expectation:?} observed={observed:?} verdict={} {detail}",
            case.id(),
            case.route(),
            serde_json::to_string(&verdict).unwrap_or_default(),
        );
        verdicts.push((case, verdict));
    }

    let failing: Vec<_> = verdicts
        .iter()
        .filter(|(_, verdict)| verdict.is_failure())
        .map(|(case, verdict)| format!("{} {verdict:?}", case.id()))
        .collect();
    let not_measured: Vec<_> = verdicts
        .iter()
        .filter(|(_, verdict)| matches!(verdict, Verdict::NotMeasured { .. }))
        .map(|(case, _)| case.id())
        .collect();
    let conforming = verdicts
        .iter()
        .filter(|(_, verdict)| *verdict == Verdict::Conforms)
        .count();
    println!(
        "harn.sandbox_conformance_summary backend={backend} cases={} conforms={conforming} \
         not_measured={} not_applicable={} failed={} failing={failing:?}",
        verdicts.len(),
        not_measured.len(),
        verdicts.len() - conforming - not_measured.len() - failing.len(),
        failing.len(),
    );

    assert_eq!(verdicts.len(), ConformanceCase::ALL.len());
    assert!(
        failing.is_empty(),
        "sandbox conformance failed on backend {backend}: {failing:#?}"
    );
    assert!(
        not_measured.is_empty() || !enforcement_required(),
        "{REQUIRE_ENFORCEMENT_ENV} declares this host must enforce, but backend {backend} \
         left these cases unmeasured: {not_measured:?}"
    );
    assert!(
        conforming > 0,
        "no case conformed on backend {backend}, so nothing was measured"
    );
}
