use harn_serve::adapters::acp::{
    HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD, HARN_PROVIDER_CATALOG_METHOD,
    HARN_SESSION_UPDATE_EXTENSIONS,
};
use harn_serve::MCP_PROTOCOL_VERSION;
use harn_vm::llm::receipts::{TOOL_CALL_RECEIPT_EXECUTORS, TOOL_CALL_RECEIPT_STATUSES};

use super::constants::*;
use super::support::*;
use super::swift::{deprecated_wire_value, deprecation_message, wire_value_property_name};
use super::values::*;

pub(super) fn generate_typescript() -> String {
    let mut out = generated_header("harn dump-protocol-artifacts", "typescript");
    out.push_str("export const HARN_PROTOCOL_ARTIFACT_VERSION = ");
    out.push_str(&json_string_literal(env!("CARGO_PKG_VERSION")));
    out.push_str("\n\n");
    for (name, value) in [
        ("MCP_PROTOCOL_VERSION", MCP_PROTOCOL_VERSION),
        ("MCP_STABLE_PROTOCOL_VERSION", MCP_PROTOCOL_VERSION),
        ("MCP_DRAFT_PROTOCOL_VERSION", MCP_DRAFT_PROTOCOL_VERSION),
        (
            "MCP_FINAL_2026_PROTOCOL_VERSION",
            MCP_FINAL_2026_PROTOCOL_VERSION,
        ),
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
        "HARN_SIDE_EFFECT_LEVELS",
        &side_effect_level_values(),
        "HarnSideEffectLevel",
    ));
    out.push_str(&ts_array_owned(
        "HARN_WORKER_STATUSES",
        &worker_status_values(),
        "HarnWorkerStatus",
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

export interface ACPNotification {
  jsonrpc: "2.0"
  method: string
  params?: ACPValue
}

export type ACPMessage = ACPRequest | ACPResponse | ACPNotification

export interface ACPExtensionMeta<T extends object = ACPObject> {
  harn?: T
}

export interface ACPContentBlock {
  type: "text" | "resource_link" | "resource" | "image" | "audio" | string
  text?: string
  _meta?: ACPExtensionMeta<ACPObject>
}

export type ACPToolExecutor =
  | "harn_builtin"
  | "host_bridge"
  | "provider_native"
  | { kind: "mcp_server"; serverName: string }

export interface HarnToolLifecycleMeta {
  audit?: ACPValue
  durationMs?: number
  error?: string
  errorCategory?: HarnToolCallErrorCategory
  executionDurationMs?: number
  executor?: ACPToolExecutor
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
  content?: ACPContentBlock[]
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
  content?: ACPContentBlock[]
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
  harnPlan?: ACPValue
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
  kind?: string
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
  serverInfo: MCPImplementation
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
