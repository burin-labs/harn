use super::*;
use harn_glob::match_name as glob_match;
use serde::Deserialize;
use std::collections::BTreeMap;

fn reset_overrides() {
    clear_user_overrides();
}

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
fn resolve_model_info_guards_bad_native_pin_on_unreliable_route() {
    reset_overrides();
    // An alias that pins tool_format = "native" for DeepSeek V3.2 on
    // OpenRouter — a route the capability registry knows is
    // native_unreliable (drops to unparsed DSML text). Before the
    // footgun-removal gate this bad pin survived resolution verbatim and
    // produced vanishing tool calls; now it is steered to the route's safe
    // text-channel format.
    let overlay = parse_config_toml(
            "[aliases.guard-ds]\nid = \"deepseek/deepseek-v3.2\"\nprovider = \"openrouter\"\ntool_format = \"native\"\n",
        )
        .expect("overlay parses");
    set_user_overrides(Some(overlay));
    let resolved = resolve_model_info("guard-ds");
    assert_eq!(
        resolved.tool_format, "text",
        "a native pin on a native_unreliable route must be auto-corrected to text"
    );
    clear_user_overrides();

    // A safe native pin (a route with no adverse parity) is untouched.
    let overlay_ok = parse_config_toml(
            "[aliases.guard-ds-ok]\nid = \"deepseek/deepseek-v3-base\"\nprovider = \"openrouter\"\ntool_format = \"native\"\n",
        )
        .expect("overlay parses");
    set_user_overrides(Some(overlay_ok));
    let resolved_ok = resolve_model_info("guard-ds-ok");
    assert_eq!(resolved_ok.tool_format, "native");
    clear_user_overrides();
}

#[test]
fn auto_select_prefers_local_provider_without_cloud_credentials() {
    // A catalog whose only provider is local and auth-free resolves to it
    // regardless of ambient cloud API keys: no preferred/credentialed cloud
    // provider is present, so the local fallback wins deterministically.
    let config = parse_config_toml(
            "[providers.ollama]\nbase_url = \"http://localhost:11434\"\nchat_endpoint = \"/v1/chat/completions\"\n",
        )
        .expect("config parses");
    assert!(provider_is_local(config.providers.get("ollama").unwrap()));
    assert_eq!(auto_select_provider(&config), "ollama");
}

#[test]
fn auto_select_falls_back_to_documented_default_when_empty() {
    let config = parse_config_toml("").expect("config parses");
    assert_eq!(auto_select_provider(&config), FALLBACK_PROVIDER);
}

#[test]
fn suppress_routes_parse_and_merge_dedupe() {
    let mut base =
        parse_config_toml("[suppress]\nroutes = [\"together:Qwen/Qwen3-Coder-Next-FP8\"]\n")
            .expect("base parses");
    assert!(!base.is_empty(), "a suppress-only overlay is not empty");
    let overlay = parse_config_toml(
        "[suppress]\nroutes = [\"together:Qwen/Qwen3-Coder-Next-FP8\", \"ollama:img:tag\"]\n",
    )
    .expect("overlay parses");
    base.merge_from(&overlay);
    assert_eq!(
        base.suppress.routes,
        vec![
            "together:Qwen/Qwen3-Coder-Next-FP8".to_string(),
            "ollama:img:tag".to_string(),
        ],
        "merge appends new selectors without duplicating existing ones"
    );
}

/// Base config for the `[patch.models]` tests: one fully-populated row.
const PATCH_BASE_TOML: &str = r#"
[models."demo/patch-target"]
name = "Patch Target"
provider = "demo"
context_window = 128000
stream_timeout = 300.0
capabilities = ["tools", "vision"]
strengths = ["coding"]

[models."demo/patch-target".pricing]
input_per_mtok = 1.0
output_per_mtok = 5.0
"#;

fn patch_base() -> ProvidersConfig {
    parse_config_toml(PATCH_BASE_TOML).expect("patch base parses")
}

fn patched_row(config: &ProvidersConfig) -> &ModelDef {
    config
        .models
        .get("demo/patch-target")
        .expect("patch target row present")
}

#[test]
fn patch_models_scalar_and_nested_field_preserve_siblings() {
    let mut base = patch_base();
    let overlay = parse_config_toml(
        "[patch.models.\"demo/patch-target\"]\nstream_timeout = 1200.0\n\
             [patch.models.\"demo/patch-target\".pricing]\noutput_per_mtok = 2.5\n",
    )
    .expect("patch overlay parses");
    assert!(!overlay.is_empty(), "a patch-only overlay is not empty");
    base.merge_from(&overlay);
    let row = patched_row(&base);
    assert_eq!(row.stream_timeout, Some(1200.0), "patched scalar applies");
    assert_eq!(row.name, "Patch Target", "unpatched scalar is intact");
    assert_eq!(row.context_window, 128000, "unpatched scalar is intact");
    assert_eq!(
        row.capabilities,
        vec!["tools".to_string(), "vision".to_string()],
        "unpatched array is intact"
    );
    let pricing = row.pricing.as_ref().expect("pricing survives the patch");
    assert_eq!(pricing.output_per_mtok, 2.5, "patched nested field applies");
    assert_eq!(
        pricing.input_per_mtok, 1.0,
        "sibling nested field is preserved by the deep merge"
    );
    assert!(base.dangling_model_patches().is_empty());
}

#[test]
fn patch_models_array_replaces_wholesale() {
    let mut base = patch_base();
    let overlay =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\ncapabilities = [\"tools\"]\n")
            .expect("patch overlay parses");
    base.merge_from(&overlay);
    let row = patched_row(&base);
    assert_eq!(
        row.capabilities,
        vec!["tools".to_string()],
        "arrays replace wholesale — no element-wise merge"
    );
    assert_eq!(
        row.strengths,
        vec!["coding".to_string()],
        "arrays the patch does not name are intact"
    );
}

#[test]
fn patch_models_wins_over_whole_row_in_same_overlay() {
    let mut base = patch_base();
    let overlay = parse_config_toml(
        "[models.\"demo/patch-target\"]\n\
             name = \"Replaced Row\"\nprovider = \"demo\"\ncontext_window = 64000\n\
             stream_timeout = 600.0\n\
             [patch.models.\"demo/patch-target\"]\nstream_timeout = 1200.0\n",
    )
    .expect("overlay parses");
    base.merge_from(&overlay);
    let row = patched_row(&base);
    assert_eq!(
        row.name, "Replaced Row",
        "the whole-row replacement lands first"
    );
    assert_eq!(row.context_window, 64000);
    assert_eq!(
        row.stream_timeout,
        Some(1200.0),
        "the same overlay's patch fields win over its whole-row fields"
    );
}

#[test]
fn patch_models_chained_layers_accumulate_and_later_wins() {
    let mut base = patch_base();
    let layer1 =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\nstream_timeout = 900.0\n")
            .expect("layer1 parses");
    let layer2 =
        parse_config_toml("[patch.models.\"demo/patch-target\".pricing]\noutput_per_mtok = 2.5\n")
            .expect("layer2 parses");
    base.merge_from(&layer1);
    base.merge_from(&layer2);
    let row = patched_row(&base);
    assert_eq!(
        row.stream_timeout,
        Some(900.0),
        "layer1's field patch survives layer2 patching a different field"
    );
    assert_eq!(
        row.pricing
            .as_ref()
            .expect("pricing present")
            .output_per_mtok,
        2.5,
        "layer2's field patch applies"
    );

    let layer3 =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\nstream_timeout = 1200.0\n")
            .expect("layer3 parses");
    base.merge_from(&layer3);
    assert_eq!(
        patched_row(&base).stream_timeout,
        Some(1200.0),
        "for the same field, the later layer's patch wins"
    );
}

#[test]
fn patch_models_sticky_across_later_whole_row_replacement() {
    let mut base = patch_base();
    let patch_layer =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\nstream_timeout = 1200.0\n")
            .expect("patch layer parses");
    base.merge_from(&patch_layer);
    // A later layer replaces the whole row (e.g. a hosted runtime-catalog
    // refresh re-ships the baseline). The accumulated patch re-applies:
    // patches mean "always tweak this field", not "tweak it once".
    let replacement_layer = parse_config_toml(
        "[models.\"demo/patch-target\"]\n\
             name = \"Refreshed Row\"\nprovider = \"demo\"\ncontext_window = 256000\n\
             stream_timeout = 300.0\n",
    )
    .expect("replacement layer parses");
    base.merge_from(&replacement_layer);
    let row = patched_row(&base);
    assert_eq!(row.name, "Refreshed Row", "the whole-row refresh lands");
    assert_eq!(row.context_window, 256000);
    assert_eq!(
        row.stream_timeout,
        Some(1200.0),
        "the sticky patch re-applies on top of the refreshed row"
    );
}

#[test]
fn patch_models_dangling_patch_reports_and_applies_when_row_arrives() {
    let mut base = patch_base();
    let dangling =
        parse_config_toml("[patch.models.\"demo/not-yet-cataloged\"]\nstream_timeout = 42.0\n")
            .expect("dangling patch parses");
    base.merge_from(&dangling);
    assert_eq!(
        base.dangling_model_patches(),
        vec!["demo/not-yet-cataloged"],
        "a patch with no matching row is reported, not dropped"
    );
    assert_eq!(
        patched_row(&base).stream_timeout,
        Some(300.0),
        "existing rows are untouched by a dangling patch"
    );

    // The row arrives from a LATER layer; the accumulated patch applies.
    let late_row = parse_config_toml(
        "[models.\"demo/not-yet-cataloged\"]\n\
             name = \"Late Arrival\"\nprovider = \"demo\"\ncontext_window = 8192\n",
    )
    .expect("late row parses");
    base.merge_from(&late_row);
    assert!(base.dangling_model_patches().is_empty());
    let row = base
        .models
        .get("demo/not-yet-cataloged")
        .expect("late row present");
    assert_eq!(row.stream_timeout, Some(42.0), "the held patch applied");
    assert_eq!(row.name, "Late Arrival");
}

#[test]
fn patch_models_type_error_keeps_unpatched_row() {
    let mut base = patch_base();
    let bad =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\nstream_timeout = \"soon\"\n")
            .expect("the patch overlay itself is valid TOML");
    base.merge_from(&bad);
    let row = patched_row(&base);
    assert_eq!(
        row.stream_timeout,
        Some(300.0),
        "a type-invalid patch keeps the unpatched row"
    );
    assert_eq!(row.name, "Patch Target", "the rest of the row is intact");
}

#[test]
fn model_rows_roundtrip_through_toml_value_for_patching() {
    // Patch application is `ModelDef -> toml::Value -> deep merge ->
    // ModelDef`. This property test guards the serialization leg: every
    // embedded catalog row must survive the round trip unchanged (a
    // missing `Serialize` derive or asymmetric serde attribute on a
    // nested def would corrupt rows the first time they are patched).
    let config = default_config();
    assert!(!config.models.is_empty());
    for (id, row) in &config.models {
        let value = toml::Value::try_from(row)
            .unwrap_or_else(|error| panic!("serialize model row {id}: {error}"));
        let roundtripped = ModelDef::deserialize(value)
            .unwrap_or_else(|error| panic!("deserialize model row {id}: {error}"));
        assert_eq!(&roundtripped, row, "model row {id} must round-trip");
    }
}

#[test]
fn test_glob_match_prefix() {
    assert!(glob_match("claude-*", "claude-sonnet-4-20250514"));
    assert!(glob_match("gpt-*", "gpt-4o"));
    assert!(!glob_match("claude-*", "gpt-4o"));
}

#[test]
fn test_glob_match_suffix() {
    assert!(glob_match("*-latest", "llama3.2-latest"));
    assert!(!glob_match("*-latest", "llama3.2"));
}

#[test]
fn test_glob_match_middle() {
    assert!(glob_match("claude-*-latest", "claude-sonnet-latest"));
    assert!(!glob_match("claude-*-latest", "claude-sonnet-beta"));
}

#[test]
fn test_glob_match_exact() {
    assert!(glob_match("gpt-4o", "gpt-4o"));
    assert!(!glob_match("gpt-4o", "gpt-4o-mini"));
}

#[test]
fn test_infer_provider_from_defaults() {
    let _guard = crate::llm::env_guard();
    let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
    unsafe {
        std::env::remove_var("HARN_DEFAULT_PROVIDER");
    }

    assert_eq!(infer_provider("claude-sonnet-4-20250514"), "anthropic");
    assert_eq!(infer_provider("gpt-4o"), "openai");
    assert_eq!(infer_provider("o1-preview"), "openai");
    assert_eq!(infer_provider("o3-mini"), "openai");
    assert_eq!(infer_provider("o4-mini"), "openai");
    assert_eq!(infer_provider("gemini-2.5-pro"), "gemini");
    assert_eq!(infer_provider("qwen/qwen3-coder"), "openrouter");
    assert_eq!(infer_provider("llama3.2:latest"), "ollama");
    assert_eq!(infer_provider("unknown-model"), "anthropic");

    unsafe {
        match prev_default_provider {
            Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
            None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
        }
    }
}

#[test]
fn test_infer_provider_prefix_rules() {
    assert_eq!(infer_provider("local:gemma-4-e4b-it"), "ollama");
    assert_eq!(infer_provider("ollama:qwen3:30b-a3b"), "ollama");
    // Even when the id also contains `/`, the local transport prefix wins.
    assert_eq!(infer_provider("local:owner/model"), "ollama");
    assert_eq!(infer_provider("hf:Qwen/Qwen3.6-35B-A3B"), "huggingface");
}

#[test]
fn test_openrouter_inference_requires_one_slash() {
    let _guard = crate::llm::env_guard();
    let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
    unsafe {
        std::env::remove_var("HARN_DEFAULT_PROVIDER");
    }

    assert_eq!(infer_provider("org/model"), "openrouter");
    assert_eq!(infer_provider("org/team/model"), "anthropic");

    unsafe {
        match prev_default_provider {
            Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
            None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
        }
    }
}

#[test]
fn test_cerebras_inference_beats_openrouter_slash_fallback() {
    let _guard = crate::llm::env_guard();
    let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
    unsafe {
        std::env::remove_var("HARN_DEFAULT_PROVIDER");
    }

    assert_eq!(infer_provider("cerebras/gpt-oss-120b"), "cerebras");
    assert_eq!(infer_provider("cerebras/zai-glm-4.7"), "cerebras");
    assert_eq!(infer_provider("cerebras/llama-3.3-70b"), "cerebras");

    unsafe {
        match prev_default_provider {
            Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
            None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
        }
    }
}

#[test]
fn test_direct_catalog_model_id_resolves_to_catalog_provider() {
    // Bare model IDs that the embedded catalog hosts on Cerebras must
    // not be misrouted by the generic `gpt-*` / single-slash inference
    // fallbacks. Regression for harn#2142 (model-info routed
    // `gpt-oss-120b` to openai, breaking host TUI credential checks).
    let _guard = crate::llm::env_guard();
    let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
    unsafe {
        std::env::remove_var("HARN_DEFAULT_PROVIDER");
    }

    for model in ["gpt-oss-120b", "zai-glm-4.7", "llama-3.3-70b"] {
        assert_eq!(
            infer_provider(model),
            "cerebras",
            "{model} should route to its catalog provider"
        );
        let resolved = resolve_model_info(model);
        assert_eq!(resolved.id, model);
        assert_eq!(resolved.provider, "cerebras");
    }

    unsafe {
        match prev_default_provider {
            Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
            None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
        }
    }
}

#[test]
fn test_equivalent_model_catalog_entries_use_capability_compatible_routes() {
    reset_overrides();

    assert_eq!(
        wire_model_id("groq/openai/gpt-oss-120b"),
        "openai/gpt-oss-120b"
    );
    assert_eq!(wire_model_id("gpt-oss-120b"), "gpt-oss-120b");

    let equivalents = equivalent_model_catalog_entries("gpt-oss-120b");
    let ids = equivalents
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();

    assert!(
        ids.contains(&"groq/openai/gpt-oss-120b"),
        "Cerebras GPT-OSS should surface the Groq serving variant"
    );
    assert!(
        !ids.contains(&"gpt-oss-120b"),
        "equivalence results should not include the source row"
    );
    assert!(equivalents
        .iter()
        .all(|(_, model)| { model.equivalence_group.as_deref() == Some("openai-gpt-oss-120b") }));
}

#[test]
fn frontier_agent_equivalents_use_request_sized_context_for_failover() {
    reset_overrides();

    let conservative = equivalent_model_catalog_entries("claude-sonnet-4-6");
    assert!(
        !conservative
            .iter()
            .any(|(id, _)| id == "deepseek-ai/DeepSeek-V4-Pro"),
        "full-window equivalence should not claim a 512K route covers every 1M Sonnet request"
    );

    let request_sized =
        equivalent_model_catalog_entries_for_context("claude-sonnet-4-6", Some(128_000));
    let ids = request_sized
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();

    assert!(
        ids.contains(&"deepseek-ai/DeepSeek-V4-Pro"),
        "Together DeepSeek should be a request-sized Sonnet failover candidate"
    );
    assert!(
        ids.contains(&"glm-5.2") || ids.contains(&"z-ai/glm-5.2"),
        "GLM 5.2 should stay in the frontier coding failover group"
    );
    assert!(request_sized
        .iter()
        .all(|(_, model)| { model.equivalence_group.as_deref() == Some("frontier-agent-coding") }));
}

#[test]
fn fireworks_gpt_oss_route_has_real_context_window() {
    // Regression: the Fireworks-served `accounts/fireworks/models/gpt-oss-120b`
    // wire id had NO catalog row, so its context window resolved to None and
    // the agent's auto-compaction budget had nothing to enforce — the prompt
    // grew until Fireworks rejected the turn with HTTP 400 [context_overflow]
    // (session 019ee303: 197467 tokens > 131071 max). Cataloging the real
    // 131072 window lets compaction trigger before the hard limit.
    reset_overrides();

    let entry = model_catalog_entry("accounts/fireworks/models/gpt-oss-120b")
        .expect("Fireworks gpt-oss-120b must be in the model catalog");
    assert_eq!(entry.context_window, 131_072);
    assert_eq!(entry.provider, "fireworks");
    assert_eq!(
        entry.equivalence_group.as_deref(),
        Some("openai-gpt-oss-120b"),
    );
}

#[test]
fn test_user_catalog_overlay_re_homes_model_provider() {
    // Users can re-home a built-in model by overlaying a catalog row;
    // the exact-match catalog lookup must honor overlays as well as the
    // embedded TOML.
    reset_overrides();
    let mut overlay = ProvidersConfig::default();
    overlay.models.insert(
        "gpt-4o".to_string(),
        ModelDef {
            name: "GPT-4o via OpenRouter".to_string(),
            provider: "openrouter".to_string(),
            context_window: 128_000,
            logical_model: None,
            equivalence_group: None,
            served_variant: None,
            wire_model: None,
            api_dialect: None,
            rate_limits: None,
            performance: None,
            architecture: None,
            local_memory: None,
            runtime_context_window: None,
            stream_timeout: None,
            capabilities: Vec::new(),
            pricing: None,
            deprecated: false,
            deprecation_note: None,
            superseded_by: None,
            serving_tiers: Vec::new(),
            quality_tags: Vec::new(),
            availability: ModelAvailability::default(),
            tier: None,
            open_weight: None,
            strengths: Vec::new(),
            benchmarks: std::collections::BTreeMap::new(),
            family: None,
            lineage: None,
            complementary_with: Vec::new(),
            avoid_as_reviewer_for: Vec::new(),
        },
    );
    set_user_overrides(Some(overlay));

    assert_eq!(infer_provider("gpt-4o"), "openrouter");

    reset_overrides();
}

#[test]
fn test_resolve_model_info_normalizes_provider_prefixes() {
    let local = resolve_model_info("local:gemma-4-e4b-it");
    assert_eq!(local.id, "gemma-4-e4b-it");
    assert_eq!(local.provider, "ollama");

    let ollama = resolve_model_info("ollama:qwen3:30b-a3b");
    assert_eq!(ollama.id, "qwen3:30b-a3b");
    assert_eq!(ollama.provider, "ollama");

    let hf = resolve_model_info("hf:Qwen/Qwen3.6-35B-A3B");
    assert_eq!(hf.id, "Qwen/Qwen3.6-35B-A3B");
    assert_eq!(hf.provider, "huggingface");

    let cerebras = resolve_model_info("cerebras/gpt-oss-120b");
    assert_eq!(cerebras.id, "gpt-oss-120b");
    assert_eq!(cerebras.provider, "cerebras");

    let cerebras_glm = resolve_model_info("cerebras/zai-glm-4.7");
    assert_eq!(cerebras_glm.id, "zai-glm-4.7");
    assert_eq!(cerebras_glm.provider, "cerebras");
}

#[test]
fn test_model_tier_from_defaults() {
    // Tier is now self-declared per model row in providers.toml.
    // Models that match an entry use the declared value; unknown
    // model ids fall through to `tier_defaults.default` ("mid").
    assert_eq!(model_tier("claude-sonnet-4-20250514"), "frontier");
    assert_eq!(model_tier("gpt-4o"), "frontier");
    assert_eq!(model_tier("Qwen/Qwen3.5-9B"), "small");
    assert_eq!(model_tier("deepseek-v4-flash"), "mid");
    assert_eq!(model_tier("deepseek-v4-pro"), "frontier");
    assert_eq!(model_tier("MiniMax-M2.7"), "frontier");
    assert_eq!(model_tier("glm-5.1"), "frontier");
    // Unknown ids resolve to the default.
    assert_eq!(model_tier("definitely-not-a-real-model"), "mid");
}

#[test]
fn test_model_family_preserves_underlying_hosted_lineage() {
    assert_eq!(
        model_family("openrouter", "anthropic/claude-sonnet-4-6"),
        "anthropic-claude"
    );
    assert_eq!(
        model_family("openrouter", "google/gemini-2.5-flash"),
        "google-gemini"
    );
    assert_eq!(
        model_family("openrouter", "openai/o3-mini"),
        "openai-reasoning"
    );
    assert_eq!(model_lineage("openrouter", "openai/gpt-5.5"), "openai-gpt5");
    assert_eq!(
        model_lineage("openrouter", "openai/o3-mini"),
        "openai-reasoning"
    );
    assert_eq!(
        model_lineage("anthropic", "claude-opus-4-8"),
        "claude-opus-adaptive"
    );
    assert_eq!(model_lineage("llamacpp", "qwen3.6-35b-a3b"), "qwen3");
}

#[test]
fn test_complementary_reviewer_uses_different_family() {
    let selection = pick_complementary_reviewer(ComplementaryReviewerOptions {
        author_model: "claude-sonnet-4-6".to_string(),
        author_provider: None,
        intent: ComplementaryReviewerIntent::PlanReview,
        max_price_multiplier: Some(3.0),
    });

    assert!(!selection.fallback, "{selection:?}");
    assert_eq!(selection.author.family, "anthropic-claude");
    assert_ne!(selection.reviewer.family, selection.author.family);
    assert_eq!(selection.reviewer.tier, "frontier");
    assert!(selection.estimated_incremental_cost.is_some());
    // Success path carries no machine-readable fallback code, so a caller
    // can treat `fallback_code.is_some()` as "must not self-review".
    assert_eq!(selection.fallback_code, None, "{selection:?}");
}

#[test]
fn test_complementary_reviewer_falls_back_deterministically_on_price_cap() {
    let selection = pick_complementary_reviewer(ComplementaryReviewerOptions {
        author_model: "gpt-4o-mini".to_string(),
        author_provider: Some("openai".to_string()),
        intent: ComplementaryReviewerIntent::Review,
        max_price_multiplier: Some(0.01),
    });

    assert!(selection.fallback, "{selection:?}");
    assert_eq!(selection.reviewer.id, "gpt-4o-mini");
    assert_eq!(selection.reviewer.family, selection.author.family);
    assert!(selection
        .fallback_reason
        .as_deref()
        .is_some_and(|reason| reason.contains("max_price_multiplier")));
    // The machine-readable code is stable regardless of the prose; a caller
    // hard-fails an independent-review step by branching on this, never by
    // parsing `fallback_reason`.
    assert_eq!(
        selection.fallback_code.as_deref(),
        Some(ReviewerFallbackCode::NoDiffFamilyWithinPrice.as_code()),
        "{selection:?}"
    );
    assert_eq!(
        ReviewerFallbackCode::NoDiffFamilyWithinPrice.as_code(),
        "no_diff_family_within_price"
    );
}

#[test]
fn test_reviewer_fallback_codes_are_stable_strings() {
    // Append-only contract: harn pipelines and Rust callers branch on these
    // exact strings, so changing one is a breaking change.
    assert_eq!(
        ReviewerFallbackCode::UnknownAuthorFamily.as_code(),
        "unknown_author_family"
    );
    assert_eq!(
        ReviewerFallbackCode::NoDiffFamilyWithinPrice.as_code(),
        "no_diff_family_within_price"
    );
    assert_eq!(
        ReviewerFallbackCode::NoDiffFamilyServerless.as_code(),
        "no_diff_family_serverless"
    );
    assert_eq!(
        ReviewerFallbackCode::AllDiffFamilyExcluded.as_code(),
        "all_diff_family_excluded"
    );
}

#[test]
fn test_resolve_model_unknown_alias() {
    let (id, provider) = resolve_model("gpt-4o");
    assert_eq!(id, "gpt-4o");
    assert!(provider.is_none());
}

#[test]
fn test_provider_names() {
    let names = provider_names();
    assert!(names.len() >= 7);
    assert!(names.contains(&"anthropic".to_string()));
    assert!(names.contains(&"together".to_string()));
    assert!(names.contains(&"local".to_string()));
    assert!(names.contains(&"mlx".to_string()));
    assert!(names.contains(&"openai".to_string()));
    assert!(names.contains(&"ollama".to_string()));
    assert!(names.contains(&"bedrock".to_string()));
    assert!(names.contains(&"azure_openai".to_string()));
    assert!(names.contains(&"vertex".to_string()));
}

#[test]
fn global_provider_file_is_an_overlay_on_builtin_defaults() {
    let mut overlay = ProvidersConfig {
        default_provider: Some("ollama".to_string()),
        ..Default::default()
    };
    overlay.aliases.insert(
        "quickstart".to_string(),
        AliasDef {
            id: "llama3.2".to_string(),
            provider: "ollama".to_string(),
            tool_format: None,
        },
    );

    let merged = merge_global_config(overlay);

    assert_eq!(merged.default_provider.as_deref(), Some("ollama"));
    assert!(merged.providers.contains_key("anthropic"));
    assert!(merged.providers.contains_key("ollama"));
    assert_eq!(merged.aliases["quickstart"].id, "llama3.2");
}

#[test]
fn partial_provider_overlay_preserves_builtin_provider_metadata() {
    let overlay = parse_config_toml(
        r#"
            [providers.ollama]
            base_url = "http://localhost:11435"
            extra_headers = { "x-local" = "1" }
            "#,
    )
    .expect("provider overlay parses");

    let merged = merge_global_config(overlay);
    let ollama = merged
        .providers
        .get("ollama")
        .expect("ollama remains configured");

    assert_eq!(ollama.base_url, "http://localhost:11435");
    assert_eq!(ollama.auth_style, "none");
    assert_eq!(ollama.chat_endpoint, "/api/chat");
    assert_eq!(ollama.completion_endpoint.as_deref(), Some("/api/generate"));
    assert_eq!(ollama.cost_per_1k_in, Some(0.0));
    assert_eq!(ollama.cost_per_1k_out, Some(0.0));
    assert_eq!(
        ollama
            .healthcheck
            .as_ref()
            .and_then(|healthcheck| healthcheck.path.as_deref()),
        Some("/api/tags")
    );
    assert_eq!(
        ollama.extra_headers.get("x-local").map(String::as_str),
        Some("1")
    );
}

#[test]
fn partial_provider_overlay_can_explicitly_replace_default_auth_style() {
    let overlay = parse_config_toml(
        r#"
            [providers.ollama]
            auth_style = "bearer"
            auth_env = "OLLAMA_API_KEY"
            "#,
    )
    .expect("provider overlay parses");

    let merged = merge_global_config(overlay);
    let ollama = merged
        .providers
        .get("ollama")
        .expect("ollama remains configured");

    assert_eq!(ollama.auth_style, "bearer");
    assert_eq!(auth_env_names(&ollama.auth_env), vec!["OLLAMA_API_KEY"]);
    assert_eq!(ollama.chat_endpoint, "/api/chat");
}

#[test]
fn test_resolve_tier_model_default_aliases() {
    // Exercise the alias-resolution machinery, not the specific catalog
    // value: the model under each tier alias evolves as the embedded
    // providers.toml is updated. The invariants worth pinning are the
    // provider routing + catalog-registration of the resolved model.
    let (model, provider) = resolve_tier_model("frontier", None)
        .expect("frontier alias must resolve from the embedded catalog");
    assert_eq!(provider, "anthropic");
    assert!(
        model_catalog_entry(&model)
            .is_some_and(|entry| entry.provider == "anthropic" && !entry.deprecated),
        "frontier alias must point at a registered, non-deprecated anthropic model (got {model})"
    );

    let (model, provider) = resolve_tier_model("small", None)
        .expect("small alias must resolve from the embedded catalog");
    assert!(
        [
            "openrouter",
            "huggingface",
            "local",
            "llamacpp",
            "mlx",
            "ollama"
        ]
        .contains(&provider.as_str()),
        "small tier should resolve to an open-weight provider (got {provider} / {model})"
    );

    let (model, provider) =
        resolve_tier_model("mid", None).expect("mid alias must resolve from the embedded catalog");
    assert_eq!(provider, "openrouter");
    assert_eq!(model, "qwen/qwen3-coder-next");
}

#[test]
fn test_resolve_tier_model_prefers_provider_scoped_aliases() {
    // tier/<provider> takes precedence over generic tier when the
    // caller scopes by provider. Don't pin the specific model — the
    // catalog evolves.
    let (model, provider) =
        resolve_tier_model("mid", Some("openai")).expect("mid tier scoped to openai must resolve");
    assert_eq!(provider, "openai");
    let entry = model_catalog_entry(&model).unwrap_or_else(|| {
        panic!("mid/openai alias must point at a registered model (got {model})")
    });
    assert_eq!(entry.tier.as_deref(), Some("mid"));
}

#[test]
fn test_provider_config_anthropic() {
    let pdef = provider_config("anthropic").unwrap();
    assert_eq!(pdef.auth_style, "header");
    assert_eq!(pdef.auth_header.as_deref(), Some("x-api-key"));
}

#[test]
fn test_provider_config_mlx() {
    let pdef = provider_config("mlx").unwrap();
    assert_eq!(pdef.base_url, "http://127.0.0.1:8002");
    assert_eq!(pdef.base_url_env.as_deref(), Some("MLX_BASE_URL"));
    assert_eq!(
        pdef.healthcheck.unwrap().path.as_deref(),
        Some("/v1/models")
    );

    let (model, provider) = resolve_model("mlx-qwen36-27b");
    assert_eq!(model, "unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit");
    assert_eq!(provider.as_deref(), Some("mlx"));
}

#[test]
fn test_enterprise_provider_defaults_and_inference() {
    let bedrock = provider_config("bedrock").unwrap();
    assert_eq!(bedrock.auth_style, "aws_sigv4");
    assert_eq!(bedrock.base_url_env.as_deref(), Some("BEDROCK_BASE_URL"));
    assert_eq!(
        infer_provider("anthropic.claude-3-5-sonnet-20240620-v1:0"),
        "bedrock"
    );
    assert_eq!(infer_provider("meta.llama3-70b-instruct-v1:0"), "bedrock");

    let azure = provider_config("azure_openai").unwrap();
    assert_eq!(azure.base_url_env.as_deref(), Some("AZURE_OPENAI_ENDPOINT"));
    assert_eq!(
        auth_env_names(&azure.auth_env),
        vec![
            "AZURE_OPENAI_API_KEY".to_string(),
            "AZURE_OPENAI_AD_TOKEN".to_string(),
            "AZURE_OPENAI_BEARER_TOKEN".to_string(),
        ]
    );

    let vertex = provider_config("vertex").unwrap();
    assert_eq!(vertex.base_url, "https://aiplatform.googleapis.com/v1");
    assert_eq!(infer_provider("gemini-1.5-pro-002"), "gemini");
}

#[test]
fn test_default_provider_env_override_for_unknown_model() {
    let _guard = crate::llm::env_guard();
    let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
    unsafe {
        std::env::set_var("HARN_DEFAULT_PROVIDER", "openai");
    }

    let inference = infer_provider_detail("unknown-model");

    unsafe {
        match prev_default_provider {
            Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
            None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
        }
    }

    assert_eq!(inference.provider, "openai");
    assert_eq!(
        inference.source,
        crate::llm::provider::ProviderInferenceSource::DefaultFallback
    );
}

#[test]
fn test_unknown_model_family_ignores_default_provider_fallback() {
    let _guard = crate::llm::env_guard();
    let prev_default_provider = std::env::var("HARN_DEFAULT_PROVIDER").ok();
    unsafe {
        std::env::set_var("HARN_DEFAULT_PROVIDER", "ollama");
    }

    let unknown = resolve_model_info("mystery-model-xyz");
    let known_family = resolve_model_info("deepseek-mystery-model");

    unsafe {
        match prev_default_provider {
            Some(value) => std::env::set_var("HARN_DEFAULT_PROVIDER", value),
            None => std::env::remove_var("HARN_DEFAULT_PROVIDER"),
        }
    }

    assert_eq!(unknown.provider, "ollama");
    assert_eq!(unknown.family, "unknown");
    assert_eq!(unknown.lineage, "unknown");
    assert_eq!(known_family.family, "deepseek");
    assert_eq!(known_family.lineage, "deepseek");
}

#[test]
fn test_resolve_base_url_no_env() {
    let pdef = ProviderDef {
        base_url: "https://example.com".to_string(),
        ..Default::default()
    };
    assert_eq!(resolve_base_url(&pdef), "https://example.com");
}

#[test]
fn test_resolve_base_url_region_env() {
    let _guard = crate::llm::env_guard();
    unsafe {
        std::env::remove_var("HARN_TEST_BASE_URL");
        std::env::set_var("HARN_TEST_REGION", "CN");
    }
    let pdef = ProviderDef {
        base_url: "https://global.example/v1".to_string(),
        base_url_env: Some("HARN_TEST_BASE_URL".to_string()),
        region_env: Some("HARN_TEST_REGION".to_string()),
        regions: BTreeMap::from([
            (
                "global".to_string(),
                ProviderRegionDef {
                    base_url: "https://global.example/v1".to_string(),
                    ..Default::default()
                },
            ),
            (
                "cn".to_string(),
                ProviderRegionDef {
                    base_url: "https://cn.example/v1".to_string(),
                    ..Default::default()
                },
            ),
        ]),
        ..Default::default()
    };
    assert_eq!(resolve_base_url(&pdef), "https://cn.example/v1");

    unsafe {
        std::env::set_var("HARN_TEST_BASE_URL", " 'https://override.example/v1' ");
    }
    assert_eq!(resolve_base_url(&pdef), "https://override.example/v1");
}

#[test]
fn test_default_config_roundtrip() {
    let config = default_config();
    assert!(!config.providers.is_empty());
    assert!(!config.inference_rules.is_empty());
    // Tier is now declared on each model row; tier_rules is allowed
    // to be empty (the rule table is a legacy fallback only).
    assert_eq!(config.tier_defaults.default, "mid");
    // At least the new open-weight frontiers should have explicit tiers.
    let frontiers = config
        .models
        .iter()
        .filter(|(_, m)| m.tier.as_deref() == Some("frontier"))
        .count();
    assert!(
        frontiers >= 4,
        "expected at least 4 frontier-tagged models, got {frontiers}"
    );
}

#[test]
fn test_local_ollama_catalog_metadata() {
    reset_overrides();

    let devstral =
        model_catalog_entry("devstral-small-2:24b").expect("devstral-small-2 catalog entry");
    assert_eq!(devstral.context_window, 262_144);
    assert!(!devstral.capabilities.iter().any(|cap| cap == "vision"));

    let gemma4 = model_catalog_entry("gemma4:26b").expect("gemma4 catalog entry");
    assert_eq!(gemma4.context_window, 262_144);
    assert!(gemma4.capabilities.iter().any(|cap| cap == "vision"));
}

#[test]
fn local_gemma4_source_tags_match_structured_capability_tags() {
    reset_overrides();
    let config = default_config();
    for id in [
        "gemma-4-e2b-it",
        "gemma-4-e4b-it",
        "gemma-4-12b-it",
        "gemma-4-26b-a4b-it",
        "gemma-4-31b-it",
    ] {
        let source = config
            .models
            .get(id)
            .unwrap_or_else(|| panic!("{id} should be in the embedded catalog"));
        let derived = effective_model_capability_tags(&source.provider, id);
        assert_eq!(
            source.capabilities, derived,
            "{}/{} source capabilities must match derived capability_tags",
            source.provider, id
        );
    }
}

#[test]
fn capability_tags_include_structured_capability_flags() {
    let caps = crate::llm::capabilities::Capabilities {
        native_tools: true,
        tool_search: vec!["web".to_string()],
        vision_supported: true,
        audio: true,
        pdf: true,
        video: true,
        files_api_supported: true,
        batch_api: true,
        prompt_caching: true,
        thinking_modes: vec!["enabled".to_string()],
        structured_output: Some("native".to_string()),
        ..Default::default()
    };

    assert_eq!(
        capability_tags_from_capabilities(&caps),
        vec![
            "streaming",
            "tools",
            "tool_search",
            "vision",
            "audio",
            "pdf",
            "video",
            "files",
            "batch",
            "prompt_caching",
            "thinking",
            "structured_output",
        ]
    );
}

#[test]
fn test_external_config_overlays_default_catalog() {
    let mut config = default_config();
    let mut overlay = ProvidersConfig {
        default_provider: Some("ollama".to_string()),
        ..Default::default()
    };
    overlay.providers.insert(
        "custom".to_string(),
        ProviderDef {
            base_url: "https://llm.example.test/v1".to_string(),
            chat_endpoint: "/chat/completions".to_string(),
            ..Default::default()
        },
    );

    config.merge_from(&overlay);

    assert_eq!(config.default_provider.as_deref(), Some("ollama"));
    assert!(config.providers.contains_key("custom"));
    assert!(config.providers.contains_key("anthropic"));
    assert!(config.providers.contains_key("ollama"));
}

#[test]
fn test_model_params_empty() {
    let params = model_params("claude-sonnet-4-20250514");
    assert!(params.is_empty());
}

#[test]
fn test_user_overrides_add_provider_and_alias() {
    reset_overrides();
    let mut overlay = ProvidersConfig::default();
    overlay.providers.insert(
        "acme".to_string(),
        ProviderDef {
            base_url: "https://llm.acme.test/v1".to_string(),
            chat_endpoint: "/chat/completions".to_string(),
            ..Default::default()
        },
    );
    overlay.aliases.insert(
        "acme-fast".to_string(),
        AliasDef {
            id: "acme/model-fast".to_string(),
            provider: "acme".to_string(),
            tool_format: Some("native".to_string()),
        },
    );
    set_user_overrides(Some(overlay));

    let (model, provider) = resolve_model("acme-fast");
    assert_eq!(model, "acme/model-fast");
    assert_eq!(provider.as_deref(), Some("acme"));
    assert!(provider_names().contains(&"acme".to_string()));
    assert_eq!(
        provider_config("acme").map(|provider| provider.base_url),
        Some("https://llm.acme.test/v1".to_string())
    );

    reset_overrides();
}

#[test]
fn test_default_tool_format_uses_capability_matrix() {
    reset_overrides();

    assert_eq!(
        default_tool_format("qwen3.6-35b-a3b-ud-q4-k-xl", "llamacpp"),
        "native"
    );
    // devstral dropped its stale heredoc `text` pin (it has no reserved-token
    // constraint, so there was no structural reason to stay on heredoc) and
    // now inherits the global `json` text-channel default. Heredoc is still
    // reachable via an explicit `preferred_tool_format = "text"` pin.
    assert_eq!(
        default_tool_format("devstral-small-2:24b", "ollama"),
        "json"
    );
    // vLLM/SGLang-served Gemma 4 exposes OpenAI-compatible function calling,
    // so the local route declares native tools (matching every hosted gemma-4
    // sibling) rather than degrading to a text tool format.
    assert_eq!(default_tool_format("gemma-4-26b-a4b-it", "local"), "native");
    // deepseek-v3.2 and qwen3-coder both pin `text` in the capability
    // matrix, so they keep heredoc rather than inheriting the json default.
    assert_eq!(
        default_tool_format("deepseek/deepseek-v3.2", "openrouter"),
        "text"
    );
    assert_eq!(
        default_tool_format("qwen/qwen3-coder-flash", "openrouter"),
        "text"
    );
    assert_eq!(
        default_tool_format("qwen/qwen3.6-flash", "openrouter"),
        "native"
    );
    assert_eq!(default_tool_format("z-ai/glm-5.2", "openrouter"), "text");
    // GPT-OSS tool defaults are provider-specific: aggregate OpenRouter and
    // Fireworks use Harn's heredoc text tools, as does DeepInfra — its
    // native Harmony channel drops tool calls into the private reasoning
    // channel (footgun), so it is pinned to text. Native-reliable hosts
    // (Cerebras, Groq) stay on provider-native tool calls.
    assert_eq!(
        default_tool_format("openai/gpt-oss-120b", "openrouter"),
        "text"
    );
    assert_eq!(
        default_tool_format("accounts/fireworks/models/gpt-oss-120b", "fireworks"),
        "text"
    );
    assert_eq!(default_tool_format("gpt-oss-120b", "cerebras"), "native");
    assert_eq!(
        default_tool_format("openai/gpt-oss-120b", "deepinfra"),
        "text"
    );
    assert_eq!(default_tool_format("openai/gpt-oss-120b", "groq"), "native");
}

#[test]
fn test_default_tool_format_unpinned_text_channel_is_json() {
    reset_overrides();

    // GLOBAL DEFAULT FLIP: a model with no capability-matrix pin and no
    // native tool support resolves to fenced-json (`json`), not heredoc
    // (`text`). This is the behavior change — an unknown text-channel model
    // gets the delimiter-safe default. (Native-capable unknowns still get
    // `native`; pinned models still honor their pin, covered above.)
    assert_eq!(default_tool_format("mystery-model-xyz", "ollama"), "json");
}

#[test]
fn test_claude_family_defaults_native_without_host_pin() {
    reset_overrides();

    // Unpinned claude-family routes on first-class tool-calling providers
    // resolve `native` from the capability matrix alone — no host alias
    // pin required. The openrouter rows exercise the family-level
    // catch-all: a dated slug, an unparseable version segment, and a new
    // family name have no versioned rule and previously fell through to
    // the global text-channel `json` default.
    for (model, provider) in [
        ("claude-sonnet-4-6", "anthropic"),
        ("claude-sonnet-5", "anthropic"),
        ("anthropic/claude-nova-1", "anthropic"),
        ("anthropic/claude-sonnet-4.6", "openrouter"),
        ("anthropic/claude-sonnet-5", "openrouter"),
        ("anthropic/claude-opus-4-5-20251101", "openrouter"),
        ("anthropic/claude-sonnet-next", "openrouter"),
        ("anthropic/claude-nova-1", "openrouter"),
        ("anthropic.claude-sonnet-4-6", "bedrock"),
    ] {
        assert_eq!(
            default_tool_format(model, provider),
            "native",
            "{provider}:{model} must default native without a host pin"
        );
    }

    // An unpinned host alias resolves native end-to-end through
    // `resolve_model_info` (alias -> id -> capability matrix -> dialect
    // guard) — the exact seam hosts consume via `llm_resolve_model`.
    let overlay = parse_config_toml(
        "[aliases.probe-sonnet]\nid = \"claude-sonnet-4-6\"\nprovider = \"anthropic\"\n",
    )
    .expect("overlay parses");
    set_user_overrides(Some(overlay));
    let resolved = resolve_model_info("probe-sonnet");
    assert_eq!(resolved.provider, "anthropic");
    assert_eq!(
        resolved.tool_format, "native",
        "an unpinned claude alias must inherit the family-level native default"
    );
    clear_user_overrides();

    // An explicit host pin still wins over the family default: a
    // text-channel `json` pin on a native-capable claude route survives
    // resolution (the dialect guard only corrects known-broken combos).
    let overlay = parse_config_toml(
            "[aliases.probe-sonnet-json]\nid = \"claude-sonnet-4-6\"\nprovider = \"anthropic\"\ntool_format = \"json\"\n",
        )
        .expect("overlay parses");
    set_user_overrides(Some(overlay));
    let pinned = resolve_model_info("probe-sonnet-json");
    assert_eq!(
        pinned.tool_format, "json",
        "an explicit host pin must win over the claude family default"
    );
    clear_user_overrides();

    // Non-claude models keep the global text-channel `json` default —
    // the catch-all is family-scoped, not a provider-wide flip.
    assert_eq!(
        default_tool_format("mystery-model-xyz", "openrouter"),
        "json"
    );
}

#[test]
fn test_user_overrides_add_model_catalog_pricing_and_qc_defaults() {
    reset_overrides();
    let mut overlay = ProvidersConfig::default();
    overlay.models.insert(
        "acme/model-fast".to_string(),
        ModelDef {
            name: "Acme Fast".to_string(),
            provider: "acme".to_string(),
            context_window: 65_536,
            logical_model: None,
            equivalence_group: None,
            served_variant: None,
            wire_model: None,
            api_dialect: None,
            rate_limits: None,
            performance: None,
            architecture: None,
            local_memory: None,
            runtime_context_window: None,
            stream_timeout: Some(42.0),
            capabilities: vec!["tools".to_string(), "streaming".to_string()],
            pricing: Some(ModelPricing {
                input_per_mtok: 1.25,
                output_per_mtok: 2.5,
                cache_read_per_mtok: Some(0.25),
                cache_write_per_mtok: None,
                input_token_bands: Vec::new(),
            }),
            deprecated: false,
            deprecation_note: None,
            superseded_by: None,
            serving_tiers: Vec::new(),
            quality_tags: Vec::new(),
            availability: ModelAvailability::default(),
            tier: None,
            open_weight: None,
            strengths: Vec::new(),
            benchmarks: std::collections::BTreeMap::new(),
            family: None,
            lineage: None,
            complementary_with: Vec::new(),
            avoid_as_reviewer_for: Vec::new(),
        },
    );
    overlay
        .qc_defaults
        .insert("acme".to_string(), "acme/model-cheap".to_string());
    set_user_overrides(Some(overlay));

    let entry = model_catalog_entry("acme/model-fast").expect("catalog entry");
    assert_eq!(entry.context_window, 65_536);
    assert_eq!(
        entry.capabilities,
        vec!["streaming".to_string(), "tools".to_string()]
    );
    assert_eq!(
        entry.pricing.as_ref().map(|pricing| pricing.input_per_mtok),
        Some(1.25)
    );
    assert_eq!(
        pricing_per_1k_for("acme", "acme/model-fast"),
        Some((0.00125, 0.0025))
    );
    assert_eq!(
        qc_default_model("acme").as_deref(),
        Some("acme/model-cheap")
    );

    reset_overrides();
}

#[test]
fn test_user_overrides_prepend_inference_rules() {
    reset_overrides();
    let mut overlay = ProvidersConfig::default();
    overlay.inference_rules.push(InferenceRule {
        pattern: Some("internal-*".to_string()),
        contains: None,
        exact: None,
        provider: "openai".to_string(),
    });
    set_user_overrides(Some(overlay));

    assert_eq!(infer_provider("internal-foo"), "openai");

    reset_overrides();
}

// ── Embedded providers.toml invariants ───────────────────────────────────
// These tests pin properties of the *system* — TOML parses, every
// alias resolves, every deprecated model has a note — without
// pinning specific catalog values. They survive future catalog
// churn and surface real schema breakage.

#[test]
fn embedded_providers_toml_parses_and_is_not_trivially_empty() {
    let config = default_config();
    assert!(
        config.providers.len() >= 10,
        "expected >=10 providers in embedded catalog, got {}",
        config.providers.len()
    );
    assert!(
        config.models.len() >= 20,
        "expected >=20 models in embedded catalog, got {}",
        config.models.len()
    );
    assert!(
        config.aliases.len() >= 15,
        "expected >=15 aliases in embedded catalog, got {}",
        config.aliases.len()
    );
    assert_eq!(config.default_provider.as_deref(), Some("anthropic"));
}

#[test]
fn embedded_catalog_every_deprecated_model_has_a_note() {
    let config = default_config();
    let offenders: Vec<&str> = config
        .models
        .iter()
        .filter(|(_, model)| {
            model.deprecated
                && model
                    .deprecation_note
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
        })
        .map(|(id, _)| id.as_str())
        .collect();
    assert!(
        offenders.is_empty(),
        "deprecated models missing a deprecation_note: {offenders:?}"
    );
}

#[test]
fn embedded_cerebras_catalog_separates_public_and_dedicated_routes() {
    let config = default_config();
    for id in ["gpt-oss-120b", "zai-glm-4.7"] {
        let model = config.models.get(id).expect("current public Cerebras row");
        assert_eq!(model.provider, "cerebras");
        assert_eq!(model.availability, ModelAvailability::Serverless);
        assert!(!model.deprecated);
    }

    let llama = config
        .models
        .get("llama-3.3-70b")
        .expect("legacy Cerebras row");
    assert_eq!(llama.provider, "cerebras");
    assert_eq!(llama.availability, ModelAvailability::Dedicated);
    assert!(llama.deprecated);
}

#[test]
fn embedded_openrouter_gpt_oss_120b_has_no_fragment_bleed() {
    // Regression for the provider-catalog leading-key bleed: the openrouter
    // `openai/gpt-oss-120b` row was the last model in its fragment with no
    // inline tier/open_weight/strengths, so the next fragment's leading bare
    // keys reattached to it after raw-text concatenation — mislabeling it as
    // `open_weight = false` with a spurious `vision` strength. It must now be
    // self-described: open weight, no vision, and a tier consistent with the
    // rest of its equivalence group.
    let config = default_config();
    let model = config
        .models
        .get("openai/gpt-oss-120b")
        .expect("openrouter gpt-oss-120b row");
    assert_eq!(model.provider, "openrouter");
    assert_eq!(
        model.open_weight,
        Some(true),
        "gpt-oss-120b is Apache-2.0 open weight, not the bled-in open_weight=false"
    );
    assert!(
        !model.strengths.iter().any(|s| s == "vision"),
        "gpt-oss-120b is text-only; the bled-in `vision` strength must be gone: {:?}",
        model.strengths
    );
    assert!(
        !model.strengths.is_empty(),
        "gpt-oss-120b must carry its own strengths, not None"
    );

    // tier is a property of the logical model: every active row in the
    // openai-gpt-oss-120b equivalence group must agree.
    let group_tiers: std::collections::BTreeSet<_> = config
        .models
        .values()
        .filter(|m| m.equivalence_group.as_deref() == Some("openai-gpt-oss-120b") && !m.deprecated)
        .map(|m| m.tier.clone())
        .collect();
    assert_eq!(
        group_tiers.len(),
        1,
        "openai-gpt-oss-120b group must share one tier, got {group_tiers:?}"
    );
}

#[test]
fn embedded_catalog_every_model_targets_a_registered_provider() {
    let config = default_config();
    let known: std::collections::BTreeSet<&str> =
        config.providers.keys().map(String::as_str).collect();
    let orphans: Vec<(&str, &str)> = config
        .models
        .iter()
        .filter(|(_, model)| !known.contains(model.provider.as_str()))
        .map(|(id, model)| (id.as_str(), model.provider.as_str()))
        .collect();
    assert!(
        orphans.is_empty(),
        "models reference unknown providers: {orphans:?}"
    );
}

#[test]
fn embedded_catalog_every_alias_targets_a_registered_provider() {
    let config = default_config();
    let known: std::collections::BTreeSet<&str> =
        config.providers.keys().map(String::as_str).collect();
    let orphans: Vec<(&str, &str)> = config
        .aliases
        .iter()
        .filter(|(_, alias)| !known.contains(alias.provider.as_str()))
        .map(|(name, alias)| (name.as_str(), alias.provider.as_str()))
        .collect();
    assert!(
        orphans.is_empty(),
        "aliases reference unknown providers: {orphans:?}"
    );
}

#[test]
fn embedded_catalog_every_qc_default_targets_a_known_model() {
    let config = default_config();
    let orphans: Vec<(&str, &str)> = config
        .qc_defaults
        .iter()
        .filter(|(_, model_id)| !config.models.contains_key(model_id.as_str()))
        .map(|(provider, model_id)| (provider.as_str(), model_id.as_str()))
        .collect();
    assert!(
        orphans.is_empty(),
        "qc_defaults reference unknown models: {orphans:?}"
    );
}

#[test]
fn embedded_catalog_pricing_rates_are_non_negative() {
    let config = default_config();
    for (id, model) in &config.models {
        let Some(pricing) = &model.pricing else {
            continue;
        };
        assert!(
            pricing.input_per_mtok >= 0.0 && pricing.output_per_mtok >= 0.0,
            "{id}: negative pricing — in={} out={}",
            pricing.input_per_mtok,
            pricing.output_per_mtok
        );
        if let Some(rate) = pricing.cache_read_per_mtok {
            assert!(rate >= 0.0, "{id}: negative cache_read rate {rate}");
        }
        if let Some(rate) = pricing.cache_write_per_mtok {
            assert!(rate >= 0.0, "{id}: negative cache_write rate {rate}");
        }
    }
}

#[test]
fn model_availability_parses_known_strings() {
    assert_eq!(
        ModelAvailability::parse("serverless"),
        Some(ModelAvailability::Serverless)
    );
    assert_eq!(
        ModelAvailability::parse("dedicated"),
        Some(ModelAvailability::Dedicated)
    );
    assert_eq!(
        ModelAvailability::parse("unknown"),
        Some(ModelAvailability::Unknown)
    );
    assert_eq!(ModelAvailability::parse("provisioned"), None);
    for value in [
        ModelAvailability::Serverless,
        ModelAvailability::Dedicated,
        ModelAvailability::Unknown,
    ] {
        assert_eq!(ModelAvailability::parse(value.as_str()), Some(value));
    }
}

#[test]
fn embedded_catalog_marks_together_dedicated_route_as_dedicated() {
    let config = default_config();
    let model = config
        .models
        .get("Qwen/Qwen3-Coder-Next-FP8")
        .expect("Together Qwen3 Coder Next FP8 is cataloged");
    assert_eq!(model.provider, "together");
    assert_eq!(model.availability, ModelAvailability::Dedicated);
}

#[test]
fn embedded_catalog_dedicated_models_are_not_targeted_by_tier_aliases() {
    // A dedicated-only model behind a tier alias would silently fail
    // every serverless caller; the catalog must keep those routes
    // separated.
    let config = default_config();
    let dedicated: std::collections::BTreeSet<(&str, &str)> = config
        .models
        .iter()
        .filter(|(_, model)| model.availability == ModelAvailability::Dedicated)
        .map(|(id, model)| (model.provider.as_str(), id.as_str()))
        .collect();
    for (name, alias) in &config.aliases {
        if matches!(
            name.as_str(),
            "frontier"
                | "mid"
                | "small"
                | "tier/frontier"
                | "tier/mid"
                | "tier/small"
                | "sonnet"
                | "opus"
                | "haiku"
        ) {
            assert!(
                !dedicated.contains(&(alias.provider.as_str(), alias.id.as_str())),
                "tier alias `{name}` targets dedicated-only route `{}/{}`",
                alias.provider,
                alias.id,
            );
        }
    }
}

#[test]
fn embedded_catalog_tier_aliases_resolve_to_active_models() {
    // The three canonical tier aliases (frontier / mid / small) MUST
    // resolve to non-deprecated catalog entries; a default that
    // routes the loop into a sunsetted model is a release blocker.
    for alias in ["frontier", "mid", "small"] {
        let (model, _provider) = resolve_tier_model(alias, None)
            .unwrap_or_else(|| panic!("tier alias `{alias}` must resolve"));
        let entry = model_catalog_entry(&model).unwrap_or_else(|| {
            panic!("tier alias `{alias}` -> `{model}` must be a registered catalog entry")
        });
        assert!(
            !entry.deprecated,
            "tier alias `{alias}` resolves to deprecated model `{model}` ({:?})",
            entry.deprecation_note
        );
    }
}

#[test]
fn opus_alias_tracks_claude_opus_4_8_with_fast_serving_tier() {
    // The `opus` alias must follow the newest Opus release, and that
    // release advertises its (off-by-default) fast-mode tier.
    let (model, provider) = resolve_model("opus");
    assert_eq!(model, "claude-opus-4-8");
    assert_eq!(provider.as_deref(), Some("anthropic"));

    let opus48 = model_catalog_entry("claude-opus-4-8").expect("opus 4.8 catalog entry");
    assert!(!opus48.deprecated, "newest Opus must not be deprecated");
    let fast = opus48
        .serving_tiers
        .iter()
        .find(|tier| tier.id == "fast")
        .expect("opus 4.8 advertises fast mode");
    let request = fast.request.as_ref().expect("fast tier has request knob");
    assert_eq!(request.param, "speed");
    assert_eq!(request.value, "fast");
    assert_eq!(fast.status.as_deref(), Some("research_preview"));
    let fast_pricing = fast
        .pricing
        .as_ref()
        .expect("fast mode carries premium pricing");
    let standard = opus48.pricing.expect("opus 4.8 standard pricing");
    assert!(
        fast_pricing.input_per_mtok > standard.input_per_mtok,
        "fast mode must be premium-priced relative to standard"
    );
}

#[test]
fn superseded_opus_models_point_at_claude_opus_4_8() {
    // Earlier Opus rows are deprecated and carry a structured
    // `superseded_by` pointer to the current flagship.
    for model in ["claude-opus-4-7", "claude-opus-4-6"] {
        let entry = model_catalog_entry(model).unwrap_or_else(|| panic!("{model} catalog entry"));
        assert!(entry.deprecated, "{model} should be deprecated");
        assert_eq!(
            entry.superseded_by.as_deref(),
            Some("claude-opus-4-8"),
            "{model} should be superseded by claude-opus-4-8"
        );
    }
}

#[test]
fn opus_46_no_longer_advertises_fast_serving_tier() {
    let opus46 = model_catalog_entry("claude-opus-4-6").expect("opus 4.6 catalog entry");
    assert!(
        !opus46.serving_tiers.iter().any(|tier| tier.id == "fast"),
        "Anthropic removed Opus 4.6 fast mode on 2026-06-29; Harn should not advertise it"
    );

    let opus47 = model_catalog_entry("claude-opus-4-7").expect("opus 4.7 catalog entry");
    assert!(
        opus47.serving_tiers.iter().any(|tier| tier.id == "fast"),
        "Opus 4.7 still advertises its own fast-mode tier"
    );
}

#[test]
fn gpt_5_5_fast_serving_tier_rides_service_tier() {
    // Fast mode is provider-agnostic: OpenAI exposes it through the
    // `service_tier` knob rather than Anthropic's `speed`.
    let entry = model_catalog_entry("gpt-5.5").expect("gpt-5.5 catalog entry");
    let fast = entry
        .serving_tiers
        .iter()
        .find(|tier| tier.id == "fast")
        .expect("gpt-5.5 advertises a fast tier");
    let request = fast.request.as_ref().expect("fast tier has request knob");
    assert_eq!(request.param, "service_tier");
    assert_eq!(fast.status.as_deref(), Some("ga"));
}

#[test]
fn gpt_5_6_family_catalog_preserves_role_and_cache_write_economics() {
    for (model, tier, input, output, cache_write) in [
        ("gpt-5.6-sol", "frontier", 5.0, 30.0, 6.25),
        ("gpt-5.6-terra", "mid", 2.5, 15.0, 3.125),
        ("gpt-5.6-luna", "small", 1.0, 6.0, 1.25),
    ] {
        let entry = model_catalog_entry(model).unwrap_or_else(|| panic!("{model} catalog entry"));
        let pricing = entry.pricing.as_ref().expect("GPT-5.6 pricing");
        assert_eq!(entry.context_window, 1_050_000);
        assert_eq!(entry.tier.as_deref(), Some(tier));
        assert_eq!(pricing.input_per_mtok, input);
        assert_eq!(pricing.output_per_mtok, output);
        assert_eq!(pricing.cache_write_per_mtok, Some(cache_write));
        assert!(entry
            .capabilities
            .iter()
            .any(|capability| capability == "vision"));
    }

    let alias = resolve_model_info("gpt-5.6");
    assert_eq!(alias.provider, "openai");
    assert_eq!(alias.id, "gpt-5.6-sol");
    assert_eq!(
        qc_defaults().get("openai").map(String::as_str),
        Some("gpt-5.6-luna")
    );
}
