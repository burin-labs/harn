use super::*;
use llm_config::ModelOperation;

fn decision_overlay() -> llm_config::ProvidersConfig {
    llm_config::parse_config_toml(
        r#"
[models.synthetic-decision-only]
name = "Synthetic decision only"
provider = "openrouter"
context_window = 8192
operations = ["decision"]
"#,
    )
    .expect("declared decision operation parses")
}

#[test]
fn decision_operation_survives_export_and_runtime_reload() {
    // An operation label is not a transport contract. Exercise a shipped
    // native route whose protocol, question kinds, and limits are declared.
    let id = "typesafe/jev-1.13.0";
    let contract = decision_contract_for_route("typesafe", id).expect("native contract");
    assert!(contract.protocol.is_native());
    let catalog = artifact_embedded(None, None);
    let model = catalog.models.iter().find(|model| model.id == id).unwrap();
    assert_eq!(model.operations, [ModelOperation::Decision]);
    assert_eq!(model.modalities.output, ["decision"]);
    assert!(!model.tool_support.native);
    assert!(!model.tool_support.text);
    let encoded = serde_json::to_value(&catalog).unwrap();
    let decoded: ProviderCatalogArtifact = serde_json::from_value(encoded).unwrap();
    let reloaded = config_from_artifact(&decoded);
    let model = &reloaded.models[id];
    assert!(model.supports_operation(ModelOperation::Decision));
    assert!(!model.supports_operation(ModelOperation::TextGeneration));
    assert!(!model.supports_operation(ModelOperation::Embedding));
    let reexported = artifact_embedded(Some(&reloaded), None);
    assert_eq!(
        reexported
            .models
            .iter()
            .find(|model| model.id == id)
            .unwrap()
            .operations,
        [ModelOperation::Decision]
    );
}

#[test]
fn an_unbacked_decision_label_does_not_grant_evaluator_admission() {
    let overlay = decision_overlay();
    let id = "synthetic-decision-only";
    let entry = &overlay.models[id];
    assert!(entry.supports_operation(ModelOperation::Decision));
    let caps = crate::llm::capabilities::lookup(&entry.provider, id);
    assert!(super::decision_contract::resolved_decision_contract(id, entry, &caps).is_none());
    let catalog = artifact_embedded(Some(&overlay), None);
    let row = catalog.models.iter().find(|model| model.id == id).unwrap();
    assert!(!row.operations.contains(&ModelOperation::Decision));
    let reloaded = config_from_artifact(&catalog);
    assert!(!reloaded.models[id].supports_operation(ModelOperation::Decision));
}

#[test]
fn legacy_routes_derive_decision_from_supported_structured_strategies() {
    let config = llm_config::embedded_config(None);
    let catalog = artifact_embedded(None, None);
    assert!(
        catalog.models.len() > 200,
        "exercise the real registry, not an empty fixture"
    );
    let mut embedding_count = 0;
    let mut declared_count = 0;
    let mut derived_count = 0;
    for model in &catalog.models {
        let legacy = &config.models[&model.id];
        if legacy.operations.is_some() {
            declared_count += 1;
        }
        if legacy.embedding_dim.is_some() {
            embedding_count += 1;
        }
        let caps = crate::llm::capabilities::lookup(&legacy.provider, &model.id);
        assert_eq!(
            model.operations,
            super::decision_contract::resolved_operations(&model.id, legacy, &caps),
            "{}",
            model.id
        );
        if model.operations.contains(&ModelOperation::Decision)
            && !legacy.supports_operation(ModelOperation::Decision)
        {
            derived_count += 1;
            assert!(model.operations.contains(&ModelOperation::TextGeneration));
            assert_ne!(
                caps.structured_output_strategy,
                crate::llm::capabilities::StructuredOutputStrategy::Unsupported,
                "{}",
                model.id
            );
        }
    }
    assert!(embedding_count > 0, "known non-text route must be measured");
    assert!(derived_count > 0, "structured routes must reach the census");
    assert!(
        declared_count >= 8,
        "measured only {declared_count} rows declaring an operation set; the \
         decision rows must be reaching this census"
    );
}

#[test]
fn unknown_operations_are_refused_in_source_and_artifact() {
    assert!(llm_config::parse_config_toml(
        r#"
[models.invalid]
name = "Invalid operation"
provider = "openrouter"
context_window = 8192
operations = ["decison"]
"#
    )
    .is_err());
    let mut encoded = serde_json::to_value(artifact_embedded(None, None)).unwrap();
    encoded["models"][0]["operations"] = serde_json::json!(["decison"]);
    assert!(serde_json::from_value::<ProviderCatalogArtifact>(encoded).is_err());
}

#[test]
fn operation_patches_preserve_the_contract() {
    let overlay = llm_config::parse_config_toml_with_diagnostics(
        r#"
[patch.models."gpt-5.6-sol"]
operations = ["text_generation", "decision"]
"#,
    )
    .unwrap();
    assert!(overlay.diagnostics.is_empty(), "{:?}", overlay.diagnostics);
    let artifact = artifact_embedded(Some(&overlay.config), None);
    let model = artifact
        .models
        .iter()
        .find(|model| model.id == "gpt-5.6-sol")
        .unwrap();
    assert_eq!(
        model.operations,
        [ModelOperation::TextGeneration, ModelOperation::Decision]
    );
    assert!(model.tool_support.native || model.tool_support.text);
}

#[test]
fn contradictory_legacy_embedding_metadata_is_refused() {
    let mut overlay = decision_overlay();
    overlay
        .models
        .get_mut("synthetic-decision-only")
        .unwrap()
        .embedding_dim = Some(1536);
    let report = validate_artifact(&artifact_embedded(Some(&overlay), None));
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("synthetic-decision-only")
                && error.contains("embedding operation")),
        "{:?}",
        report.errors
    );
}
