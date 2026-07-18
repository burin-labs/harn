use super::{clear_user_overrides, lookup};

#[test]
fn moonshot_kimi_k3_requires_max_effort_and_catalog_owned_reasoning_replay() {
    clear_user_overrides();
    let caps = lookup("moonshot", "moonshot/kimi-k3");
    assert!(caps.native_tools);
    assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
    // 2026-07-18 credentialed probe: native+auto and the Harn text channel both
    // carry the large-string argument byte-exact, so the modes are
    // interchangeable. Forced tool_choice 400s under mandatory thinking, so
    // `required` is not an advertised mode.
    assert_eq!(caps.tool_mode_parity.as_deref(), Some("interchangeable"));
    assert!(caps.prompt_caching);
    assert!(caps.vision_supported);
    assert_eq!(caps.allowed_tool_choice_modes, vec!["auto", "none"]);
    assert!(caps.requires_completion_tokens);
    assert!(caps.reasoning_effort_supported);
    assert_eq!(caps.thinking_modes, vec!["effort"]);
    assert_eq!(caps.reasoning_effort_levels, vec!["max"]);
    assert!(!caps.reasoning_none_supported);
    assert!(!caps.reasoning_disable_supported);
    assert_eq!(
        caps.reasoning_history_wire_field
            .map(|field| field.as_str()),
        Some("reasoning_content")
    );
    assert!(!caps.temperature_supported);
    assert!(!caps.top_p_supported);
    assert!(!caps.frequency_penalty_supported);
    assert!(!caps.presence_penalty_supported);
}
