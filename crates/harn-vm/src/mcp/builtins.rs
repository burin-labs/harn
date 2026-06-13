use super::*;
use crate::value::VmDictExt;

pub(crate) fn vm_value_to_serde(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::Int(n) => serde_json::json!(*n),
        VmValue::Float(n) => serde_json::json!(*n),
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_serde).collect())
        }
        VmValue::Dict(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

pub(crate) async fn call_mcp_tool(
    client: &VmMcpClientHandle,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, VmError> {
    let (content, _hint) = call_mcp_tool_with_hint(client, tool_name, arguments).await?;
    Ok(content)
}

/// Variant of [`call_mcp_tool`] that also returns the MCP cache hint
/// from the underlying envelope (when present). Used by
/// [`crate::mcp_host`] so the supervised host can populate its
/// per-(server, tool, args-hash) response cache from the same envelope
/// the unwrapping path consumes — no second network round-trip.
pub(crate) async fn call_mcp_tool_with_hint(
    client: &VmMcpClientHandle,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<(serde_json::Value, Option<McpCacheHint>), VmError> {
    // Attach a unique progress token so the server can emit
    // `notifications/progress` updates that we route back into the
    // active session's `agent_inbox`. Tokens are scoped to the active
    // session id (when one is bound) and reaped on call completion via
    // the RAII `ProgressTokenGuard`.
    let progress_token = client_progress::issue_token(&client.name, tool_name);
    let _progress_guard = progress_token
        .as_ref()
        .map(|tok| client_progress::ProgressTokenGuard {
            token: tok.token.clone(),
        });
    let mut result = client
        .call(
            "tools/call",
            tool_call_params(tool_name, arguments.clone(), progress_token.as_ref(), None),
        )
        .await?;
    for _ in 0..MCP_INPUT_REQUIRED_MAX_ROUNDS {
        if result.get("resultType").and_then(|value| value.as_str())
            != Some(RESULT_TYPE_INPUT_REQUIRED)
        {
            break;
        }
        let Some(input_round) = resolve_input_required_result(&client.name, &result).await? else {
            break;
        };
        result = client
            .call(
                "tools/call",
                tool_call_params(
                    tool_name,
                    arguments.clone(),
                    progress_token.as_ref(),
                    Some(input_round),
                ),
            )
            .await?;
    }
    if result.get("resultType").and_then(|value| value.as_str()) == Some(RESULT_TYPE_INPUT_REQUIRED)
    {
        return Err(VmError::Runtime(format!(
            "MCP tool '{tool_name}' still required input after {MCP_INPUT_REQUIRED_MAX_ROUNDS} rounds"
        )));
    }

    if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
        let error_text = extract_content_text(&result);
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            error_text,
        ))));
    }

    let cache_hint = McpCacheHint::from_result(&result);
    if let Some(structured) = result.get("structuredContent") {
        return Ok((structured.clone(), cache_hint));
    }
    let content = result
        .get("content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    let unwrapped =
        if content.len() == 1 && content[0].get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = content[0].get("text").and_then(|t| t.as_str()) {
                serde_json::Value::String(text.to_string())
            } else if content.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::Array(content)
            }
        } else if content.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::Value::Array(content)
        };
    Ok((unwrapped, cache_hint))
}

pub(crate) fn tool_call_params(
    tool_name: &str,
    arguments: serde_json::Value,
    progress_token: Option<&client_progress::ProgressTokenContext>,
    input_round: Option<McpInputRound>,
) -> serde_json::Value {
    let mut params = serde_json::Map::from_iter([
        (
            "name".to_string(),
            serde_json::Value::String(tool_name.to_string()),
        ),
        ("arguments".to_string(), arguments),
    ]);
    if let Some(tok) = progress_token {
        params.insert(
            "_meta".to_string(),
            serde_json::json!({ "progressToken": tok.token }),
        );
    }
    if let Some(input_round) = input_round {
        params.insert("inputResponses".to_string(), input_round.input_responses);
        if let Some(request_state) = input_round.request_state {
            params.insert("requestState".to_string(), request_state);
        }
    }
    serde_json::Value::Object(params)
}

pub(crate) async fn resolve_input_required_result(
    server_name: &str,
    result: &serde_json::Value,
) -> Result<Option<McpInputRound>, VmError> {
    let Some(input_requests) = result
        .get("inputRequests")
        .and_then(|value| value.as_object())
    else {
        return Ok(None);
    };
    let mut responses = serde_json::Map::new();
    for (key, input_request) in input_requests {
        let Some(method) = input_request.get("method").and_then(|value| value.as_str()) else {
            return Err(VmError::Runtime(format!(
                "MCP input_required request {key:?} is missing method"
            )));
        };
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": format!("input-{key}"),
            "method": method,
            "params": input_request
                .get("params")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        });
        let response = handle_inbound_client_request(server_name, &request)
            .await
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "MCP input_required request {key:?} used unsupported method {method:?}"
                ))
            })?;
        if let Some(result) = response.get("result") {
            responses.insert(key.clone(), result.clone());
        } else if let Some(error) = response.get("error") {
            return Err(jsonrpc_error_to_vm_error(error));
        } else {
            responses.insert(key.clone(), serde_json::Value::Null);
        }
    }
    Ok(Some(McpInputRound {
        input_responses: serde_json::Value::Object(responses),
        request_state: result.get("requestState").cloned(),
    }))
}

pub fn register_mcp_builtins(vm: &mut Vm) {
    vm.register_builtin("mcp_roots", mcp_roots_builtin);
    vm.register_builtin("harn.mcp.roots", mcp_roots_builtin);
    crate::mcp_file_upload::register_mcp_file_upload_builtins(vm);
    register_harn_mcp_namespace(vm);
    register_supervised_mcp_host_builtins(vm);

    vm.register_async_builtin("mcp_connect", |_ctx, args| async move {
        let command = args.first().map(|a| a.display()).unwrap_or_default();
        if command.is_empty() {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "mcp_connect: command is required",
            ))));
        }

        let cmd_args: Vec<String> = match args.get(1) {
            Some(VmValue::List(list)) => list.iter().map(|v| v.display()).collect(),
            _ => Vec::new(),
        };

        let options = mcp_connect_options(args.get(2))?;
        let handle = mcp_connect_stdio_impl(
            &command,
            &cmd_args,
            &BTreeMap::new(),
            options.protocol_mode,
            options.protocol_version,
        )
        .await?;
        Ok(VmValue::mcp_client(handle))
    });

    // Lazy registry: ensure a registered server is booted and return its
    // live client handle. Used by skill activation (`requires_mcp`) and
    // by user code that wants to trigger a lazy connect explicitly.
    vm.register_async_builtin("mcp_ensure_active", |_ctx, args| async move {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => String::new(),
        };
        if name.is_empty() {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "mcp_ensure_active: server name is required",
            ))));
        }
        let handle = crate::mcp_registry::ensure_active(&name).await?;
        Ok(VmValue::mcp_client(handle))
    });

    // Decrement the binder refcount for a registered server. Called by
    // skill deactivation paths and by user code that manually bound via
    // `mcp_ensure_active`. No-op when the name isn't registered.
    vm.register_builtin("mcp_release", |args, _out| {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_release: server name is required",
                ))));
            }
        };
        crate::mcp_registry::release(&name);
        Ok(VmValue::Nil)
    });

    // Return the declared MCP servers and their current state as a list
    // of dicts. Purely diagnostic — useful for `harn` scripts that want
    // to show connection state in a status-line or dashboard.
    vm.register_builtin("mcp_registry_status", |_args, _out| {
        let mut out = Vec::new();
        for entry in crate::mcp_registry::snapshot_status() {
            let mut dict = BTreeMap::new();
            dict.put_str("name", entry.name.as_str());
            dict.insert("lazy".to_string(), VmValue::Bool(entry.lazy));
            dict.insert("active".to_string(), VmValue::Bool(entry.active));
            dict.insert(
                "ref_count".to_string(),
                VmValue::Int(entry.ref_count as i64),
            );
            if let Some(card) = entry.card {
                dict.put_str("card", card.as_str());
            }
            out.push(VmValue::Dict(std::sync::Arc::new(dict)));
        }
        Ok(VmValue::List(std::sync::Arc::new(out)))
    });

    // Fetch (or read from cache) the Server Card for a registered MCP
    // server, or from an explicit URL / local path.
    //
    // `mcp_server_card("notion")`           -> looks up `card = ...` in harn.toml
    // `mcp_server_card("https://.../card")` -> fetches that URL directly
    // `mcp_server_card("./card.json")`      -> reads that file directly
    vm.register_async_builtin("mcp_server_card", |_ctx, args| async move {
        let target = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_server_card: server name, URL, or path is required",
                ))));
            }
        };

        // Source resolution: if the arg looks like a URL or path
        // (contains '/', '\\', or starts with a scheme), use it as-is.
        // Otherwise treat it as a registered server name and look up
        // its `card` field. This matches the user model: "I already
        // wrote down where the card lives in harn.toml — just use it."
        let source = if target.starts_with("http://")
            || target.starts_with("https://")
            || target.contains('/')
            || target.contains('\\')
            || target.ends_with(".json")
        {
            target.clone()
        } else {
            match crate::mcp_registry::get_registration(&target) {
                Some(reg) => match reg.card {
                    Some(card) => card,
                    None => {
                        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                            format!(
                            "mcp_server_card: server '{target}' has no 'card' field in harn.toml"
                        ),
                        ))));
                    }
                },
                None => {
                    return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                        format!(
                        "mcp_server_card: no MCP server '{target}' registered (check harn.toml) \
                         — pass a URL or path directly instead"
                    ),
                    ))));
                }
            }
        };

        let card = crate::mcp_card::fetch_server_card(&source, None)
            .await
            .map_err(|e| {
                VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                    "mcp_server_card: {e}"
                ))))
            })?;
        Ok(json_to_vm_value(&card))
    });

    vm.register_async_builtin("mcp_list_tools", |_ctx, args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_list_tools: argument must be an MCP client",
                ))));
            }
        };

        let result = client.call("tools/list", serde_json::json!({})).await?;
        client.record_cache_hint("tools/list", &result).await;
        let mut tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        if client.protocol_mode().await? == McpProtocolMode::Modern {
            tools = filter_tools_for_client(&tools);
            client.store_http_tool_headers(&tools).await;
        }

        // Tag every tool with its originating server name so
        // downstream indexers (tool_search BM25) can surface them
        // under queries like "github" or "mcp:github". Harmless to
        // non-indexing callers — just an extra dict key.
        let server_name = client.name.clone();
        for tool in tools.iter_mut() {
            if let Some(obj) = tool.as_object_mut() {
                obj.entry("_mcp_server")
                    .or_insert_with(|| serde_json::Value::String(server_name.clone()));
            }
        }

        let vm_tools: Vec<VmValue> = tools.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(std::sync::Arc::new(vm_tools)))
    });

    vm.register_async_builtin("mcp_call", |_ctx, args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_call: first argument must be an MCP client",
                ))));
            }
        };

        let tool_name = args.get(1).map(|a| a.display()).unwrap_or_default();
        if tool_name.is_empty() {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "mcp_call: tool name is required",
            ))));
        }

        let arguments = match args.get(2) {
            Some(VmValue::Dict(d)) => {
                let obj: serde_json::Map<String, serde_json::Value> = d
                    .iter()
                    .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                    .collect();
                serde_json::Value::Object(obj)
            }
            _ => serde_json::json!({}),
        };

        Ok(json_to_vm_value(
            &call_mcp_tool(&client, &tool_name, arguments).await?,
        ))
    });

    vm.register_async_builtin("mcp_server_info", |_ctx, args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_server_info: argument must be an MCP client",
                ))));
            }
        };

        let guard = client.inner.lock().await;
        if guard.is_none() {
            return Err(VmError::Runtime("MCP client is disconnected".into()));
        }
        drop(guard);

        let mut info = BTreeMap::new();
        info.put_str("name", client.name.as_str());
        info.insert("connected".to_string(), VmValue::Bool(true));
        let initialize = client
            .initialize_result
            .lock()
            .await
            .clone()
            .unwrap_or(serde_json::Value::Null);
        if !initialize.is_null() {
            if let Some(instructions) = initialize
                .get("instructions")
                .or_else(|| {
                    initialize
                        .get("serverInfo")
                        .and_then(|value| value.get("instructions"))
                })
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
            {
                info.put_str("instructions", instructions);
            }
            info.insert("initialize".to_string(), json_to_vm_value(&initialize));
        }
        let cache_hints = client.cache_hints.lock().await;
        if !cache_hints.is_empty() {
            info.insert(
                "cache_hints".to_string(),
                json_to_vm_value(&cache_hints_to_json(cache_hints.iter())),
            );
        }
        Ok(VmValue::Dict(std::sync::Arc::new(info)))
    });

    vm.register_async_builtin("mcp_disconnect", |_ctx, args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_disconnect: argument must be an MCP client",
                ))));
            }
        };

        client.disconnect().await?;
        Ok(VmValue::Nil)
    });

    vm.register_async_builtin("mcp_list_resources", |_ctx, args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_list_resources: argument must be an MCP client",
                ))));
            }
        };

        let result = client.call("resources/list", serde_json::json!({})).await?;
        client.record_cache_hint("resources/list", &result).await;
        let resources = result
            .get("resources")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        let vm_resources: Vec<VmValue> = resources.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(std::sync::Arc::new(vm_resources)))
    });

    vm.register_async_builtin("mcp_read_resource", |_ctx, args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_read_resource: first argument must be an MCP client",
                ))));
            }
        };

        let uri = args.get(1).map(|a| a.display()).unwrap_or_default();
        if uri.is_empty() {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "mcp_read_resource: URI is required",
            ))));
        }

        let result = client
            .call("resources/read", serde_json::json!({ "uri": uri }))
            .await?;
        client.record_cache_hint("resources/read", &result).await;

        let contents = result
            .get("contents")
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();

        if contents.len() == 1 {
            if let Some(text) = contents[0].get("text").and_then(|t| t.as_str()) {
                return Ok(VmValue::String(std::sync::Arc::from(text)));
            }
        }

        if contents.is_empty() {
            Ok(VmValue::Nil)
        } else {
            Ok(VmValue::List(std::sync::Arc::new(
                contents.iter().map(json_to_vm_value).collect(),
            )))
        }
    });

    vm.register_async_builtin("mcp_list_resource_templates", |_ctx, args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_list_resource_templates: argument must be an MCP client",
                ))));
            }
        };

        let result = client
            .call("resources/templates/list", serde_json::json!({}))
            .await?;
        client
            .record_cache_hint("resources/templates/list", &result)
            .await;

        let templates = result
            .get("resourceTemplates")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        let vm_templates: Vec<VmValue> = templates.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(std::sync::Arc::new(vm_templates)))
    });

    vm.register_async_builtin("mcp_list_prompts", |_ctx, args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_list_prompts: argument must be an MCP client",
                ))));
            }
        };

        let result = client.call("prompts/list", serde_json::json!({})).await?;
        client.record_cache_hint("prompts/list", &result).await;

        let prompts = result
            .get("prompts")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default();

        let vm_prompts: Vec<VmValue> = prompts.iter().map(json_to_vm_value).collect();
        Ok(VmValue::List(std::sync::Arc::new(vm_prompts)))
    });

    vm.register_async_builtin("mcp_get_prompt", |_ctx, args| async move {
        let client = match args.first() {
            Some(VmValue::McpClient(c)) => c.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "mcp_get_prompt: first argument must be an MCP client",
                ))));
            }
        };

        let name = args.get(1).map(|a| a.display()).unwrap_or_default();
        if name.is_empty() {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "mcp_get_prompt: prompt name is required",
            ))));
        }

        let arguments = match args.get(2) {
            Some(VmValue::Dict(d)) => {
                let obj: serde_json::Map<String, serde_json::Value> = d
                    .iter()
                    .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                    .collect();
                serde_json::Value::Object(obj)
            }
            _ => serde_json::json!({}),
        };

        let result = client
            .call(
                "prompts/get",
                serde_json::json!({
                    "name": name,
                    "arguments": arguments,
                }),
            )
            .await?;

        Ok(json_to_vm_value(&result))
    });
}

pub(crate) fn mcp_roots_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::List(std::sync::Arc::new(
        current_mcp_roots()
            .iter()
            .map(|root| json_to_vm_value(&root.script_json()))
            .collect(),
    )))
}

pub(crate) fn register_harn_mcp_namespace(vm: &mut Vm) {
    let mcp_namespace = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
        (
            "_namespace".to_string(),
            VmValue::String(std::sync::Arc::from("harn.mcp")),
        ),
        (
            "roots".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.roots")),
        ),
        (
            "configure".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.configure")),
        ),
        (
            "file_input".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.file_input")),
        ),
        (
            "upload_file".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.upload_file")),
        ),
        // Supervised host primitive (#2504, A.7). spawn/tools/call/stop
        // route to `crate::mcp_host` and add restart-budget, circuit
        // breaker, response cache, and allowlist enforcement on top of
        // the lazy-boot `mcp_registry` they share.
        (
            "spawn".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.spawn")),
        ),
        (
            "tools".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.tools")),
        ),
        (
            "call".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.call")),
        ),
        (
            "stop".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.stop")),
        ),
        (
            "discover".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.discover")),
        ),
        (
            "reload".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.reload")),
        ),
        (
            "status".to_string(),
            VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.status")),
        ),
    ])));
    vm.set_global(
        "harn",
        VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
            (
                "_namespace".to_string(),
                VmValue::String(std::sync::Arc::from("harn")),
            ),
            (
                "mcp_roots".to_string(),
                VmValue::BuiltinRef(std::sync::Arc::from("harn.mcp.roots")),
            ),
            ("mcp".to_string(), mcp_namespace),
        ]))),
    );
}

/// Register the supervised MCP host builtins owned by [`crate::mcp_host`].
/// Each builtin is a thin adapter that converts VM values to/from JSON
/// and delegates to the host module. The host module owns supervision
/// state, the response cache, allowlist enforcement, and tracing spans.
pub(crate) fn register_supervised_mcp_host_builtins(vm: &mut Vm) {
    vm.register_async_builtin("harn.mcp.spawn", |_ctx, args| async move {
        let spec_value = args
            .first()
            .ok_or_else(|| VmError::Runtime("harn.mcp.spawn: spec dict is required".into()))?;
        let spec = vm_value_to_serde(spec_value);
        let options: crate::mcp_host::SpawnOptions = match args.get(1) {
            Some(VmValue::Dict(_)) => {
                let options_json = vm_value_to_serde(args.get(1).unwrap());
                serde_json::from_value(options_json).map_err(|e| {
                    VmError::Runtime(format!("harn.mcp.spawn: invalid options dict: {e}"))
                })?
            }
            _ => Default::default(),
        };
        let name = crate::mcp_host::spawn(spec, options).await?;
        Ok(VmValue::String(std::sync::Arc::from(name)))
    });

    vm.register_async_builtin("harn.mcp.tools", |_ctx, args| async move {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => {
                return Err(VmError::Runtime(
                    "harn.mcp.tools: server name (string) is required".into(),
                ));
            }
        };
        let tools = crate::mcp_host::tools(&name).await?;
        Ok(VmValue::List(std::sync::Arc::new(
            tools.iter().map(json_to_vm_value).collect(),
        )))
    });

    vm.register_async_builtin("harn.mcp.call", |_ctx, args| async move {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => {
                return Err(VmError::Runtime(
                    "harn.mcp.call: server name (string) is required".into(),
                ));
            }
        };
        let tool = match args.get(1) {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => {
                return Err(VmError::Runtime(
                    "harn.mcp.call: tool name (string) is required".into(),
                ));
            }
        };
        let tool_args = match args.get(2) {
            Some(VmValue::Dict(d)) => {
                let obj: serde_json::Map<String, serde_json::Value> = d
                    .iter()
                    .map(|(k, v)| (k.clone(), vm_value_to_serde(v)))
                    .collect();
                serde_json::Value::Object(obj)
            }
            Some(VmValue::Nil) | None => serde_json::json!({}),
            Some(other) => vm_value_to_serde(other),
        };
        let result = crate::mcp_host::call(&name, &tool, tool_args).await?;
        Ok(json_to_vm_value(&result))
    });

    vm.register_builtin("harn.mcp.stop", |args, _out| {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => {
                return Err(VmError::Runtime(
                    "harn.mcp.stop: server name (string) is required".into(),
                ));
            }
        };
        crate::mcp_host::stop(&name)?;
        Ok(VmValue::Nil)
    });

    vm.register_builtin("harn.mcp.reload", |args, _out| {
        let name = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            Some(other) => other.display(),
            None => {
                return Err(VmError::Runtime(
                    "harn.mcp.reload: server name (string) is required".into(),
                ));
            }
        };
        crate::mcp_host::reload(&name)?;
        Ok(VmValue::Nil)
    });

    vm.register_async_builtin("harn.mcp.discover", |_ctx, _args| async move {
        let entries = crate::mcp_host::discover().await?;
        Ok(VmValue::List(std::sync::Arc::new(
            entries.iter().map(json_to_vm_value).collect(),
        )))
    });

    vm.register_builtin("harn.mcp.status", |_args, _out| {
        let entries = crate::mcp_host::status();
        let list: Vec<VmValue> = entries
            .into_iter()
            .map(|s| {
                let mut dict = BTreeMap::new();
                dict.put_str("name", s.name);
                dict.insert("active".to_string(), VmValue::Bool(s.active));
                dict.insert("lazy".to_string(), VmValue::Bool(s.lazy));
                dict.insert("ref_count".to_string(), VmValue::Int(s.ref_count as i64));
                dict.insert(
                    "restart_count".to_string(),
                    VmValue::Int(s.restart_count as i64),
                );
                dict.insert(
                    "consecutive_failures".to_string(),
                    VmValue::Int(s.consecutive_failures as i64),
                );
                dict.put_str("circuit", s.circuit.as_str());
                dict.insert("ejected".to_string(), VmValue::Bool(s.ejected));
                dict.insert(
                    "cache_entries".to_string(),
                    VmValue::Int(s.cache_entries as i64),
                );
                VmValue::Dict(std::sync::Arc::new(dict))
            })
            .collect();
        Ok(VmValue::List(std::sync::Arc::new(list)))
    });
}
