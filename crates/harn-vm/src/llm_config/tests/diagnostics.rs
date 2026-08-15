//! providers.toml parse diagnostics: unknown fields must be reported with
//! the path that produced them, and current schema fields must stay silent.

use super::super::*;

fn diagnostic_texts(src: &str) -> Vec<String> {
    parse_config_toml_with_diagnostics(src)
        .expect("config parses")
        .diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.to_string())
        .collect()
}

#[test]
fn parse_config_warns_on_unknown_model_fast_mode_field() {
    let diagnostics = diagnostic_texts(
        r#"
[models."demo/model"]
name = "Demo"
provider = "demo"
context_window = 4096
fast_mode = true
"#,
    );
    assert!(
        diagnostics.iter().any(
            |diagnostic| diagnostic.contains("models.demo/model.fast_mode")
                && diagnostic.contains("unknown providers.toml field")
                && diagnostic.contains("serving_tiers")
        ),
        "expected fast_mode migration diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn parse_config_warns_on_unknown_provider_field() {
    let diagnostics = diagnostic_texts(
        r#"
[providers.demo]
base_url = "https://example.invalid"
chat_endpoint = "/v1/chat/completions"
surprise_knob = true
"#,
    );
    assert!(
        diagnostics.iter().any(
            |diagnostic| diagnostic.contains("providers.demo.surprise_knob")
                && diagnostic.contains("unknown providers.toml field")
        ),
        "expected provider unknown-field diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn parse_config_warns_on_unknown_patch_model_field() {
    let diagnostics = diagnostic_texts(
        r#"
[patch.models."demo/model"]
stream_timeout = 120.0
fast_mode = true
"#,
    );
    assert!(
        diagnostics.iter().any(|diagnostic| diagnostic
            .contains("patch.models.demo/model.fast_mode")
            && diagnostic.contains("serving_tiers")),
        "expected patch model fast_mode diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn parse_config_warns_on_patch_model_batch_table() {
    let diagnostics = diagnostic_texts(
        r#"
[patch.models."demo/model".batch]
supported = true
endpoint = "/v1/batches"
"#,
    );
    assert!(
        diagnostics.iter().any(
            |diagnostic| diagnostic.contains("patch.models.demo/model.batch")
                && diagnostic.contains("unknown providers.toml field")
        ),
        "expected patch model batch diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn parse_config_accepts_current_vllm_lora_runtime_fields() {
    let diagnostics = diagnostic_texts(
        r#"
[providers.vllm.local_runtime]
kind = "managed_process"
command = "vllm"
prefix_args = ["serve"]
enable_lora_arg = "--enable-lora"
lora_modules_arg = "--lora-modules"
lora_modules_value_format = "name_path"
max_lora_rank_arg = "--max-lora-rank"
"#,
    );
    assert!(
        diagnostics.is_empty(),
        "current local-runtime LoRA fields must not warn, got {diagnostics:?}"
    );
}

#[test]
fn parse_config_warns_on_unknown_presentation_field() {
    let diagnostics = diagnostic_texts(
        r#"
[presentation.families.demo]
label = "Demo"
plain_description = "Demo family"
model_id = "demo-model"
dimensions = []
presets = []
surprise_knob = true
"#,
    );
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("presentation.families.demo.surprise_knob")
                && diagnostic.contains("unknown providers.toml field")
        }),
        "expected presentation unknown-field diagnostic, got {diagnostics:?}"
    );
}
