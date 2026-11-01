use std::collections::BTreeSet;

use super::{artifact, config_from_artifact, validate_artifact};

#[test]
fn catalog_exposes_a_curated_list_of_resolvable_quick_models() {
    let catalog = serde_json::to_value(artifact()).expect("serialize catalog");
    let picks = catalog["quick_models"]
        .as_array()
        .expect("the catalog supplies an ordered quick list");
    assert!((5..=10).contains(&picks.len()), "{picks:?}");
    let mut seen = BTreeSet::new();
    for pick in picks {
        let identity = pick["id"].as_str().expect("quick model identity");
        assert!(seen.insert(identity), "duplicate quick model: {identity}");
        let routes = pick["model_ids"].as_array().expect("model routes");
        assert!(!routes.is_empty());
        for route in routes {
            assert!(
                catalog["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|model| model["id"] == *route),
                "unknown route {route} for {identity}"
            );
        }
    }
}

#[test]
fn quick_models_survive_an_artifact_round_trip() {
    let original = artifact();
    let config = config_from_artifact(&original);
    assert_eq!(config.presentation.quick_models, original.quick_models);
}

#[test]
fn validation_rejects_unknown_or_duplicate_quick_model_routes() {
    let mut catalog = artifact();
    catalog.quick_models[0]
        .model_ids
        .push("unknown-model".into());
    assert!(validate_artifact(&catalog)
        .errors
        .iter()
        .any(|error| error.contains("unknown-model")));
    catalog.quick_models[0].model_ids.pop();
    let duplicate = catalog.quick_models[0].model_ids[0].clone();
    catalog.quick_models[0].model_ids.push(duplicate);
    assert!(validate_artifact(&catalog)
        .errors
        .iter()
        .any(|error| error.contains("duplicate or unknown route")));
}
