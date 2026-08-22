use super::*;

#[test]
fn compact_model_display_names_remove_route_noise() {
    assert_eq!(
        compact_model_display_name("Acme Model (regional route)"),
        "Acme Model"
    );
    assert_eq!(
        compact_model_display_name("Already compact"),
        "Already compact"
    );
    assert_eq!(
        compact_model_display_name("Qwen3.6 35B-A3B (MLX 4-bit)"),
        "Qwen3.6 35B-A3B 4-bit"
    );
    assert_eq!(
        compact_model_display_name("Acme Coder (FP8, dedicated)"),
        "Acme Coder FP8"
    );
}

#[test]
fn generated_catalog_preserves_authored_model_display_name() {
    let overlay = llm_config::parse_config_toml(
        r#"
[models."fixture/presentation-model"]
name = "Fixture Presentation Model (dedicated route)"
display_name = "Fixture Model"
provider = "openai"
context_window = 8192
"#,
    )
    .expect("overlay parses");
    let catalog = artifact_embedded(Some(&overlay), None);
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == "fixture/presentation-model")
        .expect("authored presentation model is exported");
    assert_eq!(model.name, "Fixture Presentation Model (dedicated route)");
    assert_eq!(model.display_name, "Fixture Model");
}

#[test]
fn validation_rejects_empty_model_display_name() {
    let mut catalog = artifact();
    catalog.models[0].display_name.clear();
    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("model")
                && message.contains("display_name cannot be empty")),
        "expected model display-name validation error, got {:?}",
        report.errors
    );
}

#[test]
fn downstream_bindings_project_and_decode_model_display_name() {
    assert!(typescript_declarations().contains("display_name: string"));
    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("public let displayName: String"));
    assert!(swift.contains(
        "displayName = try container.decodeIfPresent(String.self, forKey: .displayName) ?? name"
    ));
}
