//! Provider config, provider status, and the model catalog surfaces.

use super::super::catalog_builtins::llm_config_builtin;
use super::super::model_projection::llm_catalog_value;
use super::super::provider_projection::{llm_provider_status_value, provider_catalog_to_vm_value};
use super::fixtures::credential_status_for;
use crate::llm_config;
use crate::value::VmValue;

#[test]
fn provider_status_reports_deferred_for_platform_managed_providers() {
    // Bedrock and Vertex both declare `credential_resolution =
    // "platform_managed"` in providers.toml. They reach the same
    // "deferred" status through that shared field, not a hardcoded
    // `matches!(name, "bedrock" | "vertex")` arm in this function — a
    // third platform-managed provider would get the same treatment by
    // declaring the field, without touching this code.
    let _guard = crate::llm::env_guard();
    std::env::remove_var("VERTEX_AI_ACCESS_TOKEN");
    std::env::remove_var("GOOGLE_OAUTH_ACCESS_TOKEN");
    std::env::remove_var("GOOGLE_APPLICATION_CREDENTIALS");
    let status = llm_provider_status_value();
    assert_eq!(credential_status_for(&status, "bedrock"), "deferred");
    assert_eq!(credential_status_for(&status, "vertex"), "deferred");
}

#[test]
fn llm_config_surfaces_bedrock_region_block() {
    let mut out = String::new();
    let args = vec![VmValue::String(arcstr::ArcStr::from("bedrock"))];
    let result = llm_config_builtin(&args, &mut out).expect("bedrock config");
    let cfg = result.as_dict().expect("config dict");

    let region = cfg
        .get("region")
        .and_then(|v| v.as_dict())
        .expect("region block present for bedrock");

    // override_key documents the chain-link field a routing_policy
    // route sets to pin a region.
    assert_eq!(
        region.get("override_key").map(|v| v.display()).as_deref(),
        Some("region")
    );

    // envs lists the resolution precedence chain.
    let envs = match region.get("envs") {
        Some(VmValue::List(items)) => items.iter().map(|v| v.display()).collect::<Vec<_>>(),
        other => panic!("expected envs list, got {other:?}"),
    };
    assert!(envs.contains(&"AWS_REGION".to_string()));
    assert!(envs.contains(&"BEDROCK_REGION".to_string()));

    // resolved is always present (string or nil) so a gateway can
    // branch on "is a region already configured?".
    assert!(region.contains_key("resolved"));
}

#[test]
fn llm_config_omits_region_block_for_single_region_provider() {
    let mut out = String::new();
    let args = vec![VmValue::String(arcstr::ArcStr::from("anthropic"))];
    let result = llm_config_builtin(&args, &mut out).expect("anthropic config");
    let cfg = result.as_dict().expect("config dict");
    assert!(
        !cfg.contains_key("region"),
        "region block should only appear for multi-region providers"
    );
}

#[test]
fn test_llm_catalog_surfaces_batch_and_tool_parity_metadata_for_scripts() {
    crate::llm::capabilities::clear_user_overrides();
    let VmValue::List(entries) = llm_catalog_value() else {
        panic!("expected model catalog list");
    };
    let openai = entries
        .iter()
        .filter_map(VmValue::as_dict)
        .find(|entry| {
            entry.get("provider").map(VmValue::display) == Some("openai".to_string())
                && entry.get("id").map(VmValue::display) == Some("gpt-4o-mini".to_string())
        })
        .expect("openai gpt-4o-mini catalog row");
    assert!(openai.contains_key("tool_mode_parity"));
    assert!(openai.contains_key("tool_mode_parity_notes"));
    assert!(matches!(openai.get("batch_api"), Some(VmValue::Bool(true))));
    match openai.get("batch_wire_format") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "openai"),
        other => panic!("expected openai batch wire format, got {other:?}"),
    }
    match openai.get("batch_input_mode") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "jsonl_file"),
        other => panic!("expected jsonl_file batch input mode, got {other:?}"),
    }
    assert!(matches!(
        openai.get("batch_discount_percent"),
        Some(VmValue::Int(50))
    ));
    assert!(matches!(
        openai.get("batch_turnaround_hours"),
        Some(VmValue::Int(24))
    ));
    assert!(matches!(
        openai.get("batch_max_requests"),
        Some(VmValue::Int(50_000))
    ));
    assert!(matches!(
        openai.get("batch_max_input_bytes"),
        Some(VmValue::Int(209_715_200))
    ));
    match openai.get("batch_result_ordering") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "custom_id_rejoin"),
        other => panic!("expected openai batch result ordering, got {other:?}"),
    }
    match openai.get("batch_partial_failure") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "per_request"),
        other => panic!("expected openai batch partial failure, got {other:?}"),
    }
    match openai.get("batch_cancellation") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "supported"),
        other => panic!("expected openai batch cancellation, got {other:?}"),
    }
    match openai.get("batch_security_notes") {
        Some(VmValue::List(notes)) => assert!(
            !notes.is_empty(),
            "expected batch security notes in llm catalog row"
        ),
        other => panic!("expected batch security notes list, got {other:?}"),
    }
    match openai.get("batch_operational_notes") {
        Some(VmValue::List(notes)) => assert!(
            notes
                .iter()
                .any(|note| note.display().contains("custom_id")),
            "expected batch operational notes in llm catalog row"
        ),
        other => panic!("expected batch operational notes list, got {other:?}"),
    }
    let openai_batch = openai
        .get("batch")
        .and_then(VmValue::as_dict)
        .expect("expected nested batch catalog row");
    match openai_batch.get("result_ordering") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "custom_id_rejoin"),
        other => panic!("expected nested openai batch ordering, got {other:?}"),
    }
    match openai_batch.get("operational_notes") {
        Some(VmValue::List(notes)) => assert!(
            notes
                .iter()
                .any(|note| note.display().contains("one model")),
            "expected nested batch operational notes in llm catalog row"
        ),
        other => panic!("expected nested batch operational notes list, got {other:?}"),
    }
}

#[test]
fn provider_catalog_builtin_surfaces_presentation_families_and_effort_levels() {
    llm_config::clear_user_overrides();
    crate::llm::capabilities::clear_user_overrides();
    let catalog = provider_catalog_to_vm_value();
    let catalog = catalog.as_dict().expect("provider catalog dict");

    let families = match catalog.get("families") {
        Some(VmValue::List(families)) => families,
        other => panic!("expected families list, got {other:?}"),
    };
    let gpt_family = families
        .iter()
        .filter_map(VmValue::as_dict)
        .find(|family| family.get("id").map(VmValue::display) == Some("openai-gpt-5-6".to_string()))
        .expect("GPT-5.6 family");
    assert_eq!(
        gpt_family.get("provider").map(VmValue::display).as_deref(),
        Some("openai")
    );

    let models = match catalog.get("models") {
        Some(VmValue::List(models)) => models,
        other => panic!("expected models list, got {other:?}"),
    };
    let sol = models
        .iter()
        .filter_map(VmValue::as_dict)
        .find(|model| model.get("id").map(VmValue::display) == Some("gpt-5.6-sol".to_string()))
        .expect("GPT-5.6 Sol model");
    assert!(sol
        .get("blurb")
        .map(VmValue::display)
        .is_some_and(|blurb| !blurb.is_empty()));
    let levels = match sol.get("reasoning_effort_levels") {
        Some(VmValue::List(levels)) => levels.iter().map(VmValue::display).collect::<Vec<_>>(),
        other => panic!("expected reasoning_effort_levels list, got {other:?}"),
    };
    assert_eq!(levels, ["none", "low", "medium", "high", "xhigh", "max"]);
}
