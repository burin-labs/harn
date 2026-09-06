//! The child-process refusal record, and the credential-denial term it
//! reports on.
//!
//! Split out of `mod.rs`, which is on the legacy source-length inventory and
//! may not grow. These belong together anyway: the denial term decides what is
//! refused, and the record is how a refusal becomes something a consumer can
//! act on instead of a bare non-zero exit.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::{ErrorCategory, VmDictExt, VmError, VmValue};

use super::{
    effective_fallback, normalize_for_policy, path_is_within, sandbox_denial_error,
    sandbox_signal_status, sandbox_user_home_dir, warn_once, ActiveBackend, PrepareOutcome,
    SandboxBackend, SandboxFallback,
};

/// How a child-process refusal was determined, and therefore how much its
/// `refused_paths` can be trusted.
///
/// One variant, because there is one producer. A consumer should still match on
/// this rather than assume: the field exists so that when a platform learns to
/// name refused paths (macOS can, via the unified log, asynchronously) the new
/// tier arrives as a variant a consumer already handles, instead of silently
/// changing what an existing value means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RefusalObservability {
    /// Classified by matching the child's own output, which is all any current
    /// backend can do. `refused_paths` is therefore always EMPTY under this
    /// tier, and that emptiness says nothing about what was refused.
    ///
    /// Lossy in both directions: a tool that localizes its errors is a refusal
    /// that never counts, and any failing child that merely prints one of the
    /// phrases is attributed to the sandbox.
    Inferred,
}

impl RefusalObservability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inferred => "inferred",
        }
    }
}

/// How completely the active process-sandbox backend reports mid-run denials.
///
/// This is separate from an individual refusal's `observability`: it describes
/// what an EMPTY refusal slot means. In particular, `InferredOnly` means that
/// no structured backend report was available, so absence must not be read as
/// proof that the sandbox allowed every operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProcessSandboxDenialReporting {
    NotEnforced,
    BackendUnavailable,
    InferredOnly,
}

impl ProcessSandboxDenialReporting {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotEnforced => "not_enforced",
            Self::BackendUnavailable => "backend_unavailable",
            Self::InferredOnly => "inferred_only",
        }
    }
}

/// Operation class reported by a backend. Current backends cannot report this
/// fact, so inferred records say `Unknown` instead of guessing from prose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProcessSandboxOperation {
    Read,
    Write,
    Unknown,
}

impl ProcessSandboxOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Unknown => "unknown",
        }
    }
}

/// Whether an OS exit signal is one of the signals a process sandbox may use
/// for a mid-run refusal. Both `std::process` and hostlib's process adapter
/// project their platform-specific exit status through this one classifier.
#[cfg(unix)]
pub fn is_process_sandbox_signal(signal: Option<i32>) -> bool {
    matches!(
        signal,
        Some(libc::SIGSYS) | Some(libc::SIGABRT) | Some(libc::SIGKILL)
    )
}

#[cfg(not(unix))]
pub fn is_process_sandbox_signal(_signal: Option<i32>) -> bool {
    false
}

/// A typed child-process sandbox refusal.
///
/// Replaces reading the identity back out of an error message string. The
/// detector is still the substring heuristic below on every platform that
/// cannot do better; what changes is that its uncertainty is now a field
/// (`observability`) instead of something a consumer has to know by folklore.
///
/// Projected into both the event stream and the command handler result, which
/// is why `command` and `cwd` are mandatory and `refused_paths` is not: those
/// two are known at spawn on every platform, and the path is not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSandboxRefusal {
    pub schema: String,
    pub command: Vec<String>,
    pub cwd: String,
    pub backend: String,
    pub operation: ProcessSandboxOperation,
    /// The refused resource when the backend reports one. `None` under the
    /// current inference-only contract; never recover this by parsing prose.
    pub resource: Option<String>,
    /// May be empty even on a real refusal. Read `observability` first.
    pub refused_paths: Vec<String>,
    pub observability: RefusalObservability,
    /// Bounded excerpt of the child's own output that triggered an `Inferred`
    /// classification. Diagnostic only; never parse it.
    pub stderr_excerpt: String,
    pub count: u32,
}

impl ProcessSandboxRefusal {
    pub const SCHEMA: &'static str = "harn.process.sandbox_refusal.v1";
    /// Keep the excerpt small enough to sit in a receipt without becoming a log.
    const MAX_EXCERPT: usize = 512;

    /// Emit the refusal as a structured event.
    ///
    /// This event-side projection is emitted at the moment of classification.
    /// The command response carries the same typed record independently, so
    /// neither consumer reconstructs it later from an error string.
    pub fn emit(&self) {
        let mut metadata = std::collections::BTreeMap::new();
        metadata.insert("schema".to_string(), serde_json::json!(self.schema));
        metadata.insert("command".to_string(), serde_json::json!(self.command));
        metadata.insert("cwd".to_string(), serde_json::json!(self.cwd));
        metadata.insert("backend".to_string(), serde_json::json!(self.backend));
        metadata.insert(
            "operation".to_string(),
            serde_json::json!(self.operation.as_str()),
        );
        metadata.insert("resource".to_string(), serde_json::json!(self.resource));
        metadata.insert(
            "refused_paths".to_string(),
            serde_json::json!(self.refused_paths),
        );
        metadata.insert(
            "observability".to_string(),
            serde_json::to_value(self.observability).unwrap_or(serde_json::Value::Null),
        );
        metadata.insert(
            "stderr_excerpt".to_string(),
            serde_json::json!(self.stderr_excerpt),
        );
        metadata.insert("count".to_string(), serde_json::json!(self.count));
        crate::events::log_warn_meta(
            "process_sandbox_refusal",
            "a child process was refused by the OS sandbox",
            metadata,
        );
    }

    pub fn inferred(backend: String, command: Vec<String>, cwd: String, evidence: &str) -> Self {
        let mut stderr_excerpt: String = evidence.chars().take(Self::MAX_EXCERPT).collect();
        if evidence.chars().count() > Self::MAX_EXCERPT {
            stderr_excerpt.push('…');
        }
        Self {
            schema: Self::SCHEMA.to_string(),
            command,
            cwd,
            backend,
            operation: ProcessSandboxOperation::Unknown,
            resource: None,
            refused_paths: Vec::new(),
            observability: RefusalObservability::Inferred,
            stderr_excerpt,
            count: 1,
        }
    }

    /// Project this record into the existing agent-handler denial contract.
    /// Consumers must never reconstruct these fields from `stderr_excerpt`.
    pub fn handler_denial_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": self.schema,
            "gate": "process_sandbox",
            "capability": "process.run",
            "backend": self.backend,
            "operation": self.operation.as_str(),
            "resource": self.resource,
            "command": self.command,
            "cwd": self.cwd,
            "refused_paths": self.refused_paths,
            "observability": self.observability.as_str(),
            "stderr_excerpt": self.stderr_excerpt,
            "count": self.count,
            "retryable": false,
            "reason": "The process sandbox refused an operation in the child process.",
        })
    }

    pub fn handler_denial_value(&self) -> VmValue {
        crate::json_to_vm_value(&self.handler_denial_json())
    }
}

/// Spawn-time contract for assessing a completed child. Capture this before a
/// background waiter changes threads: execution policy is scoped to the
/// spawning thread, but reporting coverage belongs to the command receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSandboxReportingContext {
    pub backend: String,
    pub reporting: ProcessSandboxDenialReporting,
}

impl ProcessSandboxReportingContext {
    pub fn current() -> Self {
        let reporting = match super::active_sandbox_policy() {
            Some(_) if ActiveBackend::available() => ProcessSandboxDenialReporting::InferredOnly,
            Some(_) => ProcessSandboxDenialReporting::BackendUnavailable,
            _ => ProcessSandboxDenialReporting::NotEnforced,
        };
        Self {
            backend: super::active_backend_filesystem_mechanism().to_string(),
            reporting,
        }
    }

    pub fn assess_exit(
        &self,
        success: bool,
        sandbox_signal: bool,
        stdout: &[u8],
        stderr: &[u8],
        command: &[String],
        cwd: &str,
    ) -> ProcessSandboxAssessment {
        let stderr_lower = String::from_utf8_lossy(stderr).to_ascii_lowercase();
        let stdout_lower = String::from_utf8_lossy(stdout).to_ascii_lowercase();
        let stderr_permission = stderr_lower.contains("operation not permitted")
            || stderr_lower.contains("permission denied")
            || stderr_lower.contains("access is denied");
        let stdout_permission = stdout_lower.contains("operation not permitted");
        let permission_errno = !success && (stderr_permission || stdout_permission);
        let refusal = (self.reporting == ProcessSandboxDenialReporting::InferredOnly
            && (permission_errno || sandbox_signal))
            .then(|| {
                let evidence = if stderr_permission || (!stdout_permission && !stderr.is_empty()) {
                    stderr
                } else {
                    stdout
                };
                ProcessSandboxRefusal::inferred(
                    self.backend.clone(),
                    command.to_vec(),
                    cwd.to_string(),
                    &String::from_utf8_lossy(evidence),
                )
            });
        ProcessSandboxAssessment {
            reporting: self.reporting,
            refusal,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSandboxAssessment {
    pub reporting: ProcessSandboxDenialReporting,
    pub refusal: Option<ProcessSandboxRefusal>,
}

pub fn process_violation_error(
    output: &std::process::Output,
    command: &[String],
    cwd: &str,
) -> Option<VmError> {
    let policy = crate::orchestration::current_execution_policy()?;
    // Only a profile that actually confined the process may attribute the
    // child's failure to the OS sandbox. Under a profile that spawned it
    // unconfined, a permission error came from the child's own work.
    if !policy.sandbox_profile.confines_processes() {
        return None;
    }
    let sandbox_signal = sandbox_signal_status(output);
    let assessment = ProcessSandboxReportingContext::current().assess_exit(
        output.status.success(),
        sandbox_signal,
        &output.stdout,
        &output.stderr,
        command,
        cwd,
    );
    if let Some(refusal) = assessment.refusal {
        refusal.emit();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let action = if sandbox_signal {
            "terminated"
        } else {
            "denied"
        };
        return Some(sandbox_denial_error(
            format!(
                "sandbox violation: process was {action} by the OS sandbox (status {})",
                output.status,
            ),
            &format!("{stderr}\n{stdout}"),
            &policy,
        ));
    }
    None
}

/// Every subtree a confined child must not read: the non-removable credential
/// defaults, resolved against this user's home, plus whatever the policy added.
///
/// Unlike the additive root lists this is NOT existence-filtered. A denial for a
/// path that does not exist yet must survive that path being created between
/// profile generation and the child's read, so the rule is emitted regardless.
pub(crate) fn process_sandbox_read_deny_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let mut denied: Vec<PathBuf> = Vec::new();
    if let Some(home) = sandbox_user_home_dir() {
        for relative in crate::orchestration::default_read_deny_home_paths() {
            denied.push(normalize_for_policy(&home.join(relative)));
        }
    }
    for root in &policy.process_sandbox.read_deny_roots {
        let normalized = normalize_for_policy(Path::new(root));
        if !denied.contains(&normalized) {
            denied.push(normalized);
        }
    }
    denied
}

/// True when `candidate` is inside any denied subtree.
pub(crate) fn path_is_denied(candidate: &Path, denied: &[PathBuf]) -> bool {
    denied.iter().any(|root| path_is_within(candidate, root))
}

/// The platform mechanism a confined spawn depends on.
///
/// Typed so a consumer can say WHICH mechanism was missing without parsing a
/// sentence. `#[non_exhaustive]`: a new host backend arrives as a variant a
/// consumer is told about rather than as new prose it has to match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SandboxMechanism {
    LinuxLandlock,
    MacosSandboxExec,
    WindowsAppContainer,
}

impl SandboxMechanism {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinuxLandlock => "linux_landlock",
            Self::MacosSandboxExec => "macos_sandbox_exec",
            Self::WindowsAppContainer => "windows_app_container",
        }
    }

    /// The mechanism fact in words. This is the ONLY thing Harn's own refusal
    /// text states; the remedy sentence belongs to whoever owns the control.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::LinuxLandlock => "Linux Landlock",
            Self::MacosSandboxExec => "macOS sandbox-exec",
            Self::WindowsAppContainer => "Windows AppContainer",
        }
    }
}

/// Why the mechanism could not confine this spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SandboxMechanismAvailability {
    /// The host does not provide it (no Landlock ABI, no `sandbox-exec`).
    AbsentOnHost,
    /// The host provides it, but this spawn entry point cannot carry it:
    /// Windows can only attach an AppContainer through the `Output`-returning
    /// path, which owns the `STARTUPINFOEX` plumbing.
    EntryPointCannotAttach,
}

impl SandboxMechanismAvailability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AbsentOnHost => "absent_on_host",
            Self::EntryPointCannotAttach => "entry_point_cannot_attach",
        }
    }
}

/// Which requirement the missing mechanism failed to satisfy.
///
/// This is the field an embedder reads to decide whether the ambient fallback
/// selector is even a control here: under [`SandboxRequirement::Profile`] the
/// profile mandates the mechanism and no selector value can weaken it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SandboxRequirement {
    /// The requested profile requires the mechanism outright.
    Profile,
    /// The profile tolerates the gap; the resolved fallback policy is what
    /// demanded the mechanism.
    Fallback,
}

impl SandboxRequirement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Fallback => "fallback",
        }
    }

    /// Whether the ambient fallback selector has any effect on this refusal.
    pub fn selector_is_honored(self) -> bool {
        matches!(self, Self::Fallback)
    }
}

/// A typed refusal for a spawn that required a platform sandbox mechanism the
/// host could not supply.
///
/// Replaces advice prose. Harn owns the mechanism fact — which mechanism, which
/// profile asked for it, which requirement went unsatisfied — and nothing else:
/// the remedy sentence depends on which controls an operator actually has,
/// which only the embedding product knows. An embedder that hardens its default
/// makes the fallback selector inert, and may use a Harn ladder name (say
/// `worktree`) for a profile of its own, so any sentence Harn appends about
/// those two inverts exactly where this refusal fires most.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxMechanismUnavailable {
    pub schema: String,
    pub mechanism: SandboxMechanism,
    pub availability: SandboxMechanismAvailability,
    /// The profile the spawn requested, in Harn's vocabulary. An embedder maps
    /// this to its own name; it must not be echoed as advice.
    pub profile: SandboxProfile,
    pub requirement: SandboxRequirement,
}

impl SandboxMechanismUnavailable {
    pub const SCHEMA: &'static str = "harn.process.sandbox_mechanism_unavailable.v1";

    /// One owner for the requirement derivation: `OsHardened` requires the
    /// mechanism by profile, every other confining profile only reaches a
    /// refusal because the resolved fallback said enforce.
    pub(crate) fn new(
        mechanism: SandboxMechanism,
        availability: SandboxMechanismAvailability,
        profile: SandboxProfile,
    ) -> Self {
        let requirement = if matches!(profile, SandboxProfile::OsHardened) {
            SandboxRequirement::Profile
        } else {
            SandboxRequirement::Fallback
        };
        Self {
            schema: Self::SCHEMA.to_string(),
            mechanism,
            availability,
            profile,
            requirement,
        }
    }

    pub fn category(&self) -> ErrorCategory {
        ErrorCategory::ToolRejected
    }

    pub(crate) fn into_error(self) -> VmError {
        VmError::SandboxMechanismUnavailable(Box::new(self))
    }

    /// The value a `catch` binding observes: every field typed, so a consumer
    /// branches on structure instead of substring-matching the message.
    pub fn thrown_value(&self) -> VmValue {
        let mut cause = std::collections::BTreeMap::new();
        cause.put_str("schema", self.schema.as_str());
        cause.put_str("mechanism", self.mechanism.as_str());
        cause.put_str("availability", self.availability.as_str());
        cause.put_str("profile", self.profile.as_str());
        cause.put_str("requirement", self.requirement.as_str());
        cause.insert(
            "selector_honored".to_string(),
            VmValue::Bool(self.requirement.selector_is_honored()),
        );

        let mut dict = std::collections::BTreeMap::new();
        dict.put_str("category", self.category().as_str());
        dict.put_str("message", self.to_string());
        dict.put_str("source", "sandbox_mechanism");
        dict.insert("sandbox_mechanism".to_string(), VmValue::dict(cause));
        VmValue::dict(dict)
    }
}

impl std::fmt::Display for SandboxMechanismUnavailable {
    /// The mechanism fact alone. No selector name, no profile name: both are
    /// structured fields above, and both are things an embedder may have
    /// remapped or made inert.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let fact = match self.availability {
            SandboxMechanismAvailability::AbsentOnHost => {
                format!(
                    "{} is not available on this host",
                    self.mechanism.display_name()
                )
            }
            SandboxMechanismAvailability::EntryPointCannotAttach => format!(
                "{} cannot be attached through this spawn entry point",
                self.mechanism.display_name()
            ),
        };
        let requirement = match self.requirement {
            SandboxRequirement::Profile => "the requested sandbox profile requires it",
            SandboxRequirement::Fallback => "the resolved sandbox fallback requires it",
        };
        write!(f, "{fact}; {requirement}")
    }
}

/// Helper for backends that can't attach confinement at all (macOS
/// without `/usr/bin/sandbox-exec`, Windows when called through the
/// `Command`-returning entry points): either fail loudly under
/// `OsHardened` / `enforce`, or warn once and proceed direct.
///
/// Linux and OpenBSD don't reach this path — they install confinement
/// in `pre_exec` and surface unavailability through `landlock_profile`
/// directly. The dead-code lint allow keeps the helper compilable on
/// targets where no backend uses it.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub(crate) fn unavailable(
    mechanism: SandboxMechanism,
    availability: SandboxMechanismAvailability,
    profile: SandboxProfile,
) -> Result<PrepareOutcome, VmError> {
    match effective_fallback(profile) {
        SandboxFallback::Off | SandboxFallback::Warn => {
            warn_once(
                "handler_sandbox_unavailable",
                &mechanism_skipped_warning(mechanism, availability),
            );
            Ok(PrepareOutcome::Direct)
        }
        SandboxFallback::Enforce => {
            Err(SandboxMechanismUnavailable::new(mechanism, availability, profile).into_error())
        }
    }
}

/// The warn-and-proceed sentence: the mechanism fact and the consequence. The
/// remedy belongs to whoever owns the control, so it is not stated here.
pub(crate) fn mechanism_skipped_warning(
    mechanism: SandboxMechanism,
    availability: SandboxMechanismAvailability,
) -> String {
    let fact = match availability {
        SandboxMechanismAvailability::AbsentOnHost => "is not available on this host",
        SandboxMechanismAvailability::EntryPointCannotAttach => {
            "cannot be attached through this spawn entry point"
        }
    };
    format!(
        "{} {fact}; process filesystem isolation is disabled",
        mechanism.display_name()
    )
}

#[cfg(test)]
#[path = "refusal_tests.rs"]
mod refusal_tests;
