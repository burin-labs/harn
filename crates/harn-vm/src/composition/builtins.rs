use super::*;
use crate::stdlib::macros::harn_builtin;

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "composition_binding_manifest(tools: list | dict, options?: dict | nil) -> dict",
    category = "composition"
)]
fn composition_binding_manifest_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let tools = args
        .first()
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(Value::Null);
    let options_json = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(Value::Null);
    let mut options = BindingManifestOptions::default();
    if let Some(ceiling) = options_json
        .get("side_effect_ceiling")
        .and_then(Value::as_str)
    {
        options.side_effect_ceiling = SideEffectLevel::parse(ceiling);
    }
    if let Some(include_denied) = options_json.get("include_denied").and_then(Value::as_bool) {
        options.include_denied = include_denied;
    }
    options.denied_tools = string_set_option(&options_json, "denied_tools");
    options.gated_tools = string_set_option(&options_json, "gated_tools");
    options.state = composition_state_option(&options_json)?;
    let manifest = binding_manifest_from_tool_surface(&tools, options);
    let value = if options_json.get("form").and_then(Value::as_str) == Some("compact") {
        manifest.to_compact_value()
    } else {
        manifest.to_value()
    };
    Ok(crate::json_to_vm_value(&value))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "composition_search_examples(query?: string, limit?: int) -> list",
    category = "composition"
)]
fn composition_search_examples_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let query = args.first().map(VmValue::display).unwrap_or_default();
    let limit = args
        .get(1)
        .and_then(VmValue::as_int)
        .map(|limit| limit.max(1) as usize)
        .unwrap_or(10);
    Ok(crate::json_to_vm_value(&composition_search_examples(
        &query, limit,
    )))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "composition_typescript_declarations(manifest: dict) -> string",
    category = "composition"
)]
fn composition_typescript_declarations_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let manifest_value = args
        .first()
        .map(crate::llm::vm_value_to_json)
        .ok_or_else(|| {
            VmError::Runtime("composition_typescript_declarations: manifest is required".into())
        })?;
    let manifest: BindingManifest = serde_json::from_value(manifest_value).map_err(|error| {
        VmError::Runtime(format!(
            "composition_typescript_declarations: invalid manifest: {error}"
        ))
    })?;
    Ok(VmValue::String(arcstr::ArcStr::from(
        composition_typescript_declarations(&manifest),
    )))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "composition_harn_api(manifest: dict) -> string",
    category = "composition"
)]
fn composition_harn_api_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let manifest_value = args
        .first()
        .map(crate::llm::vm_value_to_json)
        .ok_or_else(|| VmError::Runtime("composition_harn_api: manifest is required".into()))?;
    let manifest: BindingManifest = serde_json::from_value(manifest_value).map_err(|error| {
        VmError::Runtime(format!("composition_harn_api: invalid manifest: {error}"))
    })?;
    Ok(VmValue::String(arcstr::ArcStr::from(composition_harn_api(
        &manifest,
    ))))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "composition_crystallization_trace(report: dict, options?: dict | nil) -> dict",
    category = "composition"
)]
fn composition_crystallization_trace_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let report_value = args
        .first()
        .map(crate::llm::vm_value_to_json)
        .ok_or_else(|| {
            VmError::Runtime("composition_crystallization_trace: report is required".into())
        })?;
    let report: CompositionExecutionReport =
        serde_json::from_value(report_value).map_err(|error| {
            VmError::Runtime(format!(
                "composition_crystallization_trace: invalid report: {error}"
            ))
        })?;
    let options = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    Ok(crate::json_to_vm_value(&composition_crystallization_trace(
        &report, &options,
    )))
}

pub fn register_composition_builtins(vm: &mut Vm) {
    vm.register_builtin_def(&COMPOSITION_BINDING_MANIFEST_IMPL_DEF);
    vm.register_builtin_def(&COMPOSITION_SEARCH_EXAMPLES_IMPL_DEF);
    vm.register_builtin_def(&COMPOSITION_TYPESCRIPT_DECLARATIONS_IMPL_DEF);
    vm.register_builtin_def(&COMPOSITION_HARN_API_IMPL_DEF);
    vm.register_builtin_def(&COMPOSITION_CRYSTALLIZATION_TRACE_IMPL_DEF);

    vm.register_async_capability_method(
        harn_builtin_meta::CapabilityId::Tools,
        "composition_execute",
        |ctx, args| async move {
            let snippet = args.first().map(VmValue::display).ok_or_else(|| {
                VmError::Runtime("composition_execute: snippet is required".into())
            })?;
            let manifest_value =
                args.get(1)
                    .map(crate::llm::vm_value_to_json)
                    .ok_or_else(|| {
                        VmError::Runtime("composition_execute: manifest is required".into())
                    })?;
            let dispatcher = args.get(2).and_then(|value| match value {
                VmValue::Closure(closure) => Some((**closure).clone()),
                VmValue::Dict(dict) => match dict.get("dispatcher") {
                    Some(VmValue::Closure(closure)) => Some((**closure).clone()),
                    _ => None,
                },
                _ => None,
            });
            let mut request = CompositionExecutionRequest {
                execution_id: Some(ctx.execution_id()),
                snippet,
                manifest: serde_json::from_value(manifest_value).map_err(|error| {
                    VmError::Runtime(format!("composition_execute: invalid manifest: {error}"))
                })?,
                ..CompositionExecutionRequest::default()
            };
            if let Some(options) = args.get(2).map(crate::llm::vm_value_to_json) {
                if let Some(session_id) = options.get("session_id").and_then(Value::as_str) {
                    request.session_id = Some(session_id.to_string());
                }
                if let Some(run_id) = options.get("run_id").and_then(Value::as_str) {
                    request.run_id = run_id.to_string();
                }
                if let Some(max_operations) = options.get("max_operations").and_then(Value::as_u64)
                {
                    request.limits.max_operations = max_operations;
                }
                if let Some(timeout_ms) = options.get("timeout_ms").and_then(Value::as_u64) {
                    request.limits.timeout_ms = Some(timeout_ms);
                }
                if let Some(max_output_bytes) =
                    options.get("max_output_bytes").and_then(Value::as_u64)
                {
                    request.limits.max_output_bytes = max_output_bytes;
                }
                if let Some(max_concurrent) = options
                    .get("max_concurrent_operations")
                    .or_else(|| options.get("max_concurrent"))
                    .and_then(Value::as_u64)
                {
                    request.limits.max_concurrent_operations =
                        usize::try_from(max_concurrent).unwrap_or(usize::MAX).max(1);
                }
                if let Some(per_server) = options
                    .get("max_concurrent_per_server")
                    .or_else(|| options.get("per_server_concurrency"))
                    .and_then(Value::as_u64)
                {
                    request.limits.max_concurrent_per_server =
                        usize::try_from(per_server).unwrap_or(usize::MAX).max(1);
                }
                let trusted_servers = string_set_option(&options, "trusted_servers");
                let trusted_mcp_servers = string_set_option(&options, "trusted_mcp_servers");
                if !trusted_servers.is_empty() || !trusted_mcp_servers.is_empty() {
                    request
                        .mcp_policy
                        .trusted_servers
                        .extend(trusted_servers.into_iter().chain(trusted_mcp_servers));
                }
                if let Some(trust_annotations) = options
                    .get("trust_annotations")
                    .or_else(|| options.get("trust_mcp_annotations"))
                    .and_then(Value::as_bool)
                {
                    request.mcp_policy.trust_annotations = trust_annotations;
                }
                if let Some(call_timeout_ms) =
                    options.get("call_timeout_ms").and_then(Value::as_u64)
                {
                    request.mcp_policy.call_timeout_ms = Some(call_timeout_ms);
                }
                if let Some(retry_options) = options.get("retry") {
                    if let Some(max_attempts) =
                        retry_options.get("max_attempts").and_then(Value::as_u64)
                    {
                        request.mcp_policy.retry.max_attempts =
                            u32::try_from(max_attempts).unwrap_or(u32::MAX).max(1);
                    }
                    if let Some(base_delay_ms) =
                        retry_options.get("base_delay_ms").and_then(Value::as_u64)
                    {
                        request.mcp_policy.retry.base_delay_ms = base_delay_ms;
                    }
                    if let Some(max_delay_ms) =
                        retry_options.get("max_delay_ms").and_then(Value::as_u64)
                    {
                        request.mcp_policy.retry.max_delay_ms = max_delay_ms;
                    }
                    if let Some(honor_retry_after) = retry_options
                        .get("honor_retry_after")
                        .and_then(Value::as_bool)
                    {
                        request.mcp_policy.retry.honor_retry_after = honor_retry_after;
                    }
                }
            }
            if request.manifest.state.is_some()
                && request
                    .session_id
                    .as_deref()
                    .is_none_or(|session_id| session_id.trim().is_empty())
            {
                request.session_id = crate::llm::current_agent_session_id();
            }
            let host: Arc<dyn CompositionToolHost> = match dispatcher {
                Some(closure) => Arc::new(ClosureCompositionToolHost::new(closure, ctx.clone())),
                None => Arc::new(StaticCompositionToolHost::new(BTreeMap::new())),
            };
            let report = execute_harn_composition(request, host).await;
            Ok(crate::json_to_vm_value(
                &serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({"ok": false})),
            ))
        },
    );
}

fn string_set_option(value: &Value, key: &str) -> BTreeSet<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn composition_state_option(value: &Value) -> Result<Option<CompositionStateBinding>, VmError> {
    let Some(state) = value.get("state") else {
        return Ok(None);
    };
    match state {
        Value::Bool(false) | Value::Null => Ok(None),
        Value::Bool(true) => Ok(Some(CompositionStateBinding::default())),
        Value::Object(fields) => {
            if fields.get("enabled").and_then(Value::as_bool) == Some(false) {
                return Ok(None);
            }
            let mut manifest_fields = fields.clone();
            manifest_fields.remove("enabled");
            let binding: CompositionStateBinding =
                serde_json::from_value(Value::Object(manifest_fields)).map_err(|error| {
                    CompositionStateError::invalid_limits(format!(
                        "invalid composition state binding: {error}"
                    ))
                    .into_vm_error()
                })?;
            binding
                .validate()
                .map_err(CompositionStateError::into_vm_error)?;
            Ok(Some(binding))
        }
        _ => Err(CompositionStateError::invalid_limits(
            "composition state option must be true, false, or an object",
        )
        .into_vm_error()),
    }
}
