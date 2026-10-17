//! Every sandbox conformance case holds on the backend this test runs on.
//!
//! The cases and their judgement live in
//! `harn_vm::process_sandbox::conformance`, and the runner that drives a real
//! child through the process tools lives in
//! `harn_hostlib::sandbox::conformance`, shared with `harn doctor sandbox`.
//! This test runs unchanged on macOS, Linux and Windows, so a backend
//! difference is a named case failure here rather than a red on one platform
//! discovered later.
//!
//! # Reading the output
//!
//! Every case prints one `harn.sandbox_conformance` line, measured or not,
//! and a summary line counts each verdict and names every failing case. A host
//! whose backend reports no enforcement produces `not_measured` for the kernel
//! cases, never a pass, and a runner class that declares it must enforce
//! (`HARN_REQUIRE_LANDLOCK_TESTS`) turns any `not_measured` into a failure.

use std::io::Write;

use harn_hostlib::sandbox::conformance::run_conformance_with_progress;
use harn_vm::process_sandbox::conformance::ConformanceCase;

/// Present in the launcher's environment and never declared by the session,
/// so the environment cases always have at least one name to withhold.
const UNDECLARED_NAME: &str = "HARN_CONFORMANCE_UNDECLARED_NAME";
use crate::sandbox_host::REQUIRE_LANDLOCK_ENV as REQUIRE_ENFORCEMENT_ENV;

#[cfg(unix)]
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    super::process_tools_e2e::lock_env()
}

#[cfg(not(unix))]
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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

/// Cases known to fail on this platform, and the issue that owns the fix.
///
/// A new failure is a regression. Missing probes are reported separately.
const KNOWN_GAPS: &[&str] = &[];

fn enforcement_required() -> bool {
    crate::sandbox_host::landlock_enforcement_required()
}

fn assert_required_tool_refuses() {
    use harn_hostlib::{BuiltinRegistry, HostlibCapability, HostlibError};
    use harn_vm::orchestration::{CapabilityPolicy, SandboxProfile};
    use harn_vm::VmValue;

    let workspace = tempfile::tempdir().expect("required confinement workspace");
    let _environment = harn_vm::stdlib::process::declare_session_environment_if_absent(
        harn_vm::security::SessionEnvironment::isolated(),
    );
    harn_vm::orchestration::push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::OsHardened,
        workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
        ..CapabilityPolicy::default()
    });
    let mut registry = BuiltinRegistry::new();
    harn_hostlib::tools::ToolsCapability.register_builtins(&mut registry);
    let command = registry
        .find("hostlib_tools_run_command")
        .expect("real process tool");
    let mut request = harn_vm::value::DictMap::new();
    request.insert(
        "cwd".into(),
        VmValue::string(workspace.path().to_string_lossy()),
    );
    request.insert(
        "argv".into(),
        VmValue::List(std::sync::Arc::new(vec![
            VmValue::string(
                std::env::current_exe()
                    .expect("test executable")
                    .to_string_lossy(),
            ),
            VmValue::string("--list"),
        ])),
    );
    let expected = harn_vm::llm::vm_value_to_json(&harn_vm::process_sandbox::host_confinement())
        ["os_hardened_refusal"]
        .clone();
    let result = (command.handler)(&[VmValue::dict(request)]);
    harn_vm::orchestration::pop_execution_policy();
    match result {
        Err(HostlibError::SandboxMechanismUnavailable { refusal, .. }) => {
            assert_eq!(
                harn_vm::llm::vm_value_to_json(&refusal.thrown_value()),
                expected
            );
        }
        other => panic!("required confinement must refuse the real process tool: {other:?}"),
    }
}

#[test]
fn every_sandbox_conformance_case_holds_on_the_active_backend() {
    let _env = lock_env();
    let _undeclared = UndeclaredName::set();
    // The guardian re-executes the current binary, and a libtest binary does
    // not own `main`, so the re-exec enters the e2e module's fixture instead.
    #[cfg(unix)]
    let _guardian = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);

    let report = run_conformance_with_progress(|progress| {
        // Write directly so libtest's in-memory print capture cannot swallow
        // progress when a test process is killed before it returns.
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{}", progress.receipt_line()).expect("write case progress");
        stdout.flush().expect("flush case progress");
    });
    println!("{}", report.summary_line());

    assert_eq!(report.cases.len(), ConformanceCase::ALL.len());
    #[cfg(windows)]
    {
        for case in [
            ConformanceCase::UnixSocketBindUnderRoot,
            ConformanceCase::UnixSocketBindUnderRootWithNetwork,
            ConformanceCase::UnixSocketBindOutsideRootRefused,
        ] {
            let measured = report
                .cases
                .iter()
                .find(|row| row.case == case.id())
                .expect("the socket case stays in the report");
            assert!(measured.expected.is_some());
            assert_eq!(measured.observed, None);
            assert_eq!(measured.unavailable_mechanism, None);
            assert!(!measured.target.is_empty());
            assert!(matches!(&measured.verdict,
                harn_vm::process_sandbox::conformance::Verdict::NotMeasured { reason }
                if reason == "the conformance driver has no Windows Unix socket bind probe"));
        }
        assert!(
            report.conforming() > 0,
            "a real positive control must be measured"
        );
        assert!(
            !report.holds(),
            "missing socket probes cannot qualify enforcement"
        );
    }
    if super::sandbox_host::unavailable_worktree_sandbox().is_some() {
        assert!(!report.enforcing);
        assert!(
            !enforcement_required(),
            "this runner requires actual confinement"
        );
        assert!(report.failing().is_empty(), "{}", report.summary_line());
        let refused = report
            .cases
            .iter()
            .filter(|case| case.unavailable_mechanism.is_some())
            .collect::<Vec<_>>();
        // The default Worktree fallback warns and measures unconfined effects.
        // Exercise a required profile separately; its refusal must be real.
        assert_required_tool_refuses();
        for case in refused {
            assert_eq!(
                case.observed,
                Some(harn_vm::process_sandbox::conformance::Observation::SpawnRefused)
            );
            assert!(matches!(
                case.verdict,
                harn_vm::process_sandbox::conformance::Verdict::NotMeasured { .. }
            ));
        }
        assert!(!report.not_measured().is_empty());
        assert!(!report.holds(), "unmeasured confinement must never qualify");
        return;
    }
    let unexpected: Vec<String> = report
        .failing()
        .iter()
        .filter(|case| !KNOWN_GAPS.contains(&case.case))
        .map(|case| format!("{} {:?} {}", case.case, case.verdict, case.detail))
        .collect();
    assert!(
        unexpected.is_empty(),
        "sandbox conformance failed on backend {}: {unexpected:#?}",
        report.backend
    );
    let failing: Vec<&str> = report.failing().iter().map(|case| case.case).collect();
    let fixed: Vec<&str> = KNOWN_GAPS
        .iter()
        .copied()
        .filter(|known| !failing.contains(known))
        .collect();
    assert!(
        fixed.is_empty(),
        "these known gaps now hold on backend {}; remove them from KNOWN_GAPS: {fixed:?}",
        report.backend
    );
    let not_measured: Vec<&str> = report.not_measured().iter().map(|case| case.case).collect();
    assert!(
        not_measured.is_empty() || !enforcement_required(),
        "{REQUIRE_ENFORCEMENT_ENV} declares this host must enforce, but backend {} left these \
         cases unmeasured: {not_measured:?}",
        report.backend
    );
    assert!(
        report.conforming() > 0,
        "no case conformed on backend {}, so nothing was measured",
        report.backend
    );
}
