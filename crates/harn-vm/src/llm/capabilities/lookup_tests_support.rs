//! Shared fixtures for the `lookup_tests_*` modules.
//!
//! These helpers are used by more than one themed module, so they live here
//! rather than being duplicated or reached for across sibling test modules.

use super::{clear_user_overrides, lookup};

/// Drop any user overrides a previous test installed.
///
/// The capability registry is process-global, so a test that sets overrides
/// and does not clear them leaks into whatever runs next.
pub(super) fn reset() {
    clear_user_overrides();
}

/// Assert a Cerebras route exposes effort-style reasoning with the expected
/// thinking-block style.
///
/// `tool_format` is deliberately NOT asserted here: Cerebras gpt-oss and
/// zai-glm have different defaults (gpt-oss harmonized to `json`, glm stays
/// `native`), and this helper is about reasoning-effort behavior. Tool-format
/// resolution is asserted in the dedicated harmonization tests.
pub(super) fn assert_cerebras_effort_reasoning(model: &str, thinking_block_style: &str) {
    let caps = lookup("cerebras", model);
    assert_eq!(caps.thinking_modes, vec!["effort"]);
    assert!(caps.reasoning_effort_supported);
    assert_eq!(caps.structured_output.as_deref(), Some("native"));
    assert_eq!(caps.structured_output_mode, "native_json");
    assert_eq!(caps.thinking_block_style, thinking_block_style);
}
