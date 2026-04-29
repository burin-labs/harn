//! Direct MCP-server integration for `agent_loop`.
//!
//! This is the "one option" path for MCP-backed tools: bootstrap the
//! configured servers before the first model turn, materialize their
//! `tools/list` results into the normal Harn tool registry, and keep the
//! live clients available for dispatch.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::llm::api::LlmCallOptions;
use crate::value::{VmError, VmValue};

pub(super) type AgentLoopMcpClients = BTreeMap<String, crate::mcp::VmMcpClientHandle>;

pub(crate) fn parse_mcp_server_specs(
    options: &Option<BTreeMap<String, VmValue>>,
) -> Result<Vec<serde_json::Value>, VmError> {
    let Some(value) = options.as_ref().and_then(|opts| opts.get("mcp_servers")) else {
        return Ok(Vec::new());
    };
    let VmValue::List(items) = value else {
        return Err(VmError::Runtime(
            "agent_loop: `mcp_servers` must be a list of server config dicts".to_string(),
        ));
    };

    let mut specs = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();
    for item in items.iter() {
        let mut spec = crate::llm::helpers::vm_value_to_json(item);
        normalize_server_spec(&mut spec)?;
        let name = spec
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            return Err(VmError::Runtime(
                "agent_loop: every `mcp_servers` entry requires a non-empty `name`".to_string(),
            ));
        }
        if !seen.insert(name.clone()) {
            return Err(VmError::Runtime(format!(
                "agent_loop: duplicate MCP server name `{name}` in `mcp_servers`"
            )));
        }
        specs.push(spec);
    }
    Ok(specs)
}

fn normalize_server_spec(spec: &mut serde_json::Value) -> Result<(), VmError> {
    let Some(obj) = spec.as_object_mut() else {
        return Err(VmError::Runtime(
            "agent_loop: each `mcp_servers` entry must be a dict".to_string(),
        ));
    };

    match obj.get("command") {
        Some(serde_json::Value::Array(items)) => {
            let Some(command) = items.first().and_then(|value| value.as_str()) else {
                return Err(VmError::Runtime(
                    "agent_loop: stdio MCP `command` arrays must start with a command string"
                        .to_string(),
                ));
            };
            let args = items
                .iter()
                .skip(1)
                .map(|value| {
                    value.as_str().map(str::to_string).ok_or_else(|| {
                        VmError::Runtime(
                            "agent_loop: stdio MCP `command` array args must be strings"
                                .to_string(),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            obj.insert(
                "command".to_string(),
                serde_json::Value::String(command.to_string()),
            );
            obj.entry("args".to_string()).or_insert_with(|| {
                serde_json::Value::Array(args.into_iter().map(serde_json::Value::String).collect())
            });
        }
        Some(serde_json::Value::String(_)) | None => {}
        Some(_) => {
            return Err(VmError::Runtime(
                "agent_loop: MCP `command` must be a string or a string array".to_string(),
            ));
        }
    }

    Ok(())
}

pub(super) async fn bootstrap_agent_loop_mcp_servers(
    opts: &mut LlmCallOptions,
    specs: &[serde_json::Value],
) -> Result<AgentLoopMcpClients, VmError> {
    if specs.is_empty() {
        return Ok(BTreeMap::new());
    }

    let mut clients = BTreeMap::new();
    let mut mcp_entries = Vec::new();

    for spec in specs {
        let server_name = spec
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let client = match crate::mcp::connect_mcp_server_from_json(spec).await {
            Ok(client) => client,
            Err(error) => {
                disconnect_clients(&clients).await;
                return Err(error);
            }
        };
        let result = match client.call("tools/list", serde_json::json!({})).await {
            Ok(result) => result,
            Err(error) => {
                let _ = client.disconnect().await;
                disconnect_clients(&clients).await;
                return Err(error);
            }
        };
        let tools = result
            .get("tools")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        for tool in tools {
            match mcp_tool_to_registry_entry(&server_name, &tool) {
                Ok(entry) => mcp_entries.push(entry),
                Err(error) => {
                    let _ = client.disconnect().await;
                    disconnect_clients(&clients).await;
                    return Err(error);
                }
            }
        }
        clients.insert(server_name, client);
    }

    if !mcp_entries.is_empty() {
        opts.tools = match merge_mcp_tools(opts.tools.clone(), &mcp_entries) {
            Ok(tools) => Some(tools),
            Err(error) => {
                disconnect_clients(&clients).await;
                return Err(error);
            }
        };
        let mcp_tools_value = VmValue::List(Rc::new(mcp_entries));
        let mut native_mcp_tools =
            match crate::llm::tools::vm_tools_to_native(&mcp_tools_value, &opts.provider) {
                Ok(tools) => tools,
                Err(error) => {
                    disconnect_clients(&clients).await;
                    return Err(error);
                }
            };
        match opts.native_tools.as_mut() {
            Some(native_tools) => native_tools.append(&mut native_mcp_tools),
            None => opts.native_tools = Some(native_mcp_tools),
        }
    }

    Ok(clients)
}

async fn disconnect_clients(clients: &AgentLoopMcpClients) {
    for client in clients.values() {
        let _ = client.disconnect().await;
    }
}

fn mcp_tool_to_registry_entry(server: &str, tool: &serde_json::Value) -> Result<VmValue, VmError> {
    let original_name = tool
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if original_name.is_empty() {
        return Err(VmError::Runtime(format!(
            "agent_loop: MCP server `{server}` returned a tool without a name"
        )));
    }
    let prefixed_name = format!("{server}__{original_name}");
    let description = tool
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let input_schema = tool
        .get("inputSchema")
        .or_else(|| tool.get("input_schema"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));

    let mut entry = BTreeMap::new();
    entry.insert("name".to_string(), VmValue::String(Rc::from(prefixed_name)));
    entry.insert(
        "description".to_string(),
        VmValue::String(Rc::from(description)),
    );
    entry.insert(
        "parameters".to_string(),
        json_schema_to_harn_parameters(&input_schema),
    );
    entry.insert(
        "annotations".to_string(),
        json_schema_to_tool_annotations(&input_schema),
    );
    entry.insert(
        "executor".to_string(),
        VmValue::String(Rc::from("mcp_server")),
    );
    entry.insert(
        "mcp_server".to_string(),
        VmValue::String(Rc::from(server.to_string())),
    );
    entry.insert(
        "_mcp_server".to_string(),
        VmValue::String(Rc::from(server.to_string())),
    );
    entry.insert(
        "_mcp_tool_name".to_string(),
        VmValue::String(Rc::from(original_name.to_string())),
    );
    if let Some(output_schema) = tool
        .get("outputSchema")
        .or_else(|| tool.get("output_schema"))
    {
        entry.insert(
            "outputSchema".to_string(),
            crate::stdlib::json_to_vm_value(output_schema),
        );
    }
    Ok(VmValue::Dict(Rc::new(entry)))
}

fn json_schema_to_harn_parameters(schema: &serde_json::Value) -> VmValue {
    let required: BTreeSet<String> = schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut params = BTreeMap::new();
    if let Some(properties) = schema.get("properties").and_then(|value| value.as_object()) {
        for (name, prop) in properties {
            let mut param = crate::stdlib::json_to_vm_value(prop);
            if let VmValue::Dict(dict) = &param {
                let mut cloned = dict.as_ref().clone();
                cloned.insert(
                    "required".to_string(),
                    VmValue::Bool(required.contains(name)),
                );
                param = VmValue::Dict(Rc::new(cloned));
            }
            params.insert(name.clone(), param);
        }
    }
    VmValue::Dict(Rc::new(params))
}

fn json_schema_to_tool_annotations(schema: &serde_json::Value) -> VmValue {
    let required: Vec<serde_json::Value> = schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|value| value.as_str())
                .map(|value| serde_json::Value::String(value.to_string()))
                .collect()
        })
        .unwrap_or_default();
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "kind": "fetch",
        "side_effect_level": "network",
        "arg_schema": {
            "required": required,
        },
    }))
}

fn merge_mcp_tools(existing: Option<VmValue>, mcp_entries: &[VmValue]) -> Result<VmValue, VmError> {
    let mut seen = BTreeSet::new();
    let mut merged = Vec::new();

    let mut root = match existing {
        Some(VmValue::Dict(dict)) => {
            let root = dict.as_ref().clone();
            if let Some(VmValue::List(tools)) = root.get("tools") {
                for tool in tools.iter() {
                    remember_tool_name(tool, &mut seen)?;
                    merged.push(tool.clone());
                }
            }
            root
        }
        Some(VmValue::List(tools)) => {
            for tool in tools.iter() {
                remember_tool_name(tool, &mut seen)?;
                merged.push(tool.clone());
            }
            let mut root = BTreeMap::new();
            root.insert(
                "_type".to_string(),
                VmValue::String(Rc::from("tool_registry")),
            );
            root
        }
        Some(_) => {
            return Err(VmError::Runtime(
                "agent_loop: `tools` must be a tool registry or list when `mcp_servers` is used"
                    .to_string(),
            ));
        }
        None => {
            let mut root = BTreeMap::new();
            root.insert(
                "_type".to_string(),
                VmValue::String(Rc::from("tool_registry")),
            );
            root
        }
    };

    for entry in mcp_entries {
        remember_tool_name(entry, &mut seen)?;
        merged.push(entry.clone());
    }

    root.insert("tools".to_string(), VmValue::List(Rc::new(merged)));
    Ok(VmValue::Dict(Rc::new(root)))
}

fn remember_tool_name(tool: &VmValue, seen: &mut BTreeSet<String>) -> Result<(), VmError> {
    let Some(dict) = tool.as_dict() else {
        return Ok(());
    };
    let name = dict
        .get("name")
        .map(|value| value.display())
        .unwrap_or_default();
    if name.is_empty() {
        return Ok(());
    }
    if !seen.insert(name.clone()) {
        return Err(VmError::Runtime(format!(
            "agent_loop: duplicate tool name `{name}` after MCP server prefixing"
        )));
    }
    Ok(())
}
