//! Anthropic prompt-cache floors and the Opus 5 capability surface.
//!
//! Split out of `lookup.rs` to keep that module under the source-length
//! ratchet, following the existing `lookup_tests_*` convention.

use super::{clear_user_overrides as reset, lookup};
use crate::llm::cache_conformance::CacheControlProfile;

#[test]
fn anthropic_opus_5_inherits_the_47_surface_with_a_512_token_cache_floor() {
    reset();
    let caps = lookup("anthropic", "claude-opus-5");
    assert_eq!(caps.thinking_modes, vec!["adaptive", "effort"]);
    assert!(caps.reasoning_effort_supported);
    assert_eq!(
        caps.reasoning_effort_levels,
        vec!["low", "medium", "high", "xhigh", "max"]
    );
    assert!(!caps.supports_assistant_prefill);
    assert_eq!(caps.prompt_cache_min_prefix_tokens, Some(512));
}

#[test]
fn anthropic_prompt_cache_floor_is_per_model_not_per_dialect() {
    reset();
    // Measured against the live API on 2026-07-24. A flat 1024 would tell
    // callers a 1400-token prefix caches on Opus 4.7 and Haiku 4.5; it
    // silently does not.
    for (model, floor) in [
        ("claude-opus-5", 512),
        ("claude-fable-5", 512),
        ("claude-mythos-5", 512),
        ("claude-mythos-preview", 2048),
        ("claude-opus-4-8", 1024),
        ("claude-sonnet-5", 1024),
        ("claude-sonnet-4-6", 1024),
        ("claude-opus-4-7", 2048),
        ("claude-opus-4-6", 4096),
        ("claude-haiku-4-5", 4096),
    ] {
        let caps = lookup("anthropic", model);
        assert_eq!(
            caps.prompt_cache_min_prefix_tokens,
            Some(floor),
            "{model} minimum cacheable prefix"
        );
        assert_eq!(
            CacheControlProfile::from_capabilities(&caps).min_useful_prefix_tokens,
            Some(floor),
            "{model} cache profile must carry the model floor, not the dialect default"
        );
    }
}
