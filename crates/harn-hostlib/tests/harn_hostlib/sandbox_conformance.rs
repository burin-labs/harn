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

use harn_hostlib::sandbox::conformance::run_conformance;
use harn_vm::process_sandbox::conformance::ConformanceCase;

/// Present in the launcher's environment and never declared by the session,
/// so the environment cases always have at least one name to withhold.
const UNDECLARED_NAME: &str = "HARN_CONFORMANCE_UNDECLARED_NAME";
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
/// The run must fail exactly these: a new failure is a regression, and a case
/// that starts holding means the fix landed and its entry must go. Windows
/// runs the command tool's children outside the AppContainer today (#8738),
/// so every filesystem refusal escapes and the backend's own socket refusal
/// never happens.
const KNOWN_GAPS: &[&str] = if cfg!(windows) {
    &[
        "fs.outside_write_refused",
        "fs.outside_read_refused",
        "fs.sibling_temp_read_refused",
        "unix_socket.bind_under_root",
        "unix_socket.bind_under_root_with_network",
        "unix_socket.bind_outside_root_refused",
    ]
} else {
    &[]
};

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
    // The guardian re-executes the current binary, and a libtest binary does
    // not own `main`, so the re-exec enters the e2e module's fixture instead.
    #[cfg(unix)]
    let _guardian = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);

    let report = run_conformance();
    for case in &report.cases {
        println!("{}", case.receipt_line());
    }
    println!("{}", report.summary_line());

    assert_eq!(report.cases.len(), ConformanceCase::ALL.len());
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
