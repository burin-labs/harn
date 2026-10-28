//! Typed sandbox control flow shared by the interpreter and native backends.

use harn_execution_policy::{CapabilityPolicy, SandboxProfile};
use serde::{Deserialize, Serialize};

use super::{ErrorCategory, VmDictExt, VmError, VmValue};

/// One thing a sandbox can hold a confined child to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfinementDimension {
    /// Writes land only under the policy's writable roots.
    Writes,
    /// Reads reach only the policy's readable roots.
    Reads,
    /// A path on the credential denylist stays unreadable, even under a root
    /// that is otherwise readable.
    CredentialReads,
    /// A policy below the `network` ceiling opens no network connection.
    Network,
    /// A confined child cannot signal or inspect processes outside its own
    /// tree.
    Process,
}

impl ConfinementDimension {
    /// Every dimension, in the order the table reports them.
    pub const ALL: [ConfinementDimension; 5] = [
        Self::Writes,
        Self::Reads,
        Self::CredentialReads,
        Self::Network,
        Self::Process,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Writes => "writes",
            Self::Reads => "reads",
            Self::CredentialReads => "credential_reads",
            Self::Network => "network",
            Self::Process => "process",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Writes => "writes",
            Self::Reads => "reads",
            Self::CredentialReads => "credential reads",
            Self::Network => "network",
            Self::Process => "process",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Writes => 0,
            Self::Reads => 1,
            Self::CredentialReads => 2,
            Self::Network => 3,
            Self::Process => 4,
        }
    }

    /// Whether a confining `policy` asks the backend to hold this dimension.
    /// Every confining profile bounds writes, reads, and the credential
    /// denylist; the network is bounded only below the `network` ceiling. No
    /// policy term asks for process isolation yet.
    pub fn required_by(self, policy: &CapabilityPolicy) -> bool {
        match self {
            Self::Writes | Self::Reads | Self::CredentialReads => true,
            Self::Network => !policy.allows_network(),
            Self::Process => false,
        }
    }
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
    OpenbsdUnveil,
    /// No OS sandbox: Windows, and any platform without a backend. Serialized
    /// as `none`, the filesystem mechanism the capability report names.
    #[serde(rename = "none")]
    Unconfined,
    LinuxBubblewrap,
}

impl SandboxMechanism {
    /// Every mechanism, so a table keyed by mechanism can be held complete.
    pub const ALL: &'static [SandboxMechanism] = &[
        Self::LinuxLandlock,
        Self::LinuxBubblewrap,
        Self::MacosSandboxExec,
        Self::OpenbsdUnveil,
        Self::Unconfined,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LinuxLandlock => "linux_landlock",
            Self::LinuxBubblewrap => "linux_bubblewrap",
            Self::MacosSandboxExec => "macos_sandbox_exec",
            Self::OpenbsdUnveil => "openbsd_unveil",
            Self::Unconfined => "none",
        }
    }

    /// The mechanism fact in words. This is the ONLY thing Harn's own refusal
    /// text states; the remedy sentence belongs to whoever owns the control.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::LinuxLandlock => "Linux Landlock",
            Self::LinuxBubblewrap => "Linux bubblewrap",
            Self::MacosSandboxExec => "macOS sandbox-exec",
            Self::OpenbsdUnveil => "OpenBSD unveil",
            Self::Unconfined => "No OS sandbox",
        }
    }

    /// Legacy tool receipt spelling, projected from the selected mechanism.
    pub fn process_kind(self) -> &'static str {
        match self {
            Self::LinuxLandlock => "landlock",
            Self::LinuxBubblewrap => "bubblewrap",
            Self::MacosSandboxExec => "sandbox-exec",
            Self::OpenbsdUnveil => "unveil",
            Self::Unconfined => "none",
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
    /// The mechanism is attached, but it does not hold the child to every
    /// dimension the profile requires; `unconfined` names them.
    DoesNotConfine,
    /// The mechanism is present, but a host prerequisite that keeps this
    /// process's own state from confined children is not met;
    /// `host_protection_gap` names it.
    HostProtectionUnavailable,
}

impl SandboxMechanismAvailability {
    /// Fact reported when a host proceeds without the requested confinement.
    pub fn description(self) -> &'static str {
        match self {
            Self::AbsentOnHost => "is not available on this host",
            Self::DoesNotConfine => "does not confine every requested dimension",
            Self::HostProtectionUnavailable => {
                "cannot protect this process from confined children on this host"
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AbsentOnHost => "absent_on_host",
            Self::DoesNotConfine => "does_not_confine",
            Self::HostProtectionUnavailable => "host_protection_unavailable",
        }
    }
}

/// The host prerequisite that left a present mechanism unable to protect the
/// spawning process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostProtectionGap {
    /// System Integrity Protection allows unrestricted DTrace, which is the
    /// case whenever SIP is disabled. XNU then shows this process's launch
    /// environment to confined children.
    UnrestrictedDtrace,
    /// The kernel is a development kernel, which skips the same redaction.
    DevelopmentKernel,
    /// The kernel's integrity configuration could not be read, so the
    /// protection cannot be confirmed.
    KernelConfigurationUnreadable,
}

impl HostProtectionGap {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnrestrictedDtrace => "unrestricted_dtrace",
            Self::DevelopmentKernel => "development_kernel",
            Self::KernelConfigurationUnreadable => "kernel_configuration_unreadable",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::UnrestrictedDtrace => "System Integrity Protection allows unrestricted DTrace",
            Self::DevelopmentKernel => "this host runs a development kernel",
            Self::KernelConfigurationUnreadable => {
                "the kernel's integrity configuration could not be read"
            }
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
    /// Required dimensions the mechanism does not hold. An absent backend may
    /// name every required dimension; older producers leave that list empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unconfined: Vec<ConfinementDimension>,
    /// Set with [`SandboxMechanismAvailability::HostProtectionUnavailable`]:
    /// which host prerequisite was not met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_protection_gap: Option<HostProtectionGap>,
}

impl SandboxMechanismUnavailable {
    pub const SCHEMA: &'static str = "harn.process.sandbox_mechanism_unavailable.v1";

    /// One owner for the requirement derivation: `OsHardened` requires the
    /// mechanism by profile, every other confining profile only reaches a
    /// refusal because the resolved fallback said enforce.
    pub fn new(
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
            unconfined: Vec::new(),
            host_protection_gap: None,
        }
    }

    /// A present mechanism whose host cannot meet `gap`.
    pub fn host_protection_unavailable(
        mechanism: SandboxMechanism,
        gap: HostProtectionGap,
        profile: SandboxProfile,
    ) -> Self {
        Self {
            host_protection_gap: Some(gap),
            ..Self::new(
                mechanism,
                SandboxMechanismAvailability::HostProtectionUnavailable,
                profile,
            )
        }
    }

    pub fn category(&self) -> ErrorCategory {
        ErrorCategory::ToolRejected
    }

    pub fn into_error(self) -> VmError {
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
        if !self.unconfined.is_empty() {
            cause.put(
                "unconfined",
                VmValue::strings(self.unconfined.iter().map(|dimension| dimension.as_str())),
            );
        }
        if let Some(gap) = self.host_protection_gap {
            cause.put_str("host_protection_gap", gap.as_str());
        }
        cause.put_bool("selector_honored", self.requirement.selector_is_honored());

        let mut dict = std::collections::BTreeMap::new();
        dict.put_str("category", self.category().as_str());
        dict.put_str("message", self.to_string());
        dict.put_str("source", "sandbox_mechanism");
        dict.put("sandbox_mechanism", VmValue::dict(cause));
        VmValue::dict(dict)
    }
}

impl std::fmt::Display for SandboxMechanismUnavailable {
    /// The mechanism fact alone. No selector name, no profile name: both are
    /// structured fields above, and both are things an embedder may have
    /// remapped or made inert.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let dimensions = self
            .unconfined
            .iter()
            .map(|dimension| dimension.display_name())
            .collect::<Vec<_>>()
            .join(", ");
        let fact = match self.availability {
            _ if self.mechanism == SandboxMechanism::Unconfined => {
                if dimensions.is_empty() {
                    NO_OS_SANDBOX.to_string()
                } else {
                    format!("{NO_OS_SANDBOX}, so nothing confines {dimensions}")
                }
            }
            SandboxMechanismAvailability::AbsentOnHost => {
                format!(
                    "{} is not available on this host",
                    self.mechanism.display_name()
                )
            }
            SandboxMechanismAvailability::DoesNotConfine => format!(
                "{} does not confine {dimensions}",
                self.mechanism.display_name(),
            ),
            SandboxMechanismAvailability::HostProtectionUnavailable => format!(
                "{} is present but cannot keep this process's environment from confined \
                 children, because {}",
                self.mechanism.display_name(),
                self.host_protection_gap.map_or(
                    "a host prerequisite is not met",
                    HostProtectionGap::display_name
                ),
            ),
        };
        let requirement = match self.requirement {
            SandboxRequirement::Profile => "the requested sandbox profile requires it",
            SandboxRequirement::Fallback => "the resolved sandbox fallback requires it",
        };
        write!(f, "{fact}; {requirement}")
    }
}

/// The fact every refusal and warning from the unconfined backend states.
pub const NO_OS_SANDBOX: &str = "this platform has no OS process sandbox";
