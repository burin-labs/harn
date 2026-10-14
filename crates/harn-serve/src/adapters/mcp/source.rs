//! What one [`McpServer`] serves: the `pub fn`s a module exports, or the
//! tools, resources, and prompts a script published.
//!
//! Sessions, authorization, task ownership, envelopes, and transports belong
//! to the server and never branch on the source. Each source answers only
//! what its declarations decide: discovery entries, task support, and the
//! responses its handlers produce.
use harn_vm::mcp_progress::ProgressContext;
use harn_vm::mcp_tasks::McpTaskSupport;

use super::script::McpScriptRuntime;
use super::*;

pub(super) enum McpSource {
    Exports(Box<ExportsSource>),
    Script(McpScriptRuntime),
}

pub(super) struct ExportsSource {
    pub(super) catalog: ExportCatalog,
    pub(super) tool_catalog: harn_vm::tool_registry::ToolCatalog,
    mcp_tools: Vec<JsonValue>,
    context: McpContextCatalog,
    executor: DispatchRuntime,
}

/// Server identity a source supplies; the server adds the card and title.
pub(super) struct SourceIdentity {
    pub(super) name: Option<String>,
    pub(super) version: String,
    pub(super) instructions: Option<String>,
    pub(super) icons: Option<JsonValue>,
}

/// The capability families a source has something to serve for.
#[derive(Default)]
pub(super) struct SourceCapabilities {
    pub(super) tools: bool,
    pub(super) resources: bool,
    pub(super) prompts: bool,
    pub(super) tasks: bool,
    pub(super) mcp_apps: bool,
    pub(super) list_changed: bool,
}

/// One admitted `tools/call`, ready to run.
pub(super) struct ToolInvocation {
    pub(super) request_id: JsonValue,
    pub(super) tool_name: String,
    pub(super) params: JsonValue,
    pub(super) client_identity: String,
    pub(super) auth: AuthRequest,
    pub(super) cancel_token: Arc<AtomicBool>,
    pub(super) progress: Option<ProgressContext>,
    pub(super) as_task: bool,
}

/// A finished `tools/call`. `cancelled` reports that execution observed the
/// cancellation token rather than producing a result.
pub(super) struct ToolCallOutcome {
    pub(super) response: JsonValue,
    pub(super) cancelled: bool,
}

impl ExportsSource {
    pub(super) fn new(core: DispatchCore) -> Self {
        let core = Arc::new(core);
        let catalog = core.catalog().clone();
        Self {
            tool_catalog: core.tool_catalog().clone(),
            mcp_tools: core.mcp_tools().to_vec(),
            context: McpContextCatalog::discover(&catalog.script_path),
            catalog,
            executor: DispatchRuntime::start("MCP", core),
        }
    }

    async fn call_tool(&self, invocation: ToolInvocation) -> ToolCallOutcome {
        let id = invocation.request_id;
        let arguments = invocation
            .params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let request = match build_call_request(
            "mcp",
            &invocation.client_identity,
            &invocation.tool_name,
            arguments,
            invocation.auth,
            invocation.cancel_token,
            invocation.progress,
            Some(mcp_request_id_to_string(&id)),
        ) {
            Ok(request) => request,
            Err(error) => {
                return ToolCallOutcome {
                    response: harn_vm::jsonrpc::error_response(id, -32602, &error),
                    cancelled: false,
                }
            }
        };
        let result = self.executor.call(request).await;
        let cancelled = matches!(&result, Err(DispatchError::Cancelled(_)));
        let response = match result {
            Ok(response) => {
                harn_vm::jsonrpc::response(id, tool_call_success(response, &self.tool_catalog))
            }
            Err(DispatchError::Validation(message)) => {
                harn_vm::jsonrpc::error_response(id, -32602, &message)
            }
            Err(DispatchError::Unauthorized(message)) => {
                harn_vm::jsonrpc::error_response(id, -32001, &message)
            }
            Err(DispatchError::Forbidden { required, granted }) => {
                forbidden_jsonrpc_error_response(id, &required, &granted)
            }
            // `@policy(kinds:)` is a `harn serve site` admission gate; the
            // MCP transport never raises it, but map it to the same
            // authorization error code defensively, using its tenant-safe
            // message.
            Err(error @ DispatchError::ForbiddenPrincipalKind { .. }) => {
                harn_vm::jsonrpc::error_response(id, -32001, &error.message())
            }
            Err(DispatchError::MissingExport(message)) => {
                harn_vm::jsonrpc::error_response(id, -32602, &message)
            }
            Err(DispatchError::Application(error)) => harn_vm::jsonrpc::response(
                id,
                harn_vm::tool_registry::application_error_mcp_result(&error),
            ),
            Err(DispatchError::Contract(error)) => {
                harn_vm::jsonrpc::response(id, tool_call_error(error.to_string()))
            }
            Err(DispatchError::Execution(message))
            | Err(DispatchError::Cancelled(message))
            | Err(DispatchError::Io(message))
            | Err(DispatchError::Cache(message)) => {
                harn_vm::jsonrpc::response(id, tool_call_error(message))
            }
            Err(error @ DispatchError::RateLimited { .. })
            | Err(error @ DispatchError::BudgetExceeded { .. })
            | Err(error @ DispatchError::SecretBackend(_)) => {
                harn_vm::jsonrpc::response(id, tool_call_error(error.message()))
            }
        };
        ToolCallOutcome {
            response,
            cancelled,
        }
    }

    fn handle_prompts_get(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        match self.context.get_prompt(name, &arguments) {
            Ok(value) => harn_vm::jsonrpc::response(id, value),
            Err(error)
                if error.starts_with("Unknown prompt")
                    || error.starts_with("Missing required argument")
                    || error.starts_with("prompt arguments") =>
            {
                harn_vm::jsonrpc::error_response(id, -32602, &error)
            }
            Err(error) => harn_vm::jsonrpc::error_response(id, -32603, &error),
        }
    }

    fn handle_completion_complete(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let Some(ref_type) = params.pointer("/ref/type").and_then(JsonValue::as_str) else {
            return harn_vm::jsonrpc::error_response(id, -32602, "completion ref.type is required");
        };
        let Some(argument_name) = params
            .pointer("/argument/name")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
        else {
            return harn_vm::jsonrpc::error_response(
                id,
                -32602,
                "completion argument.name is required",
            );
        };
        let value = params
            .pointer("/argument/value")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let completion = match ref_type {
            "ref/prompt" => {
                let name = params
                    .pointer("/ref/name")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default();
                self.context.complete_prompt(name, argument_name, value)
            }
            "ref/resource" => {
                let uri_template = params
                    .pointer("/ref/uri")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default();
                self.context
                    .complete_resource_template(uri_template, argument_name, value)
            }
            other => Err(format!("Unsupported completion ref.type: {other}")),
        };
        match completion {
            Ok(completion) => harn_vm::jsonrpc::response(id, json!({ "completion": completion })),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32602, &error),
        }
    }
}

impl McpSource {
    pub(super) fn identity(&self) -> SourceIdentity {
        match self {
            Self::Exports(exports) => SourceIdentity {
                name: None,
                version: env!("CARGO_PKG_VERSION").to_string(),
                instructions: exports.catalog.instructions.clone(),
                icons: exports.context.server_icons(),
            },
            Self::Script(script) => {
                let surface = script.surface();
                SourceIdentity {
                    name: Some(surface.server_name.clone()),
                    version: surface.server_version.clone(),
                    instructions: surface.instructions.clone(),
                    icons: None,
                }
            }
        }
    }

    pub(super) fn capabilities(&self) -> SourceCapabilities {
        match self {
            Self::Exports(exports) => SourceCapabilities {
                tools: !exports.catalog.functions.is_empty(),
                resources: exports.context.has_resources(),
                prompts: exports.context.has_prompts(),
                tasks: exports
                    .catalog
                    .functions
                    .values()
                    .any(|function| function.job.is_some()),
                mcp_apps: false,
                list_changed: false,
            },
            Self::Script(script) => {
                let surface = script.surface();
                SourceCapabilities {
                    tools: !surface.tools.is_empty(),
                    resources: !surface.resources.is_empty()
                        || !surface.resource_templates.is_empty(),
                    prompts: !surface.prompts.is_empty(),
                    tasks: surface
                        .task_support
                        .values()
                        .any(|support| support.allows_task()),
                    mcp_apps: surface.serves_mcp_apps,
                    list_changed: script.list_changes(),
                }
            }
        }
    }

    pub(super) fn tool_entries(&self) -> Vec<JsonValue> {
        match self {
            Self::Exports(exports) => exports.mcp_tools.clone(),
            Self::Script(script) => script.surface().tools.clone(),
        }
    }

    /// The named tool's task support, or `None` when no such tool is served.
    /// An `@job` export may run as a task; a script tool declares it.
    pub(super) fn task_support(&self, tool_name: &str) -> Option<McpTaskSupport> {
        match self {
            Self::Exports(exports) => exports.catalog.function(tool_name).map(|function| {
                if function.job.is_some() {
                    McpTaskSupport::Optional
                } else {
                    McpTaskSupport::Forbidden
                }
            }),
            Self::Script(script) => script.surface().task_support.get(tool_name).copied(),
        }
    }

    /// Refuse, before a task exists, a call the source would never run.
    /// Exports validate inside dispatch, which a task already observes.
    pub(super) async fn admit_tool(&self, id: &JsonValue, params: &JsonValue) -> Option<JsonValue> {
        match self {
            Self::Exports(_) => None,
            Self::Script(script) => script.admit_tool(id.clone(), params.clone()).await,
        }
    }

    pub(super) fn resource_entries(&self) -> Vec<JsonValue> {
        match self {
            Self::Exports(exports) => exports.context.resource_entries(),
            Self::Script(script) => script.surface().resources.clone(),
        }
    }

    pub(super) fn resource_template_entries(&self) -> Vec<JsonValue> {
        match self {
            Self::Exports(exports) => exports.context.resource_templates(),
            Self::Script(script) => script.surface().resource_templates.clone(),
        }
    }

    pub(super) fn prompt_entries(&self) -> Vec<JsonValue> {
        match self {
            Self::Exports(exports) => exports.context.prompt_entries(),
            Self::Script(script) => script.surface().prompts.clone(),
        }
    }

    pub(super) async fn read_resource(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        match self {
            Self::Exports(exports) => {
                let uri = params
                    .get("uri")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default();
                match exports.context.read_resource(uri) {
                    Some((text, mime_type)) => harn_vm::jsonrpc::response(
                        id,
                        json!({
                            "contents": [{
                                "uri": uri,
                                "text": text,
                                "mimeType": mime_type,
                            }]
                        }),
                    ),
                    None => harn_vm::jsonrpc::error_response(
                        id,
                        -32002,
                        &format!("Resource not found: {uri}"),
                    ),
                }
            }
            Self::Script(script) => script.read_resource(id, params.clone()).await,
        }
    }

    pub(super) async fn get_prompt(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        match self {
            Self::Exports(exports) => exports.handle_prompts_get(id, params),
            Self::Script(script) => script.get_prompt(id, params.clone()).await,
        }
    }

    pub(super) async fn complete(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        match self {
            Self::Exports(exports) => exports.handle_completion_complete(id, params),
            Self::Script(script) => script.complete(id, params.clone()).await,
        }
    }

    pub(super) async fn call_tool(&self, invocation: ToolInvocation) -> ToolCallOutcome {
        match self {
            Self::Exports(exports) => exports.call_tool(invocation).await,
            Self::Script(script) => ToolCallOutcome {
                response: script
                    .call_tool(
                        invocation.request_id,
                        invocation.params,
                        invocation.progress,
                        invocation.as_task,
                    )
                    .await,
                cancelled: false,
            },
        }
    }
}

/// Render a JSON-RPC request id into a stable string for the obs
/// ambient `request_id`. Numbers and strings round-trip as-is; `null`
/// and any other unexpected shape fall back to a fresh `req_*` id so
/// every dispatch still carries one through to `harness.obs.*`.
fn mcp_request_id_to_string(id: &JsonValue) -> String {
    match id {
        JsonValue::String(text) => text.clone(),
        JsonValue::Number(number) => number.to_string(),
        _ => crate::http_codec::fresh_request_id(),
    }
}

/// Standard JSON-RPC error response for a scope mismatch. The error
/// body carries the canonical `forbidden` payload so MCP clients can
/// render an actionable prompt without parsing the message string.
fn forbidden_jsonrpc_error_response(
    id: JsonValue,
    required: &std::collections::BTreeSet<String>,
    granted: &std::collections::BTreeSet<String>,
) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32003,
            "message": crate::forbidden_message(required, granted),
            "data": crate::forbidden_data_payload(required, granted),
        }
    })
}
