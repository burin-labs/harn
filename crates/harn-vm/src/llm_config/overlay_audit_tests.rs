//! Behaviour of the overlay redundancy audit, plus the structural guard that
//! keeps its section table in step with `ProvidersConfig`.

use super::overlay_audit::strip_all_auditable_entries;
use super::{
    audit_overlay, parse_config_toml, OverlayFinding, OverlayFindingKind, ProvidersConfig,
};

/// A baseline with one provider, one fully specified model row, one alias, and
/// one routing rule — enough for every finding kind without the weight of the
/// real embedded catalog.
fn baseline() -> ProvidersConfig {
    parse_config_toml(
        r#"
[providers.demo]
display_name = "Demo"
base_url = "https://demo.invalid/v1"
auth_style = "bearer"
chat_endpoint = "/chat/completions"

[aliases]
fast = { id = "demo-fast", provider = "demo" }

[models.demo-fast]
name = "Demo Fast"
provider = "demo"
context_window = 200000
pricing = { input_per_mtok = 1.0, output_per_mtok = 4.0, cache_read_per_mtok = 0.1 }
capabilities = ["tools", "streaming"]

[[tier_rules]]
contains = "fast"
tier = "low"
"#,
    )
    .expect("baseline parses")
}

fn overlay(src: &str) -> ProvidersConfig {
    parse_config_toml(src).expect("overlay parses")
}

fn findings(src: &str) -> Vec<OverlayFinding> {
    audit_overlay(&baseline(), &overlay(src))
}

fn only_finding(src: &str) -> OverlayFinding {
    let found = findings(src);
    assert_eq!(
        found.len(),
        1,
        "expected exactly one finding, got {found:?}"
    );
    found.into_iter().next().expect("one finding")
}

#[test]
fn an_entry_restating_the_baseline_is_redundant() {
    let finding = only_finding(
        r#"
[aliases]
fast = { id = "demo-fast", provider = "demo" }
"#,
    );
    assert_eq!(finding.address(), "aliases.fast");
    assert_eq!(
        finding.kind,
        OverlayFindingKind::Redundant {
            restored_fields: Vec::new()
        }
    );
    assert!(finding.is_actionable());
    assert!(finding.preserves_catalog());
}

#[test]
fn an_entry_that_changes_the_merge_is_left_alone() {
    assert_eq!(
        findings(
            r#"
[aliases]
fast = { id = "demo-fast", provider = "demo", tool_format = "native" }
"#
        ),
        Vec::new()
    );
}

#[test]
fn a_new_route_is_left_alone() {
    assert_eq!(
        findings(
            r#"
[models.demo-private]
name = "Demo Private"
provider = "demo"
context_window = 8192
"#
        ),
        Vec::new()
    );
}

#[test]
fn a_whole_row_copy_narrows_to_the_fields_it_actually_changes() {
    // The classic overlay-rot shape: the row is copied verbatim to widen one
    // field, freezing pricing against every upstream refresh.
    let finding = only_finding(
        r#"
[models.demo-fast]
name = "Demo Fast"
provider = "demo"
context_window = 1000000
pricing = { input_per_mtok = 1.0, output_per_mtok = 4.0, cache_read_per_mtok = 0.1 }
capabilities = ["tools", "streaming"]
"#,
    );
    assert_eq!(finding.address(), "models.demo-fast");
    let OverlayFindingKind::Narrowable {
        patch_toml,
        inherited_fields,
        ..
    } = &finding.kind
    else {
        panic!("expected a narrowable finding, got {:?}", finding.kind);
    };
    assert!(
        patch_toml.contains("context_window = 1000000"),
        "patch should carry the widened window: {patch_toml}"
    );
    assert!(
        !patch_toml.contains("input_per_mtok"),
        "patch should not restate baseline pricing: {patch_toml}"
    );
    for inherited in ["name", "pricing", "capabilities", "provider"] {
        assert!(
            inherited_fields.iter().any(|field| field == inherited),
            "{inherited} should go back to upstream: {inherited_fields:?}"
        );
    }
}

#[test]
fn the_suggested_patch_reproduces_the_whole_row_it_replaces() {
    // The suggestion is only safe to apply blind if pasting it back yields the
    // same catalog, so prove the round trip rather than trusting the diff.
    let whole_row = r#"
[models.demo-fast]
name = "Demo Fast"
provider = "demo"
context_window = 1000000
pricing = { input_per_mtok = 1.0, output_per_mtok = 4.0, cache_read_per_mtok = 0.1 }
capabilities = ["tools", "streaming"]
"#;
    let OverlayFindingKind::Narrowable { patch_toml, .. } = only_finding(whole_row).kind else {
        panic!("expected a narrowable finding");
    };

    let mut from_whole_row = baseline();
    from_whole_row.merge_from(&overlay(whole_row));
    let mut from_patch = baseline();
    from_patch.merge_from(&overlay(&patch_toml));
    // The accumulator is the one intended difference: the patch form keeps
    // tweaking this field if a later layer replaces the row wholesale.
    from_patch.patch.models.clear();
    assert_eq!(from_whole_row, from_patch);
}

#[test]
fn a_nested_table_narrows_field_by_field() {
    let finding = only_finding(
        r#"
[models.demo-fast]
name = "Demo Fast"
provider = "demo"
context_window = 200000
pricing = { input_per_mtok = 1.0, output_per_mtok = 9.0, cache_read_per_mtok = 0.1 }
capabilities = ["tools", "streaming"]
"#,
    );
    let OverlayFindingKind::Narrowable { patch_toml, .. } = &finding.kind else {
        panic!("expected a narrowable finding, got {:?}", finding.kind);
    };
    assert!(
        patch_toml.contains("output_per_mtok = 9.0") && !patch_toml.contains("input_per_mtok"),
        "only the changed rate belongs in the patch: {patch_toml}"
    );
}

#[test]
fn a_row_that_forgets_a_baseline_field_reports_what_narrowing_gives_back() {
    // Whole-row replacement drops `pricing` with no warning, and the loss is
    // indistinguishable from upstream never having priced the route.
    let finding = only_finding(
        r#"
[models.demo-fast]
name = "Demo Fast"
provider = "demo"
context_window = 1000000
capabilities = ["tools", "streaming"]
"#,
    );
    let OverlayFindingKind::Narrowable {
        restored_fields, ..
    } = &finding.kind
    else {
        panic!("expected a narrowable finding, got {:?}", finding.kind);
    };
    assert_eq!(restored_fields, &["pricing".to_string()]);
    assert!(finding.is_actionable());
    assert!(
        !finding.preserves_catalog(),
        "handing pricing back changes the shipped catalog, and the finding must say so"
    );
}

#[test]
fn a_forgotten_nested_field_is_reported_by_its_dotted_path() {
    let finding = only_finding(
        r#"
[models.demo-fast]
name = "Demo Fast"
provider = "demo"
context_window = 1000000
pricing = { input_per_mtok = 1.0, output_per_mtok = 4.0 }
capabilities = ["tools", "streaming"]
"#,
    );
    let OverlayFindingKind::Narrowable {
        restored_fields,
        patch_toml,
        ..
    } = &finding.kind
    else {
        panic!("expected a narrowable finding, got {:?}", finding.kind);
    };
    assert_eq!(
        restored_fields,
        &["pricing.cache_read_per_mtok".to_string()]
    );
    assert!(
        !patch_toml.contains("pricing"),
        "the row changes no rate, so pricing does not belong in the patch: {patch_toml}"
    );
}

#[test]
fn a_verbatim_copy_that_only_forgets_fields_is_simply_deleted() {
    let finding = only_finding(
        r#"
[models.demo-fast]
name = "Demo Fast"
provider = "demo"
context_window = 200000
capabilities = ["tools", "streaming"]
"#,
    );
    assert_eq!(
        finding.kind,
        OverlayFindingKind::Redundant {
            restored_fields: vec!["pricing".to_string()]
        }
    );
    assert!(!finding.preserves_catalog());
}

#[test]
fn a_patch_for_a_route_that_no_longer_exists_is_dangling() {
    let finding = only_finding(
        r"
[patch.models.demo-retired]
context_window = 4096
",
    );
    assert_eq!(finding.address(), "patch.models.demo-retired");
    assert_eq!(
        finding.kind,
        OverlayFindingKind::Dangling {
            target: "model demo-retired".to_string()
        }
    );
    // Advisory: a later layer may still contribute the row.
    assert!(!finding.is_actionable());
}

#[test]
fn a_suppression_that_hides_nothing_is_dangling() {
    let finding = only_finding(
        r#"
[suppress]
routes = ["demo:demo-retired"]
"#,
    );
    assert_eq!(finding.address(), r#"suppress.routes."demo:demo-retired""#);
    assert!(matches!(finding.kind, OverlayFindingKind::Dangling { .. }));
}

#[test]
fn a_live_suppression_is_left_alone() {
    assert_eq!(
        findings(
            r#"
[suppress]
routes = ["demo:demo-fast"]
"#
        ),
        Vec::new()
    );
}

#[test]
fn a_model_defaults_pattern_matching_no_route_is_dangling() {
    let finding = only_finding(
        r#"
[model_defaults."retired-family-*"]
temperature = 0.2
"#,
    );
    assert_eq!(finding.address(), r#"model_defaults."retired-family-*""#);
    assert!(matches!(finding.kind, OverlayFindingKind::Dangling { .. }));
    // Advisory: sampling defaults legitimately cover Ollama tags and local
    // GGUFs that the static catalog never enumerates.
    assert!(!finding.is_actionable());
}

#[test]
fn a_model_defaults_pattern_may_name_the_provider_qualified_route() {
    assert_eq!(
        findings(
            r#"
[model_defaults."demo/demo-*"]
temperature = 0.2
"#
        ),
        Vec::new()
    );
}

#[test]
fn tool_calling_policy_for_an_unknown_alias_is_dangling() {
    let finding = only_finding(
        r#"
[alias_tool_calling."ghost"]
native = "unreliable"
"#,
    );
    assert_eq!(finding.address(), "alias_tool_calling.ghost");
    assert!(matches!(finding.kind, OverlayFindingKind::Dangling { .. }));
}

#[test]
fn a_rule_the_baseline_already_declares_is_advisory_only() {
    // Overlay rules are prepended, so an identical copy still changes which
    // rule wins for inputs that also match something in between. Report it,
    // but never let a gate delete it.
    let finding = only_finding(
        r#"
[[tier_rules]]
contains = "fast"
tier = "low"
"#,
    );
    assert_eq!(finding.address(), "tier_rules.0");
    assert_eq!(
        finding.kind,
        OverlayFindingKind::DuplicateOfBaseline { baseline_index: 0 }
    );
    assert!(!finding.is_actionable());
}

/// Every section of a `providers.toml` overlay must be addressable, or its
/// entries can never be reported.
///
/// The destructure is the guard: adding a field to [`ProvidersConfig`] fails
/// to compile here until someone decides how the audit addresses it, and the
/// emptiness assertion then fails until a `SECTIONS` row actually removes it.
#[test]
fn every_config_section_is_auditable() {
    let populated = overlay(EVERY_SECTION);
    let ProvidersConfig {
        default_provider,
        providers,
        aliases,
        alias_tool_calling,
        models,
        qc_defaults,
        inference_rules,
        tier_rules,
        tier_defaults,
        model_defaults,
        model_roles,
        suppress,
        patch,
        model_ladders,
        presentation,
    } = &populated;
    assert!(default_provider.is_some());
    assert!(!providers.is_empty());
    assert!(!aliases.is_empty());
    assert!(!alias_tool_calling.is_empty());
    assert!(!models.is_empty());
    assert!(!qc_defaults.is_empty());
    assert!(!inference_rules.is_empty());
    assert!(!tier_rules.is_empty());
    assert_ne!(tier_defaults, &super::TierDefaults::default());
    assert!(!model_defaults.is_empty());
    assert!(!model_roles.is_empty());
    assert!(!suppress.routes.is_empty());
    assert!(!patch.models.is_empty());
    assert!(!model_ladders.is_empty());
    assert!(!presentation.variants.is_empty());
    assert!(!presentation.families.is_empty());

    let stripped = strip_all_auditable_entries(&populated);
    assert!(
        stripped.is_empty(),
        "the audit cannot address every overlay section; leftover: {stripped:?}"
    );
}

/// An overlay that exercises every section [`ProvidersConfig`] carries.
const EVERY_SECTION: &str = r#"
default_provider = "demo"

[providers.other]
display_name = "Other"
base_url = "https://other.invalid/v1"
auth_style = "bearer"
chat_endpoint = "/chat/completions"

[aliases]
slow = { id = "demo-slow", provider = "demo" }

[alias_tool_calling."slow"]
native = "unreliable"

[models.demo-slow]
name = "Demo Slow"
provider = "demo"
context_window = 8192

[patch.models.demo-slow]
context_window = 16384

[qc_defaults]
reviewer = "slow"

[[inference_rules]]
contains = "demo-"
provider = "demo"

[[tier_rules]]
contains = "slow"
tier = "low"

[tier_defaults]
default = "high"

[model_defaults."demo-slow"]
temperature = 0.2

[model_roles."reviewer"]
temperature = 0.1

[suppress]
routes = ["demo:demo-slow"]

[model_ladders.cheap]
label = "Cheap first"
steps = [{ model = "demo-slow", provider = "demo" }]

[presentation.variants.balanced]
order = 1
label = "Balanced"
description = "A middle option."
selector = { kind = "alias", name = "slow" }

[presentation.families.demo]
label = "Demo"
plain_description = "The demo family."
dimensions = [
  { key = "size", label = "Size", plain_description = "How big.", kind = "model", ordered_values = [
    { value = "slow", label = "Slow", plain_description = "The slow one.", relative_cost_hint = 1, relative_speed_hint = 1, model_id = "demo-slow" },
  ] },
]
presets = [
  { id = "default", label = "Default", plain_blurb = "Start here.", coordinates = { size = "slow" } },
]
"#;
