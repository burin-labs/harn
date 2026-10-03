//! OpenRouter sampling-option claims settled by provider contract probe run
//! 37030330916 and the 2026-10-02 re-probes. Each positive is paired with a
//! route that returned 404 "No endpoints found that can handle the requested
//! parameters", so a rule widened to every OpenRouter route fails here.

use super::lookup_tests_support::reset;
use super::*;
use crate::llm_config::{effective_training_default, TrainingDefault};

#[test]
fn openrouter_top_k_is_claimed_only_where_an_endpoint_serves_it() {
    reset();
    for model in [
        "qwen/qwen3.6-flash",
        "qwen/qwen3.6-35b-a3b",
        "qwen/qwen3-coder",
        "qwen/qwen3.5-plus-20260420",
        "qwen/qwen3.7-max",
        "qwen/qwen3.8-max-0902",
        "z-ai/glm-5.2",
        "minimax/minimax-m3",
        "minimax/minimax-m2.5",
        "moonshotai/kimi-k2.6",
        "moonshotai/kimi-k2.7-code",
        "moonshotai/kimi-k3",
        "stepfun/step-3.7-flash",
        "openrouter/free",
    ] {
        assert!(lookup("openrouter", model).top_k_supported, "{model}");
    }
    for model in [
        "x-ai/grok-4.7",
        "openai/gpt-6-luna",
        "openai/gpt-5.4-mini",
        "google/gemini-3.5-flash-lite",
        "mistralai/mistral-small-2603",
        "bytedance-seed/seed-2.0-lite",
    ] {
        assert!(!lookup("openrouter", model).top_k_supported, "{model}");
    }
}

#[test]
fn openrouter_luna_sampling_stays_unsupported_because_openrouter_drops_it() {
    reset();
    // Accepted without require_parameters only because OpenRouter strips the
    // field; with require_parameters both return 404.
    for model in ["openai/gpt-6-luna", "openai/gpt-5.6-luna"] {
        let caps = lookup("openrouter", model);
        assert!(!caps.temperature_supported, "{model}");
        assert!(!caps.top_p_supported, "{model}");
        assert!(caps.seed_supported, "{model}: seed is forwarded");
    }
}

/// The unforwarded-parameter overlay is per route and per option. Each
/// declined option is paired with a neighbour OpenRouter does forward, so an
/// overlay widened to a family wildcard or to every option fails here.
#[test]
fn openrouter_overlay_declines_only_parameters_no_endpoint_forwards() {
    reset();
    let fable = lookup("openrouter", "anthropic/claude-fable-5");
    assert!(!fable.temperature_supported);
    assert!(!fable.seed_supported);
    let haiku = lookup("openrouter", "anthropic/claude-haiku-4.5");
    assert!(haiku.temperature_supported, "listed for Haiku 4.5");
    assert!(!haiku.frequency_penalty_supported);

    let gemini = lookup("openrouter", "google/gemini-3.8-flash");
    assert!(!gemini.frequency_penalty_supported);
    assert!(gemini.temperature_supported, "listed for Gemini 3.8 Flash");

    assert!(!lookup("openrouter", "x-ai/grok-4.7").stop_supported);
    assert!(
        lookup("openrouter", "x-ai/grok-4.6").stop_supported,
        "Grok 4.6 lists stop"
    );

    let sol = lookup("openrouter", "openai/gpt-6.1-sol");
    assert!(!sol.stop_supported);
    assert!(!sol.presence_penalty_supported);
    assert!(sol.seed_supported, "seed is listed for GPT-6.1 Sol");

    assert!(!lookup("openrouter", "z-ai/glm-5.3-flashx").seed_supported);
    assert!(
        lookup("openrouter", "z-ai/glm-5.2").seed_supported,
        "GLM 5.2 lists seed"
    );

    // The overlay is OpenRouter's contract only; the direct route keeps its own.
    assert!(lookup("openai", "gpt-5.4-mini").temperature_supported);
    assert!(!lookup("openrouter", "openai/gpt-5.4-mini").temperature_supported);
}

/// Plain calls with thinking unset failed on these routes because Harn sent a
/// reasoning-disable OpenRouter rejects. GPT-5.5, which answered the same
/// sweep, is the contrast that keeps its disable.
#[test]
fn openrouter_mandatory_reasoning_routes_never_receive_a_disable() {
    reset();
    for model in [
        "x-ai/grok-4.5",
        "x-ai/grok-4.6",
        "x-ai/grok-4.7",
        "openai/gpt-5.4-pro",
        "openai/gpt-5.5-pro",
    ] {
        assert!(
            !lookup("openrouter", model).reasoning_disable_supported,
            "{model}"
        );
    }
    assert!(lookup("openrouter", "openai/gpt-5.5").reasoning_disable_supported);
}

#[test]
fn openrouter_kimi_k27_code_keeps_reasoning_on_and_routes_around_moonshot() {
    reset();
    let caps = lookup("openrouter", "moonshotai/kimi-k2.7-code");
    assert!(!caps.reasoning_disable_supported);
    assert!(!caps.reasoning_none_supported);
    // Moonshot AI's endpoints reject the penalties they list and drop
    // sampling; with them ignored, every option is forwarded.
    assert_eq!(
        caps.provider_route_denylist,
        vec!["Moonshot AI".to_string()]
    );
    assert!(caps.temperature_supported);
    assert!(caps.top_p_supported);
    assert!(caps.frequency_penalty_supported);
    assert!(caps.presence_penalty_supported);

    // Contrast: K2.6 accepts a reasoning-disable (0 reasoning tokens on
    // 2026-10-02, while K2.7 Code returned 400) and keeps Moonshot routable.
    let k26 = lookup("openrouter", "moonshotai/kimi-k2.6");
    assert!(k26.reasoning_disable_supported);
    assert!(k26.provider_route_denylist.is_empty());
}

#[test]
fn openrouter_free_nemotron_is_declared_to_train_unlike_the_nim_route() {
    assert_eq!(
        effective_training_default("openrouter", "nvidia/nemotron-3-super-120b-a12b:free"),
        Some(TrainingDefault::Trains),
    );
    assert_ne!(
        effective_training_default("openrouter", "cohere/north-mini-code:free"),
        Some(TrainingDefault::Trains),
        "the Cohere free route served the probe account normally"
    );
}
