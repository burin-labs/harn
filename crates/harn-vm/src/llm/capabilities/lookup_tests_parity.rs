//! Provenance of a route's `tool_mode_parity` verdict (#5885).

use super::model::ToolModeParitySource;
use super::{clear_user_overrides, lookup, set_user_overrides_toml};

/// #5885: a route whose parity verdict nobody wrote down must say so.
///
/// `text_only` arrives identically whether a capability row states it or the
/// `native_tools = false` fallback computes it, so a consumer gating on the
/// verdict alone cannot tell a finding from a default. Provenance is the only
/// discriminator, and this test exercises exactly that ambiguity: both routes
/// below resolve to `text_only`, and only one of them is anybody's claim.
///
/// The declared half is supplied by an override rather than read off a shipped
/// row. It used to read a real `text_only` row, and when the 2026-08-19 CUDA
/// receipt re-verdicted that row the test broke over a fact that is not its
/// subject. No shipped row declares `text_only` today, so depending on one
/// would make this contract hostage to the next receipt. Constructing the
/// declared side here keeps the ambiguity under test permanently reproducible.
#[test]
fn tool_mode_parity_reports_whether_anyone_declared_it() {
    clear_user_overrides();

    // Reported in #5885. No row states a parity verdict for it.
    let derived = lookup("fireworks", "accounts/fireworks/models/gpt-oss-120b");
    assert_eq!(derived.tool_mode_parity.as_deref(), Some("text_only"));
    assert_eq!(
        derived.tool_mode_parity_source,
        Some(ToolModeParitySource::Derived)
    );

    // Same verdict, but authored rather than computed.
    set_user_overrides_toml(
        r#"
[[provider.parity-provenance-fixture]]
model_match = "declared-text-only"
native_tools = false
preferred_tool_format = "json"
tool_mode_parity = "text_only"
"#,
    )
    .expect("declared parity override parses");
    let declared = lookup("parity-provenance-fixture", "declared-text-only");
    assert_eq!(declared.tool_mode_parity.as_deref(), Some("text_only"));
    assert_eq!(
        declared.tool_mode_parity_source,
        Some(ToolModeParitySource::Declared)
    );
    clear_user_overrides();

    // An unmatched route has no verdict at all, so it has no provenance
    // either -- distinct from "derived", which is a computed answer.
    let unmatched = lookup("no-such-provider", "no-such-model");
    assert_eq!(unmatched.tool_mode_parity, None);
    assert_eq!(unmatched.tool_mode_parity_source, None);
}
