use super::super::activity::ActivityVocabulary;
use super::swift_enum;

pub(super) fn append_activity_types(out: &mut String, activity: &ActivityVocabulary) {
    out.push_str(&swift_enum("HarnActivityKind", &activity.kinds));
    out.push_str(&swift_enum(
        "HarnToolPermissionOutcome",
        &activity.permission_outcomes,
    ));
    out.push_str(&swift_enum(
        "HarnToolPermissionDecider",
        &activity.permission_deciders,
    ));
    out.push_str(&swift_enum(
        "HarnToolPermissionPolicyLayer",
        &activity.permission_policy_layers,
    ));
    out.push_str(&swift_enum(
        "HarnToolPermissionPolicyOutcome",
        &activity.permission_policy_outcomes,
    ));
    out.push_str(&swift_enum(
        "HarnToolPermissionGrantScope",
        &activity.permission_grant_scopes,
    ));
    out.push_str(&swift_enum(
        "HarnToolPermissionGrantExpiry",
        &activity.permission_grant_expiries,
    ));
    out.push_str(ACTIVITY_STRUCTS);
}

const ACTIVITY_STRUCTS: &str =
    "public struct HarnToolPermissionScope: Codable, Sendable, Equatable {\n\
     \x20   public let toolKind: HarnACPToolKind\n\
     \x20   public let sideEffect: HarnSideEffectLevel\n\
     \x20   public let capabilities: [String]\n\n\
     \x20   enum CodingKeys: String, CodingKey {\n\
     \x20       case capabilities\n\
     \x20       case toolKind = \"tool_kind\"\n\
     \x20       case sideEffect = \"side_effect\"\n\
     \x20   }\n\
     }\n\n\
     public struct HarnToolPermissionPolicyEvidence: Codable, Sendable, Equatable {\n\
     \x20   public let layer: HarnToolPermissionPolicyLayer\n\
     \x20   public let outcome: HarnToolPermissionPolicyOutcome\n\
     \x20   public let ruleId: String?\n\
     \x20   public let riskLabels: [String]\n\n\
     \x20   enum CodingKeys: String, CodingKey {\n\
     \x20       case layer, outcome\n\
     \x20       case ruleId = \"rule_id\"\n\
     \x20       case riskLabels = \"risk_labels\"\n\
     \x20   }\n\
     }\n\n\
     public struct HarnToolPermissionDecisionMetadata: Codable, Sendable, Equatable {\n\
     \x20   public let schema: String\n\
     \x20   public let outcome: HarnToolPermissionOutcome\n\
     \x20   public let decider: HarnToolPermissionDecider\n\
     \x20   public let policyEvaluations: [HarnToolPermissionPolicyEvidence]\n\
     \x20   public let grantScope: HarnToolPermissionGrantScope?\n\n\
     \x20   enum CodingKeys: String, CodingKey {\n\
     \x20       case schema, outcome, decider\n\
     \x20       case policyEvaluations = \"policy_evaluations\"\n\
     \x20       case grantScope = \"grant_scope\"\n\
     \x20   }\n\
     }\n\n\
     public struct HarnToolPermissionGrantEvidence: Codable, Sendable, Equatable {\n\
     \x20   public let scope: HarnToolPermissionGrantScope\n\
     \x20   public let expires: HarnToolPermissionGrantExpiry\n\
     \x20   public let reusable: Bool\n\
     }\n\n\
     public struct HarnToolPermissionRequester: Codable, Sendable, Equatable {\n\
     \x20   public let sessionId: String\n\
     \x20   public let agentId: String?\n\
     \x20   public let modelProvider: String?\n\
     \x20   public let modelId: String?\n\n\
     \x20   enum CodingKeys: String, CodingKey {\n\
     \x20       case sessionId = \"session_id\"\n\
     \x20       case agentId = \"agent_id\"\n\
     \x20       case modelProvider = \"model_provider\"\n\
     \x20       case modelId = \"model_id\"\n\
     \x20   }\n\
     }\n\n\
     public struct HarnToolPermissionActivityRecord: Codable, Sendable, Equatable {\n\
     \x20   public let schema: String\n\
     \x20   public let kind: HarnActivityKind\n\
     \x20   public let id: String\n\
     \x20   public let requestId: String\n\
     \x20   public let toolName: String\n\
     \x20   public let scope: HarnToolPermissionScope\n\
     \x20   public let outcome: HarnToolPermissionOutcome\n\
     \x20   public let decider: HarnToolPermissionDecider\n\
     \x20   public let policyEvaluations: [HarnToolPermissionPolicyEvidence]\n\
     \x20   public let grant: HarnToolPermissionGrantEvidence?\n\
     \x20   public let requester: HarnToolPermissionRequester\n\
     \x20   public let occurredAtMs: Int64\n\n\
     \x20   enum CodingKeys: String, CodingKey {\n\
     \x20       case schema, kind, id, scope, outcome, decider, grant, requester\n\
     \x20       case requestId = \"request_id\"\n\
     \x20       case toolName = \"tool_name\"\n\
     \x20       case policyEvaluations = \"policy_evaluations\"\n\
     \x20       case occurredAtMs = \"occurred_at_ms\"\n\
     \x20   }\n\
     }\n\n";
