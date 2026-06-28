use harn_serve::adapters::acp::{
    ACP_SCHEMA_COMPATIBILITY, HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD,
    HARN_CONTENT_EXTENSION_FIELDS, HARN_PROVIDER_CATALOG_METHOD, HARN_SESSION_UPDATE_EXTENSIONS,
    HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use harn_serve::{A2A_PROTOCOL_VERSION, MCP_PROTOCOL_VERSION};
use harn_vm::llm::receipts::{
    tool_call_receipt_schema, TOOL_CALL_RECEIPT_EXECUTORS, TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT,
    TOOL_CALL_RECEIPT_SCHEMA_VERSION, TOOL_CALL_RECEIPT_STATUSES,
};
use serde_json::json;

use super::constants::*;
use super::support::*;
use super::values::*;

pub(super) fn generate_tool_call_receipt_schema() -> Result<String, String> {
    serde_json::to_string_pretty(&tool_call_receipt_schema())
        .map_err(|error| format!("failed to serialize tool-call receipt schema: {error}"))
}

/// Render the protocol-artifact manifest the running Harn would write.
///
/// Re-exposed via `harn package artifacts manifest` so downstream automation
/// (downstream hosts, Harn Cloud) can compare against vendored copies without
/// shelling out to `dump-protocol-artifacts`.
pub(crate) fn manifest_json() -> Result<String, String> {
    generate_manifest()
}

pub(super) fn generate_manifest() -> Result<String, String> {
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
            "dispatchedMethods": ACP_DISPATCHED_METHODS,
            "transportControlMethods": ACP_TRANSPORT_CONTROL_METHODS,
            "handledMethods": concat_unique_wire_values(&[
                ACP_TRANSPORT_CONTROL_METHODS,
                ACP_DISPATCHED_METHODS,
            ]),
            "deprecatedAgentMethods": deprecated_wire_values(ACP_DEPRECATED_AGENT_METHODS),
            "clientMethods": ACP_CLIENT_METHODS,
            "agentNotifications": ACP_AGENT_NOTIFICATIONS,
            "sessionUpdateVariants": all_acp_session_updates(),
            "harnSessionUpdateExtensions": HARN_SESSION_UPDATE_EXTENSIONS,
            "harnSessionTimelineMethods": HARN_SESSION_TIMELINE_METHODS,
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
            "legacy20250618ProtocolVersion": MCP_LEGACY_2025_06_18_PROTOCOL_VERSION,
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

pub(super) fn deprecated_wire_values(values: &[DeprecatedWireValue]) -> serde_json::Value {
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

pub(super) fn generate_readme() -> String {
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

pub(super) fn generate_round_trip_fixture() -> Result<String, String> {
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
