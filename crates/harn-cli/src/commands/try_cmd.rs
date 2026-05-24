//! `harn try "<prompt>"` — dispatches to the embedded
//! `cli/try.harn` script (see harn#2302 / W2). The shim is
//! intentionally tiny because all the agent_loop wiring lives in
//! the .harn impl now.
//!
//! `HARN_CLI_IMPL=rust` is reserved for the parity-snapshot harness
//! to keep both impls comparable until the harn impl is the default
//! everywhere (see C1 ratchet, #2314).

use harn_vm::llm_config;

use crate::cli::TryArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

#[path = "try_cmd_legacy.rs"]
mod legacy;

pub(crate) async fn run(args: TryArgs) {
    if !mock_provider_active() && llm_config::available_provider_names().is_empty() {
        eprintln!("{}", crate::commands::doctor::no_credentials_hint());
        std::process::exit(1);
    }

    let resolved = resolve_try_model();

    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        legacy::run(args, &resolved.provider, &resolved.model).await;
        return;
    }

    let _max = ScopedEnvVar::set("HARN_TRY_MAX_ITERS", &args.max_iterations.to_string());
    let _provider = ScopedEnvVar::set("HARN_TRY_PROVIDER", &resolved.provider);
    let _model = ScopedEnvVar::set("HARN_TRY_MODEL", &resolved.model);
    let _tool_format = args
        .tool_format
        .as_deref()
        .map(|value| ScopedEnvVar::set("HARN_TRY_TOOL_FORMAT", value));
    let _override_reason = args
        .override_reason
        .as_deref()
        .map(|value| ScopedEnvVar::set("HARN_TRY_OVERRIDE_REASON", value));
    let exit =
        dispatch::dispatch_to_embedded_script("try", vec![args.prompt], /* json_mode */ false)
            .await;
    if exit != 0 {
        std::process::exit(exit);
    }
}

fn mock_provider_active() -> bool {
    std::env::var("HARN_LLM_PROVIDER")
        .map(|v| v == "mock")
        .unwrap_or(false)
}

struct ResolvedTryModel {
    provider: String,
    model: String,
}

fn resolve_try_model() -> ResolvedTryModel {
    if let Ok(provider) = std::env::var("HARN_LLM_PROVIDER") {
        let provider = provider.trim().to_string();
        if !provider.is_empty() && !provider.eq_ignore_ascii_case("auto") {
            let model = std::env::var("HARN_LLM_MODEL")
                .ok()
                .map(|raw| harn_vm::llm_config::resolve_model(&raw).0)
                .unwrap_or_else(|| harn_vm::llm_config::default_model_for_provider(&provider));
            return ResolvedTryModel { provider, model };
        }
    }

    if let Ok(raw_model) = std::env::var("HARN_LLM_MODEL") {
        let resolved = harn_vm::llm_config::resolve_model_info(&raw_model);
        return ResolvedTryModel {
            provider: resolved.provider,
            model: resolved.id,
        };
    }

    let provider = harn_vm::llm_config::default_provider();
    let model = harn_vm::llm_config::default_model_for_provider(&provider);
    ResolvedTryModel { provider, model }
}
