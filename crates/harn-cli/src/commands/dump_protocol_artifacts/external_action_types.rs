//! Complete value-free external-action activity DTO projections.

pub(super) fn append_rust_external_action_types(out: &mut String) {
    out.push_str(RUST_TYPES);
}

pub(super) fn append_swift_external_action_types(out: &mut String) {
    out.push_str(SWIFT_TYPES);
}

pub(super) fn append_typescript_external_action_types(out: &mut String) {
    out.push_str(TYPESCRIPT_TYPES);
}

const RUST_TYPES: &str = r#"#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionActor {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionMoney {
    pub currency: String,
    pub amount_minor: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionDisclosureReceipt {
    pub recipient: String,
    pub purpose: String,
    pub field_classes: Vec<HarnExternalActionProtectedFieldClass>,
    pub source: HarnExternalActionDisclosureSource,
    pub authentication_assurance: HarnExternalActionAuthenticationAssurance,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionError {
    pub kind: HarnExternalActionErrorKind,
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionReceiptReconciliation {
    pub attempt_id: String,
    pub previous_receipt_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionRetryLink {
    pub schema: String,
    pub previous_action_id: String,
    pub previous_receipt_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionReceipt {
    pub schema: String,
    pub id: String,
    pub action_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_fingerprint: Option<String>,
    pub intent_fingerprint: String,
    pub idempotency_key: String,
    pub provider: String,
    pub capability: String,
    pub operation: String,
    pub environment: HarnExternalActionEnvironment,
    pub adapter_id: String,
    pub outcome: HarnExternalActionOutcome,
    pub status: HarnExternalActionReceiptStatus,
    pub next_action: HarnExternalActionNextAction,
    pub dispatch_attempted: bool,
    pub recorded_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_action_id: Option<String>,
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<HarnExternalActionError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation: Option<HarnExternalActionReceiptReconciliation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosure: Option<HarnExternalActionDisclosureReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<HarnExternalActionRetryLink>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionPolicyEvaluation {
    pub layer: HarnExternalActionPolicyLayer,
    pub outcome: HarnExternalActionPolicyEvaluationOutcome,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionDecision {
    pub outcome: HarnExternalActionDecisionOutcome,
    pub decider: HarnExternalActionDecider,
    pub decided_at_ms: i64,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<HarnExternalActionActor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionAuthorizationRecord {
    pub method: HarnExternalActionAuthorizationMethod,
    pub authentication_assurance: HarnExternalActionAuthenticationAssurance,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionRequester {
    pub actor: HarnExternalActionActor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionDispatchRecord {
    pub attempted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionReconciliationRecord {
    pub attempted: bool,
    pub status: HarnExternalActionReconciliationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_receipt_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnExternalActionActivityRecord {
    pub schema: String,
    pub kind: HarnActivityKind,
    pub id: String,
    pub action_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_fingerprint: Option<String>,
    pub intent_fingerprint: String,
    pub provider: String,
    pub capability: String,
    pub operation: String,
    pub environment: HarnExternalActionEnvironment,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_spend: Option<HarnExternalActionMoney>,
    pub status: HarnExternalActionActivityStatus,
    pub updated_at_ms: i64,
    pub requester: HarnExternalActionRequester,
    pub policy_evaluations: Vec<HarnExternalActionPolicyEvaluation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<HarnExternalActionDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<HarnExternalActionAuthorizationRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosure: Option<HarnExternalActionDisclosureReceipt>,
    pub dispatch: HarnExternalActionDispatchRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation: Option<HarnExternalActionReconciliationRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<HarnExternalActionReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<HarnExternalActionRetryLink>,
}

"#;

const TYPESCRIPT_TYPES: &str = r#"export interface HarnExternalActionActor {
  kind: string
  id: string
}

export interface HarnExternalActionMoney {
  currency: string
  amount_minor: number
}

export interface HarnExternalActionDisclosureReceipt {
  recipient: string
  purpose: string
  field_classes: HarnExternalActionProtectedFieldClass[]
  source: HarnExternalActionDisclosureSource
  authentication_assurance: HarnExternalActionAuthenticationAssurance
}

export interface HarnExternalActionError {
  kind: HarnExternalActionErrorKind
  code: string
  message: string
  retryable: boolean
}

export interface HarnExternalActionReceiptReconciliation {
  attempt_id: string
  previous_receipt_id: string
}

export interface HarnExternalActionRetryLink {
  schema: "harn.external_action_retry_link.v1"
  previous_action_id: string
  previous_receipt_id: string
}

export interface HarnExternalActionReceipt {
  schema: "harn.external_action_receipt.v1"
  id: string
  action_id: string
  effect_fingerprint?: string
  intent_fingerprint: string
  idempotency_key: string
  provider: string
  capability: string
  operation: string
  environment: HarnExternalActionEnvironment
  adapter_id: string
  outcome: HarnExternalActionOutcome
  status: HarnExternalActionReceiptStatus
  next_action: HarnExternalActionNextAction
  dispatch_attempted: boolean
  recorded_at_ms: number
  provider_action_id?: string
  evidence_refs: string[]
  error?: HarnExternalActionError
  reconciliation?: HarnExternalActionReceiptReconciliation
  disclosure?: HarnExternalActionDisclosureReceipt
  retry?: HarnExternalActionRetryLink
}

export interface HarnExternalActionPolicyEvaluation {
  layer: HarnExternalActionPolicyLayer
  outcome: HarnExternalActionPolicyEvaluationOutcome
  reason_code: string
  policy_id?: string
}

export interface HarnExternalActionDecision {
  outcome: HarnExternalActionDecisionOutcome
  decider: HarnExternalActionDecider
  decided_at_ms: number
  reason_code: string
  actor?: HarnExternalActionActor
}

export interface HarnExternalActionAuthorizationRecord {
  method: HarnExternalActionAuthorizationMethod
  authentication_assurance: HarnExternalActionAuthenticationAssurance
  issued_at_ms: number
  expires_at_ms: number
}

export interface HarnExternalActionRequester {
  actor: HarnExternalActionActor
  agent_id?: string
  model_provider?: string
  model_id?: string
  session_id?: string
  run_id?: string
}

export interface HarnExternalActionDispatchRecord {
  attempted: boolean
  adapter_id?: string
}

export interface HarnExternalActionReconciliationRecord {
  attempted: boolean
  status: HarnExternalActionReconciliationStatus
  attempt_id?: string
  previous_receipt_id?: string
}

export interface HarnExternalActionActivityRecord {
  schema: "harn.external_action_activity.v1"
  kind: "external_action"
  id: string
  action_id: string
  effect_fingerprint?: string
  intent_fingerprint: string
  provider: string
  capability: string
  operation: string
  environment: HarnExternalActionEnvironment
  summary: string
  external_spend?: HarnExternalActionMoney
  status: HarnExternalActionActivityStatus
  updated_at_ms: number
  requester: HarnExternalActionRequester
  policy_evaluations: HarnExternalActionPolicyEvaluation[]
  decision?: HarnExternalActionDecision
  authorization?: HarnExternalActionAuthorizationRecord
  disclosure?: HarnExternalActionDisclosureReceipt
  dispatch: HarnExternalActionDispatchRecord
  reconciliation?: HarnExternalActionReconciliationRecord
  receipt?: HarnExternalActionReceipt
  retry?: HarnExternalActionRetryLink
}

"#;

const SWIFT_TYPES: &str = r#"public struct HarnExternalActionActor: Codable, Sendable, Equatable {
    public let kind: String
    public let id: String
}

public struct HarnExternalActionMoney: Codable, Sendable, Equatable {
    public let currency: String
    public let amountMinor: Int64

    enum CodingKeys: String, CodingKey {
        case currency
        case amountMinor = "amount_minor"
    }
}

public struct HarnExternalActionDisclosureReceipt: Codable, Sendable, Equatable {
    public let recipient: String
    public let purpose: String
    public let fieldClasses: [HarnExternalActionProtectedFieldClass]
    public let source: HarnExternalActionDisclosureSource
    public let authenticationAssurance: HarnExternalActionAuthenticationAssurance

    enum CodingKeys: String, CodingKey {
        case recipient, purpose, source
        case fieldClasses = "field_classes"
        case authenticationAssurance = "authentication_assurance"
    }
}

public struct HarnExternalActionError: Codable, Sendable, Equatable {
    public let kind: HarnExternalActionErrorKind
    public let code: String
    public let message: String
    public let retryable: Bool
}

public struct HarnExternalActionReceiptReconciliation: Codable, Sendable, Equatable {
    public let attemptId: String
    public let previousReceiptId: String

    enum CodingKeys: String, CodingKey {
        case attemptId = "attempt_id"
        case previousReceiptId = "previous_receipt_id"
    }
}

public struct HarnExternalActionRetryLink: Codable, Sendable, Equatable {
    public let schema: String
    public let previousActionId: String
    public let previousReceiptId: String

    enum CodingKeys: String, CodingKey {
        case schema
        case previousActionId = "previous_action_id"
        case previousReceiptId = "previous_receipt_id"
    }
}

public struct HarnExternalActionReceipt: Codable, Sendable, Equatable {
    public let schema: String
    public let id: String
    public let actionId: String
    public let effectFingerprint: String?
    public let intentFingerprint: String
    public let idempotencyKey: String
    public let provider: String
    public let capability: String
    public let operation: String
    public let environment: HarnExternalActionEnvironment
    public let adapterId: String
    public let outcome: HarnExternalActionOutcome
    public let status: HarnExternalActionReceiptStatus
    public let nextAction: HarnExternalActionNextAction
    public let dispatchAttempted: Bool
    public let recordedAtMs: Int64
    public let providerActionId: String?
    public let evidenceRefs: [String]
    public let error: HarnExternalActionError?
    public let reconciliation: HarnExternalActionReceiptReconciliation?
    public let disclosure: HarnExternalActionDisclosureReceipt?
    public let retry: HarnExternalActionRetryLink?

    enum CodingKeys: String, CodingKey {
        case schema, id, provider, capability, operation, environment, outcome, status, error
        case reconciliation, disclosure, retry
        case actionId = "action_id"
        case effectFingerprint = "effect_fingerprint"
        case intentFingerprint = "intent_fingerprint"
        case idempotencyKey = "idempotency_key"
        case adapterId = "adapter_id"
        case nextAction = "next_action"
        case dispatchAttempted = "dispatch_attempted"
        case recordedAtMs = "recorded_at_ms"
        case providerActionId = "provider_action_id"
        case evidenceRefs = "evidence_refs"
    }
}

public struct HarnExternalActionPolicyEvaluation: Codable, Sendable, Equatable {
    public let layer: HarnExternalActionPolicyLayer
    public let outcome: HarnExternalActionPolicyEvaluationOutcome
    public let reasonCode: String
    public let policyId: String?

    enum CodingKeys: String, CodingKey {
        case layer, outcome
        case reasonCode = "reason_code"
        case policyId = "policy_id"
    }
}

public struct HarnExternalActionDecision: Codable, Sendable, Equatable {
    public let outcome: HarnExternalActionDecisionOutcome
    public let decider: HarnExternalActionDecider
    public let decidedAtMs: Int64
    public let reasonCode: String
    public let actor: HarnExternalActionActor?

    enum CodingKeys: String, CodingKey {
        case outcome, decider, actor
        case decidedAtMs = "decided_at_ms"
        case reasonCode = "reason_code"
    }
}

public struct HarnExternalActionAuthorizationRecord: Codable, Sendable, Equatable {
    public let method: HarnExternalActionAuthorizationMethod
    public let authenticationAssurance: HarnExternalActionAuthenticationAssurance
    public let issuedAtMs: Int64
    public let expiresAtMs: Int64

    enum CodingKeys: String, CodingKey {
        case method
        case authenticationAssurance = "authentication_assurance"
        case issuedAtMs = "issued_at_ms"
        case expiresAtMs = "expires_at_ms"
    }
}

public struct HarnExternalActionRequester: Codable, Sendable, Equatable {
    public let actor: HarnExternalActionActor
    public let agentId: String?
    public let modelProvider: String?
    public let modelId: String?
    public let sessionId: String?
    public let runId: String?

    enum CodingKeys: String, CodingKey {
        case actor
        case agentId = "agent_id"
        case modelProvider = "model_provider"
        case modelId = "model_id"
        case sessionId = "session_id"
        case runId = "run_id"
    }
}

public struct HarnExternalActionDispatchRecord: Codable, Sendable, Equatable {
    public let attempted: Bool
    public let adapterId: String?

    enum CodingKeys: String, CodingKey {
        case attempted
        case adapterId = "adapter_id"
    }
}

public struct HarnExternalActionReconciliationRecord: Codable, Sendable, Equatable {
    public let attempted: Bool
    public let status: HarnExternalActionReconciliationStatus
    public let attemptId: String?
    public let previousReceiptId: String?

    enum CodingKeys: String, CodingKey {
        case attempted, status
        case attemptId = "attempt_id"
        case previousReceiptId = "previous_receipt_id"
    }
}

public struct HarnExternalActionActivityRecord: Codable, Sendable, Equatable {
    public let schema: String
    public let kind: HarnActivityKind
    public let id: String
    public let actionId: String
    public let effectFingerprint: String?
    public let intentFingerprint: String
    public let provider: String
    public let capability: String
    public let operation: String
    public let environment: HarnExternalActionEnvironment
    public let summary: String
    public let externalSpend: HarnExternalActionMoney?
    public let status: HarnExternalActionActivityStatus
    public let updatedAtMs: Int64
    public let requester: HarnExternalActionRequester
    public let policyEvaluations: [HarnExternalActionPolicyEvaluation]
    public let decision: HarnExternalActionDecision?
    public let authorization: HarnExternalActionAuthorizationRecord?
    public let disclosure: HarnExternalActionDisclosureReceipt?
    public let dispatch: HarnExternalActionDispatchRecord
    public let reconciliation: HarnExternalActionReconciliationRecord?
    public let receipt: HarnExternalActionReceipt?
    public let retry: HarnExternalActionRetryLink?

    enum CodingKeys: String, CodingKey {
        case schema, kind, id, provider, capability, operation, environment, summary, status
        case requester, decision, authorization, disclosure, dispatch, reconciliation, receipt, retry
        case actionId = "action_id"
        case effectFingerprint = "effect_fingerprint"
        case intentFingerprint = "intent_fingerprint"
        case externalSpend = "external_spend"
        case updatedAtMs = "updated_at_ms"
        case policyEvaluations = "policy_evaluations"
    }
}

"#;
