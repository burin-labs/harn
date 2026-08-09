//! `harn provider` / `harn model info`: reading a model's resolved route,
//! capability row, and local readiness, and rendering the provider catalog.

use crate::*;

pub(crate) async fn print_model_info(args: &ModelInfoArgs) -> bool {
    let resolved = harn_vm::llm_config::resolve_model_info(&args.model);
    let api_key_result = harn_vm::llm::resolve_api_key(&resolved.provider);
    let api_key_set = api_key_result.is_ok();
    let api_key = api_key_result.unwrap_or_default();
    let context_window =
        harn_vm::llm::fetch_provider_max_context(&resolved.provider, &resolved.id, &api_key).await;
    let readiness = local_provider_readiness(&resolved.provider, &resolved.id, &api_key).await;
    let catalog = harn_vm::llm_config::model_catalog_entry(&resolved.id);
    let runtime_context_window = catalog
        .as_ref()
        .and_then(|entry| entry.runtime_context_window);
    let capabilities = harn_vm::llm::capabilities::lookup(&resolved.provider, &resolved.id);
    let batch_api =
        harn_vm::llm_config::effective_batch_api_supported(&resolved.provider, &capabilities);
    let mut payload = serde_json::json!({
        "alias": args.model,
        "id": resolved.id,
        "provider": resolved.provider,
        "resolved_alias": resolved.alias,
        "tool_format": resolved.tool_format,
        "tier": resolved.tier,
        "api_key_set": api_key_set,
        "context_window": context_window,
        "runtime_context_window": runtime_context_window,
        "readiness": readiness,
        "catalog": catalog,
        "capabilities": {
            "native_tools": capabilities.native_tools,
            "defer_loading": capabilities.defer_loading,
            "tool_search": capabilities.tool_search,
            "max_tools": capabilities.max_tools,
            "prompt_caching": capabilities.prompt_caching,
            "batch_api": batch_api,
            "batch_wire_format": if batch_api { capabilities.batch_wire_format } else { None },
            "batch_input_mode": if batch_api { capabilities.batch_input_mode } else { None },
            "batch_discount_percent": if batch_api { capabilities.batch_discount_percent } else { None },
            "batch_turnaround_hours": if batch_api { capabilities.batch_turnaround_hours } else { None },
            "vision": capabilities.vision,
            "vision_supported": capabilities.vision_supported,
            "audio": capabilities.audio,
            "pdf": capabilities.pdf,
            "files_api_supported": capabilities.files_api_supported,
            "json_schema": capabilities.json_schema,
            "prefers_xml_scaffolding": capabilities.prefers_xml_scaffolding,
            "prefers_markdown_scaffolding": capabilities.prefers_markdown_scaffolding,
            "structured_output_mode": capabilities.structured_output_mode,
            "supports_assistant_prefill": capabilities.supports_assistant_prefill,
            "prefers_role_developer": capabilities.prefers_role_developer,
            "prefers_xml_tools": capabilities.prefers_xml_tools,
            "thinking": !capabilities.thinking_modes.is_empty(),
            "thinking_block_style": capabilities.thinking_block_style,
            "thinking_modes": capabilities.thinking_modes,
            "interleaved_thinking_supported": capabilities.interleaved_thinking_supported,
            "anthropic_beta_features": capabilities.anthropic_beta_features,
            "preserve_thinking": capabilities.preserve_thinking,
            "server_parser": capabilities.server_parser,
            "honors_chat_template_kwargs": capabilities.honors_chat_template_kwargs,
            "recommended_endpoint": capabilities.recommended_endpoint,
            "text_tool_wire_format_supported": capabilities.text_tool_wire_format_supported,
            "preferred_tool_format": capabilities.preferred_tool_format,
            "tool_mode_parity": capabilities.tool_mode_parity,
            "tool_mode_parity_notes": capabilities.tool_mode_parity_notes,
        },
        "qc_default_model": harn_vm::llm_config::qc_default_model(&resolved.provider),
    });

    let should_verify = args.verify || args.warm;
    let mut ok = true;
    if should_verify {
        if commands::local::runtime::uses_ollama_wire_protocol(&resolved.provider) {
            let mut readiness = harn_vm::llm::OllamaReadinessOptions::new(resolved.id.clone());
            readiness.warm = args.warm;
            readiness.observe_loaded = true;
            readiness.keep_alive = args
                .keep_alive
                .as_deref()
                .and_then(harn_vm::llm::normalize_ollama_keep_alive);
            let result = harn_vm::llm::ollama_readiness(readiness).await;
            ok = result.valid;
            payload["readiness"] = serde_json::to_value(&result).unwrap_or_else(|error| {
                serde_json::json!({
                    "valid": false,
                    "status": "serialization_error",
                    "message": format!("failed to serialize readiness result: {error}"),
                })
            });
        } else {
            ok = false;
            payload["readiness"] = serde_json::json!({
                "valid": false,
                "status": "unsupported_provider",
                "message": format!(
                    "models info --verify is only supported for models whose local runtime declares the Ollama API protocol; resolved provider is '{}'",
                    resolved.provider
                ),
                "provider": resolved.provider,
            });
        }
    }

    println!(
        "{}",
        serde_json::to_string(&payload).unwrap_or_else(|error| {
            command_error(&format!("failed to serialize model info: {error}"))
        })
    );
    ok
}

pub(crate) async fn local_provider_readiness(
    provider: &str,
    model: &str,
    api_key: &str,
) -> Option<serde_json::Value> {
    let def = harn_vm::llm_config::provider_config(provider)?;
    if def.auth_style != "none" || !harn_vm::llm::supports_model_readiness_probe(&def) {
        return None;
    }
    let readiness = harn_vm::llm::readiness::probe_provider_readiness_with_options(
        provider,
        harn_vm::llm::readiness::ProviderReadinessOptions {
            requested_model: Some(model),
            base_url_override: None,
            api_key_override: Some(api_key),
        },
    )
    .await;
    Some(serde_json::to_value(readiness).unwrap_or_else(|error| {
        serde_json::json!({
            "ok": false,
            "status": "bad_response",
            "message": format!("failed to serialize readiness result: {error}"),
            "provider": provider,
        })
    }))
}

pub(crate) fn build_provider_catalog_payload(available_only: bool) -> serde_json::Value {
    let provider_names = if available_only {
        harn_vm::llm::available_provider_names()
    } else {
        harn_vm::llm_config::provider_names()
    };
    let providers: Vec<_> = provider_names
        .into_iter()
        .filter_map(|name| {
            harn_vm::llm_config::provider_config(&name).map(|def| {
                serde_json::json!({
                    "name": name,
                    "display_name": def.display_name,
                    "icon": def.icon,
                    "base_url": harn_vm::llm_config::resolve_base_url(&def),
                    "base_url_env": def.base_url_env,
                    "region_env": def.region_env,
                    "regions": def.regions,
                    "auth_style": def.auth_style,
                    "auth_envs": harn_vm::llm_config::auth_env_names(&def.auth_env),
                    "auth_available": harn_vm::llm::provider_auth_status(&name).available,
                    "features": def.features,
                    "cost_per_1k_in": def.cost_per_1k_in,
                    "cost_per_1k_out": def.cost_per_1k_out,
                    "latency_p50_ms": def.latency_p50_ms,
                    "performance": def.performance,
                })
            })
        })
        .collect();
    let models: Vec<_> = harn_vm::llm_config::model_catalog_entries()
        .into_iter()
        .map(|(id, model)| {
            serde_json::json!({
                "id": id,
                "name": model.name,
                "provider": model.provider,
                "context_window": model.context_window,
                "runtime_context_window": model.runtime_context_window,
                "stream_timeout": model.stream_timeout,
                "capabilities": model.capabilities,
                "pricing": model.pricing,
                "performance": model.performance,
            })
        })
        .collect();
    let aliases: Vec<_> = harn_vm::llm_config::alias_entries()
        .into_iter()
        .map(|(name, alias)| {
            serde_json::json!({
                "name": name,
                "id": alias.id,
                "provider": alias.provider,
                "tool_format": alias.tool_format,
                "tool_calling": harn_vm::llm_config::alias_tool_calling_entry(&name),
            })
        })
        .collect();
    let routing_routes = harn_vm::provider_catalog::artifact().routing_routes;
    serde_json::json!({
        "providers": providers,
        "known_model_names": harn_vm::llm_config::known_model_names(),
        "available_providers": harn_vm::llm::available_provider_names(),
        "aliases": aliases,
        "models": models,
        "routing_routes": routing_routes,
        "qc_defaults": harn_vm::llm_config::qc_defaults(),
    })
}

/// Dispatch shim for `harn provider catalog show`. Aggregation stays in
/// Rust (the script can't reach `llm_config` for the catalog walk);
/// the .harn renderer in `stdlib/cli/providers/catalog.harn` only
/// re-emits the JSON envelope.
///
/// Lock keeps concurrent in-process callers from racing on the global
/// env var the dispatch wedge reads — same pattern as the other
/// partial-port commands (see harn#2305 / #2309).
pub(crate) async fn dispatch_provider_catalog(available_only: bool) -> i32 {
    static DISPATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let payload = build_provider_catalog_payload(available_only);
    let payload_json = match serde_json::to_string(&payload) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise provider catalog payload: {error}");
            return 1;
        }
    };
    let _guard = DISPATCH_LOCK.lock().await;
    let _payload_guard =
        crate::env_guard::ScopedEnvVar::set("HARN_PROVIDER_CATALOG_PAYLOAD_JSON", &payload_json);
    // `--available-only` doesn't enable JSON; the catalog dump is JSON-
    // only on both impls, but pass `true` so the dispatch wedge sets
    // HARN_OUTPUT_JSON for symmetry with peer scripts.
    crate::dispatch::dispatch_to_embedded_script("providers/catalog", Vec::new(), true).await
}

pub(crate) async fn run_provider_ready(
    provider: &str,
    model: Option<&str>,
    base_url: Option<&str>,
    json: bool,
) {
    let readiness =
        harn_vm::llm::readiness::probe_provider_readiness(provider, model, base_url).await;
    if json {
        match serde_json::to_string_pretty(&readiness) {
            Ok(payload) => println!("{payload}"),
            Err(error) => command_error(&format!("failed to serialize readiness result: {error}")),
        }
    } else if readiness.ok {
        println!("{}", readiness.message);
    } else {
        eprintln!("{}", readiness.message);
    }
    if !readiness.ok {
        process::exit(1);
    }
}
