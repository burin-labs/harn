//! Provenance of a route's `tool_mode_parity` verdict (#5885).

use super::model::ToolModeParitySource;
use super::{clear_user_overrides, lookup};

/// #5885: a route whose parity verdict nobody wrote down must say so.
///
/// `text_only` used to arrive identically whether a capability row stated
/// it or the `native_tools = false` fallback computed it, so a consumer
/// gating on the verdict could not tell a finding from a default. Both of
/// these resolve to `text_only`; only one of them is anybody's claim.
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

    // A verdict authored on the row rather than computed. The value differs
    // from the derived case above; what this pins is the provenance.
    let declared = lookup("llamacpp", "qwen3.6-35b-a3b");
    assert_eq!(
        declared.tool_mode_parity.as_deref(),
        Some("interchangeable")
    );
    assert_eq!(
        declared.tool_mode_parity_source,
        Some(ToolModeParitySource::Declared)
    );

    // An unmatched route has no verdict at all, so it has no provenance
    // either -- distinct from "derived", which is a computed answer.
    let unmatched = lookup("no-such-provider", "no-such-model");
    assert_eq!(unmatched.tool_mode_parity, None);
    assert_eq!(unmatched.tool_mode_parity_source, None);
}
