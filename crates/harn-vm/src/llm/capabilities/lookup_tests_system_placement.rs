use super::{
    clear_user_overrides, lookup, resolve_system_message_placement, SystemMessagePlacement,
};

/// Native mid-conversation sections are gated to the documented Claude models.
/// The Opus `extends` row must layer placement onto the 4.7 capabilities.
#[test]
fn native_directive_gated_to_opus_4_8_and_layers_via_extends() {
    clear_user_overrides();

    // Opus 5 inherits the same >= 4.8 gate through a second `extends` hop.
    for model in ["claude-opus-4-8", "claude-opus-5"] {
        let caps = lookup("anthropic", model);
        assert_eq!(
            caps.system_message_placement,
            Some(SystemMessagePlacement::NativeDirective),
            "{model}"
        );
        assert!(
            caps.native_tools,
            "{model}: extends must layer the placement, not replace the 4.7 capabilities"
        );
        assert!(!caps.thinking_modes.is_empty(), "{model}");
        assert_eq!(
            resolve_system_message_placement(&caps),
            SystemMessagePlacement::NativeDirective,
            "{model}"
        );
    }
    for model in [
        "claude-fable-5",
        "claude-mythos-5",
        "anthropic/claude-fable-5",
        "anthropic/claude-mythos-5",
    ] {
        assert_eq!(
            resolve_system_message_placement(&lookup("anthropic", model)),
            SystemMessagePlacement::Fold,
            "{model}"
        );
    }

    // 4.7 and older Claude have no native mid-conversation channel: unset, so
    // the anthropic wire dialect derives `Fold`.
    let opus_47 = lookup("anthropic", "claude-opus-4-7");
    assert_eq!(opus_47.system_message_placement, None);
    assert_eq!(
        resolve_system_message_placement(&opus_47),
        SystemMessagePlacement::Fold
    );
    assert_eq!(
        resolve_system_message_placement(&lookup("anthropic", "claude-haiku-4-5")),
        SystemMessagePlacement::Fold
    );
    assert_eq!(
        resolve_system_message_placement(&lookup("anthropic", "claude-sonnet-5")),
        SystemMessagePlacement::Fold
    );
    assert_eq!(
        resolve_system_message_placement(&lookup("anthropic", "anthropic/claude-opus-4-8")),
        SystemMessagePlacement::Fold,
        "third-party-prefixed ids must not inherit the direct Claude API grant"
    );
}

/// The unset default derives from the wire dialect: OpenAI/Ollama carry system
/// and developer messages inline; Gemini and Bedrock fold.
#[test]
fn placement_derives_from_wire_dialect_when_unset() {
    clear_user_overrides();
    assert_eq!(
        resolve_system_message_placement(&lookup("openai", "gpt-5.4")),
        SystemMessagePlacement::Inline
    );
    assert_eq!(
        resolve_system_message_placement(&lookup("ollama", "llama3.3:70b")),
        SystemMessagePlacement::Inline
    );
    assert_eq!(
        resolve_system_message_placement(&lookup("gemini", "gemini-2.5-pro")),
        SystemMessagePlacement::Fold
    );
    // Bedrock's non-`anthropic` catch-all rows set `fold` explicitly so a
    // Converse route never mis-derives to `inline` from its wire format.
    assert_eq!(
        resolve_system_message_placement(&lookup("bedrock", "meta.llama3-70b")),
        SystemMessagePlacement::Fold
    );
}
