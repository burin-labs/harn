use super::*;

#[test]
fn decision_operations_override_contradictory_tool_defaults() {
    let overlay = toml::from_str(
        r#"
[[provider.openrouter]]
model_match = "openrouter/cloudflare/clef"
extends = true
native_tools = true
text_tool_wire_format_supported = true
tool_search = ["hosted"]
"#,
    )
    .expect("contradictory capability fixture");
    let config = llm_config::embedded_config(None);
    let view = CatalogView::new(&config, crate::capabilities::builtin_file(), Some(&overlay));
    let declared = view
        .capabilities("openrouter", "openrouter/cloudflare/clef")
        .capabilities();
    assert!(declared.native_tools && declared.text_tool_wire_format_supported);
    assert_eq!(declared.tool_search, vec!["hosted"]);
    let catalog = view.artifact();
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == "openrouter/cloudflare/clef")
        .expect("decision-only fixture is projected");
    assert_eq!(model.operations, vec![llm_config::ModelOperation::Decision]);
    assert!(!model.tool_support.native && !model.tool_support.text);
    assert!(model.tool_support.tool_search.is_empty());
    assert!(!model
        .capability_tags
        .iter()
        .any(|tag| tag == "tools" || tag == "tool_search"));
}

#[test]
fn capability_tags_follow_normalized_tool_support() {
    let catalog = artifact();
    assert!(!catalog.models.is_empty());
    for id in [
        "openrouter/cloudflare/clef",
        "openrouter/cloudflare/clef-flash",
    ] {
        let model = catalog
            .models
            .iter()
            .find(|model| model.provider == "openrouter" && model.id == id)
            .expect("decision-only route is in the generated catalog");
        assert_eq!(model.operations, vec![llm_config::ModelOperation::Decision]);
        assert!(!model.tool_support.native && !model.tool_support.text);
    }
    assert!(catalog.models.iter().any(|model| model.tool_support.native));
    assert!(catalog.models.iter().any(|model| model.tool_support.text));
    assert!(catalog
        .models
        .iter()
        .any(|model| !model.tool_support.tool_search.is_empty()));
    let contradictions: Vec<_> = catalog
        .models
        .iter()
        .filter(|model| {
            let has_tools = model.capability_tags.iter().any(|tag| tag == "tools");
            let has_search = model.capability_tags.iter().any(|tag| tag == "tool_search");
            let supports_tools = model.tool_support.native || model.tool_support.text;
            let supports_search = !model.tool_support.tool_search.is_empty();
            has_tools != supports_tools || has_search != supports_search
        })
        .map(|model| format!("{}:{}", model.provider, model.id))
        .collect();
    assert!(
        contradictions.is_empty(),
        "contradictory routes: {contradictions:?}"
    );

    let downstream = config_from_artifact(&catalog);
    for model in &catalog.models {
        assert_eq!(
            downstream.models[&model.id]
                .capabilities
                .iter()
                .any(|tag| tag == "tools"),
            model.tool_support.native || model.tool_support.text,
            "consumer eligibility for {}:{}",
            model.provider,
            model.id
        );
        let route = catalog.routing_routes.iter().find(|route| {
            route.provider == model.provider
                && route.model == model.wire_model.as_deref().unwrap_or(&model.id)
        });
        if model.deprecation.status == DeprecationStatus::Active {
            let route = route.expect("active model has a routing projection");
            assert_eq!(
                route.capabilities.iter().any(|tag| tag == "tools"),
                model.tool_support.native || model.tool_support.text,
                "routing eligibility for {}:{}",
                model.provider,
                model.id
            );
        }
    }
}

#[test]
fn validation_rejects_tool_tags_that_disagree_with_support() {
    let catalog = artifact();
    assert!(validate_artifact(&catalog).errors.is_empty());
    for tag in ["tools", "tool_search"] {
        for present in [false, true] {
            let mut contradictory = catalog.clone();
            let model = contradictory
                .models
                .iter_mut()
                .find(|model| model.capability_tags.iter().any(|value| value == tag) != present)
                .expect("catalog contains both supporting and unsupported controls");
            let id = model.id.clone();
            if present {
                model.capability_tags.push(tag.to_string());
            } else {
                model.capability_tags.retain(|value| value != tag);
            }
            let errors = validate_artifact(&contradictory).errors;
            let diagnostic = format!("model {id} capability_tags {tag} contradicts tool_support");
            assert!(
                errors.contains(&diagnostic),
                "missing {diagnostic}: {errors:?}"
            );
        }
    }
}
