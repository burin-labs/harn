use super::{clear_user_overrides, lookup};

#[test]
fn moonshot_kimi_k3_exposes_its_documented_effort_ladder_and_reasoning_replay() {
    clear_user_overrides();
    let caps = lookup("moonshot", "moonshot/kimi-k3");
    assert!(caps.native_tools);
    assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
    // Native and text modes are interchangeable. A 2026-08-26 direct control
    // also confirmed that required tool choice works with mandatory thinking.
    assert_eq!(caps.tool_mode_parity.as_deref(), Some("interchangeable"));
    assert!(caps.prompt_caching);
    assert!(caps.vision_supported);
    assert_eq!(
        caps.allowed_tool_choice_modes,
        vec!["auto", "required", "none"]
    );
    assert!(caps.requires_completion_tokens);
    assert!(caps.reasoning_effort_supported);
    assert_eq!(caps.thinking_modes, vec!["effort"]);
    assert_eq!(caps.reasoning_effort_levels, vec!["low", "high", "max"]);
    assert!(!caps.reasoning_none_supported);
    assert!(!caps.reasoning_disable_supported);
    assert_eq!(
        caps.reasoning_history_wire_field
            .map(|field| field.as_str()),
        Some("reasoning_content")
    );
    assert_eq!(
        caps.reasoning_round_trip,
        super::ReasoningRoundTripPolicy::EchoSameKey
    );
    assert!(!caps.temperature_supported);
    assert!(!caps.top_p_supported);
    assert!(!caps.frequency_penalty_supported);
    assert!(!caps.presence_penalty_supported);
}
