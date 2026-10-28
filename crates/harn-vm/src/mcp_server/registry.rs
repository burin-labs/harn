use std::collections::BTreeMap;
use std::sync::Arc;

use super::convert::{prompt_value_to_messages, vm_value_to_content, vm_value_to_json};
use super::defs::{
    McpCompletionSource, McpPromptDef, McpResourceDef, McpResourceTemplateDef, McpServerMetadata,
    McpToolDef,
};
use super::tools_schema::McpToolSet;
use super::uri::{match_uri_template, uri_template_variables};
use crate::mcp_progress::{scope_context, ProgressContext};
use crate::mcp_protocol::parse_request_metadata;
use crate::mcp_tasks::McpTaskSupport;
use crate::stdlib::json_to_vm_value;
use crate::value::VmError;
use crate::vm::Vm;

tokio::task_local! {
    static SERVED_MCP_COMPOSITION: Option<ServedComposition>;
}

struct ServedComposition {
    entries: BTreeMap<String, Arc<crate::value::DictMap>>,
    prepared: Arc<crate::tool_registry::PreparedToolCatalog>,
}

pub(crate) fn is_served_composition() -> bool {
    SERVED_MCP_COMPOSITION
        .try_with(Option::is_some)
        .unwrap_or(false)
}

pub(crate) fn composition_allows_tool(name: &str, entry: &Arc<crate::value::DictMap>) -> bool {
    SERVED_MCP_COMPOSITION
        .try_with(|allowed| {
            allowed
                .as_ref()
                .and_then(|served| served.entries.get(name))
                .is_some_and(|published| Arc::ptr_eq(published, entry))
        })
        .unwrap_or(false)
}

pub(crate) fn composition_catalog(
    name: &str,
) -> Result<Arc<crate::tool_registry::PreparedToolCatalog>, VmError> {
    let unavailable = || VmError::CategorizedError {
        message: format!("tool '{name}' has no prepared MCP server catalog entry"),
        category: crate::value::ErrorCategory::SchemaValidation,
    };
    SERVED_MCP_COMPOSITION
        .try_with(|served| {
            let served = served.as_ref().ok_or_else(unavailable)?;
            served.prepared.entry(name).ok_or_else(unavailable)?;
            Ok(Arc::clone(&served.prepared))
        })
        .map_err(|_| unavailable())?
}

#[cfg(test)]
pub(crate) async fn scope_composition_for_test<F: std::future::Future>(
    entries: BTreeMap<String, Arc<crate::value::DictMap>>,
    future: F,
) -> F::Output {
    let mut registry = crate::value::DictMap::new();
    registry.insert(
        "_type".into(),
        crate::value::VmValue::String("tool_registry".into()),
    );
    registry.insert(
        "tools".into(),
        crate::value::VmValue::list(
            entries
                .values()
                .cloned()
                .map(crate::value::VmValue::Dict)
                .collect(),
        ),
    );
    let catalog =
        crate::tool_registry::tool_registry_catalog(&crate::value::VmValue::dict(registry))
            .unwrap();
    let prepared = Arc::new(crate::tool_registry::PreparedToolCatalog::prepare(catalog).unwrap());
    SERVED_MCP_COMPOSITION
        .scope(Some(ServedComposition { entries, prepared }), future)
        .await
}

/// Tools, resources, and prompts a script published for `harn serve mcp`.
///
/// The registry owns the script's handlers and the projections of its
/// declarations. It does not speak a transport, negotiate sessions, authorize
/// callers, or keep tasks: the one MCP protocol server in `harn-serve` owns
/// those and consumes this registry as one of its sources. Handler-running
/// methods take the request id and return a complete JSON-RPC response so the
/// server can file or envelope it unchanged.
pub struct McpScriptRegistry {
    server_name: String,
    server_version: String,
    instructions: Option<String>,
    tools: McpToolSet,
    resources: Vec<McpResourceDef>,
    resource_templates: Vec<McpResourceTemplateDef>,
    prompts: Vec<McpPromptDef>,
}

/// Everything about a script registry that the protocol server answers
/// without running a handler. It is `Send`, so a server on any thread can
/// serve discovery while the registry's VM stays on its own thread.
#[derive(Clone, Debug, Default)]
pub struct McpScriptSurface {
    pub server_name: String,
    pub server_version: String,
    pub instructions: Option<String>,
    /// `tools/list` entries.
    pub tools: Vec<serde_json::Value>,
    /// Each listed tool's declared task support.
    pub task_support: BTreeMap<String, McpTaskSupport>,
    /// `resources/list` entries.
    pub resources: Vec<serde_json::Value>,
    /// `resources/templates/list` entries.
    pub resource_templates: Vec<serde_json::Value>,
    /// `prompts/list` entries.
    pub prompts: Vec<serde_json::Value>,
    /// Whether a `ui://` resource carries the MCP Apps MIME type.
    pub serves_mcp_apps: bool,
}

impl McpScriptRegistry {
    pub fn new(
        server_name: String,
        tools: McpToolSet,
        resources: Vec<McpResourceDef>,
        resource_templates: Vec<McpResourceTemplateDef>,
        prompts: Vec<McpPromptDef>,
    ) -> Self {
        Self {
            server_name,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            instructions: None,
            tools,
            resources,
            resource_templates,
            prompts,
        }
    }

    /// Apply script-supplied server metadata from `mcp_server_metadata(...)`.
    pub fn with_metadata(mut self, metadata: McpServerMetadata) -> Self {
        if let Some(name) = metadata.name {
            self.server_name = name;
        }
        if let Some(version) = metadata.version {
            self.server_version = version;
        }
        self.instructions = metadata.instructions;
        self
    }

    pub fn surface(&self) -> McpScriptSurface {
        McpScriptSurface {
            server_name: self.server_name.clone(),
            server_version: self.server_version.clone(),
            instructions: self.instructions.clone(),
            tools: self.tools.prepared().mcp_tools().to_vec(),
            task_support: self
                .tools
                .iter()
                .map(|tool| {
                    let support = tool
                        .catalog
                        .execution
                        .as_ref()
                        .map(|execution| execution.task_support)
                        .unwrap_or_default();
                    (tool.catalog.name.clone(), support)
                })
                .collect(),
            resources: self.resources.iter().map(resource_entry).collect(),
            resource_templates: self
                .resource_templates
                .iter()
                .map(resource_template_entry)
                .collect(),
            prompts: self.prompts.iter().map(prompt_entry).collect(),
            serves_mcp_apps: self.resources.iter().any(|resource| {
                resource.uri.starts_with("ui://")
                    && resource.mime_type.as_deref() == Some("text/html;profile=mcp-app")
            }),
        }
    }

    /// Run one tool handler and shape its `tools/call` response.
    ///
    /// `progress` binds `mcp_report_progress(...)` for the handler. A task has
    /// no request to carry an `input_required` round, so `as_task` turns a
    /// handler's request for client input into an error.
    pub async fn call_tool(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
        progress: Option<ProgressContext>,
        as_task: bool,
    ) -> serde_json::Value {
        let tool = match self.admitted_tool(id, params) {
            Ok(tool) => tool,
            Err(response) => return response,
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let args_vm = json_to_vm_value(&arguments);

        // `scope_context` is a tokio task-local rather than a thread-local
        // guard, so concurrent calls sharing an OS thread stay isolated.
        let result = match crate::mcp_input::scope_input_context(
            params,
            client_capabilities(params),
            scope_context(
                progress,
                SERVED_MCP_COMPOSITION.scope(
                    (tool.catalog.name == "harn.code.execute_composition").then(|| {
                        ServedComposition {
                            entries: self.tools.published_entries().clone(),
                            prepared: self.tools.prepared_catalog(),
                        }
                    }),
                    crate::tool_handler_scope::scope(
                        vm.call_closure_pub(&tool.handler, &[args_vm]),
                    ),
                ),
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };

        match crate::tool_registry::classify_tool_result(
            self.tools.prepared(),
            &tool.catalog.name,
            result,
        ) {
            Ok(crate::tool_registry::ToolInvocationOutcome::Success { value, json, text }) => {
                let result = successful_tool_call_result(&self.tools, tool, &value, json, text)
                    .unwrap_or_else(|error| tool_error_result(&error));
                crate::jsonrpc::response(id.clone(), result)
            }
            Ok(crate::tool_registry::ToolInvocationOutcome::ApplicationError(error)) => {
                crate::jsonrpc::response(
                    id.clone(),
                    crate::tool_registry::application_error_mcp_result(&error),
                )
            }
            Err(crate::tool_registry::ToolInvocationError::Runtime(VmError::McpInputRequired(
                _,
            ))) if as_task => crate::jsonrpc::error_response(
                id.clone(),
                -32000,
                "Tool requested client input, which a task cannot carry",
            ),
            Err(crate::tool_registry::ToolInvocationError::Runtime(VmError::McpInputRequired(
                required,
            ))) => crate::jsonrpc::response(id.clone(), crate::mcp_input::input_result(*required)),
            Err(error) => {
                crate::jsonrpc::response(id.clone(), tool_error_result(&error.to_string()))
            }
        }
    }

    /// Refuse a `tools/call` this registry would never run: an unknown or
    /// unlisted tool, arguments outside its input schema, or a tool that needs
    /// approved preparation. A server checks this before handing out a task,
    /// so a refused call never leaves one behind.
    pub fn admit_tool(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> Result<(), serde_json::Value> {
        self.admitted_tool(id, params).map(|_| ())
    }

    fn admitted_tool(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) -> Result<&McpToolDef, serde_json::Value> {
        let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let Some(tool) = self
            .tools
            .iter()
            .find(|tool| tool.catalog.name == tool_name)
        else {
            return Err(crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                &format!("Unknown tool: {tool_name}"),
            ));
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        if let Err(error) = self.tools.prepared().validate_input(tool_name, &arguments) {
            return Err(crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                &error.to_string(),
            ));
        }
        if let Err(error) = tool
            .invocation_requirement
            .require_direct(&tool.catalog.name)
        {
            return Err(crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                &error.to_string(),
            ));
        }
        Ok(tool)
    }

    /// Read a static resource or render a template. A URI neither one
    /// matches is the MCP `-32002` resource-not-found error.
    pub async fn read_resource(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("");

        // Static resources take precedence over templates.
        if let Some(resource) = self.resources.iter().find(|r| r.uri == uri) {
            let mut content = serde_json::json!({ "uri": resource.uri, "text": resource.text });
            if let Some(ref mime) = resource.mime_type {
                content["mimeType"] = serde_json::json!(mime);
            }
            if let Some(ref meta) = resource.meta {
                content["_meta"] = meta.clone();
            }
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "contents": [content] }
            });
        }

        for tmpl in &self.resource_templates {
            if let Some(args) = match_uri_template(&tmpl.uri_template, uri) {
                let args_vm = json_to_vm_value(&serde_json::json!(args));
                let result = match crate::mcp_input::scope_input_context(
                    params,
                    client_capabilities(params),
                    vm.call_closure_pub(&tmpl.handler, &[args_vm]),
                )
                .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        return crate::jsonrpc::error_response(id.clone(), -32602, &error)
                    }
                };
                return match result {
                    Ok(value) => {
                        let mut content = serde_json::json!({
                            "uri": uri,
                            "text": value.display(),
                        });
                        if let Some(ref mime) = tmpl.mime_type {
                            content["mimeType"] = serde_json::json!(mime);
                        }
                        serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": { "contents": [content] }
                        })
                    }
                    Err(VmError::McpInputRequired(required)) => crate::jsonrpc::response(
                        id.clone(),
                        crate::mcp_input::input_result(*required),
                    ),
                    Err(e) => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32603, "message": format!("{e}") }
                    }),
                };
            }
        }

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32002, "message": format!("Resource not found: {uri}") }
        })
    }

    /// Render one prompt through its handler.
    pub async fn get_prompt(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");

        let prompt = match self.prompts.iter().find(|p| p.name == name) {
            Some(p) => p,
            None => {
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": format!("Unknown prompt: {name}") }
                });
            }
        };

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let args_vm = json_to_vm_value(&arguments);

        let result = match crate::mcp_input::scope_input_context(
            params,
            client_capabilities(params),
            vm.call_closure_pub(&prompt.handler, &[args_vm]),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };

        match result {
            Ok(value) => {
                let messages = prompt_value_to_messages(&value);
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "messages": messages }
                })
            }
            Err(VmError::McpInputRequired(required)) => {
                crate::jsonrpc::response(id.clone(), crate::mcp_input::input_result(*required))
            }
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": format!("{e}") }
            }),
        }
    }

    /// Answer `completion/complete` for a prompt or resource-template argument.
    pub async fn complete(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let Some(ref_type) = params.pointer("/ref/type").and_then(|value| value.as_str()) else {
            return crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                "completion ref.type is required",
            );
        };
        match ref_type {
            "ref/prompt" => self.complete_prompt_argument(id, params, vm).await,
            "ref/resource" => {
                self.complete_resource_template_argument(id, params, vm)
                    .await
            }
            other => crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                &format!("Unsupported completion ref.type: {other}"),
            ),
        }
    }

    async fn complete_prompt_argument(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let name = params
            .pointer("/ref/name")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let argument_name = match completion_argument_name(params) {
            Ok(name) => name,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };
        let value = completion_argument_value(params);
        let prompt = match self.prompts.iter().find(|prompt| prompt.name == name) {
            Some(prompt) => prompt,
            None => {
                return crate::jsonrpc::error_response(
                    id.clone(),
                    -32602,
                    &format!("Unknown prompt: {name}"),
                );
            }
        };
        let Some(argument) = prompt
            .arguments
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|argument| argument.name == argument_name)
        else {
            return crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                &format!("Unknown prompt argument: {argument_name}"),
            );
        };
        let candidates =
            match completion_source_candidates(argument.completion.as_ref(), params, vm).await {
                Ok(candidates) => candidates,
                Err(error) => return crate::jsonrpc::error_response(id.clone(), -32603, &error),
            };
        crate::mcp_protocol::completion_result(id.clone(), candidates, value)
    }

    async fn complete_resource_template_argument(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
        vm: &mut Vm,
    ) -> serde_json::Value {
        let uri = params
            .pointer("/ref/uri")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let argument_name = match completion_argument_name(params) {
            Ok(name) => name,
            Err(error) => return crate::jsonrpc::error_response(id.clone(), -32602, &error),
        };
        let value = completion_argument_value(params);
        let template = match self
            .resource_templates
            .iter()
            .find(|template| template.uri_template == uri)
        {
            Some(template) => template,
            None => {
                return crate::jsonrpc::error_response(
                    id.clone(),
                    -32602,
                    &format!("Unknown resource template: {uri}"),
                );
            }
        };
        if !uri_template_variables(&template.uri_template)
            .iter()
            .any(|name| name == argument_name)
        {
            return crate::jsonrpc::error_response(
                id.clone(),
                -32602,
                &format!("Unknown resource template argument: {argument_name}"),
            );
        }
        let candidates =
            match completion_source_candidates(template.completions.get(argument_name), params, vm)
                .await
            {
                Ok(candidates) => candidates,
                Err(error) => return crate::jsonrpc::error_response(id.clone(), -32603, &error),
            };
        crate::mcp_protocol::completion_result(id.clone(), candidates, value)
    }
}

fn successful_tool_call_result(
    tools: &McpToolSet,
    tool: &McpToolDef,
    value: &crate::value::VmValue,
    value_json: serde_json::Value,
    text: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut result = serde_json::json!({
        "content": match text {
            Some(text) => vec![serde_json::json!({"type": "text", "text": text})],
            None => vm_value_to_content(value),
        },
        "isError": false,
    });
    let entry = tools
        .prepared()
        .entry(&tool.catalog.name)
        .expect("prepared MCP catalog covers every executable handler");
    if let Some(structured) = tools
        .prepared()
        .catalog()
        .mcp_structured_content(entry, value_json)
        .map_err(|error| error.to_string())?
    {
        result["structuredContent"] = structured;
    }
    Ok(result)
}

fn client_capabilities(params: &serde_json::Value) -> serde_json::Value {
    serde_json::to_value(parse_request_metadata(params).client_capabilities())
        .unwrap_or_else(|_| serde_json::json!({}))
}

fn completion_argument_name(params: &serde_json::Value) -> Result<&str, String> {
    params
        .pointer("/argument/name")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "completion argument.name is required".to_string())
}

fn completion_argument_value(params: &serde_json::Value) -> &str {
    params
        .pointer("/argument/value")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
}

async fn completion_source_candidates(
    source: Option<&McpCompletionSource>,
    params: &serde_json::Value,
    vm: &mut Vm,
) -> Result<Vec<String>, String> {
    let Some(source) = source else {
        return Ok(Vec::new());
    };
    let mut candidates = source.values.clone();
    if let Some(handler) = source.handler.as_ref() {
        let request = json_to_vm_value(params);
        let value = vm
            .call_closure_pub(handler, &[request])
            .await
            .map_err(|error| format!("{error}"))?;
        candidates.extend(completion_candidates_from_json(&vm_value_to_json(&value)));
    }
    Ok(candidates)
}

fn completion_candidates_from_json(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => {
            items.iter().filter_map(json_completion_string).collect()
        }
        serde_json::Value::Object(map) => map
            .get("values")
            .or_else(|| map.get("completion").and_then(|value| value.get("values")))
            .map(completion_candidates_from_json)
            .unwrap_or_default(),
        _ => json_completion_string(value).into_iter().collect(),
    }
}

fn json_completion_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

fn tool_error_result(message: &str) -> serde_json::Value {
    serde_json::json!({
        "content": [{"type": "text", "text": message}],
        "isError": true,
    })
}

fn resource_entry(resource: &McpResourceDef) -> serde_json::Value {
    let mut entry = serde_json::json!({ "uri": resource.uri, "name": resource.name });
    if let Some(ref title) = resource.title {
        entry["title"] = serde_json::json!(title);
    }
    if let Some(ref desc) = resource.description {
        entry["description"] = serde_json::json!(desc);
    }
    if let Some(ref mime) = resource.mime_type {
        entry["mimeType"] = serde_json::json!(mime);
    }
    if let Some(ref meta) = resource.meta {
        entry["_meta"] = meta.clone();
    }
    entry
}

fn resource_template_entry(template: &McpResourceTemplateDef) -> serde_json::Value {
    let mut entry =
        serde_json::json!({ "uriTemplate": template.uri_template, "name": template.name });
    if let Some(ref title) = template.title {
        entry["title"] = serde_json::json!(title);
    }
    if let Some(ref desc) = template.description {
        entry["description"] = serde_json::json!(desc);
    }
    if let Some(ref mime) = template.mime_type {
        entry["mimeType"] = serde_json::json!(mime);
    }
    entry
}

fn prompt_entry(prompt: &McpPromptDef) -> serde_json::Value {
    let mut entry = serde_json::json!({ "name": prompt.name });
    if let Some(ref title) = prompt.title {
        entry["title"] = serde_json::json!(title);
    }
    if let Some(ref desc) = prompt.description {
        entry["description"] = serde_json::json!(desc);
    }
    if let Some(ref args) = prompt.arguments {
        let args_json: Vec<serde_json::Value> = args
            .iter()
            .map(|a| {
                let mut arg = serde_json::json!({ "name": a.name, "required": a.required });
                if let Some(ref title) = a.title {
                    arg["title"] = serde_json::json!(title);
                }
                if let Some(ref desc) = a.description {
                    arg["description"] = serde_json::json!(desc);
                }
                arg
            })
            .collect();
        entry["arguments"] = serde_json::json!(args_json);
    }
    entry
}
