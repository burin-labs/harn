use super::{
    clear_user_overrides, lookup, resolve_system_message_placement, SystemMessagePlacement,
};

/// Opus 4.8 is the only Claude that accepts a native mid-conversation system
/// directive. The `extends` capability row must layer the placement onto the
/// 4.7 row without wiping the rest of its capabilities.
#[test]
fn native_directive_gated_to_opus_4_8_and_layers_via_extends() {
    clear_user_overrides();

    let opus_48 = lookup("anthropic", "claude-opus-4-8");
    assert_eq!(
        opus_48.system_message_placement,
        Some(SystemMessagePlacement::NativeDirective)
    );
    assert!(
        opus_48.native_tools,
        "extends must layer the placement, not replace the 4.7 capabilities"
    );
    assert!(!opus_48.thinking_modes.is_empty());
    assert_eq!(
        resolve_system_message_placement(&opus_48),
        SystemMessagePlacement::NativeDirective
    );

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
