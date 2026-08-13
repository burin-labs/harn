use harn_serve::adapters::acp::{
    ACP_PROMPT_ERROR_DATA_SCHEMA, HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD,
    HARN_PROMPT_RESULT_EXTENSION_FIELDS, HARN_PROVIDER_CATALOG_METHOD,
    HARN_SESSION_UPDATE_EXTENSIONS,
};
use harn_serve::MCP_PROTOCOL_VERSION;
use harn_vm::llm::receipts::{TOOL_CALL_RECEIPT_EXECUTORS, TOOL_CALL_RECEIPT_STATUSES};

use super::activity::ActivityVocabulary;
use super::connector_setup::ConnectorSetupVocabulary;
use super::constants::*;
use super::external_action::ExternalActionVocabulary;
use super::external_action_types::append_typescript_external_action_types;
use super::support::*;
use super::swift::{deprecated_wire_value, deprecation_message, wire_value_property_name};
use super::values::*;

#[cfg(test)]
pub(super) fn generate_typescript() -> String {
    generate_typescript_for_version(
        env!("CARGO_PKG_VERSION"),
        &ExternalActionVocabulary::load_for_tests(),
        &ConnectorSetupVocabulary::load_for_tests(),
        &ActivityVocabulary::load_for_tests(),
    )
}

pub(super) fn generate_typescript_for_version(
    artifact_version: &str,
    external_actions: &ExternalActionVocabulary,
    connector_setup: &ConnectorSetupVocabulary,
    activity: &ActivityVocabulary,
) -> String {
    let mut out = generated_header("harn dump-protocol-artifacts", "typescript");
    out.push_str("export const HARN_PROTOCOL_ARTIFACT_VERSION = ");
    out.push_str(&json_string_literal(artifact_version));
    out.push_str("\n\n");
    out.push_str("export const HARN_TOOL_PERMISSION_DECISION_SCHEMA = \"harn.tool_permission_decision.v1\" as const\n");
    out.push_str("export const HARN_TOOL_PERMISSION_ACTIVITY_SCHEMA = \"harn.tool_permission_activity.v1\" as const\n\n");
    out.push_str("export const HARN_EXTERNAL_ACTION_ACTIVITY_SCHEMA = \"harn.external_action_activity.v1\" as const\n");
    out.push_str("export const HARN_EXTERNAL_ACTION_RECEIPT_SCHEMA = \"harn.external_action_receipt.v1\" as const\n\n");
    for (name, value) in [
        ("MCP_PROTOCOL_VERSION", MCP_PROTOCOL_VERSION),
        (
            "MCP_JSON_SCHEMA_2020_12_DIALECT",
            MCP_JSON_SCHEMA_2020_12_DIALECT,
        ),
    ] {
        out.push_str("export const ");
        out.push_str(name);
        out.push_str(" = ");
        out.push_str(&json_string_literal(value));
        out.push('\n');
    }
    out.push_str(&format!(
        "export const MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR = {{ code: {}, message: {} }} as const\n\n",
        MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE,
        json_string_literal(MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE)
    ));
    out.push_str(&ts_array(
        "ACP_AGENT_METHODS",
        ACP_AGENT_METHODS,
        "ACPAgentMethod",
    ));
    out.push_str(&ts_wire_value_object(
        "ACP_AGENT_METHOD",
        ACP_AGENT_METHODS,
        ACP_DEPRECATED_AGENT_METHODS,
    ));
    out.push_str(&ts_array(
        "ACP_CLIENT_METHODS",
        ACP_CLIENT_METHODS,
        "ACPClientMethod",
    ));
    out.push_str(&ts_array(
        "ACP_AGENT_NOTIFICATIONS",
        ACP_AGENT_NOTIFICATIONS,
        "ACPAgentNotification",
    ));
    let all_session_updates = all_acp_session_updates();
    out.push_str(&ts_array_owned(
        "ACP_SESSION_UPDATES",
        &all_session_updates,
        "ACPSessionUpdate",
    ));
    out.push_str(&ts_array(
        "HARN_ACP_SESSION_UPDATE_EXTENSIONS",
        HARN_SESSION_UPDATE_EXTENSIONS,
        "HarnACPSessionUpdateExtension",
    ));
    out.push_str("export const HARN_AGENT_EVENT_METHOD = ");
    out.push_str(&json_string_literal(HARN_AGENT_EVENT_METHOD));
    out.push('\n');
    out.push_str("export const HARN_PROVIDER_CATALOG_METHOD = ");
    out.push_str(&json_string_literal(HARN_PROVIDER_CATALOG_METHOD));
    out.push('\n');
    out.push_str("export const ACP_PROMPT_ERROR_DATA_SCHEMA = ");
    out.push_str(&json_string_literal(ACP_PROMPT_ERROR_DATA_SCHEMA));
    out.push_str(" as const\n");
    out.push_str(&ts_array_owned(
        "AGENT_TERMINAL_CLASSES",
        &agent_terminal_class_values(),
        "AgentTerminalClass",
    ));
    out.push_str(&ts_array_owned(
        "AGENT_TERMINAL_KINDS",
        &agent_terminal_kind_values(),
        "AgentTerminalKind",
    ));
    out.push_str(&ts_array_owned(
        "AGENT_TERMINAL_OWNERS",
        &agent_terminal_owner_values(),
        "AgentTerminalOwner",
    ));
    out.push_str(&ts_array(
        "HARN_PROMPT_RESULT_EXTENSION_FIELDS",
        HARN_PROMPT_RESULT_EXTENSION_FIELDS,
        "HarnPromptResultExtensionField",
    ));
    out.push_str(&ts_array(
        "HARN_AGENT_EVENT_KINDS",
        HARN_AGENT_EVENT_KINDS,
        "HarnAgentEventKind",
    ));
    out.push_str(&ts_array(
        "ACP_CONTENT_BLOCK_TYPES",
        ACP_CONTENT_BLOCK_TYPES,
        "ACPContentBlockType",
    ));
    out.push_str(&ts_array_owned(
        "ACP_TOOL_KINDS",
        &tool_kind_values(),
        "ACPToolKind",
    ));
    out.push_str(&ts_array_owned(
        "ACP_TOOL_CALL_STATUSES",
        &tool_call_status_values(),
        "ACPToolCallStatus",
    ));
    out.push_str(&ts_array_owned(
        "HARN_TOOL_CALL_ERROR_CATEGORIES",
        &tool_call_error_category_values(),
        "HarnToolCallErrorCategory",
    ));
    out.push_str(&ts_array_owned(
        "HARN_TOOL_MUTATION_STATUSES",
        &tool_mutation_status_values(),
        "HarnToolMutationStatus",
    ));
    out.push_str(&ts_array_owned(
        "HARN_SIDE_EFFECT_LEVELS",
        &side_effect_level_values(),
        "HarnSideEffectLevel",
    ));
    out.push_str(&ts_array_owned(
        "HARN_WORKER_STATUSES",
        &worker_status_values(),
        "HarnWorkerStatus",
    ));
    out.push_str(&ts_array_owned(
        "HARN_AGENT_LIFECYCLE_STATES",
        &agent_lifecycle_state_values(),
        "HarnAgentLifecycleState",
    ));
    out.push_str(&ts_array_owned(
        "HARN_AGENT_LIFECYCLE_EVENTS",
        &agent_lifecycle_event_values(),
        "HarnAgentLifecycleEvent",
    ));
    out.push_str(&ts_array(
        "HARN_TOOL_CALL_RECEIPT_STATUSES",
        TOOL_CALL_RECEIPT_STATUSES,
        "HarnToolCallReceiptStatus",
    ));
    out.push_str(&ts_array(
        "HARN_TOOL_CALL_RECEIPT_EXECUTORS",
        TOOL_CALL_RECEIPT_EXECUTORS,
        "HarnToolCallReceiptExecutor",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_OUTCOMES",
        &external_actions.outcomes,
        "HarnExternalActionOutcome",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_RECEIPT_STATUSES",
        &external_actions.receipt_statuses,
        "HarnExternalActionReceiptStatus",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_NEXT_ACTIONS",
        &external_actions.next_actions,
        "HarnExternalActionNextAction",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_ENVIRONMENTS",
        &external_actions.environments,
        "HarnExternalActionEnvironment",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_AUTHORIZATION_METHODS",
        &external_actions.authorization_methods,
        "HarnExternalActionAuthorizationMethod",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_AUTHENTICATION_ASSURANCES",
        &external_actions.authentication_assurances,
        "HarnExternalActionAuthenticationAssurance",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_DISCLOSURE_SOURCES",
        &external_actions.disclosure_sources,
        "HarnExternalActionDisclosureSource",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_ERROR_KINDS",
        &external_actions.error_kinds,
        "HarnExternalActionErrorKind",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_PROTECTED_FIELD_CLASSES",
        &external_actions.protected_field_classes,
        "HarnExternalActionProtectedFieldClass",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_PASSENGER_GENDERS",
        &external_actions.passenger_genders,
        "HarnExternalActionPassengerGender",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_ACTIVITY_STATUSES",
        &external_actions.activity_statuses,
        "HarnExternalActionActivityStatus",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_POLICY_LAYERS",
        &external_actions.policy_layers,
        "HarnExternalActionPolicyLayer",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_POLICY_EVALUATION_OUTCOMES",
        &external_actions.policy_evaluation_outcomes,
        "HarnExternalActionPolicyEvaluationOutcome",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_DECISION_OUTCOMES",
        &external_actions.decision_outcomes,
        "HarnExternalActionDecisionOutcome",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_DECIDERS",
        &external_actions.deciders,
        "HarnExternalActionDecider",
    ));
    out.push_str(&ts_array_owned(
        "EXTERNAL_ACTION_RECONCILIATION_STATUSES",
        &external_actions.reconciliation_statuses,
        "HarnExternalActionReconciliationStatus",
    ));
    append_typescript_external_action_types(&mut out);
    out.push_str(&ts_array_owned(
        "ACTIVITY_KINDS",
        &activity.kinds,
        "HarnActivityKind",
    ));
    out.push_str(&ts_array_owned(
        "TOOL_PERMISSION_OUTCOMES",
        &activity.permission_outcomes,
        "HarnToolPermissionOutcome",
    ));
    out.push_str(&ts_array_owned(
        "TOOL_PERMISSION_DECIDERS",
        &activity.permission_deciders,
        "HarnToolPermissionDecider",
    ));
    out.push_str(&ts_array_owned(
        "TOOL_PERMISSION_POLICY_LAYERS",
        &activity.permission_policy_layers,
        "HarnToolPermissionPolicyLayer",
    ));
    out.push_str(&ts_array_owned(
        "TOOL_PERMISSION_POLICY_OUTCOMES",
        &activity.permission_policy_outcomes,
        "HarnToolPermissionPolicyOutcome",
    ));
    out.push_str(&ts_array_owned(
        "TOOL_PERMISSION_GRANT_SCOPES",
        &activity.permission_grant_scopes,
        "HarnToolPermissionGrantScope",
    ));
    out.push_str(&ts_array_owned(
        "TOOL_PERMISSION_GRANT_EXPIRIES",
        &activity.permission_grant_expiries,
        "HarnToolPermissionGrantExpiry",
    ));
    out.push_str(
        "export interface HarnToolPermissionScope {\n\
         \x20 tool_kind: ACPToolKind\n\
         \x20 side_effect: HarnSideEffectLevel\n\
         \x20 capabilities: string[]\n\
         }\n\n\
         export interface HarnToolPermissionPolicyEvidence {\n\
         \x20 layer: HarnToolPermissionPolicyLayer\n\
         \x20 outcome: HarnToolPermissionPolicyOutcome\n\
         \x20 rule_id?: string\n\
         \x20 risk_labels: string[]\n\
         }\n\n\
         export interface HarnToolPermissionDecisionMetadata {\n\
         \x20 schema: \"harn.tool_permission_decision.v1\"\n\
         \x20 outcome: HarnToolPermissionOutcome\n\
         \x20 decider: HarnToolPermissionDecider\n\
         \x20 policy_evaluations: HarnToolPermissionPolicyEvidence[]\n\
         \x20 grant_scope?: HarnToolPermissionGrantScope\n\
         }\n\n\
         export interface HarnToolPermissionGrantEvidence {\n\
         \x20 scope: HarnToolPermissionGrantScope\n\
         \x20 expires: HarnToolPermissionGrantExpiry\n\
         \x20 reusable: false\n\
         }\n\n\
         export interface HarnToolPermissionRequester {\n\
         \x20 session_id: string\n\
         \x20 agent_id?: string\n\
         \x20 model_provider?: string\n\
         \x20 model_id?: string\n\
         }\n\n\
         export interface HarnToolPermissionActivityRecord {\n\
         \x20 schema: \"harn.tool_permission_activity.v1\"\n\
         \x20 kind: \"tool_permission\"\n\
         \x20 id: string\n\
         \x20 request_id: string\n\
         \x20 tool_name: string\n\
         \x20 scope: HarnToolPermissionScope\n\
         \x20 outcome: HarnToolPermissionOutcome\n\
         \x20 decider: HarnToolPermissionDecider\n\
         \x20 policy_evaluations: HarnToolPermissionPolicyEvidence[]\n\
         \x20 grant?: HarnToolPermissionGrantEvidence\n\
         \x20 requester: HarnToolPermissionRequester\n\
         \x20 occurred_at_ms: number\n\
         }\n\n",
    );
    out.push_str(&ts_array_owned(
        "CONNECTOR_SETUP_STAGES",
        &connector_setup.stages,
        "HarnConnectorSetupStage",
    ));
    out.push_str(&ts_array_owned(
        "CONNECTOR_SETUP_STATUSES",
        &connector_setup.statuses,
        "HarnConnectorSetupStatus",
    ));
    out.push_str(&ts_array_owned(
        "CONNECTOR_SETUP_INTERACTIONS",
        &connector_setup.interactions,
        "HarnConnectorSetupInteraction",
    ));
    out.push_str(&ts_array_owned(
        "CONNECTOR_SETUP_CONFIGURATION_FIELDS",
        &connector_setup.configuration_fields,
        "HarnConnectorSetupConfigurationField",
    ));
    out.push_str(&ts_array_owned(
        "CONNECTOR_SETUP_ERROR_CODES",
        &connector_setup.error_codes,
        "HarnConnectorSetupErrorCode",
    ));
    out.push_str(
        "export interface HarnConnectorSetupEvent {\n\
         \x20 schema: \"harn.connector_setup.event.v1\"\n\
         \x20 sequence: number\n\
         \x20 connector: string\n\
         \x20 stage: HarnConnectorSetupStage\n\
         \x20 status: HarnConnectorSetupStatus\n\
         \x20 interaction: HarnConnectorSetupInteraction\n\
         \x20 message: string\n\
         \x20 error_code?: HarnConnectorSetupErrorCode\n\
         \x20 recovery?: string\n\
         }\n\n",
    );
    out.push_str(&ts_array(
        "A2A_TASK_STATES",
        A2A_TASK_STATES,
        "A2ATaskState",
    ));
    out.push_str(&ts_array(
        "A2A_TASK_EVENT_TYPES",
        A2A_TASK_EVENT_TYPES,
        "A2ATaskEventType",
    ));
    out.push_str(&ts_array(
        "MCP_PROTOCOL_VERSIONS",
        MCP_PROTOCOL_VERSIONS,
        "MCPProtocolVersion",
    ));
    out.push_str(&ts_array("MCP_METHODS", MCP_METHODS, "MCPMethod"));
    out.push_str(&ts_array(
        "MCP_REQUIRED_METADATA_KEYS",
        MCP_REQUIRED_METADATA_KEYS,
        "MCPRequiredMetadataKey",
    ));
    out.push_str(&ts_array(
        "MCP_METADATA_KEYS",
        MCP_METADATA_KEYS,
        "MCPMetadataKey",
    ));
    out.push_str(&ts_array(
        "MCP_STANDARD_HTTP_HEADERS",
        MCP_STANDARD_HTTP_HEADERS,
        "MCPStandardHTTPHeader",
    ));
    out.push_str(&ts_array(
        "MCP_CACHE_RESULT_FIELDS",
        MCP_CACHE_RESULT_FIELDS,
        "MCPCacheResultField",
    ));
    out.push_str(&ts_array(
        "MCP_CACHE_SCOPES",
        MCP_CACHE_SCOPES,
        "MCPCacheScope",
    ));
    out.push_str(&ts_array(
        "MCP_RESULT_TYPES",
        MCP_RESULT_TYPES,
        "MCPResultType",
    ));
    out.push_str(&ts_array(
        "MCP_LOGGING_LEVELS",
        MCP_LOGGING_LEVELS,
        "MCPLoggingLevel",
    ));
    out.push_str(&ts_array(
        "MCP_OAUTH_CLIENT_REGISTRATION_MODES",
        MCP_OAUTH_CLIENT_REGISTRATION_MODES,
        "MCPOAuthClientRegistrationMode",
    ));
    out.push_str(&ts_array(
        "MCP_OAUTH_AUTH_MODES",
        MCP_OAUTH_AUTH_MODES,
        "MCPOAuthAuthMode",
    ));
    out.push_str(&ts_array(
        "MCP_OAUTH_APPLICATION_TYPES",
        MCP_OAUTH_APPLICATION_TYPES,
        "MCPOAuthApplicationType",
    ));

    out.push_str(
        r#"
export type ACPObject = { [key: string]: ACPValue }
export type ACPValue = null | boolean | number | string | ACPValue[] | ACPObject
export type JsonRpcId = number | string | null

export type HarnPlanStepStatus = "pending" | "in_progress" | "completed" | "blocked" | "cancelled"
export type HarnPlanApprovalState = "unrequested" | "requested" | "approved" | "rejected"
export type HarnPlanCommentState = "open" | "addressed" | "resolved" | "reopened"

export interface HarnPlanAuthor {
  id: string
  display_name?: string
}

export interface HarnPlanSource {
  kind: string
  uri?: string
}

export interface HarnPlanStep {
  id: string
  content: string
  status: HarnPlanStepStatus
  priority?: ACPValue
}

export interface HarnPlanApproval {
  state: HarnPlanApprovalState
  request_id?: string
  reviewer?: string
  reviewers?: string[]
  approved_at?: string
  reason?: string
}

export interface HarnPlanArtifact {
  _type: "plan_artifact"
  schema_version: "harn.plan.v1"
  id: string
  tool: string
  title: string
  summary: string
  steps: HarnPlanStep[]
  assumptions: string[]
  open_questions: string[]
  verification_commands: string[]
  approval: HarnPlanApproval
}

export interface HarnPlanRevision {
  revision_id: string
  parent_revision_id?: string
  markdown: string
  plan: HarnPlanArtifact
  author: HarnPlanAuthor
  source: HarnPlanSource
  created_at: string
  operation:
    | { kind: "create" | "edit"; event_id: string }
    | { kind: "comment"; event_id: string; comment_id: string }
    | { kind: "comment_state"; event_id: string; comment_id: string; state: HarnPlanCommentState }
}

export interface HarnPlanCommentAnchor {
  step_id?: string
  quoted_text?: string
  range?: { start: number; end: number }
}

export interface HarnPlanComment {
  comment_id: string
  anchor: HarnPlanCommentAnchor
  body: string
  state: HarnPlanCommentState
  author: HarnPlanAuthor
  created_at: string
  updated_at: string
}

export interface HarnPlanCommentResolutionReceipt {
  receipt_id: string
  comment_id: string
  input_revision_id: string
  output_revision_id: string
  agent_run_id: string
  event_id: string
  explanation?: string
  created_at: string
}

export interface HarnPlanDocument {
  _type: "plan_document"
  schema_version: "harn.plan_document.v1"
  document_id: string
  current_revision: HarnPlanRevision
  comments: HarnPlanComment[]
  resolution_receipts: HarnPlanCommentResolutionReceipt[]
  created_at: string
  updated_at: string
}

export interface ACPRequest {
  jsonrpc: "2.0"
  id: Exclude<JsonRpcId, null>
  method: string
  params?: ACPValue
}

export interface ACPResponse {
  jsonrpc: "2.0"
  id: JsonRpcId
  result?: ACPValue
  error?: ACPError
}

export interface ACPError {
  code: number
  message: string
  data?: ACPValue
}

export interface HarnACPPromptErrorData {
  schema: typeof ACP_PROMPT_ERROR_DATA_SCHEMA
  terminalClass: AgentTerminalClass
  category?: string
  kind?: string
  reason?: string
  code?: string
  retryable?: boolean
  retryAfterMs?: number
  provider?: string
  model?: string
}

export interface HarnAgentTerminalOutcome {
  kind: AgentTerminalKind
  reason: string
  owner: AgentTerminalOwner
}

export interface HarnACPPromptResult {
  stopReason: string
  _meta?: {
    harn: {
      terminal?: HarnAgentTerminalOutcome
    }
  }
}

export interface ACPNotification {
  jsonrpc: "2.0"
  method: string
  params?: ACPValue
}

export type ACPMessage = ACPRequest | ACPResponse | ACPNotification

export type HarnHostInjectionKind = "host_tool_result" | "host_attachment"
export type HarnHostInjectionDelivery =
  | "turn_boundary"
  | "immediate"
  | "after_next_tool_call"

export interface HarnHostInjectionProvenance {
  initiator: string
  source: string
  host?: string
  ts_ms: number
}

export interface HarnHostInjectionEvent {
  kind: HarnHostInjectionKind
  delivery?: HarnHostInjectionDelivery
  payload: ACPObject
  provenance: HarnHostInjectionProvenance
}

export interface ACPSessionInjectHostEventParams {
  sessionId: string
  event: HarnHostInjectionEvent
}

export interface ACPExtensionMeta<T extends object = ACPObject> {
  harn?: T
}

export interface ACPContentBlock {
  type: "text" | "resource_link" | "resource" | "image" | "audio" | string
  text?: string
  _meta?: ACPExtensionMeta<ACPObject>
}

export interface ACPToolCallDiff {
  type: "diff"
  path: string
  oldText?: string | null
  newText: string
  _meta?: ACPExtensionMeta<ACPObject>
}

export interface ACPToolCallContentBlock {
  type: "content"
  content: ACPContentBlock
  _meta?: ACPExtensionMeta<ACPObject>
}

export interface ACPToolCallTerminal {
  type: "terminal"
  terminalId: string
  _meta?: ACPExtensionMeta<ACPObject>
}

export type ACPToolCallContent =
  | ACPToolCallDiff
  | ACPToolCallContentBlock
  | ACPToolCallTerminal

export type ACPToolExecutor =
  | "harn_builtin"
  | "host_bridge"
  | "provider_native"
  | { kind: "mcp_server"; serverName: string }

export interface HarnToolLifecycleMeta {
  audit?: ACPValue
  changedPaths?: string[]
  data?: ACPValue
  durationMs?: number
  error?: string
  errorCategory?: HarnToolCallErrorCategory
  executionDurationMs?: number
  executor?: ACPToolExecutor
  mutationStatus?: HarnToolMutationStatus
  parsing?: boolean
  rawInputPartial?: string
}

export interface ToolCallReceipt {
  schema_version: number
  session_id: string
  run_id: string | null
  tool_call_id: string
  tool_name: string
  iteration: number
  turn_index: number | null
  emit_order: number
  reason: string | null
  kind: string | null
  executor: HarnToolCallReceiptExecutor | null
  status: HarnToolCallReceiptStatus
  error_category: string | null
  duration_ms: number
  args_hash: string
  result_hash: string | null
  audit: ACPValue
  emitted_at: string
  model: string | null
  provider: string | null
}

export interface ACPToolCall {
  sessionUpdate: "tool_call"
  toolCallId: string
  title: string
  kind?: ACPToolKind
  status?: ACPToolCallStatus
  content?: ACPToolCallContent[]
  locations?: ACPValue[]
  rawInput?: ACPValue
  rawOutput?: ACPValue
  _meta?: ACPExtensionMeta<HarnToolLifecycleMeta>
}

export interface ACPToolCallUpdate {
  sessionUpdate: "tool_call_update"
  toolCallId: string
  title?: string | null
  kind?: ACPToolKind
  status?: ACPToolCallStatus | null
  content?: ACPToolCallContent[]
  locations?: ACPValue[]
  rawInput?: ACPValue
  rawOutput?: ACPValue
  _meta?: ACPExtensionMeta<HarnToolLifecycleMeta>
}

export interface ACPMessageChunkUpdate {
  sessionUpdate: "agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk"
  content: ACPContentBlock
}

export interface ACPUserMessageUpdate {
  sessionUpdate: "user_message"
  messageId: string
  content: ACPContentBlock[]
}

export interface ACPPlanUpdate {
  sessionUpdate: "plan"
  entries: ACPValue[]
  harnPlanDocument?: HarnPlanDocument
}

export interface ACPSessionTruncatedUpdate {
  sessionUpdate: "session_truncated"
  keptTurnCount: number
  removedTurnCount: number
  newTipTurnId?: string | null
  reason?: string
}

export interface ACPHarnExtensionUpdate {
  sessionUpdate: HarnACPSessionUpdateExtension
  _meta?: ACPExtensionMeta<ACPObject>
}

export type ACPSessionUpdateEnvelope =
  | ACPUserMessageUpdate
  | ACPMessageChunkUpdate
  | ACPToolCall
  | ACPToolCallUpdate
  | ACPPlanUpdate
  | ACPSessionTruncatedUpdate
  | ACPHarnExtensionUpdate

export interface ACPSessionUpdateParams {
  sessionId: string
  update: ACPSessionUpdateEnvelope
}

export interface ACPSessionUpdateNotification {
  jsonrpc: "2.0"
  method: "session/update"
  params: ACPSessionUpdateParams
}

export interface HarnAgentEventNotification {
  jsonrpc: "2.0"
  method: typeof HARN_AGENT_EVENT_METHOD
  params: ACPObject & {
    sessionId: string
    kind: HarnAgentEventKind
  }
}

export interface ACPPermissionToolCall {
  sessionUpdate?: "tool_call_update"
  toolCallId: string
  title?: string
  kind?: ACPToolKind
  content?: ACPToolCallContent[]
  rawInput?: ACPValue
  _meta?: ACPExtensionMeta<ACPObject>
}

export type ACPPermissionOptionKind =
  | "allow_once"
  | "allow_always"
  | "reject_once"
  | "reject_always"

export interface ACPPermissionOption {
  optionId: string
  name: string
  kind: ACPPermissionOptionKind
}

export interface ACPSessionRequestPermissionParams {
  sessionId: string
  toolCall: ACPPermissionToolCall
  options: ACPPermissionOption[]
}

export type ACPPermissionOutcome =
  | { outcome: "selected"; optionId: string }
  | { outcome: "cancelled" }

export interface ACPSessionRequestPermissionResult {
  outcome: ACPPermissionOutcome
  reason?: string
}

export interface ACPPromptCapabilities {
  image?: boolean
  audio?: boolean
  embeddedContext?: boolean
}

export interface ACPAgentCapabilities {
  _meta?: ACPExtensionMeta<{
    schemaCompatibility?: string
    sessionUpdateExtensions?: HarnACPSessionUpdateExtension[]
    toolLifecycleExtensionFields?: string[]
    contentExtensionFields?: string[]
    promptResultExtensionFields?: HarnPromptResultExtensionField[]
    extensionMethods?: Record<string, ACPObject>
    hostCapabilityOperations?: Record<string, string[]>
    extensionContract?: string
  }>
  loadSession?: boolean
  session?: ACPObject
  promptCapabilities?: ACPPromptCapabilities
  mcpCapabilities?: ACPObject
  sessionCapabilities?: ACPObject
}

export interface ACPClientCapabilities {
  fs?: {
    readTextFile?: boolean
    writeTextFile?: boolean
  }
  terminal?: {
    create?: boolean
  }
}

export interface HarnToolArgSchema {
  path_params: string[]
  dependency_key_params: string[]
  dependency_range_params: Array<Record<string, string>>
  arg_aliases: Record<string, string>
  required: string[]
}

export interface HarnToolAnnotations {
  kind: ACPToolKind
  side_effect_level: HarnSideEffectLevel
  arg_schema: HarnToolArgSchema
  capabilities: Record<string, string[]>
  emits_artifacts: boolean
  result_readers: string[]
  inline_result: boolean
}

export interface A2ATaskStatus {
  state: A2ATaskState
  message?: A2AMessage
  timestamp?: string
}

export interface A2ATask {
  id: string
  contextId?: string | null
  status: A2ATaskStatus
  history?: A2AMessage[]
  artifacts?: ACPValue[]
  metadata?: ACPObject
}

export interface A2AMessage {
  id: string
  role: "user" | "agent"
  parts: ACPValue[]
}

export type A2ATaskEvent =
  | { type: "status"; taskId: string; status: A2ATaskStatus }
  | { type: "message" | "worker_update"; taskId: string; message?: A2AMessage }
  | { statusUpdate: { taskId: string; contextId?: string | null; status: A2ATaskStatus } }

export type MCPJsonSchema202012 = ACPObject

export interface MCPImplementation {
  name: string
  version: string
  title?: string
  description?: string
  websiteUrl?: string
}

export interface MCPRequestMeta {
  "io.modelcontextprotocol/protocolVersion": MCPProtocolVersion | string
  "io.modelcontextprotocol/clientInfo": MCPImplementation
  "io.modelcontextprotocol/clientCapabilities": ACPObject
  "io.modelcontextprotocol/logLevel"?: MCPLoggingLevel
  progressToken?: string | number
  traceparent?: string
  tracestate?: string
  baggage?: string
  [key: string]: ACPValue | MCPImplementation | undefined
}

export interface MCPHTTPHeaders {
  "MCP-Protocol-Version": MCPProtocolVersion | string
  "Mcp-Method": MCPMethod | string
  "Mcp-Name"?: string
  [header: string]: string | undefined
}

export interface MCPCacheHints {
  ttlMs: number
  cacheScope: MCPCacheScope
}

export interface MCPDiscoverResult {
  resultType: "complete"
  supportedVersions: (MCPProtocolVersion | string)[]
  capabilities: ACPObject
  ttlMs: number
  cacheScope: MCPCacheScope
  instructions?: string
  _meta?: ACPObject
}

export interface MCPInputRequiredResult {
  resultType: "input_required"
  inputRequests?: Record<string, ACPObject>
  requestState?: string
  _meta?: ACPObject
}

export interface MCPUnsupportedProtocolVersionError {
  jsonrpc: "2.0"
  id?: JsonRpcId
  error: {
    code: typeof MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR.code
    message: typeof MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR.message
    data: {
      requested: string
      supported: (MCPProtocolVersion | string)[]
    }
  }
}

export interface MCPTool {
  name: string
  title?: string
  description?: string
  inputSchema: MCPJsonSchema202012
  outputSchema?: MCPJsonSchema202012
  annotations?: ACPObject
}

export interface MCPResource {
  uri: string
  name: string
  title?: string
  description?: string
  mimeType?: string
}

export interface MCPResourceTemplate {
  uriTemplate: string
  name: string
  title?: string
  description?: string
  mimeType?: string
}

export interface MCPPrompt {
  name: string
  title?: string
  description?: string
  arguments?: ACPObject[]
}

export interface MCPOAuthProtectedResourceMetadata {
  resource?: string
  authorization_servers: string[]
  scopes_supported?: string[]
  bearer_methods_supported?: string[]
  [key: string]: ACPValue | undefined
}

export interface MCPOAuthAuthorizationServerMetadata {
  issuer: string
  authorization_endpoint: string
  token_endpoint: string
  registration_endpoint?: string
  token_endpoint_auth_methods_supported?: string[]
  code_challenge_methods_supported?: string[]
  scopes_supported?: string[]
  client_id_metadata_document_supported?: boolean
  authorization_response_iss_parameter_supported?: boolean
  [key: string]: ACPValue | undefined
}

export interface MCPOAuthWwwAuthenticateChallenge {
  scheme: string
  params: Record<string, string>
}

export interface MCPOAuthDiscoveryResult {
  protectedResourceMetadataUrl: string
  protectedResourceMetadata: MCPOAuthProtectedResourceMetadata
  authorizationServerIssuer: string
  authorizationServerMetadataUrl: string
  authorizationServerMetadataKind: "oauth_authorization_server" | "openid_configuration"
  authorizationServerMetadata: MCPOAuthAuthorizationServerMetadata
  challenge?: MCPOAuthWwwAuthenticateChallenge
  scopes: string[]
}

export interface MCPOAuthDynamicClientRegistrationRequest {
  client_name: string
  redirect_uris: string[]
  grant_types: string[]
  response_types: string[]
  token_endpoint_auth_method: string
  application_type: MCPOAuthApplicationType
  scope?: string
}

export function isRequest(msg: ACPMessage): msg is ACPRequest {
  return "id" in msg && "method" in msg
}

export function isResponse(msg: ACPMessage): msg is ACPResponse {
  return "id" in msg && !("method" in msg)
}

export function isNotification(msg: ACPMessage): msg is ACPNotification {
  return !("id" in msg) && "method" in msg
}
"#,
    );
    out.push_str(
        r"
export interface HarnSessionTimelineCursor {
  topics: Record<string, number>
}

export interface HarnSessionTimelineQuery {
  sessionId?: string | null
  runId?: string | null
  runPath?: string | null
  projectId?: string | null
  fromCursor: HarnSessionTimelineCursor
  limit?: number | null
}

export interface HarnSessionTimelineReference {
  kind: string
  id?: string
  topic?: string
  eventId?: number
}

export interface HarnSessionTimelineLink {
  kind: string
  targetId?: string
  traceId?: string
  spanId?: string
  eventId?: string
}

/** Harn-owned semantic chronology row. `kind` remains open for forward compatibility. */
export interface HarnSessionTimelineNode {
  id: string
  parentId?: string
  children: string[]
  category: string
  kind: string
  name: string
  status: string
  traceId?: string
  spanId?: string
  occurredAtMs?: number
  startMs?: number
  durationMs?: number
  attributes: ACPValue
  references: HarnSessionTimelineReference[]
  links: HarnSessionTimelineLink[]
  order: number
}

export interface HarnSessionTimelineCoverage {
  returned: number
  available: number | null
  truncated: boolean
}

export interface HarnSessionTimelineSnapshot {
  schemaVersion: number
  query: HarnSessionTimelineQuery
  cursor: HarnSessionTimelineCursor
  coverage: HarnSessionTimelineCoverage
  nodes: HarnSessionTimelineNode[]
}

export interface HarnSessionTimelineUpdate {
  schemaVersion: number
  cursor: HarnSessionTimelineCursor
  node: HarnSessionTimelineNode
}
",
    );
    out
}

pub(super) fn ts_array(name: &str, values: &[&str], type_name: &str) -> String {
    ts_array_owned(name, &strs_to_strings(values), type_name)
}

pub(super) fn ts_array_owned(name: &str, values: &[String], type_name: &str) -> String {
    let mut out = format!("export const {name} = [\n");
    for value in values {
        out.push_str("  ");
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("] as const\n");
    out.push_str(&format!(
        "export type {type_name} = (typeof {name})[number]\n\n"
    ));
    out
}

pub(super) fn ts_wire_value_object(
    name: &str,
    values: &[&str],
    deprecated_values: &[DeprecatedWireValue],
) -> String {
    let mut out = format!("export const {name} = {{\n");
    for value in values {
        if let Some(deprecated) = deprecated_wire_value(deprecated_values, value) {
            out.push_str("  /** @deprecated ");
            out.push_str(&deprecation_message(deprecated));
            out.push_str(" */\n");
        }
        out.push_str("  ");
        out.push_str(&wire_value_property_name(value));
        out.push_str(": ");
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("} as const\n\n");
    out
}
