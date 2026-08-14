use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::orchestration::{CapabilityPolicy, ToolApprovalPolicy};

use super::receipt::RunAuthorityReceipt;

pub const RUN_AUTHORITY_PLAN_SCHEMA: &str = "harn.run_authority_plan.v1";
pub const RUN_AUTHORITY_RECEIPT_SCHEMA: &str = "harn.run_authority.v1";
pub const RUN_AUTHORITY_PLAN_V1_SCHEMA_JSON: &str =
    include_str!("../../schemas/run-authority-plan.v1.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunInteractivity {
    Interactive,
    NonInteractive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalAvailability {
    Available,
    Unavailable,
}

/// Prepared-run receipts reuse the canonical permission activity decider
/// vocabulary rather than defining a host-specific approval taxonomy.
pub type AuthorityDecider = crate::orchestration::ToolPermissionDecider;

#[derive(Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct RunBudget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend_microusd: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns: Option<u64>,
}

impl RunBudget {
    pub(crate) fn exceeds(&self, ceiling: &Self) -> Vec<&'static str> {
        let mut exceeded = Vec::new();
        if exceeds(self.spend_microusd, ceiling.spend_microusd) {
            exceeded.push("spend_microusd");
        }
        if exceeds(self.time_ms, ceiling.time_ms) {
            exceeded.push("time_ms");
        }
        if exceeds(self.turns, ceiling.turns) {
            exceeded.push("turns");
        }
        exceeded
    }

    pub(crate) fn missing_dimensions(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.spend_microusd.is_none() {
            missing.push("spend_microusd");
        }
        if self.time_ms.is_none() {
            missing.push("time_ms");
        }
        if self.turns.is_none() {
            missing.push("turns");
        }
        missing
    }
}

fn exceeds(requested: Option<u64>, ceiling: Option<u64>) -> bool {
    matches!((requested, ceiling), (Some(requested), Some(ceiling)) if requested > ceiling)
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct RuntimeContractProvenance {
    pub harn_version: String,
    pub harn_revision: String,
    pub host_name: String,
    pub host_version: String,
    pub host_revision: String,
    pub contracts_version: String,
    pub runtime_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretSourceKind {
    ProcessLocal,
    DurableBroker,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretConsumerKind {
    Provider,
    Process,
    Mcp,
    Connector,
}

pub const PLATFORM_IDENTITY_REF_SCHEME: &str = "harn-identity://";

/// A value-free identity reference. Platform credentials never enter a run
/// plan; hosts resolve this reference behind a consumer-bound broker.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PlatformIdentityReference(String);

impl PlatformIdentityReference {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let path = raw
            .strip_prefix(PLATFORM_IDENTITY_REF_SCHEME)
            .ok_or_else(|| {
                "identity reference must use harn-identity://<namespace>/<name>".to_string()
            })?;
        let mut segments = path.split('/');
        let namespace = segments.next().unwrap_or_default();
        let name = segments.next().unwrap_or_default();
        if namespace.is_empty()
            || name.is_empty()
            || segments.next().is_some()
            || raw.chars().any(char::is_whitespace)
        {
            return Err(
                "identity reference must use harn-identity://<namespace>/<name>".to_string(),
            );
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PlatformIdentityReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PlatformIdentityReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformIdentitySourceKind {
    SdkProfile,
    WorkloadIdentity,
    InstanceMetadata,
    HostedBroker,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityRenewalMode {
    None,
    BrokerManaged,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct IdentityBrokerBinding {
    pub provider: String,
    pub audience: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub consumer: SecretConsumerBinding,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct IdentityBrokerRequirement {
    pub reference: PlatformIdentityReference,
    pub broker_id: String,
    pub source: PlatformIdentitySourceKind,
    pub renewal: IdentityRenewalMode,
    pub binding: IdentityBrokerBinding,
}

/// Observed broker properties supplied by a host. `bindings` is an exact
/// allow-set, not a pattern language, so provider/audience/tenant/consumer
/// drift changes readiness and the plan fingerprint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityBrokerFacts {
    pub broker_id: String,
    pub sources: BTreeSet<PlatformIdentitySourceKind>,
    pub supports_non_interactive: bool,
    pub may_prompt_gui: bool,
    pub material_outside_sandbox: bool,
    pub opaque_process_local_handles: bool,
    pub renewal_modes: BTreeSet<IdentityRenewalMode>,
    pub bindings: BTreeSet<IdentityBrokerBinding>,
}

/// A canonical Harn secret reference. Construction rejects raw values so a
/// run authority plan cannot accidentally serialize credential material.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SecretReference(String);

impl SecretReference {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let id = crate::secrets::parse_secret_ref(raw)
            .ok()
            .flatten()
            .ok_or_else(|| {
                "secret reference must use harn-secret://<namespace>/<name>".to_string()
            })?;
        Ok(Self(format!("{}{}", crate::secrets::SECRET_REF_SCHEME, id)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretReference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct SecretConsumerBinding {
    pub kind: SecretConsumerKind,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct SecretRequirement {
    pub reference: SecretReference,
    pub source: SecretSourceKind,
    pub consumer: SecretConsumerBinding,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretBrokerFacts {
    pub outside_sandbox: bool,
    pub supports_non_interactive: bool,
    pub may_prompt_gui: bool,
    pub zeroizing_handles: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct NetworkRequirement {
    pub destination: String,
    pub protocol: String,
    pub port: u16,
}

impl NetworkRequirement {
    pub(crate) fn url(&self) -> String {
        format!("{}://{}:{}", self.protocol, self.destination, self.port)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSocketKind {
    Unix,
    TcpLoopback,
    Docker,
    SshAgent,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ProcessSocketRequirement {
    pub socket_kind: ProcessSocketKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct McpRequirement {
    pub server: String,
    pub tool: String,
    pub side_effect: String,
}

/// An exact post-readiness command that may discover narrower process read
/// roots. The ceiling is authority reviewed during preparation; probe output
/// cannot widen beyond it.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ToolchainProbeRequirement {
    pub probe_id: String,
    pub executable: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    pub working_directory: String,
    pub read_root_ceiling: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolchainProbeResult {
    #[serde(default)]
    pub discovered_read_roots: Vec<String>,
    /// A sandbox adapter reports any operation the probe attempted beyond the
    /// declared command. These observations are evaluated as lease deltas and
    /// cannot be silently discarded.
    #[serde(default)]
    pub attempted_authority: Vec<AuthorityRequirement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthorityRequirement {
    FilesystemRead {
        root: String,
    },
    FilesystemWrite {
        root: String,
    },
    ProcessReadRoot {
        root: String,
    },
    ProcessWriteRoot {
        root: String,
    },
    ProcessSandbox {
        profile: String,
        preset: String,
    },
    ProcessSocket(ProcessSocketRequirement),
    Network(NetworkRequirement),
    Secret(SecretRequirement),
    Environment {
        name: String,
    },
    Tool {
        pattern: String,
    },
    HostCapability {
        capability: String,
        operation: String,
    },
    SideEffectCeiling {
        level: String,
    },
    RecursionLimit {
        depth: usize,
    },
    Mcp(McpRequirement),
    ToolchainProbe(ToolchainProbeRequirement),
    IdentityBroker(IdentityBrokerRequirement),
    Budget {
        budget: RunBudget,
    },
    Provenance {
        provenance: RuntimeContractProvenance,
    },
    Startup {
        deadline_at_ms: u64,
        receipt_uri: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunIntent {
    pub intent_id: String,
    pub capability_policy: CapabilityPolicy,
    #[serde(default)]
    pub network: Vec<NetworkRequirement>,
    #[serde(default)]
    pub secrets: Vec<SecretRequirement>,
    #[serde(default)]
    pub admitted_environment: Vec<String>,
    #[serde(default)]
    pub process_sockets: Vec<ProcessSocketRequirement>,
    #[serde(default)]
    pub mcp: Vec<McpRequirement>,
    #[serde(default)]
    pub toolchain_probes: Vec<ToolchainProbeRequirement>,
    #[serde(default)]
    pub identity_brokers: Vec<IdentityBrokerRequirement>,
    pub budget: RunBudget,
    pub provenance: RuntimeContractProvenance,
    pub interactivity: RunInteractivity,
    pub startup_deadline_at_ms: u64,
    pub receipt_uri: String,
}

#[derive(Clone, Debug)]
pub struct HostFacts {
    pub capability_ceiling: CapabilityPolicy,
    pub approval_policy: ToolApprovalPolicy,
    pub approval_availability: ApprovalAvailability,
    pub approved_batches: BTreeMap<String, AuthorityDecider>,
    pub net_policy: crate::harness_net::NetPolicy,
    pub secret_bindings: BTreeSet<SecretRequirement>,
    pub secret_brokers: BTreeMap<SecretSourceKind, SecretBrokerFacts>,
    pub admitted_environment: BTreeSet<String>,
    pub process_sockets: BTreeSet<ProcessSocketRequirement>,
    pub mcp: BTreeSet<McpRequirement>,
    pub toolchain_probes: BTreeSet<ToolchainProbeRequirement>,
    pub identity_brokers: BTreeMap<String, IdentityBrokerFacts>,
    pub budget_ceiling: RunBudget,
    pub provenance: RuntimeContractProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAuthorityPlanV1 {
    pub schema: String,
    pub intent_id: String,
    pub capability_policy: CapabilityPolicy,
    pub requirements: Vec<AuthorityRequirement>,
    pub budget: RunBudget,
    pub provenance: RuntimeContractProvenance,
    pub interactivity: RunInteractivity,
    pub startup_deadline_at_ms: u64,
    pub receipt_uri: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthorityDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requirement_fingerprint: Option<String>,
    pub actionable: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalGroup {
    pub semantic_group: String,
    pub requirement_fingerprints: Vec<String>,
    pub summaries: Vec<String>,
    pub risk_labels: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApprovalBatch {
    pub batch_fingerprint: String,
    pub plan_fingerprint: String,
    pub groups: Vec<ApprovalGroup>,
}

#[derive(Debug)]
pub enum PreparationOutcome {
    Ready {
        authority_lease: Box<AuthorityLease>,
        receipt: RunAuthorityReceipt,
    },
    NeedsApproval {
        batched_requests: ApprovalBatch,
        receipt: RunAuthorityReceipt,
    },
    Blocked {
        diagnostics: Vec<AuthorityDiagnostic>,
        receipt: Option<RunAuthorityReceipt>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityLeaseDelta {
    pub(crate) parent_lease_fingerprint: String,
    pub(crate) requirement: AuthorityRequirement,
    pub(crate) requirement_fingerprint: String,
    pub(crate) expires_at_ms: u64,
}

impl AuthorityLeaseDelta {
    pub fn parent_lease_fingerprint(&self) -> &str {
        &self.parent_lease_fingerprint
    }

    pub fn requirement(&self) -> &AuthorityRequirement {
        &self.requirement
    }

    pub fn requirement_fingerprint(&self) -> &str {
        &self.requirement_fingerprint
    }

    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeaseDeltaOutcome {
    Covered,
    Attenuated(AuthorityLeaseDelta),
    Blocked(AuthorityDiagnostic),
}

#[derive(Debug)]
pub enum ToolchainDiscoveryOutcome {
    Discovered {
        deltas: Vec<AuthorityLeaseDelta>,
        receipt: RunAuthorityReceipt,
    },
    NeedsApproval {
        batched_requests: ApprovalBatch,
        receipt: RunAuthorityReceipt,
    },
    Blocked {
        diagnostics: Vec<AuthorityDiagnostic>,
        receipt: RunAuthorityReceipt,
    },
}

#[derive(Debug)]
pub struct AuthorityLease {
    pub(crate) lease_fingerprint: String,
    pub(crate) plan_fingerprint: String,
    pub(crate) plan: RunAuthorityPlanV1,
    pub(crate) requested_fingerprints: BTreeMap<String, AuthorityRequirement>,
    pub(crate) requirement_fingerprints: BTreeMap<String, AuthorityRequirement>,
    pub(crate) approval_policy: ToolApprovalPolicy,
    pub(crate) net_policy: crate::harness_net::NetPolicy,
    pub(crate) deciders: BTreeMap<String, AuthorityDecider>,
    pub(crate) approval_availability: ApprovalAvailability,
    pub(crate) prior_used: BTreeSet<String>,
    pub(crate) prior_denied: Vec<super::receipt::DeniedAuthority>,
    pub(crate) prior_policy_decisions: Vec<super::receipt::PolicyDecisionEvidence>,
    pub(crate) invalidated: Option<AuthorityDiagnostic>,
    pub(crate) expires_at_ms: u64,
}

impl AuthorityLease {
    pub fn fingerprint(&self) -> &str {
        &self.lease_fingerprint
    }

    pub fn plan_fingerprint(&self) -> &str {
        &self.plan_fingerprint
    }

    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    pub fn plan(&self) -> &RunAuthorityPlanV1 {
        &self.plan
    }
}
