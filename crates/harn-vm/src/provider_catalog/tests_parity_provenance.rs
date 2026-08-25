//! `tool_support.parity` provenance in the generated catalog artifact (#5885).

use super::*;

/// #5885: the artifact has to say which kind of thing `parity` is.
///
/// `parity` carries a declaration -- either authored on a capability row or
/// computed from `native`/`text`. A forced-format sweep is a different kind
/// of evidence and lands in `empirical_parity`. Without `parity_source` a
/// consumer could not tell an authored verdict from a computed default, so a
/// guard meant to fire on a finding fired on a fallback.
#[test]
fn tool_support_parity_carries_its_provenance() {
    let artifact = artifact();
    let mut declared = 0;
    let mut derived = 0;
    for model in &artifact.models {
        if !model
            .modalities
            .output
            .iter()
            .any(|output| output == "text")
        {
            continue;
        }
        let support = &model.tool_support;
        match (support.parity.as_deref(), support.parity_source.as_deref()) {
            (Some(_), Some("declared")) => declared += 1,
            (Some(_), Some("derived")) => derived += 1,
            (verdict, source) => panic!(
                "{}/{}: parity {verdict:?} must carry a declared/derived source, got {source:?}",
                model.provider, model.id
            ),
        }
    }
    assert!(declared > 0 && derived > 0, "expected both provenances");

    // The class the issue reported: one verdict reached from BOTH directions,
    // which is what makes the verdict alone ambiguous and `parity_source`
    // load-bearing.
    //
    // This used to name `text_only` specifically. That was never the subject:
    // the subject is the ambiguity class, and pinning it to one verdict made
    // the test break whenever a row was re-receipted. The 2026-08-19 CUDA
    // receipt moved the last declared `text_only` row, so the class is now
    // carried by a different verdict. Find whichever verdict currently carries
    // it instead of asserting which one that must be.
    let mut sources_by_verdict: std::collections::BTreeMap<&str, std::collections::BTreeSet<&str>> =
        Default::default();
    for model in &artifact.models {
        if !model
            .modalities
            .output
            .iter()
            .any(|output| output == "text")
        {
            continue;
        }
        if let (Some(parity), Some(source)) = (
            model.tool_support.parity.as_deref(),
            model.tool_support.parity_source.as_deref(),
        ) {
            sources_by_verdict.entry(parity).or_default().insert(source);
        }
    }
    let dual_sourced: Vec<&str> = sources_by_verdict
        .iter()
        .filter(|(_, sources)| sources.contains("declared") && sources.contains("derived"))
        .map(|(verdict, _)| *verdict)
        .collect();
    assert!(
        !dual_sourced.is_empty(),
        "at least one parity verdict must be reachable BOTH ways, or the field would be \
         unambiguous and `parity_source` would have nothing to disambiguate; \
         verdict sources today: {sources_by_verdict:?}"
    );
}

/// The published schema has to admit the field, or a consumer validating
/// against it would reject a catalog that carries provenance.
#[test]
fn generated_schema_declares_the_parity_source_vocabulary() {
    assert_eq!(
        schema_value()["$defs"]["tool_support"]["properties"]["parity_source"]["enum"],
        serde_json::json!(["declared", "derived"])
    );
}
