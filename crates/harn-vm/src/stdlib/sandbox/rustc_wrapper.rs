//! Whether a Cargo `rustc` wrapper runs inside the active sandbox, decided by
//! measuring it once and recorded where every caller can read it.
//!
//! A wrapper such as a compiler cache is configured outside the command: in
//! the environment, or in a `.cargo/config.toml` anywhere from the working
//! directory up to `CARGO_HOME`. Cargo resolves it, not the command, so the
//! decision is a property of the (policy, working directory) pair and is made
//! once per pair.
//!
//! # How the decision is measured
//!
//! Cargo's resolution rules are Cargo's to change, so this module does not
//! read Cargo configuration. It asks Cargo: a throwaway crate is built with
//! `cargo build -v --offline` from the command's working directory, confined
//! under the command's own policy and environment, so a wrapper resolves and
//! runs exactly as it would for the command.
//!
//! - The build succeeds and names no wrapper: `not_configured`.
//! - The build succeeds through a wrapper: `kept`, unless the wrapper left a
//!   confined long-lived process behind (below).
//! - The build fails, and succeeds with every wrapper switched off: the wrapper
//!   cannot run under this profile, so it is `disabled` with Cargo's words as
//!   the reason.
//! - Both fail, or the probe cannot run at all: `unmeasured`, and switched
//!   off. A wrapper stays on only when a build proved it runs; Cargo that
//!   cannot build an empty crate here gains nothing from it, and a wrapper
//!   left on unproven could still start a confined server.
//!
//! # The long-lived process rule
//!
//! A compiler cache usually talks to a per-user server and starts one when
//! none is running. A server started inside a sandbox keeps that sandbox for
//! its whole life and then serves later builds of other projects with this
//! run's confinement. So a wrapper whose build left a process running is
//! `disabled`, and that process is stopped: it can only do harm. A server
//! that was already running outside the sandbox was not started by the build,
//! so a wrapper that merely connects to it is kept.
//!
//! Only a process the probe build provably started is ever stopped. The
//! build's `cargo` leads a new session, and every process it starts stays in
//! that session unless it calls `setsid`; the kernel reports membership, so
//! nothing is picked by resemblance. A process that escaped the session is
//! seen only on Linux, through a nonce in its environment, and is never
//! stopped: the decision reads `unmeasured` and it keeps running.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use crate::orchestration::CapabilityPolicy;

/// The four Cargo settings that name a wrapper. An empty value is Cargo's
/// switch for "no wrapper", which overrides configuration files too.
pub const RUSTC_WRAPPER_ENV_KEYS: [&str; 4] = [
    "RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
];

const PROBE_CRATE: &str = "harn_rustc_wrapper_probe";

/// What the sandbox did with the wrapper Cargo resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RustcWrapperDisposition {
    /// Cargo resolved no wrapper, or Cargo is not reachable from the child.
    NotConfigured,
    /// The wrapper runs under this profile and is left in place.
    Kept,
    /// The wrapper cannot run under this profile, or would leave a confined
    /// long-lived process behind, so every wrapper setting is switched off.
    Disabled,
    /// The probe could not tell: the build failed with and without the
    /// wrapper, or could not run. Switched off, since only a proven wrapper
    /// is kept; the reason says what failed.
    Unmeasured,
}

/// The recorded decision for one (policy, working directory) pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RustcWrapperDecision {
    pub disposition: RustcWrapperDisposition,
    /// The wrapper command as Cargo ran or tried to run it, when it named one.
    pub wrapper: Option<String>,
    pub reason: String,
    /// The directory Cargo resolved configuration from.
    pub cwd: String,
}

impl RustcWrapperDecision {
    /// Whether spawns under this decision get every wrapper switched off:
    /// all but a proven wrapper.
    pub fn disables(&self) -> bool {
        self.disposition != RustcWrapperDisposition::Kept
    }

    /// Whether the decision took away a wrapper the caller configured. Only
    /// that is worth a warning; with no wrapper configured, switching the
    /// settings off changes nothing.
    pub fn drops_configured_wrapper(&self) -> bool {
        matches!(
            self.disposition,
            RustcWrapperDisposition::Disabled | RustcWrapperDisposition::Unmeasured
        )
    }
}

type DecisionKey = (String, String, Vec<(String, String)>);

fn decisions() -> &'static Mutex<BTreeMap<DecisionKey, RustcWrapperDecision>> {
    static DECISIONS: OnceLock<Mutex<BTreeMap<DecisionKey, RustcWrapperDecision>>> =
        OnceLock::new();
    DECISIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

thread_local! {
    /// Set while the probe's own `cargo` runs, so its spawn is not decided by
    /// the decision it is measuring.
    static PROBING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn probing() -> bool {
    PROBING.with(std::cell::Cell::get)
}

/// The caller-supplied wrapper settings, which Cargo reads before its
/// configuration files and which therefore belong to the decision's key.
fn caller_wrapper_env(env: &[(String, String)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = env
        .iter()
        .filter(|(key, _)| {
            RUSTC_WRAPPER_ENV_KEYS
                .iter()
                .any(|wrapper| key.eq_ignore_ascii_case(wrapper))
        })
        .map(|(key, value)| (key.to_ascii_uppercase(), value.clone()))
        .collect();
    pairs.sort();
    pairs
}

/// The recorded decision for `policy` and `cwd`, measuring it on first use.
///
/// `caller_env` is the spawn's own environment overlay; only its wrapper
/// settings are read.
pub fn rustc_wrapper_decision(
    policy: &CapabilityPolicy,
    cwd: &Path,
    caller_env: &[(String, String)],
) -> RustcWrapperDecision {
    let caller = caller_wrapper_env(caller_env);
    let key = (
        serde_json::to_string(policy).unwrap_or_default(),
        cwd.display().to_string(),
        caller.clone(),
    );
    if let Some(decision) = decisions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
    {
        return decision.clone();
    }
    let decision = measure(policy, cwd, &caller);
    record(&decision);
    decisions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .entry(key)
        .or_insert(decision)
        .clone()
}

/// Every decision made in this process, for a receipt.
pub fn rustc_wrapper_decisions() -> Vec<RustcWrapperDecision> {
    decisions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .cloned()
        .collect()
}

fn record(decision: &RustcWrapperDecision) {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "disposition".to_string(),
        serde_json::to_value(decision.disposition).unwrap_or_default(),
    );
    metadata.insert("wrapper".to_string(), serde_json::json!(decision.wrapper));
    metadata.insert("reason".to_string(), serde_json::json!(decision.reason));
    metadata.insert("cwd".to_string(), serde_json::json!(decision.cwd));
    let message = match decision.disposition {
        RustcWrapperDisposition::Disabled => {
            "a Cargo rustc wrapper was switched off in the sandbox"
        }
        RustcWrapperDisposition::Kept => "a Cargo rustc wrapper was kept in the sandbox",
        RustcWrapperDisposition::Unmeasured => {
            "a Cargo rustc wrapper could not be measured and was switched off in the sandbox"
        }
        RustcWrapperDisposition::NotConfigured => "no Cargo rustc wrapper applies",
    };
    // stderr shows info by default, and scripts assert on exact stderr, so the
    // ordinary no-wrapper case stays at debug.
    if decision.drops_configured_wrapper() {
        crate::events::log_warn_meta("process_sandbox_rustc_wrapper", message, metadata);
    } else if decision.disposition == RustcWrapperDisposition::NotConfigured {
        crate::events::log_debug_meta("process_sandbox_rustc_wrapper", message, metadata);
    } else {
        crate::events::log_info_meta("process_sandbox_rustc_wrapper", message, metadata);
    }
}

fn decision(
    disposition: RustcWrapperDisposition,
    wrapper: Option<String>,
    reason: impl Into<String>,
    cwd: &Path,
) -> RustcWrapperDecision {
    RustcWrapperDecision {
        disposition,
        wrapper,
        reason: reason.into(),
        cwd: cwd.display().to_string(),
    }
}

fn measure(
    policy: &CapabilityPolicy,
    cwd: &Path,
    caller: &[(String, String)],
) -> RustcWrapperDecision {
    if crate::testbench::process_tape::active_tape().is_some() {
        return decision(
            RustcWrapperDisposition::Unmeasured,
            None,
            "a process tape is active, and the probe would be a spawn the tape does not hold",
            cwd,
        );
    }
    let configured = configured_wrapper(cwd, caller);
    if configured.is_none() {
        return decision(
            RustcWrapperDisposition::NotConfigured,
            None,
            "no wrapper setting in the child's environment and no Cargo configuration that \
             could name one",
            cwd,
        );
    }
    let Some(scratch_parent) = crate::stdlib::sandbox::workspace_local_tmpdir(policy) else {
        return decision(
            RustcWrapperDisposition::Unmeasured,
            None,
            "no workspace-local scratch directory to build the probe crate in",
            cwd,
        );
    };
    let scratch = match Scratch::create(&scratch_parent) {
        Ok(scratch) => scratch,
        Err(error) => {
            return decision(
                RustcWrapperDisposition::Unmeasured,
                None,
                format!("could not create the probe crate: {error}"),
                cwd,
            )
        }
    };
    if let Err(error) = write_probe_crate(scratch.path()) {
        return decision(
            RustcWrapperDisposition::Unmeasured,
            None,
            format!("could not write the probe crate: {error}"),
            cwd,
        );
    }

    if let Some(known) = configured.as_deref().and_then(known_wrapper) {
        known.prepare();
    }
    let nonce = probe_nonce();
    let with_wrapper = match build(scratch.path(), cwd, caller.to_vec(), Some(&nonce)) {
        Ok(outcome) => outcome,
        Err(reason) => return decision(RustcWrapperDisposition::NotConfigured, None, reason, cwd),
    };
    let wrapper = with_wrapper.wrapper.clone();

    if let Some(session) = &with_wrapper.session {
        let survivors = session.members();
        if !survivors.is_empty() {
            let stopped = session.stop(&survivors);
            return decision(
                RustcWrapperDisposition::Disabled,
                wrapper,
                format!(
                    "the wrapper left {} long-lived process(es) running in the probe build's \
                     session, which would keep this sandbox and serve later builds with it; \
                     {stopped} were stopped",
                    survivors.len()
                ),
                cwd,
            );
        }
        let escaped = escaped_processes(&nonce, session);
        if escaped > 0 {
            return decision(
                RustcWrapperDisposition::Unmeasured,
                wrapper,
                format!(
                    "{escaped} process(es) the probe build started left its session, so they \
                     cannot be proven the probe's to stop and were left running"
                ),
                cwd,
            );
        }
    }
    if with_wrapper.success {
        return match wrapper {
            None => decision(
                RustcWrapperDisposition::NotConfigured,
                None,
                "Cargo resolved no wrapper",
                cwd,
            ),
            Some(wrapper) => decision(
                RustcWrapperDisposition::Kept,
                Some(wrapper),
                "a crate compiled through the wrapper under this profile",
                cwd,
            ),
        };
    }

    let blanked = RUSTC_WRAPPER_ENV_KEYS
        .iter()
        .map(|key| (key.to_string(), String::new()))
        .collect();
    match build(scratch.path(), cwd, blanked, None) {
        Ok(without) if without.success => decision(
            RustcWrapperDisposition::Disabled,
            wrapper.or_else(|| with_wrapper.failed_wrapper.clone()),
            format!(
                "the wrapper cannot run under this profile: {}",
                with_wrapper.error_line
            ),
            cwd,
        ),
        _ => decision(
            RustcWrapperDisposition::Unmeasured,
            wrapper.or(with_wrapper.failed_wrapper),
            format!(
                "the probe crate failed to build with and without the wrapper: {}",
                with_wrapper.error_line
            ),
            cwd,
        ),
    }
}

/// The probe crate's directory, removed on every return path.
struct Scratch(PathBuf);

impl Scratch {
    fn create(parent: &Path) -> std::io::Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = parent.join(format!("rustc-wrapper-probe-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The wrapper Cargo could resolve for a build from `cwd`, as a hint, or
/// `None` when it certainly resolves none.
///
/// Never decides the probe, only whether to run it: Cargo reads a wrapper from
/// its environment or from a configuration file in `cwd`, an ancestor of it,
/// or `CARGO_HOME`, and a file that neither mentions a wrapper nor includes
/// another file cannot name one. The hint is the first value found, used only
/// to pick a [`KnownWrapper`] row; the probe still asks Cargo. A file that
/// mentions a wrapper in a form this does not read yields an empty hint,
/// which still runs the probe.
fn configured_wrapper(cwd: &Path, caller: &[(String, String)]) -> Option<String> {
    let child_env: BTreeMap<String, String> =
        match crate::stdlib::process::session_closed_env_for_command(
            "cargo",
            caller.iter().cloned(),
        ) {
            Ok(Some(env)) => env.into_iter().collect(),
            _ => std::env::vars().chain(caller.iter().cloned()).collect(),
        };
    if let Some(value) = RUSTC_WRAPPER_ENV_KEYS
        .iter()
        .filter_map(|key| child_env.get(*key))
        .find(|value| !value.is_empty())
    {
        return Some(value.clone());
    }
    let cargo_home = child_env.get("CARGO_HOME").map(PathBuf::from).or_else(|| {
        child_env
            .get("HOME")
            .map(|home| Path::new(home).join(".cargo"))
    });
    let mut dirs: Vec<PathBuf> = cwd.ancestors().map(|dir| dir.join(".cargo")).collect();
    dirs.extend(cargo_home);
    dirs.iter()
        .flat_map(|dir| [dir.join("config"), dir.join("config.toml")])
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .filter(|text| text.contains("wrapper") || text.contains("include"))
        .map(|text| {
            text.lines()
                .filter(|line| line.trim_start().starts_with("rustc-wrapper"))
                .find_map(|line| line.split_once('=').map(|(_, value)| value))
                .map(|value| value.trim().trim_matches('"').to_string())
                .unwrap_or_default()
        })
        .next()
}

/// A wrapper whose long-lived helper the host starts itself, outside the
/// sandbox, before the probe, so the probe and every build after it connect
/// to an unconfined helper instead of starting a confined one.
pub struct KnownWrapper {
    /// Matched against the wrapper's file name, which also catches a script
    /// named for the tool it runs.
    pub name_contains: &'static str,
    /// The command that starts the helper, a no-op when it is running.
    pub prepare: &'static [&'static str],
}

/// Every wrapper the host prepares. Anything else falls back to the
/// long-lived process rule.
pub const KNOWN_WRAPPERS: &[KnownWrapper] = &[KnownWrapper {
    name_contains: "sccache",
    prepare: &["sccache", "--start-server"],
}];

fn known_wrapper(configured: &str) -> Option<&'static KnownWrapper> {
    let name = Path::new(configured).file_name()?.to_string_lossy();
    KNOWN_WRAPPERS
        .iter()
        .find(|known| name.contains(known.name_contains))
}

impl KnownWrapper {
    fn prepare(&self) {
        let Some((program, args)) = self.prepare.split_first() else {
            return;
        };
        let _ = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn write_probe_crate(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root.join("src"))?;
    // `[workspace]` keeps an enclosing workspace from claiming the crate.
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{PROBE_CRATE}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
             publish = false\n\n[workspace]\n"
        ),
    )?;
    std::fs::write(root.join("src").join("main.rs"), "fn main() {}\n")
}

struct BuildOutcome {
    /// The session the build's `cargo` led, where the OS keeps one.
    session: Option<ProbeSession>,
    success: bool,
    /// The wrapper Cargo ran, read from its verbose `Running` line.
    wrapper: Option<String>,
    /// The wrapper Cargo tried and failed to run, read from its error.
    failed_wrapper: Option<String>,
    error_line: String,
}

fn build(
    scratch: &Path,
    cwd: &Path,
    mut env: Vec<(String, String)>,
    nonce: Option<&str>,
) -> Result<BuildOutcome, String> {
    let manifest = scratch.join("Cargo.toml");
    let target = scratch.join("target");
    let args = vec![
        "build".to_string(),
        "-v".to_string(),
        "--offline".to_string(),
        "--manifest-path".to_string(),
        manifest.display().to_string(),
        "--target-dir".to_string(),
        target.display().to_string(),
    ];
    // The run's own spawn path, minus the step that turns a non-zero exit
    // into a sandbox-violation error: the probe needs Cargo's words, and a
    // wrapper the profile refuses is exactly the failure being measured.
    let sandbox = crate::stdlib::sandbox::active_sandbox_policy()
        .ok_or_else(|| "no sandbox policy is active".to_string())?;
    let (policy, profile) = sandbox;
    if let Some(nonce) = nonce {
        env.push((PROBE_NONCE_ENV.to_string(), nonce.to_string()));
    }
    let overlay =
        crate::stdlib::process::session_closed_env_for_command("cargo", env.clone().into_iter())
            .map_err(|error| format!("the session environment refused the probe: {error:?}"))?;
    let config = crate::stdlib::sandbox::ProcessCommandConfig {
        cwd: Some(PathBuf::from(cwd)),
        closed_env: overlay.is_some(),
        env: overlay.unwrap_or(env),
        ..crate::stdlib::sandbox::ProcessCommandConfig::default()
    };
    PROBING.with(|flag| flag.set(true));
    let output =
        crate::stdlib::sandbox::sandboxed_process_config(&config, &policy).and_then(|config| {
            use crate::stdlib::sandbox::SandboxBackend;
            #[cfg(unix)]
            {
                crate::stdlib::sandbox::ActiveBackend::run_to_output_in_session(
                    "cargo", &args, &config, &policy, profile,
                )
                .map(|(output, session)| (output, Some(ProbeSession::started(session))))
            }
            #[cfg(not(unix))]
            {
                crate::stdlib::sandbox::ActiveBackend::run_to_output(
                    "cargo", &args, &config, &policy, profile,
                )
                .map(|output| (output, None))
            }
        });
    PROBING.with(|flag| flag.set(false));
    let (output, session) =
        output.map_err(|error| format!("cargo is not reachable from the child: {error:?}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(BuildOutcome {
        session,
        success: output.status.success(),
        wrapper: running_wrapper(&stderr),
        failed_wrapper: failed_wrapper(&stderr),
        error_line: stderr
            .lines()
            .find(|line| line.trim_start().starts_with("error"))
            .unwrap_or_else(|| stderr.lines().last().unwrap_or_default())
            .trim()
            .to_string(),
    })
}

/// The wrapper in Cargo's verbose line for the probe crate:
/// ``Running `<wrapper>... <rustc> --crate-name harn_rustc_wrapper_probe ...` ``.
/// Everything before the compiler is the wrapper chain; a line whose first
/// word is the compiler names none.
fn running_wrapper(stderr: &str) -> Option<String> {
    let line = stderr
        .lines()
        .find(|line| line.contains("Running `") && line.contains(PROBE_CRATE))?;
    let command = line.split_once("Running `")?.1;
    let before_crate = command.split(" --crate-name").next()?;
    let words: Vec<&str> = before_crate.split_whitespace().collect();
    let compiler = words.iter().rposition(|word| {
        Path::new(word.trim_matches('`'))
            .file_stem()
            .is_some_and(|stem| stem == "rustc")
    })?;
    (compiler > 0).then(|| words[..compiler].join(" "))
}

/// The wrapper in Cargo's failure for the compiler it could not run, either
/// "could not execute process `<wrapper> <rustc> -vV`" (the wrapper is
/// missing) or "process didn't exit successfully: `<wrapper> <rustc> -vV`"
/// (the wrapper ran and failed).
fn failed_wrapper(stderr: &str) -> Option<String> {
    let command = stderr.lines().find_map(|line| {
        [
            "could not execute process `",
            "process didn't exit successfully: `",
        ]
        .iter()
        .find_map(|marker| line.split_once(marker))
        .and_then(|(_, rest)| rest.split('`').next())
    })?;
    let words: Vec<&str> = command.split_whitespace().collect();
    let compiler = words.iter().rposition(|word| {
        Path::new(word)
            .file_stem()
            .is_some_and(|stem| stem == "rustc")
    })?;
    (compiler > 0).then(|| words[..compiler].join(" "))
}

/// The environment variable that carries a probe build's nonce.
const PROBE_NONCE_ENV: &str = "RUSTC_WRAPPER_PROBE_NONCE";

/// Unique to one probe build in this process.
fn probe_nonce() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{sequence}", std::process::id())
}

/// Every session this process started for a probe build. Stopping refuses
/// any other, so no path here can signal a process a probe did not start.
fn started_sessions() -> &'static Mutex<BTreeSet<u32>> {
    static STARTED: OnceLock<Mutex<BTreeSet<u32>>> = OnceLock::new();
    STARTED.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// The session a probe build's `cargo` led. Every process the build starts
/// stays in it unless it calls `setsid` itself, and the kernel, not a
/// resemblance, says which processes those are.
struct ProbeSession(u32);

impl ProbeSession {
    /// Only a Unix spawn leads a session, so only Unix starts one.
    #[cfg(unix)]
    fn started(id: u32) -> Self {
        started_sessions()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id);
        Self(id)
    }

    fn registered(&self) -> bool {
        started_sessions()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&self.0)
    }

    /// Processes still in the session once the build has returned.
    fn members(&self) -> Vec<u32> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        system
            .processes()
            .keys()
            .map(|pid| pid.as_u32())
            .filter(|pid| self.holds(*pid))
            .collect()
    }

    /// Whether `pid` is in this session now, excluding the id's own holder:
    /// the leader, the probe's `cargo`, has exited.
    ///
    /// What keeps the session from being impersonated is the kernel, not that
    /// exclusion: Linux and XNU never reallocate a pid while it is still in
    /// use as a session or process-group id, so while any member survives, no
    /// new process can take this id. Once the last member exits the id can be
    /// reused. The residual race is a new session created under a reused id
    /// between [`Self::members`] reading the table and the re-check before a
    /// signal, which needs the id to be freed and reallocated in that window.
    #[cfg(unix)]
    fn holds(&self, pid: u32) -> bool {
        pid != self.0
            && pid != std::process::id()
            && unsafe { libc::getsid(pid as libc::pid_t) } == self.0 as libc::pid_t
    }

    #[cfg(not(unix))]
    fn holds(&self, _pid: u32) -> bool {
        false
    }

    /// Stops each of `pids` that is still in the session when its signal is
    /// sent; returns how many were signalled. A session no probe started is
    /// refused outright.
    fn stop(&self, pids: &[u32]) -> usize {
        if !self.registered() {
            return 0;
        }
        pids.iter()
            .filter(|pid| self.holds(**pid) && signal(**pid))
            .count()
    }
}

#[cfg(unix)]
fn signal(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) == 0 }
}

#[cfg(not(unix))]
fn signal(_pid: u32) -> bool {
    false
}

/// Processes carrying the probe's nonce outside its session: started by the
/// build, then escaped it with `setsid`. Linux exposes another process's
/// environment; macOS does not, so there an escape is not seen. These are
/// only counted, never signalled.
#[cfg(target_os = "linux")]
fn escaped_processes(nonce: &str, session: &ProbeSession) -> usize {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let entry = format!("{PROBE_NONCE_ENV}={nonce}");
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_environ(UpdateKind::Always),
    );
    system
        .processes()
        .iter()
        .filter(|(pid, process)| {
            pid.as_u32() != std::process::id()
                && !session.holds(pid.as_u32())
                && process
                    .environ()
                    .iter()
                    .any(|variable| variable.to_str() == Some(entry.as_str()))
        })
        .count()
}

#[cfg(not(target_os = "linux"))]
fn escaped_processes(_nonce: &str, _session: &ProbeSession) -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Starts a session whose leader exits at once and leaves one `sleep`
    /// behind in it, the shape a daemonizing wrapper leaves: returns the
    /// session id.
    #[cfg(unix)]
    fn session_with_one_member() -> u32 {
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("60");
        // SAFETY: `setsid`, `fork` and `_exit` are async-signal-safe, and the
        // closure touches no Rust-owned memory. The leader exits without
        // exec; its forked child continues to exec `sleep` inside the session.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                match libc::fork() {
                    -1 => Err(std::io::Error::last_os_error()),
                    0 => Ok(()),
                    _ => libc::_exit(0),
                }
            });
        }
        let mut leader = command.spawn().expect("start session leader");
        let session = leader.id();
        leader.wait().expect("leader exits");
        session
    }

    /// Polls until the session is empty, for at most a few seconds.
    #[cfg(unix)]
    fn emptied(session: &ProbeSession) -> bool {
        (0..5_000).any(|_| {
            std::thread::yield_now();
            session.members().is_empty()
        })
    }

    /// A process the session's leader left behind is a member and is
    /// stopped; a process in another session, started by the same test at
    /// the same moment, is neither.
    #[cfg(unix)]
    #[test]
    fn only_a_member_of_the_probes_session_is_stopped() {
        let session = ProbeSession::started(session_with_one_member());
        let mut bystander = std::process::Command::new("/bin/sleep");
        bystander.arg("60");
        crate::op_interrupt::configure_kill_group(&mut bystander);
        let mut bystander = bystander.spawn().expect("start bystander");

        let members = session.members();
        assert_eq!(members.len(), 1, "{members:?}");
        assert!(!members.contains(&bystander.id()));
        assert_eq!(session.stop(&members), 1);
        assert!(emptied(&session), "the member outlived its signal");
        assert!(
            bystander.try_wait().expect("poll bystander").is_none(),
            "a process outside the session was stopped"
        );
        bystander.kill().expect("stop bystander");
        let _ = bystander.wait();
    }

    /// Stopping is refused for a session no probe started, even when the
    /// pids really are its members.
    #[cfg(unix)]
    #[test]
    fn a_session_no_probe_started_is_never_signalled() {
        let leader = session_with_one_member();
        let unregistered = ProbeSession(leader);
        let members = unregistered.members();
        assert_eq!(members.len(), 1, "{members:?}");
        assert_eq!(unregistered.stop(&members), 0);
        assert_eq!(unregistered.members(), members, "a member was signalled");
        let session = ProbeSession::started(leader);
        assert_eq!(session.stop(&members), 1);
        assert!(emptied(&session), "the member outlived its signal");
    }

    #[test]
    fn the_running_line_names_the_wrapper_chain_before_the_compiler() {
        let stderr = "   Compiling harn_rustc_wrapper_probe v0.0.0\n     Running `/home/u/bin/log-wrapper /home/u/.rustup/toolchains/x/bin/rustc --crate-name harn_rustc_wrapper_probe --edition=2021 src/main.rs`\n";
        assert_eq!(
            running_wrapper(stderr).as_deref(),
            Some("/home/u/bin/log-wrapper")
        );
        let bare = "     Running `/home/u/.rustup/toolchains/x/bin/rustc --crate-name harn_rustc_wrapper_probe src/main.rs`\n";
        assert_eq!(running_wrapper(bare), None);
    }

    #[test]
    fn a_failed_execution_names_the_wrapper_cargo_tried() {
        let stderr =
            "error: could not execute process `/missing/wrapper /x/rustc -vV` (never executed)\n";
        assert_eq!(failed_wrapper(stderr).as_deref(), Some("/missing/wrapper"));
    }

    #[test]
    fn only_the_callers_wrapper_settings_key_the_decision() {
        let env = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("rustc_wrapper".to_string(), "sccache".to_string()),
        ];
        assert_eq!(
            caller_wrapper_env(&env),
            vec![("RUSTC_WRAPPER".to_string(), "sccache".to_string())]
        );
    }
}
