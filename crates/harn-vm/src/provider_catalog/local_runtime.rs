//! Validation for catalog-declared local provider lifecycle contracts.

use super::*;

pub(super) fn validate(
    provider_id: &str,
    runtime: &llm_config::LocalRuntimeDef,
    result: &mut ProviderCatalogValidation,
) {
    let owner = format!("provider {provider_id}");
    if let Err(error) = runtime.lifecycle() {
        result.errors.push(format!("{owner} {error}"));
    }
    if runtime.kind == Some(llm_config::LocalRuntimeKind::ManagedProcess)
        && runtime
            .command
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        result
            .errors
            .push(format!("{owner} local_runtime.command cannot be empty"));
    }
    for (field, value) in [
        ("command", runtime.command.as_deref()),
        ("model_source", runtime.model_source.as_deref()),
        ("model_source_env", runtime.model_source_env.as_deref()),
        ("model_arg", runtime.model_arg.as_deref()),
        ("served_model_arg", runtime.served_model_arg.as_deref()),
        ("host_arg", runtime.host_arg.as_deref()),
        ("port_arg", runtime.port_arg.as_deref()),
        ("ctx_arg", runtime.ctx_arg.as_deref()),
        ("parallel_arg", runtime.parallel_arg.as_deref()),
        ("gpu_layers_arg", runtime.gpu_layers_arg.as_deref()),
        ("cache_type_k_arg", runtime.cache_type_k_arg.as_deref()),
        ("cache_type_v_arg", runtime.cache_type_v_arg.as_deref()),
        ("cache_ram_arg", runtime.cache_ram_arg.as_deref()),
        (
            "chat_template_kwargs_arg",
            runtime.chat_template_kwargs_arg.as_deref(),
        ),
        ("jinja_arg", runtime.jinja_arg.as_deref()),
        ("reasoning_arg", runtime.reasoning_arg.as_deref()),
        (
            "reasoning_format_arg",
            runtime.reasoning_format_arg.as_deref(),
        ),
        ("flash_attn_arg", runtime.flash_attn_arg.as_deref()),
        ("metrics_arg", runtime.metrics_arg.as_deref()),
        ("enable_lora_arg", runtime.enable_lora_arg.as_deref()),
        ("lora_modules_arg", runtime.lora_modules_arg.as_deref()),
        (
            "lora_modules_value_format",
            runtime.lora_modules_value_format.as_deref(),
        ),
        ("max_lora_rank_arg", runtime.max_lora_rank_arg.as_deref()),
        ("source_url", runtime.source_url.as_deref()),
        ("last_verified", runtime.last_verified.as_deref()),
        ("notes", runtime.notes.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            result
                .errors
                .push(format!("{owner} local_runtime.{field} cannot be empty"));
        }
    }
    if let Some(format) = runtime.lora_modules_value_format.as_deref() {
        if !matches!(format, "name_path" | "json_with_base_model") {
            result.errors.push(format!(
                "{owner} local_runtime.lora_modules_value_format must be name_path or json_with_base_model"
            ));
        }
    }
    if let Some(source_url) = runtime.source_url.as_deref() {
        if !(source_url.starts_with("https://") || source_url.starts_with("http://")) {
            result.warnings.push(format!(
                "{owner} local_runtime.source_url should be an absolute URL"
            ));
        }
    }
    validate_last_verified(
        &owner,
        "local_runtime.last_verified",
        runtime.last_verified.as_deref(),
        result,
    );
}
