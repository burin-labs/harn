use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::json;

use harn_serve::adapters::acp::{
    ACP_PROMPT_ERROR_DATA_SCHEMA, HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD,
    HARN_CONTENT_EXTENSION_FIELDS, HARN_PROMPT_RESULT_EXTENSION_FIELDS,
    HARN_PROVIDER_CATALOG_METHOD, HARN_SESSION_UPDATE_EXTENSIONS,
    HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use harn_vm::llm::receipts::{
    TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT, TOOL_CALL_RECEIPT_SCHEMA_VERSION, TOOL_CALL_RECEIPT_STATUSES,
};
use harn_vm::orchestration::SESSION_VIEW_QUERY_METHOD;
use harn_vm::session_timeline::{
    SESSION_TIMELINE_QUERY_METHOD, SESSION_TIMELINE_SUBSCRIBE_METHOD,
    SESSION_TIMELINE_UNSUBSCRIBE_METHOD,
};

use super::constants::*;
use super::go::*;
use super::manifest::*;
use super::python::*;
use super::rust::*;
use super::support::*;
use super::swift::*;
use super::typescript::*;
use super::values::*;
use super::*;

#[rustfmt::skip]
#[path = "../../../../../spec/protocol-artifacts/harn-protocol.rs"]
mod generated_rust_binding;

fn protocol_source() -> ProtocolArtifactSource {
    ProtocolArtifactSource::from_anchor(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        .expect("harn-cli is compiled from the Harn workspace")
}

/// Shift-left guard: every `ACP`/`Harn`/`A2A`/`MCP`-prefixed *type* name
/// referenced in the emitted TypeScript bindings must be declared in the
/// same artifact. The TS section once used the Python-only name
/// `HarnExtensionMeta` for a `_meta` field; it compiled here (it's just a
/// Rust string) but broke `tsc` downstream in an IDE host, where the
/// failure surfaced far from its cause. This test fails harn's own
/// `cargo test` on any dangling protocol-type reference, so the class of
/// bug can't escape the harn build again.
#[test]
fn typescript_artifact_has_no_dangling_type_references() {
    let ts = generate_typescript();
    let declared: std::collections::HashSet<&str> = regex::Regex::new(
            r"(?m)^\s*(?:export\s+)?(?:declare\s+)?(?:const\s+)?(?:interface|type|enum|class)\s+([A-Za-z_][A-Za-z0-9_]*)",
        )
        .unwrap()
        .captures_iter(&ts)
        .map(|c| c.get(1).unwrap().as_str())
        .collect();
    let mut dangling: Vec<&str> = regex::Regex::new(r"\b((?:ACP|Harn|A2A|MCP)[A-Za-z0-9]+)\b")
        .unwrap()
        .captures_iter(&ts)
        .map(|c| c.get(1).unwrap().as_str())
        .filter(|name| !declared.contains(name))
        .collect();
    dangling.sort_unstable();
    dangling.dedup();
    assert!(
        dangling.is_empty(),
        "emitted TypeScript references undeclared protocol type(s): {dangling:?}. Every \
             ACP/Harn/A2A/MCP-prefixed type used in the bindings must be declared in the same \
             artifact; this guard shift-lefts the downstream `tsc` failure into harn's build."
    );
}

#[test]
fn generated_types_include_harn_wire_vocabularies() {
    let ts = generate_typescript();
    assert!(ts.contains("export type JsonRpcId = number | string | null"));
    assert!(ts.contains("export const MCP_DRAFT_PROTOCOL_VERSION = \"DRAFT-2026-v1\""));
    assert!(ts.contains("export const MCP_LEGACY_2025_06_18_PROTOCOL_VERSION = \"2025-06-18\""));
    assert!(ts.contains("export interface MCPRequestMeta"));
    assert!(ts.contains("export interface MCPDiscoverResult"));
    assert!(ts.contains("export interface MCPInputRequiredResult"));
    assert!(ts.contains("export interface MCPOAuthDiscoveryResult"));
    assert!(ts.contains("MCP_OAUTH_CLIENT_REGISTRATION_MODES"));
    assert!(ts.contains("MCP_OAUTH_AUTH_MODES"));
    assert!(ts.contains("application_type: MCPOAuthApplicationType"));
    assert!(ts.contains("MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR"));
    assert!(ts.contains("server/discover"));
    assert!(ts.contains("io.modelcontextprotocol/protocolVersion"));
    assert!(ts.contains("MCP-Protocol-Version"));
    assert!(ts.contains("ttlMs"));
    assert!(ts.contains("cacheScope"));
    assert!(ts.contains("sessionClose: \"session/close\""));
    assert!(ts.contains("@deprecated Use session/close; session/stop will be removed"));
    for value in HARN_SESSION_UPDATE_EXTENSIONS
        .iter()
        .chain(HARN_AGENT_EVENT_KINDS.iter())
        .chain(HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS.iter())
    {
        assert!(ts.contains(value), "TypeScript artifact missing {value}");
    }
    let swift = generate_swift();
    assert!(swift.contains("public enum HarnACPAgentMethod"));
    assert!(swift.contains("mcpDraftProtocolVersion = \"DRAFT-2026-v1\""));
    assert!(swift.contains("mcpLegacy20250618ProtocolVersion = \"2025-06-18\""));
    assert!(swift.contains("public struct HarnMCPRequestMeta"));
    assert!(swift.contains("public struct HarnMCPDiscoverResult"));
    assert!(swift.contains("public struct HarnMCPInputRequiredResult"));
    assert!(swift.contains("public struct HarnMCPOAuthDiscoveryResult"));
    assert!(swift.contains("public enum HarnMCPOAuthClientRegistrationMode"));
    assert!(swift.contains("public enum HarnMCPOAuthAuthMode"));
    assert!(swift.contains("case applicationType = \"application_type\""));
    assert!(swift.contains("HarnMCPUnsupportedProtocolVersionError"));
    assert!(swift.contains("case protocolVersion = \"io.modelcontextprotocol/protocolVersion\""));
    assert!(swift.contains("case protocolVersion = \"MCP-Protocol-Version\""));
    assert!(swift.contains("case sessionClose = \"session/close\""));
    assert!(swift.contains("@available(*, deprecated"));
    assert!(swift.contains("public static let allCases: [Self]"));
    assert!(swift.contains("public enum HarnACPClientMethod"));
    assert!(swift.contains("public enum HarnACPAgentNotification"));
    assert!(swift.contains("public enum HarnJsonRpcId"));
    assert!(swift.contains("public var id: HarnJsonRpcId"));
    assert!(swift.contains("public init?(jsonObject: Any)"));
    assert!(swift.contains("case let value as NSNumber: return jsonNumber(value)"));
    assert!(swift.contains("CFGetTypeID(value) == CFBooleanGetTypeID()"));
    assert!(swift.contains("public static func success(id: HarnJsonRpcId"));
    assert!(swift.contains("public struct HarnToolCallReceipt"));
    for value in tool_kind_values()
        .into_iter()
        .chain(tool_call_status_values())
        .chain(tool_call_error_category_values())
        .chain(tool_mutation_status_values())
        .chain(worker_status_values())
        .chain(
            TOOL_CALL_RECEIPT_STATUSES
                .iter()
                .map(|value| (*value).to_string()),
        )
    {
        assert!(swift.contains(&value), "Swift artifact missing {value}");
    }
    assert!(swift.contains("public enum HarnWorkerStatus"));
    assert!(ts.contains("export type HarnWorkerStatus"));
    assert!(ts.contains("HARN_WORKER_STATUSES"));
    assert!(ts.contains("export interface ToolCallReceipt"));
    assert!(ts.contains("HARN_TOOL_CALL_RECEIPT_STATUSES"));
    assert!(ts.contains("export interface HarnACPPromptErrorData"));
    assert!(ts.contains("export interface HarnACPPromptResult"));
    assert!(ts.contains("export interface HarnAgentTerminalOutcome"));
    assert!(ts.contains("export interface ACPToolCallDiff"));
    assert!(ts.contains("content?: ACPToolCallContent[]"));
    assert!(swift.contains("public struct HarnACPPromptErrorData"));
    assert!(swift.contains("public struct HarnACPPromptResult"));
    assert!(swift.contains("public struct HarnAgentTerminalOutcome"));
    for value in agent_terminal_class_values() {
        assert!(ts.contains(&value), "TypeScript artifact missing {value}");
        assert!(swift.contains(&value), "Swift artifact missing {value}");
    }
    for value in agent_terminal_kind_values() {
        assert!(ts.contains(&value), "TypeScript artifact missing {value}");
        assert!(swift.contains(&value), "Swift artifact missing {value}");
    }
}

#[test]
fn swift_case_name_emits_valid_identifiers() {
    // Bare `case private = ...` / `case public = ...` won't compile in
    // Swift — both are reserved keywords. The wire vocabulary uses these
    // (e.g. MCPCacheScope) so the emitter has to backtick them.
    assert_eq!(swift_case_name("private"), "`private`");
    assert_eq!(swift_case_name("public"), "`public`");
    assert_eq!(swift_case_name("class"), "`class`");
    // Non-keyword identifiers should pass through unchanged.
    assert_eq!(swift_case_name("application_type"), "applicationType");
    assert_eq!(swift_case_name("session/close"), "sessionClose");
    assert_eq!(
        swift_case_name("harn.acp.prompt_error.v1"),
        "harnAcpPromptErrorV1"
    );
    assert_eq!(
        swift_case_name("session:prompt-error"),
        "sessionPromptError"
    );
    // CamelCased compounds that happen to start with a keyword fragment
    // (e.g. "private_room" -> "privateRoom") are valid identifiers and
    // must not be escaped.
    assert_eq!(swift_case_name("private_room"), "privateRoom");
    // The full emitted Swift artifact should contain the escaped form for
    // any enum whose wire value lands on a reserved keyword.
    let swift = generate_swift();
    assert!(
        !swift.contains("case private = "),
        "Swift artifact contains unescaped `case private = ...`"
    );
    assert!(
        !swift.contains("case public = "),
        "Swift artifact contains unescaped `case public = ...`"
    );
    assert!(swift.contains("case `private` = \"private\""));
    assert!(swift.contains("case `public` = \"public\""));
}

#[test]
fn generated_rust_includes_harn_wire_vocabularies() {
    let rust = generate_rust();
    assert!(
        rust.starts_with("// GENERATED by `harn dump-protocol-artifacts` - do not edit by hand."),
        "Rust artifact missing provenance header"
    );
    assert!(rust.contains("pub const HARN_PROTOCOL_ARTIFACT_VERSION: &str ="));
    assert!(rust.contains("pub const ACP_PROMPT_ERROR_DATA_SCHEMA: &str ="));
    assert!(rust.contains("pub const AGENT_TERMINAL_CLASSES: &[&str] = &["));
    assert!(rust.contains("pub const AGENT_TERMINAL_KINDS: &[&str] = &["));
    assert!(rust.contains("pub const AGENT_TERMINAL_OWNERS: &[&str] = &["));
    assert!(rust.contains(&format!(
        "pub const HARN_AGENT_EVENT_METHOD: &str = {};",
        json_string_literal(HARN_AGENT_EVENT_METHOD)
    )));
    assert!(rust.contains(&format!(
        "pub const HARN_PROVIDER_CATALOG_METHOD: &str = {};",
        json_string_literal(HARN_PROVIDER_CATALOG_METHOD)
    )));
    // Method-name constants, both the stable and the full dispatched surface.
    assert!(rust.contains("pub const ACP_AGENT_METHOD_SESSION_PROMPT: &str = \"session/prompt\""));
    assert!(rust.contains("pub const ACP_AGENT_METHODS: &[&str] = &["));
    assert!(rust.contains("pub const ACP_DISPATCHED_METHODS: &[&str] = &["));
    assert!(rust.contains("pub const ACP_TRANSPORT_CONTROL_METHODS: &[&str] = &["));
    assert!(rust.contains("pub const ACP_HANDLED_METHODS: &[&str] = &["));
    assert!(rust.contains("pub const ACP_TRANSPORT_CONTROL_METHOD_SESSION_SET_BUDGET"));
    assert!(rust.contains("pub const ACP_CLIENT_METHODS: &[&str] = &["));
    assert!(rust.contains("pub const HARN_SESSION_TIMELINE_METHODS: &[&str] = &["));
    // Session-update discriminators (base + Harn extensions).
    assert!(rust.contains("pub const ACP_SESSION_UPDATES: &[&str] = &["));
    assert!(rust.contains("pub const HARN_ACP_SESSION_UPDATE_EXTENSIONS: &[&str] = &["));
    // `_meta.harn` content extension keys.
    assert!(rust.contains(
        "pub const HARN_CONTENT_EXTENSION_FIELD_PERMISSION_PREVIEW: &str = \"permission_preview\""
    ));
    assert!(rust
        .contains("pub const HARN_CONTENT_EXTENSION_FIELD_VISIBLE_TEXT: &str = \"visible_text\""));
    assert!(rust.contains(
        "pub const HARN_CONTENT_EXTENSION_FIELD_VISIBLE_DELTA: &str = \"visible_delta\""
    ));
    assert!(rust.contains("pub const HARN_CONTENT_EXTENSION_FIELDS: &[&str] = &["));
    assert!(rust.contains("pub const HARN_PROMPT_RESULT_EXTENSION_FIELDS: &[&str] = &["));
    assert!(rust.contains("pub enum ACPPermissionOptionKind"));
    assert!(rust.contains("pub struct ACPSessionRequestPermissionParams"));
    assert!(rust.contains("pub enum ACPPermissionOutcome"));
    assert!(rust.contains("pub struct HarnAgentEventParams"));
    assert!(rust.contains("pub enum HarnAgentEventKind"));
    // Dotted / slashed wire names must collapse to valid const identifiers.
    assert!(rust.contains(
        "pub const ACP_DISPATCHED_METHOD_HARN_HITL_RESPOND: &str = \"harn.hitl.respond\""
    ));
    assert!(rust.contains("pub const ACP_DISPATCHED_METHOD_AGENT_RESUME: &str = \"agent/resume\""));
    // Every published wire value must appear somewhere in the artifact.
    for value in ACP_AGENT_METHODS
        .iter()
        .chain(ACP_DISPATCHED_METHODS.iter())
        .chain(ACP_TRANSPORT_CONTROL_METHODS.iter())
        .chain(HARN_SESSION_TIMELINE_METHODS.iter())
        .chain(ACP_CLIENT_METHODS.iter())
        .chain(HARN_SESSION_UPDATE_EXTENSIONS.iter())
        .chain(HARN_AGENT_EVENT_KINDS.iter())
        .chain(HARN_CONTENT_EXTENSION_FIELDS.iter())
        .chain(HARN_PROMPT_RESULT_EXTENSION_FIELDS.iter())
        .chain(HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS.iter())
    {
        assert!(rust.contains(value), "Rust artifact missing {value}");
    }
    for value in all_acp_session_updates() {
        assert!(rust.contains(&value), "Rust artifact missing {value}");
    }
}

#[test]
fn generated_rust_permission_shapes_round_trip() {
    use generated_rust_binding::{
        ACPPermissionOptionKind, ACPPermissionOutcome, ACPSessionRequestPermissionParams,
        ACPSessionRequestPermissionResult,
    };

    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/protocols/fixtures/acp/session_request_permission.valid.json"
    )))
    .expect("canonical permission fixture parses");
    let request = fixture["documents"][0]["params"].clone();
    let decoded: ACPSessionRequestPermissionParams =
        serde_json::from_value(request.clone()).expect("generated permission request decodes");
    assert_eq!(decoded.session_id, "session-1");
    assert_eq!(decoded.tool_call.tool_call_id, "tool-1");
    assert_eq!(decoded.options[0].kind, ACPPermissionOptionKind::AllowOnce);
    assert_eq!(
        serde_json::to_value(decoded).expect("generated permission request encodes"),
        request
    );

    let result = fixture["documents"][1]["result"].clone();
    let decoded: ACPSessionRequestPermissionResult =
        serde_json::from_value(result.clone()).expect("generated permission result decodes");
    assert_eq!(
        decoded.outcome,
        ACPPermissionOutcome::Selected {
            option_id: "allow".to_string()
        }
    );
    assert_eq!(
        serde_json::to_value(decoded).expect("generated permission result encodes"),
        result
    );
}

#[test]
fn generated_rust_agent_event_shapes_round_trip() {
    use generated_rust_binding::{
        HarnAgentEventKind, HarnAgentEventNotification, HarnAgentEventParams,
    };

    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/protocols/fixtures/acp/agent_event_ext_notifications.valid.json"
    )))
    .expect("canonical agent-event fixture parses");
    let mut notification = fixture["documents"]
        .as_array()
        .expect("agent-event documents")
        .iter()
        .find(|document| document["params"]["kind"] == "iteration_end")
        .expect("iteration_end fixture")
        .clone();
    notification["_harn"] = json!({"replayed": true});
    let decoded: HarnAgentEventNotification =
        serde_json::from_value(notification.clone()).expect("generated agent event decodes");
    assert_eq!(decoded.params.kind, HarnAgentEventKind::IterationEnd);
    assert_eq!(decoded.params.fields["iteration"], json!(0));
    assert_eq!(decoded.fields["_harn"]["replayed"], json!(true));
    assert_eq!(
        serde_json::to_value(decoded).expect("generated agent event encodes"),
        notification
    );

    let future = json!({
        "sessionId": "session-2",
        "kind": "future_agent_event",
        "payload": {"kept": true}
    });
    let decoded: HarnAgentEventParams =
        serde_json::from_value(future.clone()).expect("unknown agent event kind decodes");
    assert_eq!(
        decoded.kind,
        HarnAgentEventKind::Other("future_agent_event".to_string())
    );
    assert_eq!(
        serde_json::to_value(decoded).expect("unknown agent event kind encodes"),
        future
    );
}

#[test]
fn rust_const_name_sanitizes_wire_values() {
    assert_eq!(
        rust_const_name("ACP_AGENT_METHOD", "session/prompt"),
        "ACP_AGENT_METHOD_SESSION_PROMPT"
    );
    assert_eq!(
        rust_const_name("ACP_DISPATCHED_METHOD", "harn.workflow.signal"),
        "ACP_DISPATCHED_METHOD_HARN_WORKFLOW_SIGNAL"
    );
    assert_eq!(
        rust_const_name("ACP_CLIENT_METHOD", "fs/read_text_file"),
        "ACP_CLIENT_METHOD_FS_READ_TEXT_FILE"
    );
    // A wire value that is only separators collapses to just the prefix.
    assert_eq!(rust_const_name("X", "/"), "X");
    // A leading digit is escaped so the identifier stays valid.
    assert_eq!(rust_const_name("V", "2026-07-28"), "V_2026_07_28");
    assert_eq!(rust_type_name("iteration_end"), "IterationEnd");
    assert_eq!(rust_type_name("mcp_auth_required"), "McpAuthRequired");
}

#[test]
fn dispatched_acp_methods_match_artifact() {
    // The published ACP_DISPATCHED_METHODS slice must reflect the real
    // surface the adapter handles. If a contributor adds or removes a
    // `match method.as_str()` arm in the ACP adapter without updating the
    // artifact, this guard fails. We read the adapter source directly so
    // the check has no runtime dependency on a live server. The dispatch
    // `match` lives in the `dispatch` submodule of the ACP adapter.
    let dispatch = protocol_source()
        .read_text("crates/harn-serve/src/adapters/acp/dispatch.rs")
        .expect("read acp adapter");
    let body = dispatch
        .split_once("match method.as_str() {")
        .expect("dispatch match block")
        .1
        .split_once("\n            _ => {")
        .expect("dispatch wildcard arm")
        .0;
    let mut dispatched = BTreeSet::new();
    for line in body.lines() {
        let trimmed = line.trim();
        // Match-arm heads look like `"method" => {` or `"a" | "b" => {`.
        if !trimmed.contains("=>") || !trimmed.starts_with('"') {
            if let Some(method) = dispatch_arm_constant_value(trimmed) {
                dispatched.insert(method);
            }
            continue;
        }
        let arm = trimmed.split("=>").next().unwrap_or("");
        for literal in arm.split('|') {
            let name = literal.trim().trim_matches('"');
            if !name.is_empty() {
                dispatched.insert(name.to_string());
            }
        }
    }
    let published: BTreeSet<String> = ACP_DISPATCHED_METHODS
        .iter()
        .map(|m| m.to_string())
        .collect();
    assert_eq!(
        published,
        dispatched,
        "ACP_DISPATCHED_METHODS is out of sync with the ACP adapter dispatch arms.\n\
             missing from artifact: {:?}\n\
             stale in artifact: {:?}",
        dispatched.difference(&published).collect::<Vec<_>>(),
        published.difference(&dispatched).collect::<Vec<_>>(),
    );
}

fn dispatch_arm_constant_value(trimmed_arm: &str) -> Option<String> {
    let name = trimmed_arm.split("=>").next()?.trim();
    match name {
        "HARN_PROVIDER_CATALOG_METHOD" => Some(HARN_PROVIDER_CATALOG_METHOD.to_string()),
        "harn_vm::session_timeline::SESSION_TIMELINE_QUERY_METHOD" => {
            Some(SESSION_TIMELINE_QUERY_METHOD.to_string())
        }
        "harn_vm::session_timeline::SESSION_TIMELINE_SUBSCRIBE_METHOD" => {
            Some(SESSION_TIMELINE_SUBSCRIBE_METHOD.to_string())
        }
        "harn_vm::session_timeline::SESSION_TIMELINE_UNSUBSCRIBE_METHOD" => {
            Some(SESSION_TIMELINE_UNSUBSCRIBE_METHOD.to_string())
        }
        "harn_vm::orchestration::SESSION_VIEW_QUERY_METHOD" => {
            Some(SESSION_VIEW_QUERY_METHOD.to_string())
        }
        _ => None,
    }
}

#[test]
fn transport_control_acp_methods_match_artifact() {
    let sessions = protocol_source()
        .read_text("crates/harn-serve/src/adapters/acp/sessions.rs")
        .expect("read acp sessions");
    let body = sessions
        .split_once("pub(super) fn apply_session_budget_rearm")
        .expect("budget rearm function")
        .1
        .split_once("\nfn rearm_dimension")
        .expect("budget rearm function end")
        .0;
    let mut handled = BTreeSet::new();
    for capture in regex::Regex::new(r#""([^"]+)""#)
        .unwrap()
        .captures_iter(body)
    {
        let value = capture.get(1).unwrap().as_str();
        if value.starts_with("session/") {
            handled.insert(value.to_string());
        }
    }
    let published: BTreeSet<String> = ACP_TRANSPORT_CONTROL_METHODS
        .iter()
        .map(|m| m.to_string())
        .collect();
    assert_eq!(
        published, handled,
        "ACP_TRANSPORT_CONTROL_METHODS is out of sync with transport pre-dispatch control frames"
    );
}

#[test]
fn generated_python_includes_harn_wire_vocabularies() {
    let py = generate_python();
    assert!(py.contains("MCP_DRAFT_PROTOCOL_VERSION: str = \"DRAFT-2026-v1\""));
    assert!(py.contains("MCP_LEGACY_2025_06_18_PROTOCOL_VERSION: str = \"2025-06-18\""));
    assert!(py.contains("MCP_REQUIRED_METADATA_KEYS: tuple"));
    assert!(py.contains("class MCPDiscoverResult(_HarnDataclass):"));
    assert!(py.contains("class MCPInputRequiredResult(_HarnDataclass):"));
    assert!(py.contains("class MCPCacheScope(str, Enum):"));
    assert!(py.contains("class ACPSessionUpdate(str, Enum):"));
    assert!(py.contains("class HarnToolCallErrorCategory(str, Enum):"));
    assert!(py.contains("class HarnToolMutationStatus(str, Enum):"));
    assert!(py.contains("class HarnWorkerStatus(str, Enum):"));
    assert!(py.contains("changedPaths: Optional[List[str]] = None"));
    assert!(py.contains("mutationStatus: Optional[HarnToolMutationStatus] = None"));
    assert!(py.contains("class ToolCallReceipt(_HarnDataclass):"));
    assert!(py.contains("class ToolCallReceiptStatus(str, Enum):"));
    assert!(py.contains("class _HarnDataclass:"));
    assert!(py.contains("def is_request("));
    assert!(py.contains("class HarnACPPromptErrorData(_HarnDataclass):"));
    assert!(py.contains("class HarnACPPromptResult(_HarnDataclass):"));
    assert!(py.contains("class HarnAgentTerminalOutcome(_HarnDataclass):"));
    assert!(py.contains("class AgentTerminalClass(str, Enum):"));
    assert!(py.contains("class AgentTerminalKind(str, Enum):"));
    for value in HARN_SESSION_UPDATE_EXTENSIONS
        .iter()
        .chain(HARN_AGENT_EVENT_KINDS.iter())
        .chain(ACP_AGENT_METHODS.iter())
    {
        assert!(py.contains(value), "Python artifact missing {value}");
    }
    assert!(
        py.contains(HARN_PROVIDER_CATALOG_METHOD),
        "Python artifact missing {HARN_PROVIDER_CATALOG_METHOD}"
    );
    for value in worker_status_values() {
        assert!(py.contains(&value), "Python artifact missing {value}");
    }
}

#[test]
fn generated_go_includes_harn_wire_vocabularies() {
    let go = generate_go();
    assert!(go.contains("package harnprotocol"));
    assert!(go.contains("const MCPDraftProtocolVersion = \"DRAFT-2026-v1\""));
    assert!(go.contains("const MCPLegacy20250618ProtocolVersion = \"2025-06-18\""));
    assert!(go.contains("type MCPRequestMeta struct"));
    assert!(go.contains("type MCPDiscoverResult struct"));
    assert!(go.contains("type MCPInputRequiredResult struct"));
    assert!(go.contains("MCPUnsupportedProtocolVersionErrorCode"));
    assert!(go.contains("type JSONRPCID struct"));
    assert!(go.contains("type HarnToolMutationStatus = string"));
    assert!(go.contains("ChangedPaths"));
    assert!(go.contains("MutationStatus"));
    assert!(go.contains("type ACPSessionUpdateNotification struct"));
    assert!(go.contains("func IsRequest(envelope map[string]json.RawMessage)"));
    assert!(go.contains("type HarnWorkerStatus = string"));
    assert!(go.contains("var HarnWorkerStatuses = []HarnWorkerStatus"));
    assert!(go.contains("type ToolCallReceipt struct"));
    assert!(go.contains("var ToolCallReceiptStatuses = []ToolCallReceiptStatus"));
    assert!(go.contains("type HarnACPPromptErrorData struct"));
    assert!(go.contains("type HarnACPPromptResult struct"));
    assert!(go.contains("type HarnAgentTerminalOutcome struct"));
    assert!(go.contains("var AgentTerminalClasses = []AgentTerminalClass"));
    assert!(go.contains("var AgentTerminalKinds = []AgentTerminalKind"));
    for value in HARN_SESSION_UPDATE_EXTENSIONS
        .iter()
        .chain(HARN_AGENT_EVENT_KINDS.iter())
        .chain(ACP_AGENT_METHODS.iter())
    {
        assert!(go.contains(value), "Go artifact missing {value}");
    }
    assert!(
        go.contains(HARN_PROVIDER_CATALOG_METHOD),
        "Go artifact missing {HARN_PROVIDER_CATALOG_METHOD}"
    );
    for value in worker_status_values() {
        assert!(go.contains(&value), "Go artifact missing {value}");
    }
}

#[test]
fn acp_prompt_error_schema_matches_runtime_terminal_classes() {
    let schema: serde_json::Value = serde_json::from_str(
        &protocol_source()
            .read_text("conformance/protocols/schemas/acp-session-update.schema.json")
            .expect("read ACP schema"),
    )
    .expect("parse ACP schema");
    assert_eq!(
        schema["$defs"]["HarnPromptErrorData"]["properties"]["schema"]["const"],
        json!(ACP_PROMPT_ERROR_DATA_SCHEMA)
    );
    assert_eq!(
        schema["$defs"]["HarnPromptErrorData"]["properties"]["terminalClass"]["enum"],
        json!(agent_terminal_class_values())
    );
    assert_eq!(
        schema["$defs"]["AgentTerminalOutcome"]["properties"]["kind"]["enum"],
        json!(agent_terminal_kind_values())
    );
    assert_eq!(
        schema["$defs"]["AgentTerminalOutcome"]["properties"]["owner"]["enum"],
        json!(agent_terminal_owner_values())
    );
}

#[test]
fn go_struct_field_formatter_aligns_long_generated_fields() {
    let raw = "\
type Example struct {
\tA string `json:\"a\"`
\tMutationStatus *HarnToolMutationStatus `json:\"mutationStatus,omitempty\"`
\tRaw json.RawMessage `json:\"raw\"`
}
";
    let formatted = "\
type Example struct {
\tA              string                  `json:\"a\"`
\tMutationStatus *HarnToolMutationStatus `json:\"mutationStatus,omitempty\"`
\tRaw            json.RawMessage         `json:\"raw\"`
}
";
    assert_eq!(format_go_struct_fields(raw), formatted);
}

#[test]
fn generated_go_artifact_is_gofmt_stable_when_gofmt_is_available() {
    let go = generate_go_artifact().expect("generate Go artifact");
    let mut child = match Command::new("gofmt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("failed to spawn gofmt: {error}"),
    };

    child
        .stdin
        .as_mut()
        .expect("gofmt stdin")
        .write_all(go.as_bytes())
        .expect("write generated Go to gofmt");
    let output = child.wait_with_output().expect("wait for gofmt");
    assert!(
        output.status.success(),
        "gofmt failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("gofmt stdout utf8"),
        go,
        "generated Go protocol artifact must be gofmt-stable before it is written or checked"
    );
}

#[test]
fn round_trip_fixture_matches_python_and_go_field_set() {
    let fixture: serde_json::Value =
        serde_json::from_str(&generate_round_trip_fixture().expect("fixture"))
            .expect("fixture json");
    // The fixture's nested envelopes should reference vocabulary that the
    // Python and Go bindings know about. Catching drift here keeps the
    // round-trip checks meaningful even if a contributor extends the fixture
    // without adding the corresponding type/enum support.
    assert_eq!(
        fixture["envelopes"]["sessionUpdateNotification"]["params"]["update"]["sessionUpdate"],
        json!("tool_call")
    );
    assert_eq!(
        fixture["envelopes"]["agentEventNotification"]["method"],
        json!(HARN_AGENT_EVENT_METHOD)
    );
    assert_eq!(
        fixture["harnProviderCatalogMethod"],
        json!(HARN_PROVIDER_CATALOG_METHOD)
    );
    assert_eq!(
        fixture["envelopes"]["agentEventNotification"]["params"]["kind"],
        json!("composition_child_call")
    );
    assert_eq!(fixture["a2aTask"]["status"]["state"], json!("working"));
    assert_eq!(
        fixture["mcpDiscoverResult"]["supportedVersions"][0],
        json!(MCP_DRAFT_PROTOCOL_VERSION)
    );
    assert_eq!(
        fixture["mcpInputRequiredResult"]["resultType"],
        json!(MCP_INPUT_REQUIRED_RESULT_TYPE)
    );
    assert_eq!(
        fixture["mcpUnsupportedProtocolVersionError"]["error"]["code"],
        json!(MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE)
    );
    assert_eq!(
        fixture["envelopes"]["errorResponse"]["error"]["data"]["schema"],
        json!(ACP_PROMPT_ERROR_DATA_SCHEMA)
    );
    assert_eq!(
        fixture["envelopes"]["response"]["result"]["_meta"]["harn"]["terminal"]["kind"],
        "policy_budget"
    );
    assert_eq!(fixture["toolCallReceipt"]["schema_version"], json!(1));
}

#[test]
fn manifest_advertises_python_and_go_bindings() {
    let manifest: serde_json::Value =
        serde_json::from_str(&generate_manifest(&protocol_source()).expect("manifest"))
            .expect("manifest json");
    assert!(manifest["bindings"]["python"]["artifact"].is_string());
    assert!(manifest["bindings"]["go"]["artifact"].is_string());
    assert!(manifest["bindings"]["go"]["modulePath"].is_string());
    assert_eq!(
        manifest["bindings"]["rust"]["artifact"],
        json!("harn-protocol.rs")
    );
    assert_eq!(
        manifest["bindings"]["rust"]["vendorPath"],
        json!("protocol/src/generated.rs")
    );
    assert_eq!(
        manifest["bindings"]["rust"]["dependencies"],
        json!(["serde", "serde_json"])
    );
    assert_eq!(manifest["bindings"]["rust"]["stability"], json!("stable"));
    assert_eq!(
        manifest["acp"]["transportControlMethods"],
        json!(ACP_TRANSPORT_CONTROL_METHODS)
    );
    assert_eq!(
        manifest["acp"]["harnSessionTimelineMethods"],
        json!(HARN_SESSION_TIMELINE_METHODS)
    );
    assert_eq!(
        manifest["acp"]["promptResultExtensionFields"],
        json!(HARN_PROMPT_RESULT_EXTENSION_FIELDS)
    );
    assert!(
        manifest["acp"]["dispatchedMethods"]
            .as_array()
            .expect("dispatched methods")
            .iter()
            .any(|value| value == SESSION_TIMELINE_QUERY_METHOD),
        "manifest missing timeline query in dispatchedMethods"
    );
    assert!(
        manifest["acp"]["handledMethods"]
            .as_array()
            .expect("handled methods")
            .iter()
            .any(|value| value == "session/set_budget"),
        "manifest missing session/set_budget in handledMethods"
    );
    assert_eq!(
        manifest["bindings"]["typescript"]["stability"],
        json!("stable")
    );
    assert_eq!(
        manifest["mcp"]["draftProtocolVersion"],
        json!("DRAFT-2026-v1")
    );
    assert_eq!(
        manifest["mcp"]["legacy20250618ProtocolVersion"],
        json!("2025-06-18")
    );
    assert_eq!(
        manifest["mcp"]["unsupportedProtocolVersionError"]["code"],
        json!(MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE)
    );
    assert_eq!(
        manifest["mcp"]["jsonSchemaDialect"],
        json!(MCP_JSON_SCHEMA_2020_12_DIALECT)
    );
    assert_eq!(manifest["bindings"]["python"]["stability"], json!("stable"));
    assert_eq!(manifest["bindings"]["go"]["stability"], json!("stable"));
    assert_eq!(
        manifest["acp"]["promptErrorDataSchema"],
        json!(ACP_PROMPT_ERROR_DATA_SCHEMA)
    );
    assert_eq!(
        manifest["acp"]["promptErrorTerminalClasses"],
        json!(agent_terminal_class_values())
    );
    assert_eq!(
        manifest["receipts"]["toolCallReceiptSchemaVersion"],
        json!(TOOL_CALL_RECEIPT_SCHEMA_VERSION)
    );
    assert_eq!(
        manifest["acp"]["deprecatedAgentMethods"]["session/stop"]["replacement"],
        json!("session/close")
    );
}

#[test]
fn generated_manifest_references_schema_artifacts() {
    let manifest: serde_json::Value =
        serde_json::from_str(&generate_manifest(&protocol_source()).expect("manifest"))
            .expect("manifest json");
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
    assert!(
        manifest["schemas"]
            .as_array()
            .expect("schema array")
            .iter()
            .any(|entry| entry["artifact"] == TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT),
        "manifest missing {TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT}"
    );
}

#[test]
fn explicit_artifact_version_is_validated_and_stamped_everywhere() {
    const VERSION: &str = "9.8.7-beta.1";
    assert_eq!(
        resolve_artifact_version(None),
        Ok(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(resolve_artifact_version(Some(VERSION)), Ok(VERSION));
    assert!(resolve_artifact_version(Some("v9.8.7")).is_err());
    assert!(resolve_artifact_version(Some("next")).is_err());

    let artifacts = generate_artifacts(&protocol_source(), VERSION).expect("artifacts");
    for path in [
        "harn-protocol.ts",
        "HarnProtocol.swift",
        "harn-protocol.rs",
        "python/harn_protocol.py",
        "go/harnprotocol/harnprotocol.go",
    ] {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.relative_path == path)
            .unwrap_or_else(|| panic!("missing generated artifact {path}"));
        assert!(
            artifact.contents.contains(VERSION),
            "{path} did not use the explicit artifact version"
        );
    }

    for path in ["manifest.json", "fixtures/round_trip.json"] {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.relative_path == path)
            .unwrap_or_else(|| panic!("missing generated artifact {path}"));
        let value: serde_json::Value =
            serde_json::from_str(&artifact.contents).expect("generated JSON");
        assert_eq!(value["artifactVersion"], json!(VERSION));
    }
    let round_trip = artifacts
        .iter()
        .find(|artifact| artifact.relative_path == "fixtures/round_trip.json")
        .expect("round-trip fixture");
    let round_trip: serde_json::Value =
        serde_json::from_str(&round_trip.contents).expect("round-trip JSON");
    assert_eq!(
        round_trip["mcpDiscoverResult"]["serverInfo"]["version"],
        json!(VERSION)
    );
}

#[test]
fn committed_protocol_artifacts_match_generator() {
    let source = protocol_source();
    let artifacts = generate_artifacts(&source, env!("CARGO_PKG_VERSION")).expect("artifacts");
    let output_root = source.repo_root().join("spec/protocol-artifacts");
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

#[test]
fn repo_root_is_discovered_at_runtime_for_relocated_cli() {
    let checkout = tempfile::tempdir().expect("checkout");
    fs::write(checkout.path().join("Cargo.toml"), "[workspace]\n").expect("workspace manifest");
    let schemas = checkout.path().join("conformance/protocols/schemas");
    fs::create_dir_all(&schemas).expect("protocol schemas");
    let nested = checkout.path().join("crates/harn-cli");
    fs::create_dir_all(&nested).expect("nested command directory");

    assert_eq!(repo_root_from(&nested), Some(checkout.path().to_path_buf()));
}
