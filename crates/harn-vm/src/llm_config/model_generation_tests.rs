//! Catalog invariants for Claude model generations: which model the `opus`
//! alias tracks, that every superseded row still points at it, and which
//! generations advertise a fast-mode serving tier. Split out of `tests.rs`
//! to keep that module under the source-length ratchet.

use super::{model_catalog_entry, resolve_model};

#[test]
fn opus_alias_tracks_the_current_opus_with_a_fast_serving_tier() {
    // Asserted as an invariant rather than against a literal id: the `opus`
    // alias must follow the newest Opus release, and that release advertises
    // its (off-by-default) fast-mode tier. Bumping the generation should mean
    // editing the catalog, not this test.
    let (model, provider) = resolve_model("opus");
    assert!(
        model.starts_with("claude-opus-"),
        "`opus` must resolve to an Opus row, got {model}"
    );
    assert_eq!(provider.as_deref(), Some("anthropic"));

    let current = model_catalog_entry(&model).expect("current opus catalog entry");
    assert!(!current.deprecated, "newest Opus must not be deprecated");
    let fast = current
        .serving_tiers
        .iter()
        .find(|tier| tier.id == "fast")
        .unwrap_or_else(|| panic!("{model} advertises fast mode"));
    let request = fast.request.as_ref().expect("fast tier has request knob");
    assert_eq!(request.param, "speed");
    assert_eq!(request.value, "fast");
    assert_eq!(fast.status.as_deref(), Some("research_preview"));
    let fast_pricing = fast
        .pricing
        .as_ref()
        .expect("fast mode carries premium pricing");
    let standard = current.pricing.expect("current opus standard pricing");
    assert!(
        fast_pricing.input_per_mtok > standard.input_per_mtok,
        "fast mode must be premium-priced relative to standard"
    );
}

#[test]
fn every_superseded_opus_points_at_the_alias_target() {
    // Each earlier Opus row is deprecated and carries a structured
    // `superseded_by` pointer, and following those pointers terminates at the
    // model the `opus` alias resolves to. Catches a half-finished generation
    // bump that leaves one row pointing at a retired successor.
    let (current, _) = resolve_model("opus");
    for model in ["claude-opus-4-8", "claude-opus-4-7", "claude-opus-4-6"] {
        if model == current {
            continue;
        }
        let entry = model_catalog_entry(model).unwrap_or_else(|| panic!("{model} catalog entry"));
        assert!(entry.deprecated, "{model} should be deprecated");
        let mut hop = entry
            .superseded_by
            .clone()
            .unwrap_or_else(|| panic!("{model} needs a superseded_by pointer"));
        for _ in 0..8 {
            if hop == current {
                break;
            }
            hop = model_catalog_entry(&hop)
                .unwrap_or_else(|| panic!("{model} -> {hop}: dangling superseded_by"))
                .superseded_by
                .clone()
                .unwrap_or_else(|| panic!("{model} -> {hop}: chain stops short of {current}"));
        }
        assert_eq!(
            hop, current,
            "{model} supersession chain must reach {current}"
        );
    }
}

#[test]
fn opus_46_no_longer_advertises_fast_serving_tier() {
    let opus46 = model_catalog_entry("claude-opus-4-6").expect("opus 4.6 catalog entry");
    assert!(
        !opus46.serving_tiers.iter().any(|tier| tier.id == "fast"),
        "Anthropic removed Opus 4.6 fast mode on 2026-06-29; Harn should not advertise it"
    );

    let opus47 = model_catalog_entry("claude-opus-4-7").expect("opus 4.7 catalog entry");
    assert!(
        opus47.serving_tiers.iter().any(|tier| tier.id == "fast"),
        "Opus 4.7 still advertises its own fast-mode tier"
    );
}
