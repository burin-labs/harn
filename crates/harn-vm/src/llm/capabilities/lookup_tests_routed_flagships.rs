//! Routed flagship rows added or corrected by the 2026-10-01 catalog refresh:
//! Qwen3.8 Max and Kimi K3 on OpenRouter, Qwen3.8 Max and MiniMax M3 on
//! DeepInfra. Each test pins the probed behaviour against the neighbouring
//! generation it would otherwise have inherited, so a rule that silently
//! falls through to the older row fails here.

use super::lookup_tests_support::reset;
use super::*;
use crate::llm_config;

#[test]
fn openrouter_qwen38_max_cannot_disable_reasoning_unlike_qwen37_max() {
    reset();
    let max38 = lookup("openrouter", "qwen/qwen3.8-max-0902");
    assert!(max38.native_tools);
    assert!(!max38.reasoning_none_supported);
    assert!(!max38.reasoning_disable_supported);
    assert!(
        max38.auto_reasoning_overrides.is_empty(),
        "a reasoning-off agent override is a 400 on this route"
    );
    assert_eq!(
        max38.reasoning_effort_levels,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert_eq!(
        max38.structured_output_strategy,
        StructuredOutputStrategy::NativeSchema
    );

    // Contrast: the previous generation and the `qwen/*` catch-all both let
    // agent turns switch reasoning off.
    for older in ["qwen/qwen3.7-max", "qwen/qwen3.8-max-prime"] {
        let caps = lookup("openrouter", older);
        assert!(caps.reasoning_none_supported, "{older}");
        assert_eq!(
            caps.auto_reasoning_overrides
                .get("agent")
                .map(String::as_str),
            Some("off"),
            "{older}"
        );
    }
}

#[test]
fn openrouter_kimi_k3_has_its_own_rule_not_the_k2_rule() {
    reset();
    let k3 = lookup("openrouter", "moonshotai/kimi-k3");
    assert!(k3.native_tools);
    assert_eq!(k3.preferred_tool_format.as_deref(), Some("native"));
    assert!(k3.prompt_caching);
    assert!(k3.vision_supported);
    assert_eq!(k3.thinking_modes, ["effort"]);
    assert_eq!(k3.reasoning_effort_levels, ["low", "high", "max"]);
    assert!(!k3.reasoning_none_supported);
    assert!(!k3.temperature_supported);

    // Contrast: K2.6 keeps the K2 rule's binary thinking toggle.
    let k26 = lookup("openrouter", "moonshotai/kimi-k2.6");
    assert_eq!(k26.thinking_modes, ["enabled"]);
    assert!(k26.reasoning_effort_levels.is_empty());
}

#[test]
fn deepinfra_alibaba_hosted_max_routes_validate_structured_output_in_harn() {
    reset();
    for model in ["deepinfra/Qwen/Qwen3.8-Max", "deepinfra/Qwen/Qwen3.7-Max"] {
        let caps = lookup("deepinfra", model);
        assert!(caps.native_tools, "{model}");
        assert_eq!(
            caps.structured_output_strategy,
            StructuredOutputStrategy::PromptValidation,
            "{model}: json_schema returns HTTP 500 upstream"
        );
    }
    let max38 = lookup("deepinfra", "deepinfra/Qwen/Qwen3.8-Max");
    assert!(!max38.reasoning_none_supported);
    assert!(!max38.vision_supported);

    // Contrast: the open-weight 2.4T build answered strict json_schema.
    let open = lookup("deepinfra", "deepinfra/Qwen/Qwen3.8-2.4T-A95B");
    assert_eq!(
        open.structured_output_strategy,
        StructuredOutputStrategy::NativeSchema
    );
}

#[test]
fn deepinfra_minimax_m3_reasons_by_effort_unlike_the_retired_m27_turbo() {
    reset();
    let m3 = lookup("deepinfra", "deepinfra/MiniMaxAI/MiniMax-M3");
    assert!(m3.native_tools);
    assert!(m3.prompt_caching);
    assert!(m3.reasoning_none_supported);
    assert_eq!(m3.thinking_modes, ["effort"]);
    assert!(!m3.vision_supported, "a red image was answered \"Brown\"");

    // Contrast: M2.7 Turbo only ever matched the host-wide fallback.
    let m27 = lookup("deepinfra", "deepinfra/MiniMaxAI/MiniMax-M2.7-Turbo");
    assert!(m27.thinking_modes.is_empty());
    assert!(!m27.prompt_caching);
}

#[test]
fn refreshed_rows_ship_and_uncatalogued_neighbours_do_not() {
    for (id, provider) in [
        ("moonshotai/kimi-k3", "openrouter"),
        ("qwen/qwen3.8-max-0902", "openrouter"),
        ("deepinfra/Qwen/Qwen3.8-Max", "deepinfra"),
        ("deepinfra/MiniMaxAI/MiniMax-M3", "deepinfra"),
    ] {
        let entry = llm_config::model_catalog_entry(id).expect("refreshed row ships");
        assert_eq!(entry.provider, provider, "{id}");
        assert!(!entry.deprecated, "{id}");
    }
    // Real slugs the providers list that were deliberately not catalogued.
    for id in [
        "qwen/qwen3.8-max-prime",
        "z-ai/glm-5.3-prime",
        "deepinfra/Qwen/Qwen3.8-Flash",
    ] {
        assert!(llm_config::model_catalog_entry(id).is_none(), "{id}");
    }
}

#[test]
fn retired_routes_point_at_live_successors_and_defaults_leave_them() {
    for (id, sunset, successor) in [
        (
            "google/gemini-2.5-flash",
            Some("2026-10-20"),
            "google/gemini-3.5-flash-lite",
        ),
        (
            "qwen/qwen3.6-max-preview",
            Some("2026-10-09"),
            "qwen/qwen3.8-max-0902",
        ),
        (
            "deepinfra/MiniMaxAI/MiniMax-M2.7-Turbo",
            None,
            "deepinfra/MiniMaxAI/MiniMax-M3",
        ),
        ("sambanova/MiniMax-M2.7", None, "sambanova/MiniMax-M3"),
    ] {
        let entry = llm_config::model_catalog_entry(id).expect("retired row stays listed");
        assert!(entry.deprecated, "{id}");
        assert_eq!(entry.sunset_date.as_deref(), sunset, "{id}");
        assert_eq!(entry.superseded_by.as_deref(), Some(successor), "{id}");
        let next = llm_config::model_catalog_entry(successor).expect("successor ships");
        assert!(!next.deprecated, "{successor}");
    }
    assert_eq!(
        llm_config::qc_default_model("openrouter").as_deref(),
        Some("google/gemini-3.5-flash-lite")
    );
}

#[test]
fn openrouter_gemini_3_never_sends_a_reasoning_disable_unlike_gemini_25() {
    reset();
    for model in [
        "google/gemini-3.5-flash-lite",
        "google/gemini-3.5-flash",
        "google/gemini-3.6-flash",
        "google/gemini-3.7-flash",
        "google/gemini-3.8-flash",
    ] {
        let caps = lookup("openrouter", model);
        assert!(!caps.reasoning_disable_supported, "{model}");
        assert!(!caps.reasoning_none_supported, "{model}");
        assert!(caps.native_tools, "{model}");
    }
    // Contrast: OpenRouter's Gemini 2.5 Flash accepted the same disable.
    assert!(lookup("openrouter", "google/gemini-2.5-flash").reasoning_disable_supported);
}
