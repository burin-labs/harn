//! Overlay and `[patch.models]` merge semantics: which layer wins, what
//! survives a whole-row replacement, and how a dangling patch behaves.

use super::super::*;
use serde::Deserialize;

#[test]
fn presentation_rows_replace_whole_same_id_records_across_overlays() {
    let mut base = parse_config_toml(
        r#"
[presentation.variants.fast]
order = 10
label = "Old fast"
description = "Old description"
selector = { kind = "alias", name = "small" }

[presentation.families.demo]
label = "Old family"
plain_description = "Old description"
model_id = "demo-model"
dimensions = []
presets = []
"#,
    )
    .expect("base parses");
    let overlay = parse_config_toml(
        r#"
[presentation.variants.fast]
order = 10
label = "New fast"
description = "New description"
selector = { kind = "model", model_id = "new-model" }

[presentation.families.demo]
label = "New family"
plain_description = "New description"
model_id = "new-model"
dimensions = []
presets = []
"#,
    )
    .expect("overlay parses");

    base.merge_from(&overlay);

    let variant = base.presentation.variants.get("fast").expect("variant");
    assert_eq!(variant.label, "New fast");
    assert!(matches!(
        &variant.selector,
        PresentationVariantSelector::Model { model_id } if model_id == "new-model"
    ));
    let family = base.presentation.families.get("demo").expect("family");
    assert_eq!(family.label, "New family");
    assert_eq!(family.model_id.as_deref(), Some("new-model"));
}

/// Base config for the `[patch.models]` tests: one fully-populated row.
const PATCH_BASE_TOML: &str = r#"
[models."demo/patch-target"]
name = "Patch Target"
provider = "demo"
context_window = 128000
stream_timeout = 300.0
capabilities = ["tools", "vision"]
strengths = ["coding"]

[models."demo/patch-target".pricing]
input_per_mtok = 1.0
output_per_mtok = 5.0
"#;

fn patch_base() -> ProvidersConfig {
    parse_config_toml(PATCH_BASE_TOML).expect("patch base parses")
}

fn patched_row(config: &ProvidersConfig) -> &ModelDef {
    config
        .models
        .get("demo/patch-target")
        .expect("patch target row present")
}

#[test]
fn patch_models_scalar_and_nested_field_preserve_siblings() {
    let mut base = patch_base();
    let overlay = parse_config_toml(
        "[patch.models.\"demo/patch-target\"]\nstream_timeout = 1200.0\n\
             [patch.models.\"demo/patch-target\".pricing]\noutput_per_mtok = 2.5\n",
    )
    .expect("patch overlay parses");
    assert!(!overlay.is_empty(), "a patch-only overlay is not empty");
    base.merge_from(&overlay);
    let row = patched_row(&base);
    assert_eq!(row.stream_timeout, Some(1200.0), "patched scalar applies");
    assert_eq!(row.name, "Patch Target", "unpatched scalar is intact");
    assert_eq!(row.context_window, 128000, "unpatched scalar is intact");
    assert_eq!(
        row.capabilities,
        vec!["tools".to_string(), "vision".to_string()],
        "unpatched array is intact"
    );
    let pricing = row.pricing.as_ref().expect("pricing survives the patch");
    assert_eq!(pricing.output_per_mtok, 2.5, "patched nested field applies");
    assert_eq!(
        pricing.input_per_mtok, 1.0,
        "sibling nested field is preserved by the deep merge"
    );
    assert!(base.dangling_model_patches().is_empty());
}

#[test]
fn patch_models_array_replaces_wholesale() {
    let mut base = patch_base();
    let overlay =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\ncapabilities = [\"tools\"]\n")
            .expect("patch overlay parses");
    base.merge_from(&overlay);
    let row = patched_row(&base);
    assert_eq!(
        row.capabilities,
        vec!["tools".to_string()],
        "arrays replace wholesale — no element-wise merge"
    );
    assert_eq!(
        row.strengths,
        vec!["coding".to_string()],
        "arrays the patch does not name are intact"
    );
}

#[test]
fn patch_models_wins_over_whole_row_in_same_overlay() {
    let mut base = patch_base();
    let overlay = parse_config_toml(
        "[models.\"demo/patch-target\"]\n\
             name = \"Replaced Row\"\nprovider = \"demo\"\ncontext_window = 64000\n\
             stream_timeout = 600.0\n\
             [patch.models.\"demo/patch-target\"]\nstream_timeout = 1200.0\n",
    )
    .expect("overlay parses");
    base.merge_from(&overlay);
    let row = patched_row(&base);
    assert_eq!(
        row.name, "Replaced Row",
        "the whole-row replacement lands first"
    );
    assert_eq!(row.context_window, 64000);
    assert_eq!(
        row.stream_timeout,
        Some(1200.0),
        "the same overlay's patch fields win over its whole-row fields"
    );
}

#[test]
fn patch_models_chained_layers_accumulate_and_later_wins() {
    let mut base = patch_base();
    let layer1 =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\nstream_timeout = 900.0\n")
            .expect("layer1 parses");
    let layer2 =
        parse_config_toml("[patch.models.\"demo/patch-target\".pricing]\noutput_per_mtok = 2.5\n")
            .expect("layer2 parses");
    base.merge_from(&layer1);
    base.merge_from(&layer2);
    let row = patched_row(&base);
    assert_eq!(
        row.stream_timeout,
        Some(900.0),
        "layer1's field patch survives layer2 patching a different field"
    );
    assert_eq!(
        row.pricing
            .as_ref()
            .expect("pricing present")
            .output_per_mtok,
        2.5,
        "layer2's field patch applies"
    );

    let layer3 =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\nstream_timeout = 1200.0\n")
            .expect("layer3 parses");
    base.merge_from(&layer3);
    assert_eq!(
        patched_row(&base).stream_timeout,
        Some(1200.0),
        "for the same field, the later layer's patch wins"
    );
}

#[test]
fn patch_models_sticky_across_later_whole_row_replacement() {
    let mut base = patch_base();
    let patch_layer =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\nstream_timeout = 1200.0\n")
            .expect("patch layer parses");
    base.merge_from(&patch_layer);
    // A later layer replaces the whole row (e.g. a hosted runtime-catalog
    // refresh re-ships the baseline). The accumulated patch re-applies:
    // patches mean "always tweak this field", not "tweak it once".
    let replacement_layer = parse_config_toml(
        "[models.\"demo/patch-target\"]\n\
             name = \"Refreshed Row\"\nprovider = \"demo\"\ncontext_window = 256000\n\
             stream_timeout = 300.0\n",
    )
    .expect("replacement layer parses");
    base.merge_from(&replacement_layer);
    let row = patched_row(&base);
    assert_eq!(row.name, "Refreshed Row", "the whole-row refresh lands");
    assert_eq!(row.context_window, 256000);
    assert_eq!(
        row.stream_timeout,
        Some(1200.0),
        "the sticky patch re-applies on top of the refreshed row"
    );
}

#[test]
fn patch_models_dangling_patch_reports_and_applies_when_row_arrives() {
    let mut base = patch_base();
    let dangling =
        parse_config_toml("[patch.models.\"demo/not-yet-cataloged\"]\nstream_timeout = 42.0\n")
            .expect("dangling patch parses");
    base.merge_from(&dangling);
    assert_eq!(
        base.dangling_model_patches(),
        vec!["demo/not-yet-cataloged"],
        "a patch with no matching row is reported, not dropped"
    );
    assert_eq!(
        patched_row(&base).stream_timeout,
        Some(300.0),
        "existing rows are untouched by a dangling patch"
    );

    // The row arrives from a LATER layer; the accumulated patch applies.
    let late_row = parse_config_toml(
        "[models.\"demo/not-yet-cataloged\"]\n\
             name = \"Late Arrival\"\nprovider = \"demo\"\ncontext_window = 8192\n",
    )
    .expect("late row parses");
    base.merge_from(&late_row);
    assert!(base.dangling_model_patches().is_empty());
    let row = base
        .models
        .get("demo/not-yet-cataloged")
        .expect("late row present");
    assert_eq!(row.stream_timeout, Some(42.0), "the held patch applied");
    assert_eq!(row.name, "Late Arrival");
}

#[test]
fn patch_models_type_error_keeps_unpatched_row() {
    let mut base = patch_base();
    let bad =
        parse_config_toml("[patch.models.\"demo/patch-target\"]\nstream_timeout = \"soon\"\n")
            .expect("the patch overlay itself is valid TOML");
    base.merge_from(&bad);
    let row = patched_row(&base);
    assert_eq!(
        row.stream_timeout,
        Some(300.0),
        "a type-invalid patch keeps the unpatched row"
    );
    assert_eq!(row.name, "Patch Target", "the rest of the row is intact");
}

#[test]
fn model_rows_roundtrip_through_toml_value_for_patching() {
    // Patch application is `ModelDef -> toml::Value -> deep merge ->
    // ModelDef`. This property test guards the serialization leg: every
    // embedded catalog row must survive the round trip unchanged (a
    // missing `Serialize` derive or asymmetric serde attribute on a
    // nested def would corrupt rows the first time they are patched).
    let config = default_config();
    assert!(!config.models.is_empty());
    for (id, row) in &config.models {
        let value = toml::Value::try_from(row)
            .unwrap_or_else(|error| panic!("serialize model row {id}: {error}"));
        let roundtripped = ModelDef::deserialize(value)
            .unwrap_or_else(|error| panic!("deserialize model row {id}: {error}"));
        assert_eq!(&roundtripped, row, "model row {id} must round-trip");
    }
}
