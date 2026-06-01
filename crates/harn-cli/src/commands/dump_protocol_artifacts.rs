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
    HARN_AGENT_EVENT_METHOD, HARN_CONTENT_EXTENSION_FIELDS, HARN_PROVIDER_CATALOG_METHOD,
    HARN_SESSION_UPDATE_EXTENSIONS, HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use harn_serve::{A2A_PROTOCOL_VERSION, MCP_PROTOCOL_VERSION};
use harn_vm::agent_events::{ToolCallErrorCategory, ToolCallStatus, WorkerEvent};
use harn_vm::llm::receipts::{
    tool_call_receipt_schema, TOOL_CALL_RECEIPT_EXECUTORS, TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT,
    TOOL_CALL_RECEIPT_SCHEMA_VERSION, TOOL_CALL_RECEIPT_STATUSES,
};
use harn_vm::tool_annotations::{SideEffectLevel, ToolKind};
use serde::Serialize;
use serde_json::json;

const ACP_AGENT_METHODS: &[&str] = &[
    "initialize",
    "session/inject",
    "session/new",
    "session/load",
    "session/replace_inject",
    "session/resume",
    "session/prompt",
    "session/revoke_inject",
    "session/truncate",
    "session/remind",
    "session/pending_injections",
    "session/revoke_reminder",
    "session/cancel_tool_call",
    "session/close",
    "session/stop",
];
const ACP_DEPRECATED_AGENT_METHODS: &[DeprecatedWireValue] = &[DeprecatedWireValue {
    value: "session/stop",
    replacement: "session/close",
}];
const ACP_CLIENT_METHODS: &[&str] = &[
    "fs/read_text_file",
    "fs/write_text_file",
    "terminal/create",
    "terminal/kill",
    "session/request_permission",
];
/// Every JSON-RPC method the ACP adapter's `dispatch` actually handles,
/// reconciled against the match arms in
/// `crates/harn-serve/src/adapters/acp/mod.rs`.
///
/// `ACP_AGENT_METHODS` is the stable, hand-curated host-facing contract that
/// the TypeScript/Swift/Python/Go bindings publish. The dispatcher additionally
/// services workspace-management, workflow-control, and HITL methods that those
/// bindings intentionally do not expose as typed enums yet. The Rust artifact
/// is the first binding to publish the *complete* handled surface so downstream
/// Rust hosts (Burin Code, harn-cloud) can route every method without
/// re-deriving it from the dispatcher by hand. Keep this list in lockstep with
/// the `match method.as_str()` arms; `dispatched_acp_methods_match_artifact`
/// guards against drift.
const ACP_DISPATCHED_METHODS: &[&str] = &[
    "initialize",
    "authenticate",
    HARN_PROVIDER_CATALOG_METHOD,
    "session/new",
    "session/load",
    "session/resume",
    "session/fork",
    "session/truncate",
    "session/set_mode",
    "session/set_config_option",
    "session/fs_mode",
    "session/fs_commit_staged",
    "session/restore_tool_call",
    "session/prompt",
    "session/cancel",
    "session/cancel_tool_call",
    "session/close",
    "session/stop",
    "session/inject",
    "session/revoke_inject",
    "session/replace_inject",
    "session/remind",
    "session/pending_injections",
    "session/revoke_reminder",
    "session/list",
    "agent/resume",
    "harn.hitl.respond",
    "workflow/signal",
    "harn.workflow.signal",
    "workflow/query",
    "harn.workflow.query",
    "workflow/update",
    "harn.workflow.update",
    "workflow/pause",
    "harn.workflow.pause",
    "workflow/resume",
    "harn.workflow.resume",
    "mcp/catalog",
    "harn.mcp.catalog",
    "mcp/authorize",
    "harn.mcp.authorize",
    "mcp/oauth_callback",
    "harn.mcp.oauth_callback",
    "mcp/import_token",
    "harn.mcp.import_token",
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

const MCP_DRAFT_PROTOCOL_VERSION: &str = "DRAFT-2026-v1";
const MCP_FINAL_2026_PROTOCOL_VERSION: &str = "2026-07-28";
const MCP_JSON_SCHEMA_2020_12_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
const MCP_PROTOCOL_VERSIONS: &[&str] = &[MCP_DRAFT_PROTOCOL_VERSION, MCP_PROTOCOL_VERSION];
const MCP_METHODS: &[&str] = &[
    "server/discover",
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
const MCP_REQUIRED_METADATA_KEYS: &[&str] = &[
    "io.modelcontextprotocol/protocolVersion",
    "io.modelcontextprotocol/clientInfo",
    "io.modelcontextprotocol/clientCapabilities",
];
const MCP_METADATA_KEYS: &[&str] = &[
    "io.modelcontextprotocol/protocolVersion",
    "io.modelcontextprotocol/clientInfo",
    "io.modelcontextprotocol/clientCapabilities",
    "io.modelcontextprotocol/logLevel",
    "progressToken",
    "traceparent",
    "tracestate",
    "baggage",
];
const MCP_STANDARD_HTTP_HEADERS: &[&str] = &["MCP-Protocol-Version", "Mcp-Method", "Mcp-Name"];
const MCP_CACHE_RESULT_FIELDS: &[&str] = &["ttlMs", "cacheScope"];
const MCP_CACHE_SCOPES: &[&str] = &["private", "public"];
const MCP_RESULT_TYPES: &[&str] = &["complete", "input_required"];
const MCP_INPUT_REQUIRED_RESULT_TYPE: &str = "input_required";
const MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE: i32 = -32004;
const MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE: &str = "Unsupported protocol version";
const MCP_OAUTH_CLIENT_REGISTRATION_MODES: &[&str] = &[
    "pre_registered",
    "client_id_metadata_document",
    "dynamic_client_registration",
    "manual",
];
const MCP_OAUTH_AUTH_MODES: &[&str] = &["cimd", "dcr", "static", "byo"];
const MCP_OAUTH_APPLICATION_TYPES: &[&str] = &["native", "web"];
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
    SchemaCopy {
        protocol: "mcp",
        source: "conformance/protocols/schemas/mcp-draft-2026-v1.schema.json",
        artifact: "schemas/mcp-draft-2026-v1.schema.json",
    },
];

struct SchemaCopy {
    protocol: &'static str,
    source: &'static str,
    artifact: &'static str,
}

struct DeprecatedWireValue {
    value: &'static str,
    replacement: &'static str,
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
        Artifact::new("harn-protocol.rs", generate_rust()),
        Artifact::new("python/harn_protocol.py", generate_python()),
        Artifact::new("python/__init__.py", PYTHON_INIT_STUB.to_string()),
        Artifact::new("go/harnprotocol/harnprotocol.go", generate_go()),
        Artifact::new("go/harnprotocol/go.mod", generate_go_mod()),
        Artifact::new("fixtures/round_trip.json", generate_round_trip_fixture()?),
        Artifact::new(
            TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT,
            generate_tool_call_receipt_schema()?,
        ),
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

fn generate_tool_call_receipt_schema() -> Result<String, String> {
    serde_json::to_string_pretty(&tool_call_receipt_schema())
        .map_err(|error| format!("failed to serialize tool-call receipt schema: {error}"))
}

const PYTHON_INIT_STUB: &str = "from .harn_protocol import *  # noqa: F401,F403\n";

/// Render the protocol-artifact manifest the running Harn would write.
///
/// Re-exposed via `harn package artifacts manifest` so downstream automation
/// (Burin Code, Harn Cloud) can compare against vendored copies without
/// shelling out to `dump-protocol-artifacts`.
pub(crate) fn manifest_json() -> Result<String, String> {
    generate_manifest()
}

fn generate_manifest() -> Result<String, String> {
    let mut schemas = SCHEMA_COPIES
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
    let receipt_schema = tool_call_receipt_schema();
    schemas.push(json!({
        "protocol": "harn",
        "source": "crates/harn-vm/src/llm/receipts.rs",
        "artifact": TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT,
        "provenance": receipt_schema
            .get("x-harn-provenance")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    }));

    serde_json::to_string_pretty(&json!({
        "schemaVersion": 1,
        "artifactVersion": env!("CARGO_PKG_VERSION"),
        "generatedBy": "harn dump-protocol-artifacts",
        "checkCommand": "make check-protocol-artifacts",
        "bindings": {
            "typescript": {
                "artifact": "harn-protocol.ts",
                "stability": "stable",
            },
            "swift": {
                "artifact": "HarnProtocol.swift",
                "stability": "stable",
            },
            "rust": {
                "artifact": "harn-protocol.rs",
                "vendorPath": "protocol/src/generated.rs",
                "stability": "stable",
            },
            "python": {
                "artifact": "python/harn_protocol.py",
                "minimumPythonVersion": "3.9",
                "stability": "stable",
            },
            "go": {
                "artifact": "go/harnprotocol/harnprotocol.go",
                "modulePath": "github.com/burin-labs/harn/spec/protocol-artifacts/go/harnprotocol",
                "stability": "stable",
            },
        },
        "schemas": schemas,
        "acp": {
            "schemaCompatibility": ACP_SCHEMA_COMPATIBILITY,
            "agentMethods": ACP_AGENT_METHODS,
            "deprecatedAgentMethods": deprecated_wire_values(ACP_DEPRECATED_AGENT_METHODS),
            "clientMethods": ACP_CLIENT_METHODS,
            "agentNotifications": ACP_AGENT_NOTIFICATIONS,
            "sessionUpdateVariants": all_acp_session_updates(),
            "harnSessionUpdateExtensions": HARN_SESSION_UPDATE_EXTENSIONS,
            "harnAgentEventMethod": HARN_AGENT_EVENT_METHOD,
            "harnProviderCatalogMethod": HARN_PROVIDER_CATALOG_METHOD,
            "harnAgentEventKinds": HARN_AGENT_EVENT_KINDS,
            "toolLifecycleExtensionFields": HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
            "contentExtensionFields": HARN_CONTENT_EXTENSION_FIELDS,
            "contentBlockTypes": ACP_CONTENT_BLOCK_TYPES,
            "toolKinds": tool_kind_values(),
            "toolCallStatuses": tool_call_status_values(),
            "toolCallErrorCategories": tool_call_error_category_values(),
            "toolExecutorSimpleValues": ACP_TOOL_EXECUTOR_SIMPLE_VALUES,
            "workerStatuses": worker_status_values(),
        },
        "a2a": {
            "protocolVersion": A2A_PROTOCOL_VERSION,
            "methods": A2A_METHODS,
            "taskStates": A2A_TASK_STATES,
            "taskEventTypes": A2A_TASK_EVENT_TYPES,
        },
        "mcp": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "stableProtocolVersion": MCP_PROTOCOL_VERSION,
            "draftProtocolVersion": MCP_DRAFT_PROTOCOL_VERSION,
            "final2026ProtocolVersion": MCP_FINAL_2026_PROTOCOL_VERSION,
            "protocolVersions": MCP_PROTOCOL_VERSIONS,
            "jsonSchemaDialect": MCP_JSON_SCHEMA_2020_12_DIALECT,
            "methods": MCP_METHODS,
            "requiredRequestMetadataKeys": MCP_REQUIRED_METADATA_KEYS,
            "metadataKeys": MCP_METADATA_KEYS,
            "standardHttpHeaders": MCP_STANDARD_HTTP_HEADERS,
            "cacheResultFields": MCP_CACHE_RESULT_FIELDS,
            "cacheScopes": MCP_CACHE_SCOPES,
            "resultTypes": MCP_RESULT_TYPES,
            "inputRequiredResultType": MCP_INPUT_REQUIRED_RESULT_TYPE,
            "unsupportedProtocolVersionError": {
                "code": MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE,
                "message": MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE,
            },
            "oauthClientRegistrationModes": MCP_OAUTH_CLIENT_REGISTRATION_MODES,
            "oauthAuthModes": MCP_OAUTH_AUTH_MODES,
            "oauthApplicationTypes": MCP_OAUTH_APPLICATION_TYPES,
            "loggingLevels": MCP_LOGGING_LEVELS,
        },
        "receipts": {
            "toolCallReceiptSchemaVersion": TOOL_CALL_RECEIPT_SCHEMA_VERSION,
            "toolCallReceiptSchema": TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT,
            "toolCallReceiptStatuses": TOOL_CALL_RECEIPT_STATUSES,
            "toolCallReceiptExecutors": TOOL_CALL_RECEIPT_EXECUTORS,
        },
    }))
    .map_err(|error| format!("failed to serialize manifest: {error}"))
}

fn deprecated_wire_values(values: &[DeprecatedWireValue]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for value in values {
        map.insert(
            value.value.to_string(),
            json!({
                "replacement": value.replacement,
                "removal": "after one release",
            }),
        );
    }
    serde_json::Value::Object(map)
}

fn generate_readme() -> String {
    format!(
        "# Harn Protocol Artifacts\n\n\
         <!-- GENERATED by `harn dump-protocol-artifacts` -- do not edit by hand. -->\n\n\
         This directory is the checked-in Harn protocol contract for downstream hosts.\n\
         It publishes JSON Schema profiles plus TypeScript, Swift, Rust, Python, and\n\
         Go bindings generated from Harn's adapter vocabulary. Hosts should consume or\n\
         vendor these artifacts directly instead of maintaining hand-written mirrors\n\
         of Harn wire enums, JSON-RPC envelopes, or extension fields.\n\n\
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
           profile (`{ACP_SCHEMA_COMPATIBILITY}`).\n\
         - `schemas/a2a-0.3.0.schema.json`: Harn's A2A schema profile (`{A2A_PROTOCOL_VERSION}`).\n\
         - `schemas/mcp-2025-11-25.schema.json`: Harn's MCP schema profile (`{MCP_PROTOCOL_VERSION}`).\n\
         - `schemas/mcp-draft-2026-v1.schema.json`: Harn's opt-in MCP RC schema\n\
           profile (`{MCP_DRAFT_PROTOCOL_VERSION}`), pinned beside the stable profile until the\n\
           final `2026-07-28` specification lands.\n\
         - `schemas/tool-call-receipt.schema.json`: Harn's typed, privacy-preserving\n\
           `ToolCallReceipt` schema for audited tool calls.\n\
         - `harn-protocol.ts`: TypeScript definitions for ACP session updates,\n\
           tool lifecycle metadata, A2A task events, and MCP metadata.\n\
         - `HarnProtocol.swift`: Swift definitions for the same host-facing surface.\n\
         - `harn-protocol.rs`: dependency-free Rust module of ACP method-name,\n\
           session-update discriminator, content-extension key, and protocol\n\
           version `pub const`s. The only binding that publishes the complete\n\
           dispatched ACP method surface; Burin Code vendors it as\n\
           `protocol/src/generated.rs`.\n\
         - `python/harn_protocol.py`: Python dataclasses, enums, and constants for\n\
           the same host-facing surface (Python 3.9+, stdlib-only).\n\
         - `go/harnprotocol/harnprotocol.go`: Go package with structs, typed string\n\
           aliases, and constants mirroring the Python and Swift bindings.\n\
         - `fixtures/round_trip.json`: representative JSON envelopes used by\n\
           `make check-bindings` to exercise Python and Go round-trips.\n\
         - `fixtures/mcp-rc/`: hand-authored MCP DRAFT-2026-v1 wire fixtures\n\
           (modern success, unsupported-version retry, cache hints,\n\
           input-required, header mismatch, no-session HTTP, recursive\n\
           `$defs` tool schema, legacy 2025-11-25 compat) replayed by\n\
           `make mcp-rc-conformance` and republished here for Burin Code\n\
           and harn-cloud test suites.\n\n\
         Compatibility rule: additive enum values and optional fields are minor-version\n\
         compatible; removing or renaming a wire value requires a Harn minor-version\n\
         migration note and a regenerated artifact diff.\n",
    )
}

fn generate_typescript() -> String {
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

fn generate_swift() -> String {
    let mut out = generated_header("harn dump-protocol-artifacts", "swift");
    out.push_str("import Foundation\n\n");
    out.push_str("public enum HarnProtocolConstants {\n");
    out.push_str(&format!(
        "    public static let artifactVersion = {}\n",
        json_string_literal(env!("CARGO_PKG_VERSION"))
    ));
    out.push_str(&format!(
        "    public static let acpSchemaCompatibility = {}\n",
        json_string_literal(ACP_SCHEMA_COMPATIBILITY)
    ));
    out.push_str(&format!(
        "    public static let harnAgentEventMethod = {}\n",
        json_string_literal(HARN_AGENT_EVENT_METHOD)
    ));
    out.push_str(&format!(
        "    public static let harnProviderCatalogMethod = {}\n",
        json_string_literal(HARN_PROVIDER_CATALOG_METHOD)
    ));
    for (name, value) in [
        ("mcpProtocolVersion", MCP_PROTOCOL_VERSION),
        ("mcpStableProtocolVersion", MCP_PROTOCOL_VERSION),
        ("mcpDraftProtocolVersion", MCP_DRAFT_PROTOCOL_VERSION),
        (
            "mcpFinal2026ProtocolVersion",
            MCP_FINAL_2026_PROTOCOL_VERSION,
        ),
        (
            "mcpJsonSchema202012Dialect",
            MCP_JSON_SCHEMA_2020_12_DIALECT,
        ),
    ] {
        out.push_str(&format!(
            "    public static let {name} = {}\n",
            json_string_literal(value)
        ));
    }
    out.push_str(&format!(
        "    public static let mcpUnsupportedProtocolVersionErrorCode = {MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE}\n"
    ));
    out.push_str(&format!(
        "    public static let mcpUnsupportedProtocolVersionErrorMessage = {}\n",
        json_string_literal(MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE)
    ));
    out.push_str(&swift_string_array(
        "mcpProtocolVersions",
        MCP_PROTOCOL_VERSIONS,
    ));
    out.push_str(&swift_string_array(
        "mcpRequiredMetadataKeys",
        MCP_REQUIRED_METADATA_KEYS,
    ));
    out.push_str(&swift_string_array("mcpMetadataKeys", MCP_METADATA_KEYS));
    out.push_str(&swift_string_array(
        "mcpStandardHTTPHeaders",
        MCP_STANDARD_HTTP_HEADERS,
    ));
    out.push_str(&swift_string_array(
        "mcpCacheResultFields",
        MCP_CACHE_RESULT_FIELDS,
    ));
    out.push_str(&swift_string_array(
        "mcpOAuthClientRegistrationModes",
        MCP_OAUTH_CLIENT_REGISTRATION_MODES,
    ));
    out.push_str(&swift_string_array(
        "mcpOAuthAuthModes",
        MCP_OAUTH_AUTH_MODES,
    ));
    out.push_str(&swift_string_array(
        "mcpOAuthApplicationTypes",
        MCP_OAUTH_APPLICATION_TYPES,
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
    out.push_str(&swift_string_array(
        "toolCallReceiptStatuses",
        TOOL_CALL_RECEIPT_STATUSES,
    ));
    out.push_str(&swift_string_array(
        "toolCallReceiptExecutors",
        TOOL_CALL_RECEIPT_EXECUTORS,
    ));
    out.push_str("}\n\n");

    out.push_str(&swift_enum_with_deprecations(
        "HarnACPAgentMethod",
        &strs_to_strings(ACP_AGENT_METHODS),
        ACP_DEPRECATED_AGENT_METHODS,
    ));
    out.push_str(&swift_enum(
        "HarnACPClientMethod",
        &strs_to_strings(ACP_CLIENT_METHODS),
    ));
    out.push_str(&swift_enum(
        "HarnACPAgentNotification",
        &strs_to_strings(ACP_AGENT_NOTIFICATIONS),
    ));
    out.push_str(&swift_enum(
        "HarnACPSessionUpdate",
        &all_acp_session_updates(),
    ));
    out.push_str(&swift_enum(
        "HarnACPContentBlockType",
        &strs_to_strings(ACP_CONTENT_BLOCK_TYPES),
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
    out.push_str(&swift_enum("HarnWorkerStatus", &worker_status_values()));
    out.push_str(&swift_enum(
        "HarnToolCallReceiptStatus",
        &strs_to_strings(TOOL_CALL_RECEIPT_STATUSES),
    ));
    out.push_str(&swift_enum(
        "HarnToolCallReceiptExecutor",
        &strs_to_strings(TOOL_CALL_RECEIPT_EXECUTORS),
    ));
    out.push_str(&swift_enum(
        "HarnA2ATaskState",
        &strs_to_strings(A2A_TASK_STATES),
    ));
    out.push_str(&swift_enum(
        "HarnA2ATaskEventType",
        &strs_to_strings(A2A_TASK_EVENT_TYPES),
    ));
    out.push_str(&swift_enum("HarnMCPMethod", &strs_to_strings(MCP_METHODS)));
    out.push_str(&swift_enum(
        "HarnMCPCacheScope",
        &strs_to_strings(MCP_CACHE_SCOPES),
    ));
    out.push_str(&swift_enum(
        "HarnMCPResultType",
        &strs_to_strings(MCP_RESULT_TYPES),
    ));
    out.push_str(&swift_enum(
        "HarnMCPLoggingLevel",
        &strs_to_strings(MCP_LOGGING_LEVELS),
    ));
    out.push_str(&swift_enum(
        "HarnMCPOAuthClientRegistrationMode",
        &strs_to_strings(MCP_OAUTH_CLIENT_REGISTRATION_MODES),
    ));
    out.push_str(&swift_enum(
        "HarnMCPOAuthAuthMode",
        &strs_to_strings(MCP_OAUTH_AUTH_MODES),
    ));
    out.push_str(&swift_enum(
        "HarnMCPOAuthApplicationType",
        &strs_to_strings(MCP_OAUTH_APPLICATION_TYPES),
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

    public init?(jsonEncodable value: Encodable) {
        let encoder = JSONEncoder()
        guard let data = try? encoder.encode(HarnAnyEncodable(value)),
              let object = try? JSONSerialization.jsonObject(with: data),
              let converted = HarnACPValue(jsonObject: object) else {
            return nil
        }
        self = converted
    }

    public init?(jsonObject: Any) {
        if let scalar = Self.jsonScalar(jsonObject) {
            self = scalar
        } else if let values = jsonObject as? [Any] {
            guard let array = Self.jsonArray(values) else { return nil }
            self = array
        } else if let values = jsonObject as? [String: Any] {
            guard let object = Self.jsonDictionary(values) else { return nil }
            self = object
        } else {
            return nil
        }
    }

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

    public var displayString: String {
        switch self {
        case .null: return "nil"
        case .bool(let value): return value ? "true" : "false"
        case .int(let value): return "\(value)"
        case .double(let value): return "\(value)"
        case .string(let value): return value
        case .array(let value): return "[\(value.map(\.displayString).joined(separator: ", "))]"
        case .object(let value):
            let pairs = value.sorted(by: { $0.key < $1.key })
                .map { "\($0.key): \($0.value.displayString)" }
            return "{\(pairs.joined(separator: ", "))}"
        }
    }

    public var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }

    public var intValue: Int? {
        if case .int(let value) = self { return value }
        if case .double(let value) = self, value.rounded() == value { return Int(value) }
        return nil
    }

    public var boolValue: Bool? {
        if case .bool(let value) = self { return value }
        return nil
    }

    public var arrayValue: [HarnACPValue]? {
        if case .array(let value) = self { return value }
        return nil
    }

    public var objectValue: [String: HarnACPValue]? {
        if case .object(let value) = self { return value }
        return nil
    }

    public subscript(_ key: String) -> HarnACPValue? {
        objectValue?[key]
    }

    private static func jsonScalar(_ jsonObject: Any) -> HarnACPValue? {
        switch jsonObject {
        case _ as NSNull: return .null
        case let value as NSNumber: return jsonNumber(value)
        case let value as String: return .string(value)
        default: return nil
        }
    }

    private static func jsonSignedInteger(_ value: Int64) -> HarnACPValue? {
        guard value <= Int64(Int.max), value >= Int64(Int.min) else { return nil }
        return .int(Int(value))
    }

    private static func jsonUnsignedInteger(_ value: UInt64) -> HarnACPValue {
        value <= UInt64(Int.max) ? .int(Int(value)) : .double(Double(value))
    }

    private static func jsonNumber(_ value: NSNumber) -> HarnACPValue? {
        #if canImport(Darwin)
        if CFGetTypeID(value) == CFBooleanGetTypeID() {
            return .bool(value.boolValue)
        }
        if CFNumberIsFloatType(value) {
            return .double(value.doubleValue)
        }
        #endif
        let objCType = String(cString: value.objCType)
        #if !canImport(Darwin)
        if objCType == "c" || objCType == "B" {
            return .bool(value.boolValue)
        }
        #endif
        if objCType == "f" || objCType == "d" {
            return .double(value.doubleValue)
        }
        if ["q", "l", "i", "s"].contains(objCType) {
            return jsonSignedInteger(value.int64Value)
        }
        if ["Q", "L", "I", "S"].contains(objCType) {
            return jsonUnsignedInteger(value.uint64Value)
        }
        return jsonSignedInteger(value.int64Value)
    }

    private static func jsonArray(_ values: [Any]) -> HarnACPValue? {
        var items: [HarnACPValue] = []
        items.reserveCapacity(values.count)
        for value in values {
            guard let item = HarnACPValue(jsonObject: value) else { return nil }
            items.append(item)
        }
        return .array(items)
    }

    private static func jsonDictionary(_ values: [String: Any]) -> HarnACPValue? {
        var fields: [String: HarnACPValue] = [:]
        fields.reserveCapacity(values.count)
        for (key, value) in values {
            guard let item = HarnACPValue(jsonObject: value) else { return nil }
            fields[key] = item
        }
        return .object(fields)
    }
}

public typealias HarnACPObject = [String: HarnACPValue]

private struct HarnAnyEncodable: Encodable {
    let value: Encodable

    init(_ value: Encodable) {
        self.value = value
    }

    func encode(to encoder: Encoder) throws {
        try value.encode(to: encoder)
    }
}

public enum HarnJsonRpcId: Codable, Sendable, Hashable, ExpressibleByIntegerLiteral, ExpressibleByStringLiteral {
    case null
    case int(Int)
    case string(String)

    public init(integerLiteral value: Int) {
        self = .int(value)
    }

    public init(stringLiteral value: String) {
        self = .string(value)
    }

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

    public var intValue: Int? {
        if case .int(let value) = self { return value }
        return nil
    }

    public var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
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

    public init(code: Int, message: String, data: HarnACPValue? = nil) {
        self.code = code
        self.message = message
        self.data = data
    }
}

public struct HarnACPResponse: Codable, Sendable, Equatable {
    public let jsonrpc: String
    public var id: HarnJsonRpcId
    public var result: HarnACPValue?
    public var error: HarnACPError?

    public init(
        jsonrpc: String = "2.0",
        id: HarnJsonRpcId,
        result: HarnACPValue? = nil,
        error: HarnACPError? = nil
    ) {
        self.jsonrpc = jsonrpc
        self.id = id
        self.result = result
        self.error = error
    }

    public static func success(id: HarnJsonRpcId, result: HarnACPValue) -> HarnACPResponse {
        HarnACPResponse(id: id, result: result)
    }

    public static func success(id: Int, result: HarnACPValue) -> HarnACPResponse {
        success(id: .int(id), result: result)
    }

    public static func error(
        id: HarnJsonRpcId,
        code: Int,
        message: String,
        data: HarnACPValue? = nil
    ) -> HarnACPResponse {
        HarnACPResponse(id: id, error: HarnACPError(code: code, message: message, data: data))
    }

    public static func error(
        id: Int,
        code: Int,
        message: String,
        data: HarnACPValue? = nil
    ) -> HarnACPResponse {
        error(id: .int(id), code: code, message: message, data: data)
    }
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

    public var displayLabel: String {
        switch self {
        case .harnBuiltin: return "harn_builtin"
        case .hostBridge: return "host_bridge"
        case .providerNative: return "provider_native"
        case .mcpServer(let name): return "mcp:\(name)"
        case .unknown(let raw): return raw
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

public struct HarnToolCallReceipt: Codable, Sendable, Equatable {
    public var schemaVersion: Int
    public var sessionId: String
    public var runId: String?
    public var toolCallId: String
    public var toolName: String
    public var iteration: Int
    public var turnIndex: Int?
    public var emitOrder: Int
    public var reason: String?
    public var kind: String?
    public var executor: HarnToolCallReceiptExecutor?
    public var status: HarnToolCallReceiptStatus
    public var errorCategory: String?
    public var durationMs: Int
    public var argsHash: String
    public var resultHash: String?
    public var audit: HarnACPValue
    public var emittedAt: String
    public var model: String?
    public var provider: String?

    enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case sessionId = "session_id"
        case runId = "run_id"
        case toolCallId = "tool_call_id"
        case toolName = "tool_name"
        case iteration
        case turnIndex = "turn_index"
        case emitOrder = "emit_order"
        case reason
        case kind
        case executor
        case status
        case errorCategory = "error_category"
        case durationMs = "duration_ms"
        case argsHash = "args_hash"
        case resultHash = "result_hash"
        case audit
        case emittedAt = "emitted_at"
        case model
        case provider
    }
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
    public var content: HarnACPValue?
    public var messageId: String?
    public var entries: [HarnACPValue]?
    public var keptTurnCount: Int?
    public var removedTurnCount: Int?
    public var newTipTurnId: String?
    public var reason: String?
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
        case messageId
        case entries
        case keptTurnCount
        case removedTurnCount
        case newTipTurnId
        case reason
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
    public var session: HarnACPObject?
    public var promptCapabilities: HarnPromptCapabilities?
    public var mcpCapabilities: HarnACPObject?
    public var sessionCapabilities: HarnACPObject?

    enum CodingKeys: String, CodingKey {
        case meta = "_meta"
        case loadSession
        case session
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

public typealias HarnMCPJsonSchema202012 = HarnACPObject

public struct HarnMCPImplementation: Codable, Sendable, Equatable {
    public var name: String
    public var version: String
    public var title: String?
    public var description: String?
    public var websiteUrl: String?
}

public struct HarnMCPRequestMeta: Codable, Sendable, Equatable {
    public var protocolVersion: String
    public var clientInfo: HarnMCPImplementation
    public var clientCapabilities: HarnACPObject
    public var logLevel: HarnMCPLoggingLevel?
    public var progressToken: HarnACPValue?
    public var traceparent: String?
    public var tracestate: String?
    public var baggage: String?

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "io.modelcontextprotocol/protocolVersion"
        case clientInfo = "io.modelcontextprotocol/clientInfo"
        case clientCapabilities = "io.modelcontextprotocol/clientCapabilities"
        case logLevel = "io.modelcontextprotocol/logLevel"
        case progressToken
        case traceparent
        case tracestate
        case baggage
    }
}

public struct HarnMCPHTTPHeaders: Codable, Sendable, Equatable {
    public var protocolVersion: String
    public var method: String
    public var name: String?

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "MCP-Protocol-Version"
        case method = "Mcp-Method"
        case name = "Mcp-Name"
    }
}

public struct HarnMCPCacheHints: Codable, Sendable, Equatable {
    public var ttlMs: Int
    public var cacheScope: HarnMCPCacheScope
}

public struct HarnMCPDiscoverResult: Codable, Sendable, Equatable {
    public var resultType: HarnMCPResultType
    public var supportedVersions: [String]
    public var capabilities: HarnACPObject
    public var serverInfo: HarnMCPImplementation
    public var instructions: String?
    public var meta: HarnACPObject?

    enum CodingKeys: String, CodingKey {
        case resultType
        case supportedVersions
        case capabilities
        case serverInfo
        case instructions
        case meta = "_meta"
    }
}

public struct HarnMCPInputRequiredResult: Codable, Sendable, Equatable {
    public var resultType: HarnMCPResultType
    public var inputRequests: HarnACPObject?
    public var requestState: String?
    public var meta: HarnACPObject?

    enum CodingKeys: String, CodingKey {
        case resultType
        case inputRequests
        case requestState
        case meta = "_meta"
    }
}

public struct HarnMCPUnsupportedProtocolVersionErrorData: Codable, Sendable, Equatable {
    public var requested: String
    public var supported: [String]
}

public struct HarnMCPUnsupportedProtocolVersionError: Codable, Sendable, Equatable {
    public var jsonrpc: String
    public var id: HarnJsonRpcId?
    public var error: HarnACPError
}

public struct HarnMCPTool: Codable, Sendable, Equatable {
    public var name: String
    public var title: String?
    public var description: String?
    public var inputSchema: HarnMCPJsonSchema202012
    public var outputSchema: HarnMCPJsonSchema202012?
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

public struct HarnMCPOAuthProtectedResourceMetadata: Codable, Sendable, Equatable {
    public var resource: String?
    public var authorizationServers: [String]
    public var scopesSupported: [String]?
    public var bearerMethodsSupported: [String]?

    enum CodingKeys: String, CodingKey {
        case resource
        case authorizationServers = "authorization_servers"
        case scopesSupported = "scopes_supported"
        case bearerMethodsSupported = "bearer_methods_supported"
    }
}

public struct HarnMCPOAuthAuthorizationServerMetadata: Codable, Sendable, Equatable {
    public var issuer: String
    public var authorizationEndpoint: String
    public var tokenEndpoint: String
    public var registrationEndpoint: String?
    public var tokenEndpointAuthMethodsSupported: [String]?
    public var codeChallengeMethodsSupported: [String]?
    public var scopesSupported: [String]?
    public var clientIdMetadataDocumentSupported: Bool?
    public var authorizationResponseIssParameterSupported: Bool?

    enum CodingKeys: String, CodingKey {
        case issuer
        case authorizationEndpoint = "authorization_endpoint"
        case tokenEndpoint = "token_endpoint"
        case registrationEndpoint = "registration_endpoint"
        case tokenEndpointAuthMethodsSupported = "token_endpoint_auth_methods_supported"
        case codeChallengeMethodsSupported = "code_challenge_methods_supported"
        case scopesSupported = "scopes_supported"
        case clientIdMetadataDocumentSupported = "client_id_metadata_document_supported"
        case authorizationResponseIssParameterSupported = "authorization_response_iss_parameter_supported"
    }
}

public struct HarnMCPOAuthWwwAuthenticateChallenge: Codable, Sendable, Equatable {
    public var scheme: String
    public var params: [String: String]
}

public struct HarnMCPOAuthDiscoveryResult: Codable, Sendable, Equatable {
    public var protectedResourceMetadataUrl: String
    public var protectedResourceMetadata: HarnMCPOAuthProtectedResourceMetadata
    public var authorizationServerIssuer: String
    public var authorizationServerMetadataUrl: String
    public var authorizationServerMetadataKind: String
    public var authorizationServerMetadata: HarnMCPOAuthAuthorizationServerMetadata
    public var challenge: HarnMCPOAuthWwwAuthenticateChallenge?
    public var scopes: [String]
}

public struct HarnMCPOAuthDynamicClientRegistrationRequest: Codable, Sendable, Equatable {
    public var clientName: String
    public var redirectUris: [String]
    public var grantTypes: [String]
    public var responseTypes: [String]
    public var tokenEndpointAuthMethod: String
    public var applicationType: HarnMCPOAuthApplicationType
    public var scope: String?

    enum CodingKeys: String, CodingKey {
        case clientName = "client_name"
        case redirectUris = "redirect_uris"
        case grantTypes = "grant_types"
        case responseTypes = "response_types"
        case tokenEndpointAuthMethod = "token_endpoint_auth_method"
        case applicationType = "application_type"
        case scope
    }
}
"#,
    );

    out
}

fn generate_python() -> String {
    let mut out = String::new();
    out.push_str("# GENERATED by `harn dump-protocol-artifacts` - do not edit by hand.\n");
    out.push_str("# Source: Harn adapter schemas and Rust wire vocabulary.\n");
    out.push_str("\"\"\"Python bindings for Harn's host/integrator protocol surface.\n\n");
    out.push_str("Mirrors the TypeScript and Swift artifacts generated alongside this\n");
    out.push_str("module. Field names match the wire JSON (camelCase) so dataclass\n");
    out.push_str("instances round-trip through ``json.dumps(asdict(obj))`` without a\n");
    out.push_str("custom encoder. Optional fields default to ``None`` and are stripped\n");
    out.push_str("by :func:`to_wire`.\n");
    out.push_str("\"\"\"\n\n");
    out.push_str("from __future__ import annotations\n\n");
    out.push_str("from dataclasses import asdict, dataclass, field, fields, is_dataclass\n");
    out.push_str("from enum import Enum\n");
    out.push_str("from typing import Any, Dict, List, Mapping, Optional, Type, TypeVar, Union\n\n");
    out.push_str("__all__ = [\n");
    let public_names = python_public_names();
    for name in &public_names {
        out.push_str("    ");
        out.push_str(&json_string_literal(name));
        out.push_str(",\n");
    }
    out.push_str("]\n\n");

    for (name, value) in [
        ("HARN_PROTOCOL_ARTIFACT_VERSION", env!("CARGO_PKG_VERSION")),
        ("HARN_AGENT_EVENT_METHOD", HARN_AGENT_EVENT_METHOD),
        ("HARN_PROVIDER_CATALOG_METHOD", HARN_PROVIDER_CATALOG_METHOD),
        ("ACP_SCHEMA_COMPATIBILITY", ACP_SCHEMA_COMPATIBILITY),
        ("A2A_PROTOCOL_VERSION", A2A_PROTOCOL_VERSION),
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
        (
            "MCP_INPUT_REQUIRED_RESULT_TYPE",
            MCP_INPUT_REQUIRED_RESULT_TYPE,
        ),
        (
            "MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE",
            MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE,
        ),
    ] {
        out.push_str(&format!("{name}: str = {}\n", json_string_literal(value)));
    }
    out.push_str(&format!(
        "MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE: int = {MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE}\n"
    ));
    out.push('\n');

    out.push_str(&py_const_tuple("ACP_AGENT_METHODS", ACP_AGENT_METHODS));
    out.push_str(&py_const_tuple("ACP_CLIENT_METHODS", ACP_CLIENT_METHODS));
    out.push_str(&py_const_tuple(
        "ACP_AGENT_NOTIFICATIONS",
        ACP_AGENT_NOTIFICATIONS,
    ));
    let session_updates = all_acp_session_updates();
    out.push_str(&py_const_tuple_owned(
        "ACP_SESSION_UPDATES",
        &session_updates,
    ));
    out.push_str(&py_const_tuple(
        "HARN_ACP_SESSION_UPDATE_EXTENSIONS",
        HARN_SESSION_UPDATE_EXTENSIONS,
    ));
    out.push_str(&py_const_tuple(
        "HARN_AGENT_EVENT_KINDS",
        HARN_AGENT_EVENT_KINDS,
    ));
    out.push_str(&py_const_tuple(
        "ACP_CONTENT_BLOCK_TYPES",
        ACP_CONTENT_BLOCK_TYPES,
    ));
    out.push_str(&py_const_tuple_owned("ACP_TOOL_KINDS", &tool_kind_values()));
    out.push_str(&py_const_tuple_owned(
        "ACP_TOOL_CALL_STATUSES",
        &tool_call_status_values(),
    ));
    out.push_str(&py_const_tuple_owned(
        "HARN_TOOL_CALL_ERROR_CATEGORIES",
        &tool_call_error_category_values(),
    ));
    out.push_str(&py_const_tuple_owned(
        "HARN_SIDE_EFFECT_LEVELS",
        &side_effect_level_values(),
    ));
    out.push_str(&py_const_tuple_owned(
        "HARN_WORKER_STATUSES",
        &worker_status_values(),
    ));
    out.push_str(&py_const_tuple(
        "HARN_TOOL_CALL_RECEIPT_STATUSES",
        TOOL_CALL_RECEIPT_STATUSES,
    ));
    out.push_str(&py_const_tuple(
        "HARN_TOOL_CALL_RECEIPT_EXECUTORS",
        TOOL_CALL_RECEIPT_EXECUTORS,
    ));
    out.push_str(&py_const_tuple(
        "HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS",
        HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
    ));
    out.push_str(&py_const_tuple(
        "HARN_CONTENT_EXTENSION_FIELDS",
        HARN_CONTENT_EXTENSION_FIELDS,
    ));
    out.push_str(&py_const_tuple("A2A_METHODS", A2A_METHODS));
    out.push_str(&py_const_tuple("A2A_TASK_STATES", A2A_TASK_STATES));
    out.push_str(&py_const_tuple(
        "A2A_TASK_EVENT_TYPES",
        A2A_TASK_EVENT_TYPES,
    ));
    out.push_str(&py_const_tuple(
        "MCP_PROTOCOL_VERSIONS",
        MCP_PROTOCOL_VERSIONS,
    ));
    out.push_str(&py_const_tuple("MCP_METHODS", MCP_METHODS));
    out.push_str(&py_const_tuple(
        "MCP_REQUIRED_METADATA_KEYS",
        MCP_REQUIRED_METADATA_KEYS,
    ));
    out.push_str(&py_const_tuple("MCP_METADATA_KEYS", MCP_METADATA_KEYS));
    out.push_str(&py_const_tuple(
        "MCP_STANDARD_HTTP_HEADERS",
        MCP_STANDARD_HTTP_HEADERS,
    ));
    out.push_str(&py_const_tuple(
        "MCP_CACHE_RESULT_FIELDS",
        MCP_CACHE_RESULT_FIELDS,
    ));
    out.push_str(&py_const_tuple("MCP_CACHE_SCOPES", MCP_CACHE_SCOPES));
    out.push_str(&py_const_tuple("MCP_RESULT_TYPES", MCP_RESULT_TYPES));
    out.push_str(&py_const_tuple("MCP_LOGGING_LEVELS", MCP_LOGGING_LEVELS));

    out.push_str(&py_str_enum("ACPAgentMethod", ACP_AGENT_METHODS));
    out.push_str(&py_str_enum("ACPClientMethod", ACP_CLIENT_METHODS));
    out.push_str(&py_str_enum(
        "ACPAgentNotification",
        ACP_AGENT_NOTIFICATIONS,
    ));
    out.push_str(&py_str_enum_owned("ACPSessionUpdate", &session_updates));
    out.push_str(&py_str_enum_owned("ACPToolKind", &tool_kind_values()));
    out.push_str(&py_str_enum_owned(
        "ACPToolCallStatus",
        &tool_call_status_values(),
    ));
    out.push_str(&py_str_enum_owned(
        "HarnToolCallErrorCategory",
        &tool_call_error_category_values(),
    ));
    out.push_str(&py_str_enum_owned(
        "HarnSideEffectLevel",
        &side_effect_level_values(),
    ));
    out.push_str(&py_str_enum_owned(
        "HarnWorkerStatus",
        &worker_status_values(),
    ));
    out.push_str(&py_str_enum(
        "ToolCallReceiptStatus",
        TOOL_CALL_RECEIPT_STATUSES,
    ));
    out.push_str(&py_str_enum(
        "ToolCallReceiptExecutor",
        TOOL_CALL_RECEIPT_EXECUTORS,
    ));
    out.push_str(&py_str_enum("A2ATaskState", A2A_TASK_STATES));
    out.push_str(&py_str_enum("A2ATaskEventType", A2A_TASK_EVENT_TYPES));
    out.push_str(&py_str_enum("MCPCacheScope", MCP_CACHE_SCOPES));
    out.push_str(&py_str_enum("MCPResultType", MCP_RESULT_TYPES));
    out.push_str(&py_str_enum("MCPLoggingLevel", MCP_LOGGING_LEVELS));

    out.push_str(PYTHON_TYPE_DEFINITIONS);
    out
}

const PYTHON_TYPE_DEFINITIONS: &str = r#"

JsonValue = Union[None, bool, int, float, str, List[Any], Dict[str, Any]]
JsonObject = Dict[str, JsonValue]
JsonRpcId = Union[int, str, None]


_T = TypeVar("_T", bound="_HarnDataclass")


class _HarnDataclass:
    """Mixin providing wire-faithful ``to_wire`` / ``from_wire`` helpers.

    ``to_wire`` strips fields that are ``None`` so the JSON envelope stays
    minimal and matches what the Rust adapters emit. ``from_wire`` accepts a
    decoded JSON object plus any unknown extension fields, ignoring keys
    that the static schema does not yet know about so additive enum or
    struct extensions stay forward-compatible.
    """

    @classmethod
    def from_wire(cls: Type[_T], data: Mapping[str, Any]) -> _T:
        if not isinstance(data, Mapping):
            raise TypeError(f"{cls.__name__}.from_wire expected a mapping, got {type(data).__name__}")
        accepted = {f.name for f in fields(cls)}  # type: ignore[arg-type]
        kwargs = {key: value for key, value in data.items() if key in accepted}
        return cls(**kwargs)

    def to_wire(self) -> JsonObject:
        if not is_dataclass(self):
            raise TypeError("to_wire requires a dataclass instance")
        return _strip_none(asdict(self))


def _strip_none(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: _strip_none(item) for key, item in value.items() if item is not None}
    if isinstance(value, list):
        return [_strip_none(item) for item in value]
    if isinstance(value, Enum):
        return value.value
    return value


@dataclass
class ACPError(_HarnDataclass):
    code: int
    message: str
    data: Optional[JsonValue] = None


@dataclass
class ACPRequest(_HarnDataclass):
    id: JsonRpcId
    method: str
    params: Optional[JsonValue] = None
    jsonrpc: str = "2.0"


@dataclass
class ACPResponse(_HarnDataclass):
    id: JsonRpcId
    result: Optional[JsonValue] = None
    error: Optional[ACPError] = None
    jsonrpc: str = "2.0"


@dataclass
class ACPNotification(_HarnDataclass):
    method: str
    params: Optional[JsonValue] = None
    jsonrpc: str = "2.0"


ACPMessage = Union[ACPRequest, ACPResponse, ACPNotification]


def is_request(message: Mapping[str, Any]) -> bool:
    return "id" in message and "method" in message


def is_response(message: Mapping[str, Any]) -> bool:
    return "id" in message and "method" not in message


def is_notification(message: Mapping[str, Any]) -> bool:
    return "id" not in message and "method" in message


@dataclass
class HarnExtensionMeta(_HarnDataclass):
    harn: Optional[JsonObject] = None


@dataclass
class ACPContentBlock(_HarnDataclass):
    type: str
    text: Optional[str] = None
    _meta: Optional[HarnExtensionMeta] = None


@dataclass
class HarnToolLifecycleMeta(_HarnDataclass):
    audit: Optional[JsonValue] = None
    durationMs: Optional[float] = None
    error: Optional[str] = None
    errorCategory: Optional[str] = None
    executionDurationMs: Optional[float] = None
    executor: Optional[JsonValue] = None
    parsing: Optional[bool] = None
    rawInputPartial: Optional[str] = None


@dataclass
class ToolCallReceipt(_HarnDataclass):
    schema_version: int
    session_id: str
    tool_call_id: str
    tool_name: str
    iteration: int
    emit_order: int
    status: str
    duration_ms: int
    args_hash: str
    audit: JsonValue
    emitted_at: str
    run_id: Optional[str] = None
    turn_index: Optional[int] = None
    reason: Optional[str] = None
    kind: Optional[str] = None
    executor: Optional[str] = None
    error_category: Optional[str] = None
    result_hash: Optional[str] = None
    model: Optional[str] = None
    provider: Optional[str] = None

    def to_wire(self) -> JsonObject:
        return asdict(self)


@dataclass
class ACPToolCall(_HarnDataclass):
    sessionUpdate: str
    toolCallId: str
    title: str
    kind: Optional[str] = None
    status: Optional[str] = None
    content: Optional[List[ACPContentBlock]] = None
    locations: Optional[List[JsonValue]] = None
    rawInput: Optional[JsonValue] = None
    rawOutput: Optional[JsonValue] = None
    _meta: Optional[HarnExtensionMeta] = None


@dataclass
class ACPToolCallUpdate(_HarnDataclass):
    sessionUpdate: str
    toolCallId: str
    title: Optional[str] = None
    kind: Optional[str] = None
    status: Optional[str] = None
    content: Optional[List[ACPContentBlock]] = None
    locations: Optional[List[JsonValue]] = None
    rawInput: Optional[JsonValue] = None
    rawOutput: Optional[JsonValue] = None
    _meta: Optional[HarnExtensionMeta] = None


@dataclass
class ACPSessionUpdateEnvelope(_HarnDataclass):
    sessionUpdate: str
    content: Optional[JsonValue] = None
    messageId: Optional[str] = None
    entries: Optional[List[JsonValue]] = None
    keptTurnCount: Optional[int] = None
    removedTurnCount: Optional[int] = None
    newTipTurnId: Optional[str] = None
    reason: Optional[str] = None
    toolCallId: Optional[str] = None
    title: Optional[str] = None
    kind: Optional[str] = None
    status: Optional[str] = None
    rawInput: Optional[JsonValue] = None
    rawOutput: Optional[JsonValue] = None
    _meta: Optional[HarnExtensionMeta] = None


@dataclass
class ACPSessionUpdateParams(_HarnDataclass):
    sessionId: str
    update: ACPSessionUpdateEnvelope


@dataclass
class ACPSessionUpdateNotification(_HarnDataclass):
    params: ACPSessionUpdateParams
    method: str = "session/update"
    jsonrpc: str = "2.0"


@dataclass
class HarnAgentEventNotification(_HarnDataclass):
    params: JsonObject
    method: str = HARN_AGENT_EVENT_METHOD
    jsonrpc: str = "2.0"


@dataclass
class HarnToolArgSchema(_HarnDataclass):
    path_params: List[str] = field(default_factory=list)
    arg_aliases: Dict[str, str] = field(default_factory=dict)
    required: List[str] = field(default_factory=list)


@dataclass
class HarnToolAnnotations(_HarnDataclass):
    kind: str
    side_effect_level: str
    arg_schema: HarnToolArgSchema
    capabilities: Dict[str, List[str]] = field(default_factory=dict)
    emits_artifacts: bool = False
    result_readers: List[str] = field(default_factory=list)
    inline_result: bool = False


@dataclass
class A2AMessage(_HarnDataclass):
    id: str
    role: str
    parts: List[JsonValue] = field(default_factory=list)


@dataclass
class A2ATaskStatus(_HarnDataclass):
    state: str
    message: Optional[A2AMessage] = None
    timestamp: Optional[str] = None


@dataclass
class A2ATask(_HarnDataclass):
    id: str
    status: A2ATaskStatus
    contextId: Optional[str] = None
    history: Optional[List[A2AMessage]] = None
    artifacts: Optional[List[JsonValue]] = None
    metadata: Optional[JsonObject] = None


MCPJsonSchema202012 = JsonObject
MCPRequestMeta = JsonObject
MCPHTTPHeaders = Dict[str, str]


@dataclass
class MCPImplementation(_HarnDataclass):
    name: str
    version: str
    title: Optional[str] = None
    description: Optional[str] = None
    websiteUrl: Optional[str] = None


@dataclass
class MCPCacheHints(_HarnDataclass):
    ttlMs: int
    cacheScope: str


@dataclass
class MCPDiscoverResult(_HarnDataclass):
    resultType: str
    supportedVersions: List[str]
    capabilities: JsonObject
    serverInfo: MCPImplementation
    instructions: Optional[str] = None
    _meta: Optional[JsonObject] = None


@dataclass
class MCPInputRequiredResult(_HarnDataclass):
    resultType: str
    inputRequests: Optional[JsonObject] = None
    requestState: Optional[str] = None
    _meta: Optional[JsonObject] = None


@dataclass
class MCPUnsupportedProtocolVersionErrorData(_HarnDataclass):
    requested: str
    supported: List[str]


@dataclass
class MCPUnsupportedProtocolVersionError(_HarnDataclass):
    error: ACPError
    id: JsonRpcId = None
    jsonrpc: str = "2.0"


@dataclass
class MCPTool(_HarnDataclass):
    name: str
    inputSchema: MCPJsonSchema202012
    title: Optional[str] = None
    description: Optional[str] = None
    outputSchema: Optional[MCPJsonSchema202012] = None
    annotations: Optional[JsonObject] = None


@dataclass
class MCPResource(_HarnDataclass):
    uri: str
    name: str
    title: Optional[str] = None
    description: Optional[str] = None
    mimeType: Optional[str] = None


@dataclass
class MCPResourceTemplate(_HarnDataclass):
    uriTemplate: str
    name: str
    title: Optional[str] = None
    description: Optional[str] = None
    mimeType: Optional[str] = None


@dataclass
class MCPPrompt(_HarnDataclass):
    name: str
    title: Optional[str] = None
    description: Optional[str] = None
    arguments: Optional[List[JsonObject]] = None
"#;

fn python_public_names() -> Vec<String> {
    let names = [
        "HARN_PROTOCOL_ARTIFACT_VERSION",
        "HARN_AGENT_EVENT_METHOD",
        "HARN_PROVIDER_CATALOG_METHOD",
        "ACP_SCHEMA_COMPATIBILITY",
        "A2A_PROTOCOL_VERSION",
        "MCP_PROTOCOL_VERSION",
        "MCP_STABLE_PROTOCOL_VERSION",
        "MCP_DRAFT_PROTOCOL_VERSION",
        "MCP_FINAL_2026_PROTOCOL_VERSION",
        "MCP_JSON_SCHEMA_2020_12_DIALECT",
        "MCP_INPUT_REQUIRED_RESULT_TYPE",
        "MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE",
        "MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE",
        "ACP_AGENT_METHODS",
        "ACP_CLIENT_METHODS",
        "ACP_AGENT_NOTIFICATIONS",
        "ACP_SESSION_UPDATES",
        "HARN_ACP_SESSION_UPDATE_EXTENSIONS",
        "HARN_AGENT_EVENT_KINDS",
        "ACP_CONTENT_BLOCK_TYPES",
        "ACP_TOOL_KINDS",
        "ACP_TOOL_CALL_STATUSES",
        "HARN_TOOL_CALL_ERROR_CATEGORIES",
        "HARN_SIDE_EFFECT_LEVELS",
        "HARN_WORKER_STATUSES",
        "HARN_TOOL_CALL_RECEIPT_STATUSES",
        "HARN_TOOL_CALL_RECEIPT_EXECUTORS",
        "HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS",
        "HARN_CONTENT_EXTENSION_FIELDS",
        "A2A_METHODS",
        "A2A_TASK_STATES",
        "A2A_TASK_EVENT_TYPES",
        "MCP_PROTOCOL_VERSIONS",
        "MCP_METHODS",
        "MCP_REQUIRED_METADATA_KEYS",
        "MCP_METADATA_KEYS",
        "MCP_STANDARD_HTTP_HEADERS",
        "MCP_CACHE_RESULT_FIELDS",
        "MCP_CACHE_SCOPES",
        "MCP_RESULT_TYPES",
        "MCP_LOGGING_LEVELS",
        "ACPAgentMethod",
        "ACPClientMethod",
        "ACPAgentNotification",
        "ACPSessionUpdate",
        "ACPToolKind",
        "ACPToolCallStatus",
        "HarnToolCallErrorCategory",
        "HarnSideEffectLevel",
        "HarnWorkerStatus",
        "ToolCallReceiptStatus",
        "ToolCallReceiptExecutor",
        "A2ATaskState",
        "A2ATaskEventType",
        "MCPCacheScope",
        "MCPResultType",
        "MCPLoggingLevel",
        "ACPError",
        "ACPRequest",
        "ACPResponse",
        "ACPNotification",
        "ACPMessage",
        "HarnExtensionMeta",
        "ACPContentBlock",
        "HarnToolLifecycleMeta",
        "ToolCallReceipt",
        "ACPToolCall",
        "ACPToolCallUpdate",
        "ACPSessionUpdateEnvelope",
        "ACPSessionUpdateParams",
        "ACPSessionUpdateNotification",
        "HarnAgentEventNotification",
        "HarnToolArgSchema",
        "HarnToolAnnotations",
        "A2AMessage",
        "A2ATaskStatus",
        "A2ATask",
        "MCPJsonSchema202012",
        "MCPRequestMeta",
        "MCPHTTPHeaders",
        "MCPImplementation",
        "MCPCacheHints",
        "MCPDiscoverResult",
        "MCPInputRequiredResult",
        "MCPUnsupportedProtocolVersionErrorData",
        "MCPUnsupportedProtocolVersionError",
        "MCPTool",
        "MCPResource",
        "MCPResourceTemplate",
        "MCPPrompt",
        "JsonValue",
        "JsonObject",
        "JsonRpcId",
        "is_request",
        "is_response",
        "is_notification",
    ];
    names.into_iter().map(String::from).collect()
}

fn py_const_tuple(name: &str, values: &[&str]) -> String {
    py_const_tuple_owned(name, &strs_to_strings(values))
}

fn py_const_tuple_owned(name: &str, values: &[String]) -> String {
    let mut out = format!("{name}: tuple = (\n");
    for value in values {
        out.push_str("    ");
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str(")\n");
    out
}

fn py_str_enum(name: &str, values: &[&str]) -> String {
    py_str_enum_owned(name, &strs_to_strings(values))
}

fn py_str_enum_owned(name: &str, values: &[String]) -> String {
    let mut out = format!("\n\nclass {name}(str, Enum):\n");
    for value in values {
        let case = py_enum_member_name(value);
        out.push_str("    ");
        out.push_str(&case);
        out.push_str(" = ");
        out.push_str(&json_string_literal(value));
        out.push('\n');
    }
    out
}

fn py_enum_member_name(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_uppercase());
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    let collapsed = collapse_repeated_underscores(&trimmed);
    if collapsed.is_empty() {
        "UNKNOWN".to_string()
    } else if collapsed
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_digit())
    {
        format!("_{collapsed}")
    } else {
        collapsed
    }
}

fn collapse_repeated_underscores(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_underscore = false;
    for ch in value.chars() {
        if ch == '_' {
            if !last_underscore {
                out.push(ch);
            }
            last_underscore = true;
        } else {
            out.push(ch);
            last_underscore = false;
        }
    }
    out
}

fn generate_go() -> String {
    let mut out = String::new();
    out.push_str("// GENERATED by `harn dump-protocol-artifacts` - do not edit by hand.\n");
    out.push_str("// Source: Harn adapter schemas and Rust wire vocabulary.\n\n");
    out.push_str("// Package harnprotocol mirrors the host/integrator surface generated for\n");
    out.push_str("// TypeScript, Swift, and Python. Field names match the wire JSON; optional\n");
    out.push_str("// fields use pointer types or `omitempty` so encoding/json round-trips\n");
    out.push_str("// produce minimal envelopes equivalent to the Rust adapters.\n");
    out.push_str("package harnprotocol\n\n");
    out.push_str("import \"encoding/json\"\n\n");

    for (doc, name, value) in [
        (
            "// ArtifactVersion pins the Harn release that generated this binding.\n",
            "ArtifactVersion",
            env!("CARGO_PKG_VERSION"),
        ),
        (
            "// HarnAgentEventMethod is the JSON-RPC method for `_harn/agentEvent` notifications.\n",
            "HarnAgentEventMethod",
            HARN_AGENT_EVENT_METHOD,
        ),
        (
            "// HarnProviderCatalogMethod is the JSON-RPC method for Harn's provider catalog extension.\n",
            "HarnProviderCatalogMethod",
            HARN_PROVIDER_CATALOG_METHOD,
        ),
        (
            "// ACPSchemaCompatibility is the upstream ACP schema version Harn tracks.\n",
            "ACPSchemaCompatibility",
            ACP_SCHEMA_COMPATIBILITY,
        ),
        (
            "// A2AProtocolVersion is the A2A protocol version Harn implements.\n",
            "A2AProtocolVersion",
            A2A_PROTOCOL_VERSION,
        ),
        (
            "// MCPProtocolVersion is the MCP protocol version Harn implements.\n",
            "MCPProtocolVersion",
            MCP_PROTOCOL_VERSION,
        ),
        (
            "// MCPStableProtocolVersion is the stable MCP protocol version Harn implements at runtime.\n",
            "MCPStableProtocolVersion",
            MCP_PROTOCOL_VERSION,
        ),
        (
            "// MCPDraftProtocolVersion is the opt-in MCP release-candidate profile identity.\n",
            "MCPDraftProtocolVersion",
            MCP_DRAFT_PROTOCOL_VERSION,
        ),
        (
            "// MCPFinal2026ProtocolVersion is the scheduled final identity for the RC profile.\n",
            "MCPFinal2026ProtocolVersion",
            MCP_FINAL_2026_PROTOCOL_VERSION,
        ),
        (
            "// MCPJSONSchema202012Dialect is the JSON Schema dialect used by the MCP RC artifact profile.\n",
            "MCPJSONSchema202012Dialect",
            MCP_JSON_SCHEMA_2020_12_DIALECT,
        ),
        (
            "// MCPInputRequiredResultType is the resultType discriminator for multi round-trip requests.\n",
            "MCPInputRequiredResultType",
            MCP_INPUT_REQUIRED_RESULT_TYPE,
        ),
        (
            "// MCPUnsupportedProtocolVersionErrorMessage is the standard message for unsupported versions.\n",
            "MCPUnsupportedProtocolVersionErrorMessage",
            MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE,
        ),
    ] {
        out.push_str(doc);
        out.push_str(&format!(
            "const {name} = {}\n\n",
            json_string_literal(value)
        ));
    }
    out.push_str(&format!(
        "// MCPUnsupportedProtocolVersionErrorCode is the JSON-RPC server error for unsupported versions.\nconst MCPUnsupportedProtocolVersionErrorCode = {MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE}\n\n"
    ));

    out.push_str(&go_typed_array(
        "ACPAgentMethod",
        "ACPAgentMethods",
        ACP_AGENT_METHODS,
    ));
    out.push_str(&go_typed_array(
        "ACPClientMethod",
        "ACPClientMethods",
        ACP_CLIENT_METHODS,
    ));
    out.push_str(&go_typed_array(
        "ACPAgentNotification",
        "ACPAgentNotifications",
        ACP_AGENT_NOTIFICATIONS,
    ));
    let session_updates = all_acp_session_updates();
    out.push_str(&go_typed_array_owned(
        "ACPSessionUpdate",
        "ACPSessionUpdates",
        &session_updates,
    ));
    out.push_str(&go_typed_array(
        "HarnACPSessionUpdateExtension",
        "HarnACPSessionUpdateExtensions",
        HARN_SESSION_UPDATE_EXTENSIONS,
    ));
    out.push_str(&go_typed_array(
        "HarnAgentEventKind",
        "HarnAgentEventKinds",
        HARN_AGENT_EVENT_KINDS,
    ));
    out.push_str(&go_typed_array(
        "ACPContentBlockType",
        "ACPContentBlockTypes",
        ACP_CONTENT_BLOCK_TYPES,
    ));
    out.push_str(&go_typed_array_owned(
        "ACPToolKind",
        "ACPToolKinds",
        &tool_kind_values(),
    ));
    out.push_str(&go_typed_array_owned(
        "ACPToolCallStatus",
        "ACPToolCallStatuses",
        &tool_call_status_values(),
    ));
    out.push_str(&go_typed_array_owned(
        "HarnToolCallErrorCategory",
        "HarnToolCallErrorCategories",
        &tool_call_error_category_values(),
    ));
    out.push_str(&go_typed_array_owned(
        "HarnSideEffectLevel",
        "HarnSideEffectLevels",
        &side_effect_level_values(),
    ));
    out.push_str(&go_typed_array_owned(
        "HarnWorkerStatus",
        "HarnWorkerStatuses",
        &worker_status_values(),
    ));
    out.push_str(&go_typed_array(
        "ToolCallReceiptStatus",
        "ToolCallReceiptStatuses",
        TOOL_CALL_RECEIPT_STATUSES,
    ));
    out.push_str(&go_typed_array(
        "ToolCallReceiptExecutor",
        "ToolCallReceiptExecutors",
        TOOL_CALL_RECEIPT_EXECUTORS,
    ));
    out.push_str(&go_typed_array(
        "A2ATaskState",
        "A2ATaskStates",
        A2A_TASK_STATES,
    ));
    out.push_str(&go_typed_array(
        "A2ATaskEventType",
        "A2ATaskEventTypes",
        A2A_TASK_EVENT_TYPES,
    ));
    out.push_str(&go_typed_array(
        "MCPProtocolVersionValue",
        "MCPProtocolVersions",
        MCP_PROTOCOL_VERSIONS,
    ));
    out.push_str(&go_typed_array("MCPMethod", "MCPMethods", MCP_METHODS));
    out.push_str(&go_typed_array(
        "MCPCacheScope",
        "MCPCacheScopes",
        MCP_CACHE_SCOPES,
    ));
    out.push_str(&go_typed_array(
        "MCPResultType",
        "MCPResultTypes",
        MCP_RESULT_TYPES,
    ));
    out.push_str(&go_typed_array(
        "MCPLoggingLevel",
        "MCPLoggingLevels",
        MCP_LOGGING_LEVELS,
    ));
    out.push_str(&go_string_array(
        "HarnToolLifecycleExtensionFields",
        HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
    ));
    out.push_str(&go_string_array(
        "HarnContentExtensionFields",
        HARN_CONTENT_EXTENSION_FIELDS,
    ));
    out.push_str(&go_string_array("A2AMethods", A2A_METHODS));
    out.push_str(&go_string_array(
        "MCPRequiredMetadataKeys",
        MCP_REQUIRED_METADATA_KEYS,
    ));
    out.push_str(&go_string_array("MCPMetadataKeys", MCP_METADATA_KEYS));
    out.push_str(&go_string_array(
        "MCPStandardHTTPHeaders",
        MCP_STANDARD_HTTP_HEADERS,
    ));
    out.push_str(&go_string_array(
        "MCPCacheResultFields",
        MCP_CACHE_RESULT_FIELDS,
    ));

    out.push_str(GO_TYPE_DEFINITIONS);
    out
}

const GO_TYPE_DEFINITIONS: &str = r#"// JSONValue is the Go counterpart of the TypeScript ACPValue / Python JsonValue
// type. Concrete fields generally use json.RawMessage so callers can defer
// decoding to their own dataclasses.
type JSONValue = json.RawMessage

// JSONObject mirrors `Record<string, JsonValue>`.
type JSONObject = map[string]json.RawMessage

// JSONRPCID encodes a JSON-RPC id, which may be an integer, a string, or null.
// Use NewJSONRPCIDInt / NewJSONRPCIDString to construct, or NullJSONRPCID for
// the null variant.
type JSONRPCID struct {
	raw     json.RawMessage
	present bool
}

// NewJSONRPCIDInt builds a JSON-RPC id from an integer.
func NewJSONRPCIDInt(value int64) JSONRPCID {
	bytes, _ := json.Marshal(value)
	return JSONRPCID{raw: bytes, present: true}
}

// NewJSONRPCIDString builds a JSON-RPC id from a string.
func NewJSONRPCIDString(value string) JSONRPCID {
	bytes, _ := json.Marshal(value)
	return JSONRPCID{raw: bytes, present: true}
}

// NullJSONRPCID returns a JSON-RPC id explicitly encoded as null.
func NullJSONRPCID() JSONRPCID {
	return JSONRPCID{raw: json.RawMessage("null"), present: true}
}

// IsPresent reports whether the id field was present on the wire.
func (id JSONRPCID) IsPresent() bool { return id.present }

// MarshalJSON implements json.Marshaler.
func (id JSONRPCID) MarshalJSON() ([]byte, error) {
	if !id.present {
		return []byte("null"), nil
	}
	return id.raw, nil
}

// UnmarshalJSON implements json.Unmarshaler.
func (id *JSONRPCID) UnmarshalJSON(data []byte) error {
	id.raw = append(id.raw[:0], data...)
	id.present = true
	return nil
}

// Raw exposes the encoded bytes for callers that need to inspect the variant.
func (id JSONRPCID) Raw() json.RawMessage { return id.raw }

// ACPRequest is a JSON-RPC request envelope.
type ACPRequest struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      JSONRPCID       `json:"id"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params,omitempty"`
}

// ACPError is the JSON-RPC error sub-envelope.
type ACPError struct {
	Code    int             `json:"code"`
	Message string          `json:"message"`
	Data    json.RawMessage `json:"data,omitempty"`
}

// ACPResponse is a JSON-RPC response envelope.
type ACPResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      JSONRPCID       `json:"id"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *ACPError       `json:"error,omitempty"`
}

// ACPNotification is a JSON-RPC notification envelope.
type ACPNotification struct {
	JSONRPC string          `json:"jsonrpc"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params,omitempty"`
}

// HarnExtensionMeta is the canonical envelope for `_meta.harn` payloads.
type HarnExtensionMeta struct {
	Harn JSONObject `json:"harn,omitempty"`
}

// ACPContentBlock is one element inside a session update or message content array.
type ACPContentBlock struct {
	Type string             `json:"type"`
	Text *string            `json:"text,omitempty"`
	Meta *HarnExtensionMeta `json:"_meta,omitempty"`
}

// HarnToolLifecycleMeta is the Harn-specific tool-call lifecycle metadata
// living under `_meta.harn` on tool_call / tool_call_update notifications.
type HarnToolLifecycleMeta struct {
	Audit               json.RawMessage `json:"audit,omitempty"`
	DurationMs          *float64        `json:"durationMs,omitempty"`
	Error               *string         `json:"error,omitempty"`
	ErrorCategory       *string         `json:"errorCategory,omitempty"`
	ExecutionDurationMs *float64        `json:"executionDurationMs,omitempty"`
	Executor            json.RawMessage `json:"executor,omitempty"`
	Parsing             *bool           `json:"parsing,omitempty"`
	RawInputPartial     *string         `json:"rawInputPartial,omitempty"`
}

// ToolCallReceipt is the typed, privacy-preserving receipt emitted for an
// audited tool call.
type ToolCallReceipt struct {
	SchemaVersion int                      `json:"schema_version"`
	SessionID     string                   `json:"session_id"`
	RunID         *string                  `json:"run_id"`
	ToolCallID    string                   `json:"tool_call_id"`
	ToolName      string                   `json:"tool_name"`
	Iteration     uint64                   `json:"iteration"`
	TurnIndex     *uint64                  `json:"turn_index"`
	EmitOrder     uint64                   `json:"emit_order"`
	Reason        *string                  `json:"reason"`
	Kind          *string                  `json:"kind"`
	Executor      *ToolCallReceiptExecutor `json:"executor"`
	Status        ToolCallReceiptStatus    `json:"status"`
	ErrorCategory *string                  `json:"error_category"`
	DurationMs    uint64                   `json:"duration_ms"`
	ArgsHash      string                   `json:"args_hash"`
	ResultHash    *string                  `json:"result_hash"`
	Audit         json.RawMessage          `json:"audit"`
	EmittedAt     string                   `json:"emitted_at"`
	Model         *string                  `json:"model"`
	Provider      *string                  `json:"provider"`
}

// ACPToolCall is the `tool_call` session update.
type ACPToolCall struct {
	SessionUpdate string             `json:"sessionUpdate"`
	ToolCallID    string             `json:"toolCallId"`
	Title         string             `json:"title"`
	Kind          *string            `json:"kind,omitempty"`
	Status        *string            `json:"status,omitempty"`
	Content       []ACPContentBlock  `json:"content,omitempty"`
	Locations     []json.RawMessage  `json:"locations,omitempty"`
	RawInput      json.RawMessage    `json:"rawInput,omitempty"`
	RawOutput     json.RawMessage    `json:"rawOutput,omitempty"`
	Meta          *HarnExtensionMeta `json:"_meta,omitempty"`
}

// ACPToolCallUpdate is the `tool_call_update` session update.
type ACPToolCallUpdate struct {
	SessionUpdate string             `json:"sessionUpdate"`
	ToolCallID    string             `json:"toolCallId"`
	Title         *string            `json:"title,omitempty"`
	Kind          *string            `json:"kind,omitempty"`
	Status        *string            `json:"status,omitempty"`
	Content       []ACPContentBlock  `json:"content,omitempty"`
	Locations     []json.RawMessage  `json:"locations,omitempty"`
	RawInput      json.RawMessage    `json:"rawInput,omitempty"`
	RawOutput     json.RawMessage    `json:"rawOutput,omitempty"`
	Meta          *HarnExtensionMeta `json:"_meta,omitempty"`
}

// ACPSessionUpdateEnvelope is the discriminated `update` payload carried inside
// an ACP `session/update` notification. Only fields relevant to a given
// `sessionUpdate` discriminator will be populated; the rest stay zero-value
// and are stripped via `omitempty` on serialization.
type ACPSessionUpdateEnvelope struct {
	SessionUpdate    string             `json:"sessionUpdate"`
	Content          json.RawMessage    `json:"content,omitempty"`
	MessageID        *string            `json:"messageId,omitempty"`
	Entries          []json.RawMessage  `json:"entries,omitempty"`
	KeptTurnCount    *int               `json:"keptTurnCount,omitempty"`
	RemovedTurnCount *int               `json:"removedTurnCount,omitempty"`
	NewTipTurnID     *string            `json:"newTipTurnId,omitempty"`
	Reason           *string            `json:"reason,omitempty"`
	ToolCallID       *string            `json:"toolCallId,omitempty"`
	Title            *string            `json:"title,omitempty"`
	Kind             *string            `json:"kind,omitempty"`
	Status           *string            `json:"status,omitempty"`
	RawInput         json.RawMessage    `json:"rawInput,omitempty"`
	RawOutput        json.RawMessage    `json:"rawOutput,omitempty"`
	Meta             *HarnExtensionMeta `json:"_meta,omitempty"`
}

// ACPSessionUpdateParams is the params payload of `session/update`.
type ACPSessionUpdateParams struct {
	SessionID string                   `json:"sessionId"`
	Update    ACPSessionUpdateEnvelope `json:"update"`
}

// ACPSessionUpdateNotification is the full `session/update` envelope.
type ACPSessionUpdateNotification struct {
	JSONRPC string                 `json:"jsonrpc"`
	Method  string                 `json:"method"`
	Params  ACPSessionUpdateParams `json:"params"`
}

// HarnAgentEventNotification is the `_harn/agentEvent` envelope.
type HarnAgentEventNotification struct {
	JSONRPC string     `json:"jsonrpc"`
	Method  string     `json:"method"`
	Params  JSONObject `json:"params"`
}

// HarnToolArgSchema describes the static slice of a Harn tool's argument shape.
type HarnToolArgSchema struct {
	PathParams []string          `json:"path_params"`
	ArgAliases map[string]string `json:"arg_aliases"`
	Required   []string          `json:"required"`
}

// HarnToolAnnotations describes Harn-side metadata for a tool.
type HarnToolAnnotations struct {
	Kind            string              `json:"kind"`
	SideEffectLevel string              `json:"side_effect_level"`
	ArgSchema       HarnToolArgSchema   `json:"arg_schema"`
	Capabilities    map[string][]string `json:"capabilities"`
	EmitsArtifacts  bool                `json:"emits_artifacts"`
	ResultReaders   []string            `json:"result_readers"`
	InlineResult    bool                `json:"inline_result"`
}

// A2AMessage is one message inside an A2A task history.
type A2AMessage struct {
	ID    string            `json:"id"`
	Role  string            `json:"role"`
	Parts []json.RawMessage `json:"parts"`
}

// A2ATaskStatus is the status block on an A2A task.
type A2ATaskStatus struct {
	State     string      `json:"state"`
	Message   *A2AMessage `json:"message,omitempty"`
	Timestamp *string     `json:"timestamp,omitempty"`
}

// A2ATask is the canonical A2A task envelope.
type A2ATask struct {
	ID        string            `json:"id"`
	ContextID *string           `json:"contextId,omitempty"`
	Status    A2ATaskStatus     `json:"status"`
	History   []A2AMessage      `json:"history,omitempty"`
	Artifacts []json.RawMessage `json:"artifacts,omitempty"`
	Metadata  JSONObject        `json:"metadata,omitempty"`
}

// MCPJSONSchema202012 is the JSON Schema dialect surface used by the MCP RC
// tool input/output schema profile.
type MCPJSONSchema202012 = JSONObject

// MCPImplementation describes an MCP client or server implementation.
type MCPImplementation struct {
	Name        string  `json:"name"`
	Version     string  `json:"version"`
	Title       *string `json:"title,omitempty"`
	Description *string `json:"description,omitempty"`
	WebsiteURL  *string `json:"websiteUrl,omitempty"`
}

// MCPRequestMeta is the per-request metadata required by the MCP RC profile.
type MCPRequestMeta struct {
	ProtocolVersion    string            `json:"io.modelcontextprotocol/protocolVersion"`
	ClientInfo         MCPImplementation `json:"io.modelcontextprotocol/clientInfo"`
	ClientCapabilities JSONObject        `json:"io.modelcontextprotocol/clientCapabilities"`
	LogLevel           *MCPLoggingLevel  `json:"io.modelcontextprotocol/logLevel,omitempty"`
	ProgressToken      json.RawMessage   `json:"progressToken,omitempty"`
	Traceparent        *string           `json:"traceparent,omitempty"`
	Tracestate         *string           `json:"tracestate,omitempty"`
	Baggage            *string           `json:"baggage,omitempty"`
}

// MCPHTTPHeaders names the HTTP headers used by the MCP RC Streamable HTTP profile.
type MCPHTTPHeaders struct {
	ProtocolVersion string  `json:"MCP-Protocol-Version"`
	Method          string  `json:"Mcp-Method"`
	Name            *string `json:"Mcp-Name,omitempty"`
}

// MCPCacheHints captures the RC ttlMs/cacheScope cache fields.
type MCPCacheHints struct {
	TTLMS      uint64        `json:"ttlMs"`
	CacheScope MCPCacheScope `json:"cacheScope"`
}

// MCPDiscoverResult is the result returned by server/discover.
type MCPDiscoverResult struct {
	ResultType        MCPResultType     `json:"resultType"`
	SupportedVersions []string          `json:"supportedVersions"`
	Capabilities      JSONObject        `json:"capabilities"`
	ServerInfo        MCPImplementation `json:"serverInfo"`
	Instructions      *string           `json:"instructions,omitempty"`
	Meta              JSONObject        `json:"_meta,omitempty"`
}

// MCPInputRequiredResult carries server-to-client requests for multi round-trip calls.
type MCPInputRequiredResult struct {
	ResultType    MCPResultType `json:"resultType"`
	InputRequests JSONObject    `json:"inputRequests,omitempty"`
	RequestState  *string       `json:"requestState,omitempty"`
	Meta          JSONObject    `json:"_meta,omitempty"`
}

// MCPUnsupportedProtocolVersionErrorData is the typed data payload for -32004.
type MCPUnsupportedProtocolVersionErrorData struct {
	Requested string   `json:"requested"`
	Supported []string `json:"supported"`
}

// MCPUnsupportedProtocolVersionError is the JSON-RPC error response shape for
// an unsupported MCP protocol version.
type MCPUnsupportedProtocolVersionError struct {
	JSONRPC string    `json:"jsonrpc"`
	ID      JSONRPCID `json:"id,omitempty"`
	Error   ACPError  `json:"error"`
}

// MCPTool is the MCP `tools/list` entry.
type MCPTool struct {
	Name         string              `json:"name"`
	Title        *string             `json:"title,omitempty"`
	Description  *string             `json:"description,omitempty"`
	InputSchema  MCPJSONSchema202012 `json:"inputSchema"`
	OutputSchema MCPJSONSchema202012 `json:"outputSchema,omitempty"`
	Annotations  JSONObject          `json:"annotations,omitempty"`
}

// MCPResource is the MCP `resources/list` entry.
type MCPResource struct {
	URI         string  `json:"uri"`
	Name        string  `json:"name"`
	Title       *string `json:"title,omitempty"`
	Description *string `json:"description,omitempty"`
	MimeType    *string `json:"mimeType,omitempty"`
}

// MCPResourceTemplate is the MCP `resources/templates/list` entry.
type MCPResourceTemplate struct {
	URITemplate string  `json:"uriTemplate"`
	Name        string  `json:"name"`
	Title       *string `json:"title,omitempty"`
	Description *string `json:"description,omitempty"`
	MimeType    *string `json:"mimeType,omitempty"`
}

// MCPPrompt is the MCP `prompts/list` entry.
type MCPPrompt struct {
	Name        string       `json:"name"`
	Title       *string      `json:"title,omitempty"`
	Description *string      `json:"description,omitempty"`
	Arguments   []JSONObject `json:"arguments,omitempty"`
}

// IsRequest reports whether a decoded JSON-RPC envelope is a request.
func IsRequest(envelope map[string]json.RawMessage) bool {
	_, hasID := envelope["id"]
	_, hasMethod := envelope["method"]
	return hasID && hasMethod
}

// IsResponse reports whether a decoded JSON-RPC envelope is a response.
func IsResponse(envelope map[string]json.RawMessage) bool {
	_, hasID := envelope["id"]
	_, hasMethod := envelope["method"]
	return hasID && !hasMethod
}

// IsNotification reports whether a decoded JSON-RPC envelope is a notification.
func IsNotification(envelope map[string]json.RawMessage) bool {
	_, hasID := envelope["id"]
	_, hasMethod := envelope["method"]
	return !hasID && hasMethod
}
"#;

fn generate_go_mod() -> String {
    "module github.com/burin-labs/harn/spec/protocol-artifacts/go/harnprotocol\n\n\
     go 1.22\n"
        .to_string()
}

fn go_typed_array(type_name: &str, slice_name: &str, values: &[&str]) -> String {
    go_typed_array_owned(type_name, slice_name, &strs_to_strings(values))
}

fn go_typed_array_owned(type_name: &str, slice_name: &str, values: &[String]) -> String {
    let mut out =
        format!("// {type_name} is the typed alias for the {slice_name} wire vocabulary.\n");
    out.push_str(&format!("type {type_name} = string\n\n"));
    out.push_str(&format!(
        "// {slice_name} enumerates every wire value Harn currently emits for {type_name}.\n"
    ));
    out.push_str(&format!("var {slice_name} = []{type_name}{{\n"));
    for value in values {
        out.push('\t');
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("}\n\n");
    out
}

fn go_string_array(name: &str, values: &[&str]) -> String {
    let mut out = format!("// {name} enumerates the wire values Harn currently emits.\n");
    out.push_str(&format!("var {name} = []string{{\n"));
    for value in values {
        out.push('\t');
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("}\n\n");
    out
}

/// Emit the dependency-free Rust binding.
///
/// Unlike the Swift/Go/Python artifacts (which publish the full typed envelope
/// surface), the Rust artifact is intentionally a flat, allocation-free module
/// of `pub const` wire-name vocabulary plus the published slices. Burin Code
/// vendors it verbatim as `protocol/src/generated.rs` and routes/matches on the
/// constants instead of hand-maintaining a parallel method enum — the exact
/// HARN_BOUNDARY "don't mirror Harn wire vocabulary by hand" fix.
fn generate_rust() -> String {
    let mut out = String::new();
    out.push_str("// GENERATED by `harn dump-protocol-artifacts` - do not edit by hand.\n");
    out.push_str("// Source: Harn adapter schemas and Rust wire vocabulary.\n\n");
    out.push_str(
        "//! Dependency-free Rust bindings for Harn's host/integrator protocol surface.\n",
    );
    out.push_str("//!\n");
    out.push_str("//! Mirrors the TypeScript, Swift, Python, and Go artifacts generated\n");
    out.push_str("//! alongside this module. Every value is the literal JSON wire string Harn\n");
    out.push_str("//! emits, so downstream Rust hosts can route and match on these constants\n");
    out.push_str("//! instead of hand-maintaining a parallel method list. Adding a wire value\n");
    out.push_str("//! is additive and minor-version compatible.\n");
    out.push_str("#![allow(dead_code)]\n\n");

    out.push_str("/// Harn release that generated this binding.\n");
    out.push_str(&format!(
        "pub const HARN_PROTOCOL_ARTIFACT_VERSION: &str = {};\n\n",
        json_string_literal(env!("CARGO_PKG_VERSION"))
    ));
    out.push_str("/// Upstream ACP schema version Harn tracks.\n");
    out.push_str(&format!(
        "pub const ACP_SCHEMA_COMPATIBILITY: &str = {};\n\n",
        json_string_literal(ACP_SCHEMA_COMPATIBILITY)
    ));
    out.push_str("/// JSON-RPC method for `_harn/agentEvent` extension notifications.\n");
    out.push_str(&format!(
        "pub const HARN_AGENT_EVENT_METHOD: &str = {};\n\n",
        json_string_literal(HARN_AGENT_EVENT_METHOD)
    ));
    out.push_str("/// JSON-RPC method for Harn's provider catalog extension.\n");
    out.push_str(&format!(
        "pub const HARN_PROVIDER_CATALOG_METHOD: &str = {};\n\n",
        json_string_literal(HARN_PROVIDER_CATALOG_METHOD)
    ));

    out.push_str(&rust_const_group(
        "ACP_AGENT_METHOD",
        "ACP_AGENT_METHODS",
        "Stable host-facing ACP agent methods (matches the TypeScript/Swift/Python/Go bindings).",
        ACP_AGENT_METHODS,
    ));
    out.push_str(&rust_const_group(
        "ACP_DISPATCHED_METHOD",
        "ACP_DISPATCHED_METHODS",
        "Every JSON-RPC method the ACP adapter actually dispatches, including the \
         workspace-management, workflow-control, and HITL methods the stable \
         bindings do not yet expose as typed enums. Reconciled against the \
         `match` arms in `harn-serve`'s ACP adapter.",
        ACP_DISPATCHED_METHODS,
    ));
    out.push_str(&rust_const_group(
        "ACP_CLIENT_METHOD",
        "ACP_CLIENT_METHODS",
        "ACP client methods the agent calls back into the host for.",
        ACP_CLIENT_METHODS,
    ));
    out.push_str(&rust_const_group(
        "ACP_AGENT_NOTIFICATION",
        "ACP_AGENT_NOTIFICATIONS",
        "ACP notifications the agent emits to the host.",
        ACP_AGENT_NOTIFICATIONS,
    ));

    let session_updates = all_acp_session_updates();
    out.push_str(&rust_const_group_owned(
        "ACP_SESSION_UPDATE",
        "ACP_SESSION_UPDATES",
        "Every `session/update` discriminator Harn emits (canonical ACP variants \
         plus Harn extensions), the union the Swift binding exposes as \
         `acpSessionUpdateExtensions` merged with the base ACP set.",
        &session_updates,
    ));
    out.push_str(&rust_const_group(
        "HARN_ACP_SESSION_UPDATE_EXTENSION",
        "HARN_ACP_SESSION_UPDATE_EXTENSIONS",
        "Harn-specific `session/update` discriminators beyond the canonical ACP \
         set (the values Swift publishes as `acpSessionUpdateExtensions`).",
        HARN_SESSION_UPDATE_EXTENSIONS,
    ));
    out.push_str(&rust_const_group(
        "HARN_AGENT_EVENT_KIND",
        "HARN_AGENT_EVENT_KINDS",
        "Pipeline-loop milestone kinds emitted via `_harn/agentEvent`.",
        HARN_AGENT_EVENT_KINDS,
    ));
    out.push_str(&rust_const_group(
        "HARN_CONTENT_EXTENSION_FIELD",
        "HARN_CONTENT_EXTENSION_FIELDS",
        "`_meta.harn` content-block extension keys (`visible_text` / \
         `visible_delta`) Harn attaches to streamed content.",
        HARN_CONTENT_EXTENSION_FIELDS,
    ));
    out.push_str(&rust_const_group(
        "HARN_TOOL_LIFECYCLE_EXTENSION_FIELD",
        "HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS",
        "`_meta.harn` tool-lifecycle extension keys on tool_call / \
         tool_call_update notifications.",
        HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
    ));

    out
}

/// Render a `pub const` per wire value plus a published slice of all of them.
fn rust_const_group(const_prefix: &str, slice_name: &str, doc: &str, values: &[&str]) -> String {
    rust_const_group_owned(const_prefix, slice_name, doc, &strs_to_strings(values))
}

fn rust_const_group_owned(
    const_prefix: &str,
    slice_name: &str,
    doc: &str,
    values: &[String],
) -> String {
    let mut out = String::new();
    for value in values {
        out.push_str(&format!(
            "pub const {}: &str = {};\n",
            rust_const_name(const_prefix, value),
            json_string_literal(value)
        ));
    }
    out.push('\n');
    out.push_str(&rust_doc_comment(doc));
    out.push_str(&format!("pub const {slice_name}: &[&str] = &[\n"));
    for value in values {
        out.push_str("    ");
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("];\n\n");
    out
}

/// Build a valid SCREAMING_SNAKE_CASE Rust const identifier from a wire value.
///
/// Wire values carry `/`, `.`, `-`, and `_` separators (e.g. `session/prompt`,
/// `harn.hitl.respond`, `tool_call_update`); all collapse to `_`, runs are
/// merged, and a leading digit is prefixed with `_`.
fn rust_const_name(prefix: &str, value: &str) -> String {
    let mut suffix = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            suffix.extend(ch.to_uppercase());
        } else {
            suffix.push('_');
        }
    }
    let suffix = collapse_repeated_underscores(suffix.trim_matches('_'));
    let name = if suffix.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}_{suffix}")
    };
    if name.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("_{name}")
    } else {
        name
    }
}

fn rust_doc_comment(doc: &str) -> String {
    let mut out = String::new();
    // Collapse the whitespace that Rust string continuations introduce so the
    // emitted doc comment is a single tidy line.
    let normalized = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    out.push_str("/// ");
    out.push_str(&normalized);
    out.push('\n');
    out
}

fn generate_round_trip_fixture() -> Result<String, String> {
    let tool_call = json!({
        "sessionUpdate": "tool_call",
        "toolCallId": "call-001",
        "title": "Read README.md",
        "kind": "read",
        "status": "in_progress",
        "rawInput": { "path": "README.md" },
        "_meta": {
            "harn": {
                "executor": "harn_builtin",
                "durationMs": 12.5,
                "executionDurationMs": 11.2,
                "parsing": false,
            }
        }
    });
    let session_update = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "sess-42",
            "update": tool_call,
        }
    });
    let agent_event = json!({
        "jsonrpc": "2.0",
        "method": HARN_AGENT_EVENT_METHOD,
        "params": {
            "sessionId": "sess-42",
            "kind": "composition_child_call",
            "runId": "cmp-42",
            "toolCallId": "tool-cmp-42",
            "toolName": "tool.write_file",
            "operationIndex": 0,
            "requestedSideEffectLevel": "workspace_write",
            "annotations": {
                "kind": "edit",
                "side_effect_level": "workspace_write",
            },
            "policyContext": {
                "ceiling": "workspace_write",
                "approval": "denied",
            },
            "rawInput": {
                "path": "src/lib.rs",
                "content": "...",
            },
        }
    });
    let tool_call_receipt = json!({
        "schema_version": TOOL_CALL_RECEIPT_SCHEMA_VERSION,
        "session_id": "sess-42",
        "run_id": "run-42",
        "tool_call_id": "call-001",
        "tool_name": "read_file",
        "iteration": 1,
        "turn_index": 0,
        "emit_order": 0,
        "reason": "Read README.md for context",
        "kind": "read",
        "executor": "harn",
        "status": "ok",
        "error_category": null,
        "duration_ms": 12,
        "args_hash": "0".repeat(64),
        "result_hash": "1".repeat(64),
        "audit": {
            "summary": "Read README.md for context",
            "layers": [{"name": "with_required_reason", "status": "ok"}],
        },
        "emitted_at": "2026-05-16T00:00:00Z",
        "model": "mock",
        "provider": "mock",
    });
    let request = json!({
        "jsonrpc": "2.0",
        "id": 17,
        "method": "session/prompt",
        "params": {"sessionId": "sess-42", "prompt": [{"type": "text", "text": "hi"}]},
    });
    let response = json!({
        "jsonrpc": "2.0",
        "id": 17,
        "result": {"ok": true},
    });
    let error_response = json!({
        "jsonrpc": "2.0",
        "id": "abc",
        "error": {"code": -32601, "message": "method not found"},
    });
    let a2a_task = json!({
        "id": "task-1",
        "contextId": "ctx-7",
        "status": {
            "state": "working",
            "message": {"id": "msg-1", "role": "agent", "parts": [{"type": "text", "text": "ok"}]}
        }
    });
    let mcp_tool = json!({
        "name": "echo",
        "title": "Echo",
        "description": "Echoes input",
        "inputSchema": {
            "$schema": MCP_JSON_SCHEMA_2020_12_DIALECT,
            "type": "object",
            "properties": {"x": {"type": "string"}}
        }
    });
    let mcp_discover_result = json!({
        "resultType": "complete",
        "supportedVersions": MCP_PROTOCOL_VERSIONS,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "harn", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "Use the stable MCP version unless the RC is explicitly enabled.",
    });
    let mcp_input_required_result = json!({
        "resultType": MCP_INPUT_REQUIRED_RESULT_TYPE,
        "requestState": "opaque-state",
        "inputRequests": {
            "confirm": {
                "method": "elicitation/create",
                "params": {
                    "message": "Confirm the action",
                    "mode": "form",
                    "requestedSchema": {
                        "$schema": MCP_JSON_SCHEMA_2020_12_DIALECT,
                        "type": "object",
                        "properties": {"confirmed": {"type": "boolean"}},
                        "required": ["confirmed"],
                    }
                }
            }
        }
    });
    let mcp_unsupported_version_error = json!({
        "jsonrpc": "2.0",
        "id": 19,
        "error": {
            "code": MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_CODE,
            "message": MCP_UNSUPPORTED_PROTOCOL_VERSION_ERROR_MESSAGE,
            "data": {
                "requested": "DRAFT-2026-v0",
                "supported": MCP_PROTOCOL_VERSIONS,
            }
        }
    });

    let fixture = json!({
        "artifactVersion": env!("CARGO_PKG_VERSION"),
        "harnAgentEventMethod": HARN_AGENT_EVENT_METHOD,
        "harnProviderCatalogMethod": HARN_PROVIDER_CATALOG_METHOD,
        "envelopes": {
            "request": request,
            "response": response,
            "errorResponse": error_response,
            "sessionUpdateNotification": session_update,
            "agentEventNotification": agent_event,
        },
        "a2aTask": a2a_task,
        "mcpTool": mcp_tool,
        "mcpDiscoverResult": mcp_discover_result,
        "mcpInputRequiredResult": mcp_input_required_result,
        "mcpUnsupportedProtocolVersionError": mcp_unsupported_version_error,
        "toolCallReceipt": tool_call_receipt,
    });

    serde_json::to_string_pretty(&fixture)
        .map_err(|error| format!("failed to serialize round-trip fixture: {error}"))
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
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("] as const\n");
    out.push_str(&format!(
        "export type {type_name} = (typeof {name})[number]\n\n"
    ));
    out
}

fn ts_wire_value_object(
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

fn swift_enum(name: &str, values: &[String]) -> String {
    swift_enum_with_deprecations(name, values, &[])
}

fn swift_enum_with_deprecations(
    name: &str,
    values: &[String],
    deprecated_values: &[DeprecatedWireValue],
) -> String {
    let mut out = format!("public enum {name}: String, Codable, Sendable, CaseIterable {{\n");
    for value in values {
        if let Some(deprecated) = deprecated_wire_value(deprecated_values, value) {
            out.push_str("    @available(*, deprecated, message: ");
            out.push_str(&json_string_literal(&deprecation_message(deprecated)));
            out.push_str(")\n");
        }
        out.push_str("    case ");
        out.push_str(&swift_case_name(value));
        out.push_str(" = ");
        out.push_str(&json_string_literal(value));
        out.push('\n');
    }
    out.push_str("\n    public static let allCases: [Self] = [\n");
    for value in values {
        out.push_str("        ");
        out.push_str(&json_string_literal(value));
        out.push_str(",\n");
    }
    out.push_str("    ].map { Self(rawValue: $0)! }\n");
    out.push_str("}\n\n");
    out
}

fn deprecated_wire_value<'a>(
    deprecated_values: &'a [DeprecatedWireValue],
    value: &str,
) -> Option<&'a DeprecatedWireValue> {
    deprecated_values
        .iter()
        .find(|deprecated| deprecated.value == value)
}

fn deprecation_message(value: &DeprecatedWireValue) -> String {
    format!(
        "Use {}; {} will be removed after one release.",
        value.replacement, value.value
    )
}

fn wire_value_property_name(value: &str) -> String {
    swift_case_name(value)
}

fn swift_string_array(name: &str, values: &[&str]) -> String {
    let mut out = format!("    public static let {name}: [String] = [\n");
    for value in values {
        out.push_str("        ");
        out.push_str(&json_string_literal(value));
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
        format!("_{out}")
    } else if SWIFT_RESERVED_KEYWORDS.contains(&out.as_str()) {
        format!("`{out}`")
    } else {
        out
    }
}

/// Swift reserved keywords that cannot appear as bare identifiers in a `case`
/// declaration. Wire values like `private` / `public` (e.g. on
/// `HarnMCPCacheScope`) camelCase down to themselves, so without escaping they
/// land in the generated Swift as `case private = "private"`, which fails to
/// compile.
const SWIFT_RESERVED_KEYWORDS: &[&str] = &[
    "associatedtype",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "defer",
    "deinit",
    "do",
    "else",
    "enum",
    "extension",
    "fallthrough",
    "false",
    "fileprivate",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "inout",
    "internal",
    "is",
    "let",
    "nil",
    "open",
    "operator",
    "private",
    "protocol",
    "public",
    "repeat",
    "return",
    "rethrows",
    "self",
    "Self",
    "static",
    "struct",
    "subscript",
    "super",
    "switch",
    "throw",
    "throws",
    "true",
    "try",
    "typealias",
    "var",
    "where",
    "while",
];

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

fn worker_status_values() -> Vec<String> {
    // Multiple lifecycle events can share a wire status (e.g.
    // `WorkerSpawned` and `WorkerResumed` both surface as `running`).
    // The published vocabulary is a set, not a multiset — dedupe while
    // preserving the first-seen order so downstream consumers see a
    // stable list.
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for event in WorkerEvent::ALL.iter() {
        let status = event.as_status().to_string();
        if seen.insert(status.clone()) {
            out.push(status);
        }
    }
    out
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

/// Output is also valid TypeScript/Swift/Python/Go because all four share
/// JSON's escape rules for the characters in our wire vocabulary
/// (printable ASCII plus `/` and `_`).
fn json_string_literal(value: &str) -> String {
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

    /// Shift-left guard: every `ACP`/`Harn`/`A2A`/`MCP`-prefixed *type* name
    /// referenced in the emitted TypeScript bindings must be declared in the
    /// same artifact. The TS section once used the Python-only name
    /// `HarnExtensionMeta` for a `_meta` field; it compiled here (it's just a
    /// Rust string) but broke `tsc` downstream in burin-code, where the
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
             artifact; this guard shift-lefts the burin-code `tsc` failure into harn's build."
        );
    }

    #[test]
    fn generated_types_include_harn_wire_vocabularies() {
        let ts = generate_typescript();
        assert!(ts.contains("export type JsonRpcId = number | string | null"));
        assert!(ts.contains("export const MCP_DRAFT_PROTOCOL_VERSION = \"DRAFT-2026-v1\""));
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
        assert!(swift.contains("public struct HarnMCPRequestMeta"));
        assert!(swift.contains("public struct HarnMCPDiscoverResult"));
        assert!(swift.contains("public struct HarnMCPInputRequiredResult"));
        assert!(swift.contains("public struct HarnMCPOAuthDiscoveryResult"));
        assert!(swift.contains("public enum HarnMCPOAuthClientRegistrationMode"));
        assert!(swift.contains("public enum HarnMCPOAuthAuthMode"));
        assert!(swift.contains("case applicationType = \"application_type\""));
        assert!(swift.contains("HarnMCPUnsupportedProtocolVersionError"));
        assert!(
            swift.contains("case protocolVersion = \"io.modelcontextprotocol/protocolVersion\"")
        );
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
    }

    #[test]
    fn swift_case_name_escapes_reserved_keywords() {
        // Bare `case private = ...` / `case public = ...` won't compile in
        // Swift — both are reserved keywords. The wire vocabulary uses these
        // (e.g. MCPCacheScope) so the emitter has to backtick them.
        assert_eq!(swift_case_name("private"), "`private`");
        assert_eq!(swift_case_name("public"), "`public`");
        assert_eq!(swift_case_name("class"), "`class`");
        // Non-keyword identifiers should pass through unchanged.
        assert_eq!(swift_case_name("application_type"), "applicationType");
        assert_eq!(swift_case_name("session/close"), "sessionClose");
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
            rust.starts_with(
                "// GENERATED by `harn dump-protocol-artifacts` - do not edit by hand."
            ),
            "Rust artifact missing provenance header"
        );
        assert!(rust.contains("pub const HARN_PROTOCOL_ARTIFACT_VERSION: &str ="));
        assert!(rust.contains(&format!(
            "pub const HARN_AGENT_EVENT_METHOD: &str = {};",
            json_string_literal(HARN_AGENT_EVENT_METHOD)
        )));
        assert!(rust.contains(&format!(
            "pub const HARN_PROVIDER_CATALOG_METHOD: &str = {};",
            json_string_literal(HARN_PROVIDER_CATALOG_METHOD)
        )));
        // Method-name constants, both the stable and the full dispatched surface.
        assert!(
            rust.contains("pub const ACP_AGENT_METHOD_SESSION_PROMPT: &str = \"session/prompt\"")
        );
        assert!(rust.contains("pub const ACP_AGENT_METHODS: &[&str] = &["));
        assert!(rust.contains("pub const ACP_DISPATCHED_METHODS: &[&str] = &["));
        assert!(rust.contains("pub const ACP_CLIENT_METHODS: &[&str] = &["));
        // Session-update discriminators (base + Harn extensions).
        assert!(rust.contains("pub const ACP_SESSION_UPDATES: &[&str] = &["));
        assert!(rust.contains("pub const HARN_ACP_SESSION_UPDATE_EXTENSIONS: &[&str] = &["));
        // `_meta.harn.visible_text` / `visible_delta` content extension keys.
        assert!(rust.contains(
            "pub const HARN_CONTENT_EXTENSION_FIELD_VISIBLE_TEXT: &str = \"visible_text\""
        ));
        assert!(rust.contains(
            "pub const HARN_CONTENT_EXTENSION_FIELD_VISIBLE_DELTA: &str = \"visible_delta\""
        ));
        assert!(rust.contains("pub const HARN_CONTENT_EXTENSION_FIELDS: &[&str] = &["));
        // Dotted / slashed wire names must collapse to valid const identifiers.
        assert!(rust.contains(
            "pub const ACP_DISPATCHED_METHOD_HARN_HITL_RESPOND: &str = \"harn.hitl.respond\""
        ));
        assert!(
            rust.contains("pub const ACP_DISPATCHED_METHOD_AGENT_RESUME: &str = \"agent/resume\"")
        );
        // Every published wire value must appear somewhere in the artifact.
        for value in ACP_AGENT_METHODS
            .iter()
            .chain(ACP_DISPATCHED_METHODS.iter())
            .chain(ACP_CLIENT_METHODS.iter())
            .chain(HARN_SESSION_UPDATE_EXTENSIONS.iter())
            .chain(HARN_AGENT_EVENT_KINDS.iter())
            .chain(HARN_CONTENT_EXTENSION_FIELDS.iter())
            .chain(HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS.iter())
        {
            assert!(rust.contains(value), "Rust artifact missing {value}");
        }
        for value in all_acp_session_updates() {
            assert!(rust.contains(&value), "Rust artifact missing {value}");
        }
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
    }

    #[test]
    fn dispatched_acp_methods_match_artifact() {
        // The published ACP_DISPATCHED_METHODS slice must reflect the real
        // surface the adapter handles. If a contributor adds or removes a
        // `match method.as_str()` arm in the ACP adapter without updating the
        // artifact, this guard fails. We read the adapter source directly so
        // the check has no runtime dependency on a live server.
        let dispatch =
            read_repo_text("crates/harn-serve/src/adapters/acp/mod.rs").expect("read acp adapter");
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
                if trimmed.starts_with("HARN_PROVIDER_CATALOG_METHOD") {
                    dispatched.insert(HARN_PROVIDER_CATALOG_METHOD.to_string());
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

    #[test]
    fn generated_python_includes_harn_wire_vocabularies() {
        let py = generate_python();
        assert!(py.contains("MCP_DRAFT_PROTOCOL_VERSION: str = \"DRAFT-2026-v1\""));
        assert!(py.contains("MCP_REQUIRED_METADATA_KEYS: tuple"));
        assert!(py.contains("class MCPDiscoverResult(_HarnDataclass):"));
        assert!(py.contains("class MCPInputRequiredResult(_HarnDataclass):"));
        assert!(py.contains("class MCPCacheScope(str, Enum):"));
        assert!(py.contains("class ACPSessionUpdate(str, Enum):"));
        assert!(py.contains("class HarnToolCallErrorCategory(str, Enum):"));
        assert!(py.contains("class HarnWorkerStatus(str, Enum):"));
        assert!(py.contains("class ToolCallReceipt(_HarnDataclass):"));
        assert!(py.contains("class ToolCallReceiptStatus(str, Enum):"));
        assert!(py.contains("class _HarnDataclass:"));
        assert!(py.contains("def is_request("));
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
        assert!(go.contains("type MCPRequestMeta struct"));
        assert!(go.contains("type MCPDiscoverResult struct"));
        assert!(go.contains("type MCPInputRequiredResult struct"));
        assert!(go.contains("MCPUnsupportedProtocolVersionErrorCode"));
        assert!(go.contains("type JSONRPCID struct"));
        assert!(go.contains("type ACPSessionUpdateNotification struct"));
        assert!(go.contains("func IsRequest(envelope map[string]json.RawMessage)"));
        assert!(go.contains("type HarnWorkerStatus = string"));
        assert!(go.contains("var HarnWorkerStatuses = []HarnWorkerStatus"));
        assert!(go.contains("type ToolCallReceipt struct"));
        assert!(go.contains("var ToolCallReceiptStatuses = []ToolCallReceiptStatus"));
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
        assert_eq!(fixture["toolCallReceipt"]["schema_version"], json!(1));
    }

    #[test]
    fn manifest_advertises_python_and_go_bindings() {
        let manifest: serde_json::Value =
            serde_json::from_str(&generate_manifest().expect("manifest")).expect("manifest json");
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
        assert_eq!(manifest["bindings"]["rust"]["stability"], json!("stable"));
        assert_eq!(
            manifest["bindings"]["typescript"]["stability"],
            json!("stable")
        );
        assert_eq!(
            manifest["mcp"]["draftProtocolVersion"],
            json!("DRAFT-2026-v1")
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
