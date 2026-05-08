//! Generate the published Harn protocol contract artifacts.
//!
//! The protocol conformance matrix pins the adapter behavior. This command
//! turns the same wire vocabulary into checked-in JSON Schema copies plus
//! TypeScript and Swift definitions that downstream hosts can consume without
//! mirroring Harn enums by hand.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use harn_serve::adapters::acp::{
    ACP_SCHEMA_COMPATIBILITY, ACP_SESSION_UPDATE_VARIANTS, HARN_AGENT_EVENT_KINDS,
    HARN_AGENT_EVENT_METHOD, HARN_CONTENT_EXTENSION_FIELDS, HARN_SESSION_UPDATE_EXTENSIONS,
    HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use harn_serve::{A2A_PROTOCOL_VERSION, MCP_PROTOCOL_VERSION};
use harn_vm::agent_events::{ToolCallErrorCategory, ToolCallStatus};
use harn_vm::tool_annotations::{SideEffectLevel, ToolKind};
use serde::Serialize;
use serde_json::json;

const ACP_AGENT_METHODS: &[&str] = &[
    "initialize",
    "session/new",
    "session/prompt",
    "session/stop",
];
const ACP_CLIENT_METHODS: &[&str] = &[
    "fs/read_text_file",
    "fs/write_text_file",
    "terminal/create",
    "terminal/kill",
    "session/request_permission",
];
const ACP_AGENT_NOTIFICATIONS: &[&str] = &["session/message", "session/update", "terminal/output"];
const ACP_CONTENT_BLOCK_TYPES: &[&str] = &["text", "resource_link", "resource", "image", "audio"];
const ACP_TOOL_EXECUTOR_SIMPLE_VALUES: &[&str] =
    &["harn_builtin", "host_bridge", "provider_native"];

const A2A_METHODS: &[&str] = &[
    "message/send",
    "message/stream",
    "tasks/get",
    "tasks/cancel",
    "tasks/resubscribe",
    "tasks/pushNotificationConfig/set",
    "tasks/pushNotificationConfig/get",
    "tasks/pushNotificationConfig/list",
    "tasks/pushNotificationConfig/delete",
    "agent/getAuthenticatedExtendedCard",
];
const A2A_TASK_STATES: &[&str] = &[
    "submitted",
    "working",
    "completed",
    "failed",
    "canceled",
    "cancelled",
    "input-required",
    "rejected",
    "auth-required",
];
const A2A_TASK_EVENT_TYPES: &[&str] = &["status", "message", "worker_update"];

const MCP_METHODS: &[&str] = &[
    "initialize",
    "tools/list",
    "tools/call",
    "resources/list",
    "resources/read",
    "resources/templates/list",
    "prompts/list",
    "prompts/get",
    "completion/complete",
    "logging/setLevel",
    "sampling/createMessage",
    "elicitation/create",
    "notifications/initialized",
    "notifications/message",
];
const MCP_LOGGING_LEVELS: &[&str] = &[
    "debug",
    "info",
    "notice",
    "warning",
    "error",
    "critical",
    "alert",
    "emergency",
];

const SCHEMA_COPIES: &[SchemaCopy] = &[
    SchemaCopy {
        protocol: "acp",
        source: "conformance/protocols/schemas/acp-session-update.schema.json",
        artifact: "schemas/acp-session-update.schema.json",
    },
    SchemaCopy {
        protocol: "a2a",
        source: "conformance/protocols/schemas/a2a-0.3.0.schema.json",
        artifact: "schemas/a2a-0.3.0.schema.json",
    },
    SchemaCopy {
        protocol: "mcp",
        source: "conformance/protocols/schemas/mcp-2025-11-25.schema.json",
        artifact: "schemas/mcp-2025-11-25.schema.json",
    },
];

struct SchemaCopy {
    protocol: &'static str,
    source: &'static str,
    artifact: &'static str,
}

#[derive(Debug)]
struct Artifact {
    relative_path: String,
    contents: String,
}

impl Artifact {
    fn new(relative_path: impl Into<String>, contents: impl Into<String>) -> Self {
        Self {
            relative_path: relative_path.into(),
            contents: ensure_trailing_newline(contents.into()),
        }
    }
}

pub(crate) fn run(output_dir: &str, check_only: bool) {
    let artifacts = generate_artifacts().unwrap_or_else(|error| {
        eprintln!("error: failed to generate protocol artifacts: {error}");
        process::exit(1);
    });
    let output_root = Path::new(output_dir);

    if check_only {
        let mut stale = Vec::new();
        for artifact in &artifacts {
            let path = output_root.join(&artifact.relative_path);
            match fs::read_to_string(&path) {
                Ok(existing)
                    if normalize_line_endings(&existing)
                        == normalize_line_endings(&artifact.contents) => {}
                Ok(_) => stale.push(path),
                Err(_) => stale.push(path),
            }
        }
        if !stale.is_empty() {
            eprintln!("error: protocol artifacts are stale or missing:");
            for path in stale {
                eprintln!("  {}", path.display());
            }
            eprintln!("hint: run `make gen-protocol-artifacts` to regenerate.");
            process::exit(1);
        }
        return;
    }

    for artifact in artifacts {
        let path = output_root.join(&artifact.relative_path);
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                eprintln!("error: cannot create {}: {error}", parent.display());
                process::exit(1);
            }
        }
        if let Err(error) = fs::write(&path, artifact.contents) {
            eprintln!("error: cannot write {}: {error}", path.display());
            process::exit(1);
        }
        println!("wrote {}", path.display());
    }
}

fn generate_artifacts() -> Result<Vec<Artifact>, String> {
    let mut artifacts = vec![
        Artifact::new("README.md", generate_readme()),
        Artifact::new("manifest.json", generate_manifest()?),
        Artifact::new("harn-protocol.ts", generate_typescript()),
        Artifact::new("HarnProtocol.swift", generate_swift()),
    ];

    for schema in SCHEMA_COPIES {
        artifacts.push(Artifact::new(
            schema.artifact,
            read_repo_text(schema.source)?,
        ));
    }
    artifacts.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(artifacts)
}

fn generate_manifest() -> Result<String, String> {
    let schemas = SCHEMA_COPIES
        .iter()
        .map(|schema| {
            Ok(json!({
                "protocol": schema.protocol,
                "source": schema.source,
                "artifact": schema.artifact,
                "provenance": schema_provenance(schema.source)?,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;

    serde_json::to_string_pretty(&json!({
        "schemaVersion": 1,
        "artifactVersion": env!("CARGO_PKG_VERSION"),
        "generatedBy": "harn dump-protocol-artifacts",
        "checkCommand": "make check-protocol-artifacts",
        "schemas": schemas,
        "acp": {
            "schemaCompatibility": ACP_SCHEMA_COMPATIBILITY,
            "agentMethods": ACP_AGENT_METHODS,
            "clientMethods": ACP_CLIENT_METHODS,
            "agentNotifications": ACP_AGENT_NOTIFICATIONS,
            "sessionUpdateVariants": all_acp_session_updates(),
            "harnSessionUpdateExtensions": HARN_SESSION_UPDATE_EXTENSIONS,
            "harnAgentEventMethod": HARN_AGENT_EVENT_METHOD,
            "harnAgentEventKinds": HARN_AGENT_EVENT_KINDS,
            "toolLifecycleExtensionFields": HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
            "contentExtensionFields": HARN_CONTENT_EXTENSION_FIELDS,
            "contentBlockTypes": ACP_CONTENT_BLOCK_TYPES,
            "toolKinds": tool_kind_values(),
            "toolCallStatuses": tool_call_status_values(),
            "toolCallErrorCategories": tool_call_error_category_values(),
            "toolExecutorSimpleValues": ACP_TOOL_EXECUTOR_SIMPLE_VALUES,
        },
        "a2a": {
            "protocolVersion": A2A_PROTOCOL_VERSION,
            "methods": A2A_METHODS,
            "taskStates": A2A_TASK_STATES,
            "taskEventTypes": A2A_TASK_EVENT_TYPES,
        },
        "mcp": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "methods": MCP_METHODS,
            "loggingLevels": MCP_LOGGING_LEVELS,
        },
    }))
    .map_err(|error| format!("failed to serialize manifest: {error}"))
}

fn generate_readme() -> String {
    format!(
        "# Harn Protocol Artifacts\n\n\
         <!-- GENERATED by `harn dump-protocol-artifacts` -- do not edit by hand. -->\n\n\
         This directory is the checked-in Harn protocol contract for downstream hosts.\n\
         It publishes JSON Schema profiles plus TypeScript and Swift bindings generated\n\
         from Harn's adapter vocabulary.\n\n\
         Regenerate with:\n\n\
         ```sh\n\
         make gen-protocol-artifacts\n\
         ```\n\n\
         Verify drift with:\n\n\
         ```sh\n\
         make check-protocol-artifacts\n\
         ```\n\n\
         Files:\n\n\
         - `manifest.json`: deterministic summary of protocol versions, advertised Harn\n\
           extension fields, and generated binding vocabulary.\n\
         - `schemas/acp-session-update.schema.json`: Harn's ACP session-update schema\n\
           profile (`{acp}`).\n\
         - `schemas/a2a-0.3.0.schema.json`: Harn's A2A schema profile (`{a2a}`).\n\
         - `schemas/mcp-2025-11-25.schema.json`: Harn's MCP schema profile (`{mcp}`).\n\
         - `harn-protocol.ts`: TypeScript definitions for ACP session updates,\n\
           tool lifecycle metadata, A2A task events, and MCP metadata.\n\
         - `HarnProtocol.swift`: Swift definitions for the same host-facing surface.\n\n\
         Compatibility rule: additive enum values and optional fields are minor-version\n\
         compatible; removing or renaming a wire value requires a Harn minor-version\n\
         migration note and a regenerated artifact diff.\n",
        acp = ACP_SCHEMA_COMPATIBILITY,
        a2a = A2A_PROTOCOL_VERSION,
        mcp = MCP_PROTOCOL_VERSION,
    )
}

fn generate_typescript() -> String {
    let mut out = generated_header("harn dump-protocol-artifacts", "typescript");
    out.push_str("export const HARN_PROTOCOL_ARTIFACT_VERSION = ");
    out.push_str(&ts_string(env!("CARGO_PKG_VERSION")));
    out.push_str("\n\n");
    out.push_str(&ts_array(
        "ACP_AGENT_METHODS",
        ACP_AGENT_METHODS,
        "ACPAgentMethod",
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
    out.push_str(&ts_string(HARN_AGENT_EVENT_METHOD));
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
    out.push_str(&ts_array("MCP_METHODS", MCP_METHODS, "MCPMethod"));
    out.push_str(&ts_array(
        "MCP_LOGGING_LEVELS",
        MCP_LOGGING_LEVELS,
        "MCPLoggingLevel",
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

export interface ACPPlanUpdate {
  sessionUpdate: "plan"
  entries: ACPValue[]
  harnPlan?: ACPValue
}

export interface ACPHarnExtensionUpdate {
  sessionUpdate: HarnACPSessionUpdateExtension
  _meta?: ACPExtensionMeta<ACPObject>
}

export type ACPSessionUpdateEnvelope =
  | ACPMessageChunkUpdate
  | ACPToolCall
  | ACPToolCallUpdate
  | ACPPlanUpdate
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

export interface ACPSessionRequestPermissionParams {
  sessionId: string
  approvalRequest?: ACPObject
  toolCall?: {
    toolCallId: string
    toolName: string
    rawInput?: ACPValue
  }
  mutation?: ACPObject
  options?: ACPValue[]
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

export interface MCPTool {
  name: string
  title?: string
  description?: string
  inputSchema: ACPObject
  outputSchema?: ACPObject
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

fn generate_swift() -> String {
    let mut out = generated_header("harn dump-protocol-artifacts", "swift");
    out.push_str("import Foundation\n\n");
    out.push_str("public enum HarnProtocolConstants {\n");
    out.push_str(&format!(
        "    public static let artifactVersion = {}\n",
        swift_string(env!("CARGO_PKG_VERSION"))
    ));
    out.push_str(&format!(
        "    public static let acpSchemaCompatibility = {}\n",
        swift_string(ACP_SCHEMA_COMPATIBILITY)
    ));
    out.push_str(&format!(
        "    public static let harnAgentEventMethod = {}\n",
        swift_string(HARN_AGENT_EVENT_METHOD)
    ));
    out.push_str(&swift_string_array(
        "acpSessionUpdateExtensions",
        HARN_SESSION_UPDATE_EXTENSIONS,
    ));
    out.push_str(&swift_string_array(
        "harnAgentEventKinds",
        HARN_AGENT_EVENT_KINDS,
    ));
    out.push_str(&swift_string_array(
        "toolLifecycleExtensionFields",
        HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
    ));
    out.push_str(&swift_string_array(
        "contentExtensionFields",
        HARN_CONTENT_EXTENSION_FIELDS,
    ));
    out.push_str("}\n\n");

    out.push_str(&swift_enum(
        "HarnACPSessionUpdate",
        &all_acp_session_updates(),
    ));
    out.push_str(&swift_enum("HarnACPToolKind", &tool_kind_values()));
    out.push_str(&swift_enum(
        "HarnACPToolCallStatus",
        &tool_call_status_values(),
    ));
    out.push_str(&swift_enum(
        "HarnToolCallErrorCategory",
        &tool_call_error_category_values(),
    ));
    out.push_str(&swift_enum(
        "HarnSideEffectLevel",
        &side_effect_level_values(),
    ));
    out.push_str(&swift_enum(
        "HarnA2ATaskState",
        &strs_to_strings(A2A_TASK_STATES),
    ));

    out.push_str(
        r#"public enum HarnACPValue: Codable, Sendable, Equatable {
    case null
    case bool(Bool)
    case int(Int)
    case double(Double)
    case string(String)
    case array([HarnACPValue])
    case object([String: HarnACPValue])

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Int.self) {
            self = .int(value)
        } else if let value = try? container.decode(Double.self) {
            self = .double(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([HarnACPValue].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: HarnACPValue].self) {
            self = .object(value)
        } else {
            self = .null
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .int(let value): try container.encode(value)
        case .double(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }
}

public typealias HarnACPObject = [String: HarnACPValue]

public enum HarnJsonRpcId: Codable, Sendable, Equatable {
    case null
    case int(Int)
    case string(String)

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Int.self) {
            self = .int(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else {
            throw DecodingError.typeMismatch(
                HarnJsonRpcId.self,
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "JSON-RPC id must be an integer, string, or null"
                )
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .int(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        }
    }
}

public struct HarnACPRequest: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var id: HarnJsonRpcId
    public var method: String
    public var params: HarnACPValue?

    public init(id: HarnJsonRpcId, method: String, params: HarnACPValue? = nil) {
        self.jsonrpc = "2.0"
        self.id = id
        self.method = method
        self.params = params
    }

    public init(id: Int, method: String, params: HarnACPValue? = nil) {
        self.init(id: .int(id), method: method, params: params)
    }

    public init(id: String, method: String, params: HarnACPValue? = nil) {
        self.init(id: .string(id), method: method, params: params)
    }
}

public struct HarnACPError: Codable, Sendable, Equatable {
    public var code: Int
    public var message: String
    public var data: HarnACPValue?
}

public struct HarnACPResponse: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var id: HarnJsonRpcId
    public var result: HarnACPValue?
    public var error: HarnACPError?
}

public struct HarnACPNotification: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var method: String
    public var params: HarnACPValue?

    public init(method: String, params: HarnACPValue? = nil) {
        self.jsonrpc = "2.0"
        self.method = method
        self.params = params
    }
}

public struct HarnACPExtensionMeta: Codable, Sendable, Equatable {
    public var harn: HarnACPObject?
}

public struct HarnACPContentBlock: Codable, Sendable, Equatable {
    public var type: String
    public var text: String?
    public var meta: HarnACPExtensionMeta?

    enum CodingKeys: String, CodingKey {
        case type
        case text
        case meta = "_meta"
    }
}

public enum HarnACPToolExecutor: Codable, Sendable, Equatable {
    case harnBuiltin
    case hostBridge
    case providerNative
    case mcpServer(name: String)
    case unknown(String)

    enum ObjectKey: String, CodingKey {
        case kind
        case serverName
    }

    public init(from decoder: Decoder) throws {
        if let raw = try? decoder.singleValueContainer().decode(String.self) {
            switch raw {
            case "harn_builtin": self = .harnBuiltin
            case "host_bridge": self = .hostBridge
            case "provider_native": self = .providerNative
            default: self = .unknown(raw)
            }
            return
        }
        let object = try decoder.container(keyedBy: ObjectKey.self)
        let kind = try object.decode(String.self, forKey: .kind)
        if kind == "mcp_server" {
            self = .mcpServer(name: try object.decode(String.self, forKey: .serverName))
        } else {
            self = .unknown(kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .harnBuiltin:
            var container = encoder.singleValueContainer()
            try container.encode("harn_builtin")
        case .hostBridge:
            var container = encoder.singleValueContainer()
            try container.encode("host_bridge")
        case .providerNative:
            var container = encoder.singleValueContainer()
            try container.encode("provider_native")
        case .mcpServer(let name):
            var container = encoder.container(keyedBy: ObjectKey.self)
            try container.encode("mcp_server", forKey: .kind)
            try container.encode(name, forKey: .serverName)
        case .unknown(let raw):
            var container = encoder.singleValueContainer()
            try container.encode(raw)
        }
    }
}

public struct HarnToolLifecycleMeta: Codable, Sendable, Equatable {
    public var audit: HarnACPValue?
    public var durationMs: Double?
    public var error: String?
    public var errorCategory: HarnToolCallErrorCategory?
    public var executionDurationMs: Double?
    public var executor: HarnACPToolExecutor?
    public var parsing: Bool?
    public var rawInputPartial: String?
}

public struct HarnACPToolCall: Codable, Sendable, Equatable {
    public var sessionUpdate: HarnACPSessionUpdate
    public var toolCallId: String
    public var title: String
    public var kind: HarnACPToolKind?
    public var status: HarnACPToolCallStatus?
    public var content: [HarnACPContentBlock]?
    public var locations: [HarnACPValue]?
    public var rawInput: HarnACPValue?
    public var rawOutput: HarnACPValue?
    public var meta: HarnACPExtensionMeta?

    enum CodingKeys: String, CodingKey {
        case sessionUpdate
        case toolCallId
        case title
        case kind
        case status
        case content
        case locations
        case rawInput
        case rawOutput
        case meta = "_meta"
    }
}

public struct HarnACPSessionUpdateEnvelope: Codable, Sendable, Equatable {
    public var sessionUpdate: HarnACPSessionUpdate
    public var content: HarnACPContentBlock?
    public var entries: [HarnACPValue]?
    public var toolCallId: String?
    public var title: String?
    public var kind: HarnACPToolKind?
    public var status: HarnACPToolCallStatus?
    public var rawInput: HarnACPValue?
    public var rawOutput: HarnACPValue?
    public var meta: HarnACPExtensionMeta?

    enum CodingKeys: String, CodingKey {
        case sessionUpdate
        case content
        case entries
        case toolCallId
        case title
        case kind
        case status
        case rawInput
        case rawOutput
        case meta = "_meta"
    }
}

public struct HarnACPSessionUpdateParams: Codable, Sendable, Equatable {
    public var sessionId: String
    public var update: HarnACPSessionUpdateEnvelope
}

public struct HarnACPSessionUpdateNotification: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var method: String
    public var params: HarnACPSessionUpdateParams
}

public struct HarnAgentEventNotification: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var method: String
    public var params: HarnACPObject
}

public struct HarnPromptCapabilities: Codable, Sendable, Equatable {
    public var image: Bool?
    public var audio: Bool?
    public var embeddedContext: Bool?
}

public struct HarnACPAgentCapabilities: Codable, Sendable, Equatable {
    public var meta: HarnACPExtensionMeta?
    public var loadSession: Bool?
    public var promptCapabilities: HarnPromptCapabilities?
    public var mcpCapabilities: HarnACPObject?
    public var sessionCapabilities: HarnACPObject?

    enum CodingKeys: String, CodingKey {
        case meta = "_meta"
        case loadSession
        case promptCapabilities
        case mcpCapabilities
        case sessionCapabilities
    }
}

public struct HarnToolArgSchema: Codable, Sendable, Equatable {
    public var pathParams: [String]
    public var argAliases: [String: String]
    public var required: [String]

    enum CodingKeys: String, CodingKey {
        case pathParams = "path_params"
        case argAliases = "arg_aliases"
        case required
    }
}

public struct HarnToolAnnotations: Codable, Sendable, Equatable {
    public var kind: HarnACPToolKind
    public var sideEffectLevel: HarnSideEffectLevel
    public var argSchema: HarnToolArgSchema
    public var capabilities: [String: [String]]
    public var emitsArtifacts: Bool
    public var resultReaders: [String]
    public var inlineResult: Bool

    enum CodingKeys: String, CodingKey {
        case kind
        case sideEffectLevel = "side_effect_level"
        case argSchema = "arg_schema"
        case capabilities
        case emitsArtifacts = "emits_artifacts"
        case resultReaders = "result_readers"
        case inlineResult = "inline_result"
    }
}

public struct HarnA2ATaskStatus: Codable, Sendable, Equatable {
    public var state: HarnA2ATaskState
    public var message: HarnACPValue?
    public var timestamp: String?
}

public struct HarnA2ATask: Codable, Sendable, Equatable {
    public var id: String
    public var contextId: String?
    public var status: HarnA2ATaskStatus
    public var history: [HarnACPValue]?
    public var artifacts: [HarnACPValue]?
    public var metadata: HarnACPObject?
}

public struct HarnMCPTool: Codable, Sendable, Equatable {
    public var name: String
    public var title: String?
    public var description: String?
    public var inputSchema: HarnACPObject
    public var outputSchema: HarnACPObject?
    public var annotations: HarnACPObject?
}

public struct HarnMCPResource: Codable, Sendable, Equatable {
    public var uri: String
    public var name: String
    public var title: String?
    public var description: String?
    public var mimeType: String?
}

public struct HarnMCPResourceTemplate: Codable, Sendable, Equatable {
    public var uriTemplate: String
    public var name: String
    public var title: String?
    public var description: String?
    public var mimeType: String?
}

public struct HarnMCPPrompt: Codable, Sendable, Equatable {
    public var name: String
    public var title: String?
    public var description: String?
    public var arguments: [HarnACPObject]?
}
"#,
    );

    out
}

fn generated_header(command: &str, language: &str) -> String {
    match language {
        "typescript" => format!(
            "// GENERATED by `{command}` - do not edit by hand.\n\
             // Source: Harn adapter schemas and Rust wire vocabulary.\n\n"
        ),
        "swift" => format!(
            "// GENERATED by `{command}` - do not edit by hand.\n\
             // Source: Harn adapter schemas and Rust wire vocabulary.\n\n"
        ),
        _ => String::new(),
    }
}

fn ts_array(name: &str, values: &[&str], type_name: &str) -> String {
    ts_array_owned(name, &strs_to_strings(values), type_name)
}

fn ts_array_owned(name: &str, values: &[String], type_name: &str) -> String {
    let mut out = format!("export const {name} = [\n");
    for value in values {
        out.push_str("  ");
        out.push_str(&ts_string(value));
        out.push_str(",\n");
    }
    out.push_str("] as const\n");
    out.push_str(&format!(
        "export type {type_name} = (typeof {name})[number]\n\n"
    ));
    out
}

fn swift_enum(name: &str, values: &[String]) -> String {
    let mut out = format!("public enum {name}: String, Codable, Sendable, CaseIterable {{\n");
    for value in values {
        out.push_str("    case ");
        out.push_str(&swift_case_name(value));
        out.push_str(" = ");
        out.push_str(&swift_string(value));
        out.push('\n');
    }
    out.push_str("}\n\n");
    out
}

fn swift_string_array(name: &str, values: &[&str]) -> String {
    let mut out = format!("    public static let {name}: [String] = [\n");
    for value in values {
        out.push_str("        ");
        out.push_str(&swift_string(value));
        out.push_str(",\n");
    }
    out.push_str("    ]\n");
    out
}

fn swift_case_name(value: &str) -> String {
    let mut out = String::new();
    for (index, part) in value
        .split(['_', '-', '/'])
        .filter(|part| !part.is_empty())
        .enumerate()
    {
        if index == 0 {
            out.push_str(part);
        } else {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.push_str(chars.as_str());
            }
        }
    }
    if out.is_empty() {
        "unknown".to_string()
    } else if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("_{}", out)
    } else {
        out
    }
}

fn all_acp_session_updates() -> Vec<String> {
    unique_ordered(
        ACP_SESSION_UPDATE_VARIANTS
            .iter()
            .chain(HARN_SESSION_UPDATE_EXTENSIONS.iter())
            .copied(),
    )
}

fn tool_kind_values() -> Vec<String> {
    ToolKind::ALL.iter().map(serde_wire_string).collect()
}

fn tool_call_status_values() -> Vec<String> {
    ToolCallStatus::ALL
        .iter()
        .map(|status| status.as_str().to_string())
        .collect()
}

fn tool_call_error_category_values() -> Vec<String> {
    ToolCallErrorCategory::ALL
        .iter()
        .map(|category| category.as_str().to_string())
        .collect()
}

fn side_effect_level_values() -> Vec<String> {
    SideEffectLevel::ALL
        .iter()
        .map(|level| level.as_str().to_string())
        .collect()
}

fn unique_ordered<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        if seen.insert(value) {
            out.push(value.to_string());
        }
    }
    out
}

fn serde_wire_string<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .expect("wire enum serializes")
        .as_str()
        .expect("wire enum serializes as string")
        .to_string()
}

fn strs_to_strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn schema_provenance(relative_path: &str) -> Result<serde_json::Value, String> {
    let source: serde_json::Value = serde_json::from_str(&read_repo_text(relative_path)?)
        .map_err(|error| format!("failed to parse {relative_path}: {error}"))?;
    Ok(source
        .get("x-harn-provenance")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

fn read_repo_text(relative_path: &str) -> Result<String, String> {
    let path = repo_root().join(relative_path);
    fs::read_to_string(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn ts_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serializes")
}

fn swift_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serializes")
}

fn ensure_trailing_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_types_include_harn_wire_vocabularies() {
        let ts = generate_typescript();
        assert!(ts.contains("export type JsonRpcId = number | string | null"));
        for value in HARN_SESSION_UPDATE_EXTENSIONS
            .iter()
            .chain(HARN_AGENT_EVENT_KINDS.iter())
            .chain(HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS.iter())
        {
            assert!(ts.contains(value), "TypeScript artifact missing {value}");
        }
        let swift = generate_swift();
        assert!(swift.contains("public enum HarnJsonRpcId"));
        assert!(swift.contains("public var id: HarnJsonRpcId"));
        for value in tool_kind_values()
            .into_iter()
            .chain(tool_call_status_values())
            .chain(tool_call_error_category_values())
        {
            assert!(swift.contains(&value), "Swift artifact missing {value}");
        }
    }

    #[test]
    fn generated_manifest_references_schema_artifacts() {
        let manifest: serde_json::Value =
            serde_json::from_str(&generate_manifest().expect("manifest")).expect("manifest json");
        for schema in SCHEMA_COPIES {
            assert!(
                manifest["schemas"]
                    .as_array()
                    .expect("schema array")
                    .iter()
                    .any(|entry| entry["artifact"] == schema.artifact),
                "manifest missing {}",
                schema.artifact
            );
        }
    }

    #[test]
    fn committed_protocol_artifacts_match_generator() {
        let artifacts = generate_artifacts().expect("artifacts");
        let output_root = repo_root().join("spec/protocol-artifacts");
        for artifact in artifacts {
            let path = output_root.join(&artifact.relative_path);
            let on_disk = fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!(
                    "failed to read {}: {error}\n\
                     hint: run `make gen-protocol-artifacts` to regenerate.",
                    path.display()
                )
            });
            assert_eq!(
                normalize_line_endings(&on_disk),
                normalize_line_endings(&artifact.contents),
                "{} is stale. Run `make gen-protocol-artifacts` to regenerate.",
                path.display()
            );
        }
    }
}
