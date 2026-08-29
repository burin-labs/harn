//! Portable, value-free activity projection for generic tool permission decisions.
//!
//! Its construction boundary accepts only a registered tool name and typed
//! policy facts. Raw arguments, stdin, environment, command text, credentials,
//! provider payloads, protected values, arbitrary policy context, and reusable
//! grants have no input or output field in this module.

use serde::{Deserialize, Serialize};

use crate::tool_annotations::{SideEffectLevel, ToolKind};

const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_TOOL_NAME_BYTES: usize = 128;
const MAX_CAPABILITIES: usize = 64;
const MAX_RISK_LABELS: usize = 32;
const MAX_POLICY_EVALUATIONS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    ExternalAction,
    ToolPermission,
}

impl ActivityKind {
    pub const ALL: [Self; 2] = [Self::ExternalAction, Self::ToolPermission];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionOutcome {
    Approved,
    Denied,
    TimedOut,
    Cancelled,
}

impl ToolPermissionOutcome {
    pub const ALL: [Self; 4] = [
        Self::Approved,
        Self::Denied,
        Self::TimedOut,
        Self::Cancelled,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionDecider {
    Person,
    RememberedRule,
    UserPolicy,
    ManagedPolicy,
    RuntimePolicy,
    HostUnavailable,
    /// The automated reviewer answered on the operator's behalf. Distinct from
    /// every policy layer above it: those decide from a written rule, this one
    /// decided from a model call, and a rollup that cannot tell them apart
    /// cannot report how often the fallback was load-bearing.
    AutoReviewer,
}

impl ToolPermissionDecider {
    pub const ALL: [Self; 7] = [
        Self::Person,
        Self::RememberedRule,
        Self::UserPolicy,
        Self::ManagedPolicy,
        Self::RuntimePolicy,
        Self::HostUnavailable,
        Self::AutoReviewer,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionPolicyLayer {
    UserPolicy,
    ManagedPolicy,
    RuntimePolicy,
    RememberedRule,
}

impl ToolPermissionPolicyLayer {
    pub const ALL: [Self; 4] = [
        Self::UserPolicy,
        Self::ManagedPolicy,
        Self::RuntimePolicy,
        Self::RememberedRule,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionPolicyOutcome {
    Allowed,
    Denied,
    ApprovalRequired,
    Unavailable,
}

impl ToolPermissionPolicyOutcome {
    pub const ALL: [Self; 4] = [
        Self::Allowed,
        Self::Denied,
        Self::ApprovalRequired,
        Self::Unavailable,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionGrantScope {
    Once,
    Session,
}

impl ToolPermissionGrantScope {
    pub const ALL: [Self; 2] = [Self::Once, Self::Session];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionGrantExpiry {
    AfterDispatch,
    SessionEnd,
}

impl ToolPermissionGrantExpiry {
    pub const ALL: [Self; 2] = [Self::AfterDispatch, Self::SessionEnd];
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolPermissionScope {
    pub tool_kind: ToolKind,
    pub side_effect: SideEffectLevel,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolPermissionPolicyEvidence {
    pub layer: ToolPermissionPolicyLayer,
    pub outcome: ToolPermissionPolicyOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    pub risk_labels: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolPermissionGrantEvidence {
    pub scope: ToolPermissionGrantScope,
    pub expires: ToolPermissionGrantExpiry,
    /// Always false. Durable activity proves a decision; it never carries an
    /// authority object that another dispatch could reuse.
    pub reusable: bool,
}

impl ToolPermissionGrantEvidence {
    fn new(scope: ToolPermissionGrantScope) -> Self {
        Self {
            scope,
            expires: match scope {
                ToolPermissionGrantScope::Once => ToolPermissionGrantExpiry::AfterDispatch,
                ToolPermissionGrantScope::Session => ToolPermissionGrantExpiry::SessionEnd,
            },
            reusable: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolPermissionRequester {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ToolPermissionActivityRecord {
    pub schema: String,
    pub kind: ActivityKind,
    pub id: String,
    pub request_id: String,
    pub tool_name: String,
    pub scope: ToolPermissionScope,
    pub outcome: ToolPermissionOutcome,
    pub decider: ToolPermissionDecider,
    pub policy_evaluations: Vec<ToolPermissionPolicyEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<ToolPermissionGrantEvidence>,
    pub requester: ToolPermissionRequester,
    pub occurred_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolPermissionActivityContext {
    pub id: String,
    pub request_id: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub model_provider: Option<String>,
    pub model_id: Option<String>,
    pub policy_layer: ToolPermissionPolicyLayer,
    pub occurred_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolPermissionPolicyFacts {
    pub outcome: ToolPermissionPolicyOutcome,
    pub rule_id: Option<String>,
    pub risk_labels: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolPermissionResolution {
    pub outcome: ToolPermissionOutcome,
    pub decider: ToolPermissionDecider,
    pub grant_scope: Option<ToolPermissionGrantScope>,
    pub policy_evaluations: Vec<ToolPermissionPolicyEvidence>,
}

impl ToolPermissionResolution {
    pub fn approved(decider: ToolPermissionDecider, scope: ToolPermissionGrantScope) -> Self {
        Self {
            outcome: ToolPermissionOutcome::Approved,
            decider,
            grant_scope: Some(scope),
            policy_evaluations: Vec::new(),
        }
    }

    pub fn terminal(outcome: ToolPermissionOutcome, decider: ToolPermissionDecider) -> Self {
        Self {
            outcome,
            decider,
            grant_scope: None,
            policy_evaluations: Vec::new(),
        }
    }

    pub fn with_host_policy_evaluations(
        mut self,
        evaluations: Vec<ToolPermissionPolicyEvidence>,
    ) -> Result<Self, ToolPermissionActivityError> {
        if evaluations
            .iter()
            .any(|evaluation| evaluation.layer == ToolPermissionPolicyLayer::RuntimePolicy)
        {
            return Err(ToolPermissionActivityError::InvalidResolution);
        }
        self.policy_evaluations = normalize_policy_evaluations(evaluations)?;
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolPermissionActivityError {
    InvalidIdentifier,
    InvalidToolName,
    InvalidResolution,
    TooManyCapabilities,
    TooManyRiskLabels,
}

impl ToolPermissionActivityRecord {
    pub fn from_policy_facts(
        tool_name: &str,
        policy: ToolPermissionPolicyFacts,
        context: ToolPermissionActivityContext,
        resolution: ToolPermissionResolution,
    ) -> Result<Self, ToolPermissionActivityError> {
        validate_identifier(&context.id)?;
        validate_identifier(&context.request_id)?;
        validate_identifier(&context.session_id)?;
        validate_optional_identifier(context.agent_id.as_deref())?;
        validate_optional_identifier(context.model_provider.as_deref())?;
        validate_optional_identifier(context.model_id.as_deref())?;
        validate_tool_name(tool_name)?;

        let terminal_with_grant = resolution.outcome != ToolPermissionOutcome::Approved
            && resolution.grant_scope.is_some();
        let invalid_approval = resolution.outcome == ToolPermissionOutcome::Approved
            && (resolution.decider == ToolPermissionDecider::HostUnavailable
                || (resolution.grant_scope.is_none()
                    && matches!(
                        resolution.decider,
                        ToolPermissionDecider::Person | ToolPermissionDecider::RememberedRule
                    )));
        if terminal_with_grant || invalid_approval {
            return Err(ToolPermissionActivityError::InvalidResolution);
        }

        let annotations = super::current_tool_annotations(tool_name).unwrap_or_default();
        let mut capabilities = annotations
            .capabilities
            .iter()
            .flat_map(|(capability, operations)| {
                operations
                    .iter()
                    .map(move |operation| format!("{capability}.{operation}"))
            })
            .collect::<Vec<_>>();
        capabilities.sort();
        capabilities.dedup();
        if capabilities.len() > MAX_CAPABILITIES
            || capabilities
                .iter()
                .any(|value| !is_safe_identifier(value, MAX_IDENTIFIER_BYTES))
        {
            return Err(ToolPermissionActivityError::TooManyCapabilities);
        }

        let mut risk_labels = policy.risk_labels;
        risk_labels.sort();
        risk_labels.dedup();
        if risk_labels.len() > MAX_RISK_LABELS
            || risk_labels
                .iter()
                .any(|value| !is_safe_identifier(value, MAX_IDENTIFIER_BYTES))
        {
            return Err(ToolPermissionActivityError::TooManyRiskLabels);
        }

        let mut policy_evaluations =
            normalize_policy_evaluations(vec![ToolPermissionPolicyEvidence {
                layer: context.policy_layer,
                outcome: policy.outcome,
                rule_id: policy.rule_id,
                risk_labels,
            }])?;
        for evaluation in &resolution.policy_evaluations {
            if policy_evaluations
                .iter()
                .any(|existing| existing.layer == evaluation.layer)
            {
                return Err(ToolPermissionActivityError::InvalidResolution);
            }
        }
        policy_evaluations.extend(resolution.policy_evaluations.clone());
        validate_decider_evidence(resolution.decider, resolution.outcome, &policy_evaluations)?;

        Ok(Self {
            schema: "harn.tool_permission_activity.v1".to_string(),
            kind: ActivityKind::ToolPermission,
            id: context.id,
            request_id: context.request_id,
            tool_name: tool_name.to_string(),
            scope: ToolPermissionScope {
                tool_kind: annotations.kind,
                side_effect: annotations.side_effect_level,
                capabilities,
            },
            outcome: resolution.outcome,
            decider: resolution.decider,
            policy_evaluations,
            grant: resolution.grant_scope.map(ToolPermissionGrantEvidence::new),
            requester: ToolPermissionRequester {
                session_id: context.session_id,
                agent_id: context.agent_id,
                model_provider: context.model_provider,
                model_id: context.model_id,
            },
            occurred_at_ms: context.occurred_at_ms,
        })
    }
}

fn normalize_policy_evaluations(
    mut evaluations: Vec<ToolPermissionPolicyEvidence>,
) -> Result<Vec<ToolPermissionPolicyEvidence>, ToolPermissionActivityError> {
    if evaluations.len() > MAX_POLICY_EVALUATIONS {
        return Err(ToolPermissionActivityError::InvalidResolution);
    }
    let mut seen_layers = Vec::new();
    for evaluation in &mut evaluations {
        if seen_layers.contains(&evaluation.layer) {
            return Err(ToolPermissionActivityError::InvalidResolution);
        }
        seen_layers.push(evaluation.layer);
        if let Some(rule_id) = evaluation.rule_id.as_deref() {
            validate_identifier(rule_id)?;
        }
        evaluation.risk_labels.sort();
        evaluation.risk_labels.dedup();
        if evaluation.risk_labels.len() > MAX_RISK_LABELS
            || evaluation
                .risk_labels
                .iter()
                .any(|value| !is_safe_identifier(value, MAX_IDENTIFIER_BYTES))
        {
            return Err(ToolPermissionActivityError::TooManyRiskLabels);
        }
    }
    Ok(evaluations)
}

fn validate_decider_evidence(
    decider: ToolPermissionDecider,
    outcome: ToolPermissionOutcome,
    evaluations: &[ToolPermissionPolicyEvidence],
) -> Result<(), ToolPermissionActivityError> {
    let layer = match decider {
        ToolPermissionDecider::RememberedRule => ToolPermissionPolicyLayer::RememberedRule,
        ToolPermissionDecider::UserPolicy => ToolPermissionPolicyLayer::UserPolicy,
        ToolPermissionDecider::ManagedPolicy => ToolPermissionPolicyLayer::ManagedPolicy,
        ToolPermissionDecider::RuntimePolicy => ToolPermissionPolicyLayer::RuntimePolicy,
        // A reviewer decision corroborates no policy LAYER, so there is no
        // layer evidence to require. It joins `Person` here for the same
        // reason: both answered an ask that the layers had already resolved to
        // `ApprovalRequired`, and demanding a matching layer evaluation would
        // reject every honest reviewer record.
        ToolPermissionDecider::Person
        | ToolPermissionDecider::HostUnavailable
        | ToolPermissionDecider::AutoReviewer => return Ok(()),
    };
    let expected = match outcome {
        ToolPermissionOutcome::Approved => ToolPermissionPolicyOutcome::Allowed,
        ToolPermissionOutcome::Denied => ToolPermissionPolicyOutcome::Denied,
        ToolPermissionOutcome::TimedOut | ToolPermissionOutcome::Cancelled => {
            ToolPermissionPolicyOutcome::Unavailable
        }
    };
    evaluations
        .iter()
        .any(|evaluation| evaluation.layer == layer && evaluation.outcome == expected)
        .then_some(())
        .ok_or(ToolPermissionActivityError::InvalidResolution)
}

fn validate_identifier(value: &str) -> Result<(), ToolPermissionActivityError> {
    if is_safe_identifier(value, MAX_IDENTIFIER_BYTES) {
        Ok(())
    } else {
        Err(ToolPermissionActivityError::InvalidIdentifier)
    }
}

fn validate_optional_identifier(value: Option<&str>) -> Result<(), ToolPermissionActivityError> {
    value.map(validate_identifier).transpose().map(|_| ())
}

fn validate_tool_name(value: &str) -> Result<(), ToolPermissionActivityError> {
    if is_safe_identifier(value, MAX_TOOL_NAME_BYTES) {
        Ok(())
    } else {
        Err(ToolPermissionActivityError::InvalidToolName)
    }
}

fn is_safe_identifier(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(outcome: ToolPermissionPolicyOutcome) -> ToolPermissionPolicyFacts {
        ToolPermissionPolicyFacts {
            outcome,
            rule_id: Some("travel-write".to_string()),
            risk_labels: vec!["network_rule".to_string()],
        }
    }

    fn context() -> ToolPermissionActivityContext {
        ToolPermissionActivityContext {
            id: "permission-1".to_string(),
            request_id: "tool-call-1".to_string(),
            session_id: "session-1".to_string(),
            agent_id: Some("assistant".to_string()),
            model_provider: Some("openai".to_string()),
            model_id: Some("gpt-5.6".to_string()),
            policy_layer: ToolPermissionPolicyLayer::UserPolicy,
            occurred_at_ms: 42,
        }
    }

    #[test]
    fn projection_is_value_free_and_retains_decision_evidence() {
        let record = ToolPermissionActivityRecord::from_policy_facts(
            "gmail.create_draft",
            policy(ToolPermissionPolicyOutcome::ApprovalRequired),
            context(),
            ToolPermissionResolution::approved(
                ToolPermissionDecider::Person,
                ToolPermissionGrantScope::Once,
            ),
        )
        .unwrap();

        let encoded = serde_json::to_string(&record).unwrap();
        assert_eq!(record.kind, ActivityKind::ToolPermission);
        assert_eq!(record.outcome, ToolPermissionOutcome::Approved);
        assert!(!record.grant.as_ref().unwrap().reusable);
        assert_eq!(
            record.policy_evaluations[0].rule_id.as_deref(),
            Some("travel-write")
        );
        for forbidden in ["arguments", "policy_decision", "receipt"] {
            assert!(
                !encoded.contains(forbidden),
                "leaked {forbidden}: {encoded}"
            );
        }
    }

    #[test]
    fn denial_has_no_grant_and_retains_every_policy_layer() {
        let mut layered_context = context();
        layered_context.policy_layer = ToolPermissionPolicyLayer::RuntimePolicy;
        let resolution = ToolPermissionResolution::terminal(
            ToolPermissionOutcome::Denied,
            ToolPermissionDecider::ManagedPolicy,
        )
        .with_host_policy_evaluations(vec![ToolPermissionPolicyEvidence {
            layer: ToolPermissionPolicyLayer::ManagedPolicy,
            outcome: ToolPermissionPolicyOutcome::Denied,
            rule_id: Some("org-no-process".to_string()),
            risk_labels: Vec::new(),
        }])
        .unwrap();
        let record = ToolPermissionActivityRecord::from_policy_facts(
            "process.exec",
            policy(ToolPermissionPolicyOutcome::ApprovalRequired),
            layered_context,
            resolution,
        )
        .unwrap();

        assert_eq!(
            record.policy_evaluations[0].outcome,
            ToolPermissionPolicyOutcome::ApprovalRequired
        );
        assert_eq!(record.policy_evaluations.len(), 2);
        assert_eq!(
            record.policy_evaluations[1].layer,
            ToolPermissionPolicyLayer::ManagedPolicy
        );
        assert_eq!(
            record.policy_evaluations[1].outcome,
            ToolPermissionPolicyOutcome::Denied
        );
        assert!(record.grant.is_none());
    }

    #[test]
    fn malformed_identifiers_and_incoherent_grants_fail_closed() {
        assert_eq!(
            ToolPermissionActivityRecord::from_policy_facts(
                "process.exec\nsecret",
                policy(ToolPermissionPolicyOutcome::ApprovalRequired),
                context(),
                ToolPermissionResolution::terminal(
                    ToolPermissionOutcome::Cancelled,
                    ToolPermissionDecider::Person,
                ),
            ),
            Err(ToolPermissionActivityError::InvalidToolName)
        );

        assert_eq!(
            ToolPermissionActivityRecord::from_policy_facts(
                "process.exec",
                policy(ToolPermissionPolicyOutcome::ApprovalRequired),
                context(),
                ToolPermissionResolution {
                    outcome: ToolPermissionOutcome::Approved,
                    decider: ToolPermissionDecider::Person,
                    grant_scope: None,
                    policy_evaluations: Vec::new(),
                },
            ),
            Err(ToolPermissionActivityError::InvalidResolution)
        );
    }

    #[test]
    fn unavailable_and_session_grant_semantics_are_explicit() {
        let unavailable = ToolPermissionActivityRecord::from_policy_facts(
            "calendar.create_hold",
            policy(ToolPermissionPolicyOutcome::ApprovalRequired),
            context(),
            ToolPermissionResolution::terminal(
                ToolPermissionOutcome::TimedOut,
                ToolPermissionDecider::HostUnavailable,
            ),
        )
        .unwrap();
        assert_eq!(
            unavailable.policy_evaluations[0].outcome,
            ToolPermissionPolicyOutcome::ApprovalRequired
        );

        let session_resolution = ToolPermissionResolution::approved(
            ToolPermissionDecider::RememberedRule,
            ToolPermissionGrantScope::Session,
        )
        .with_host_policy_evaluations(vec![ToolPermissionPolicyEvidence {
            layer: ToolPermissionPolicyLayer::RememberedRule,
            outcome: ToolPermissionPolicyOutcome::Allowed,
            rule_id: Some("remember-travel".to_string()),
            risk_labels: Vec::new(),
        }])
        .unwrap();
        let session = ToolPermissionActivityRecord::from_policy_facts(
            "calendar.create_hold",
            policy(ToolPermissionPolicyOutcome::ApprovalRequired),
            context(),
            session_resolution,
        )
        .unwrap();
        assert_eq!(
            session.grant.unwrap().expires,
            ToolPermissionGrantExpiry::SessionEnd
        );
    }
}
