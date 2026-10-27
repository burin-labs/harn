use super::*;

const SOURCE: &str = r#"
[models.previous]
name = "Previous"
provider = "direct"
context_window = 100

[models.current]
name = "Current"
provider = "direct"
context_window = 200
pricing = { input_per_mtok = 1.0, output_per_mtok = 5.0 }

[models.current.registration]
aliases = ["current-short"]
supersedes = ["previous"]

[models.current.registration.request]
reasoning_none_supported = true
reasoning_effort_levels = ["none", "low", "max"]
structured_output = "native"

[models.current.registration.routes.gateway]
provider = "gateway"
aliases = ["gateway-short"]
model = { name = "Current through gateway", wire_model = "creator/current", pricing = { input_per_mtok = 2.0, output_per_mtok = 10.0 } }
request = { reasoning_none_supported = false, reasoning_effort_levels = ["low"] }

[models.current.registration.selection]
family = "example"
dimension = "variant"
before = "previous"
value = { value = "current", label = "Current", plain_description = "Current model.", relative_cost_hint = 2, relative_speed_hint = 4 }

[presentation.families.example]
label = "Example"
plain_description = "Choose a model."
presets = []

[[presentation.families.example.dimensions]]
key = "variant"
label = "Model"
plain_description = "Choose a model."
kind = "model"
ordered_values = [{ value = "previous", label = "Previous", plain_description = "Previous model.", relative_cost_hint = 3, relative_speed_hint = 3, model_id = "previous" }]
"#;

#[test]
fn one_registration_projects_routes_requests_aliases_replacement_and_picker() {
    let projection = expand_model_registrations(SOURCE).unwrap().unwrap();
    let parsed =
        crate::config::parse_config_toml_with_diagnostics(&projection.catalog_source).unwrap();
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let config = parsed.config;
    assert_eq!(config.models.len(), 3);
    assert!(!config.models.contains_key("current-unserved"));
    assert_eq!(config.models["gateway"].context_window, 200);
    assert_eq!(config.models["gateway"].provider, "gateway");
    assert_eq!(
        config.models["gateway"].wire_model.as_deref(),
        Some("creator/current")
    );
    assert_eq!(
        config.models["previous"].superseded_by.as_deref(),
        Some("current")
    );
    assert_eq!(config.aliases["current-short"].id, "current");
    assert_eq!(config.aliases["gateway-short"].provider, "gateway");
    let values = &config.presentation.families["example"].dimensions[0].ordered_values;
    assert_eq!(values.len(), 2);
    assert_eq!(values[0].model_id.as_deref(), Some("current"));
    assert_eq!(values[1].value, "previous");
    let capabilities: crate::capabilities::CapabilitiesFile =
        toml::from_str(&projection.capability_source).unwrap();
    assert_eq!(
        capabilities.provider["direct"][0].reasoning_none_supported,
        Some(true)
    );
    assert_eq!(
        capabilities.provider["gateway"][0].reasoning_none_supported,
        Some(false)
    );
    assert_eq!(
        capabilities.provider["gateway"][0]
            .structured_output
            .as_deref(),
        Some("native")
    );
    assert!(capabilities
        .provider
        .values()
        .flatten()
        .all(|rule| rule.extends));
    let rules: Value = toml::from_str(&projection.capability_source).unwrap();
    assert_eq!(
        rules["provider"]["gateway"][0]["model_match"][1].as_str(),
        Some("creator/current")
    );
    assert!(!projection.catalog_source.contains("registration"));
}

#[test]
fn unknown_request_policy_and_authored_matches_are_refused() {
    for (original, replacement, expected) in [
        (
            "reasoning_none_supported = true",
            "reasoning_none_suported = true",
            "unknown fields",
        ),
        (
            "reasoning_none_supported = true",
            "model_match = \"*\"",
            "exact match",
        ),
        (
            "reasoning_none_supported = true",
            "extends = false",
            "fall-through",
        ),
        (
            "before = \"previous\"",
            "before = \"absent\"",
            "unknown preceding picker value",
        ),
    ] {
        let error = expand_model_registrations(&SOURCE.replace(original, replacement))
            .err()
            .expect("invalid registration must fail");
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn duplicate_authorities_do_not_silently_replace_each_other() {
    for (suffix, expected) in [
        ("\n[models.gateway]\nname = \"Already exists\"\nprovider = \"gateway\"\ncontext_window = 10\n", "already has a model row"),
        ("\n[aliases.current-short]\nid = \"previous\"\nprovider = \"direct\"\n", "duplicate alias"),
    ] {
        let error = expand_model_registrations(&format!("{SOURCE}{suffix}"))
            .err().expect("duplicate authority must fail");
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn ordinary_catalog_sources_do_not_gain_a_second_projection() {
    assert!(
        expand_model_registrations("[models.example]\nname = \"Example\"\n")
            .unwrap()
            .is_none()
    );
}
