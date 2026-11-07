use super::super::{capability_projection, model_projection, provider_projection};
use crate::llm::capabilities::Capabilities;
use crate::llm_config;
use crate::value::{DictMap, VmValue};

#[test]
fn provider_projection_preserves_omission_empty_strings_and_native_numbers() {
    let mut provider = llm_config::ProviderDef::default();
    let empty = provider_projection::provider_def_to_vm_value(None, &provider);
    let fields = empty.as_dict().expect("provider record");
    assert!(matches!(fields.get("base_url"), Some(VmValue::String(value)) if value.is_empty()));
    assert!(matches!(fields.get("auth_envs"), Some(VmValue::List(value)) if value.is_empty()));
    for key in [
        "icon",
        "args",
        "env",
        "features",
        "cost_per_1k_in",
        "auth_style_explicit",
    ] {
        assert!(!fields.contains_key(key), "{key}");
    }
    provider.icon = Some(String::new());
    provider.args = vec![String::new()];
    provider.cost_per_1k_in = Some(f64::NAN);
    provider.latency_p50_ms = Some(u64::MAX);
    let populated = provider_projection::provider_def_to_vm_value(None, &provider);
    let fields = populated.as_dict().expect("provider record");
    assert!(matches!(fields.get("icon"), Some(VmValue::String(value)) if value.is_empty()));
    assert!(matches!(fields.get("args"), Some(VmValue::List(value)) if value.len() == 1));
    assert!(matches!(fields.get("cost_per_1k_in"), Some(VmValue::Float(value)) if value.is_nan()));
    assert!(matches!(
        fields.get("latency_p50_ms"),
        Some(VmValue::Int(-1))
    ));
}

#[test]
fn model_projection_retains_nulls_empty_collections_and_numeric_edges() {
    let mut model: llm_config::ModelDef = serde_json::from_value(serde_json::json!({
        "name": "projection fixture", "provider": "projection-fixture", "context_window": 1,
        "operations": [], "display_name": "not exposed",
        "completion_review": {"scrutiny": "standard", "evidence": "fixture"},
    }))
    .expect("model fixture");
    model.context_window = u64::MAX;
    model.runtime_context_window = Some(u64::MAX);
    model.stream_timeout = Some(f64::NAN);
    model.benchmarks.insert("nan".into(), f64::NAN);
    model.rate_limits = Some(llm_config::RateLimitsDef::default());
    model.performance = Some(llm_config::ServingPerformanceDef::default());
    model.architecture = Some(llm_config::ModelArchitectureDef::default());
    let result = model_projection::model_def_to_vm_value(
        "fixture",
        &model,
        model_projection::PricingView::Stored,
    );
    let fields = result.as_dict().expect("model record");
    assert!(!fields.contains_key("display_name"));
    for key in [
        "blurb",
        "rate_limits",
        "performance",
        "architecture",
        "pricing",
    ] {
        assert!(matches!(fields.get(key), Some(VmValue::Nil)), "{key}");
    }
    for key in ["operations", "capabilities", "strengths", "serving_tiers"] {
        assert!(
            matches!(fields.get(key), Some(VmValue::List(items)) if items.is_empty()),
            "{key}"
        );
    }
    for key in ["context_window", "runtime_context_window"] {
        assert!(matches!(fields.get(key), Some(VmValue::Int(-1))), "{key}");
    }
    assert!(matches!(fields.get("stream_timeout"), Some(VmValue::Float(value)) if value.is_nan()));
    assert!(
        matches!(fields["benchmarks"].as_dict().expect("benchmarks").get("nan"), Some(VmValue::Float(value)) if value.is_nan())
    );
    assert!(matches!(
        fields["completion_review"]
            .as_dict()
            .expect("review")
            .get("max_judge_calls"),
        Some(VmValue::Nil)
    ));
}

#[test]
fn batch_projection_preserves_zero_and_masks_overflow_and_unsupported_metadata() {
    let mut caps = Capabilities::default();
    caps.batch_wire_format = Some("wire".into());
    caps.batch_input_mode = Some("input".into());
    caps.batch_discount_percent = Some(0);
    caps.batch_max_requests = Some(u64::MAX);
    caps.batch_max_input_bytes = Some(i64::MAX as u64);
    caps.batch_partial_failure = Some(String::new());
    caps.batch_regions = vec!["region".into()];
    for enabled in [false, true] {
        let mut fields = DictMap::new();
        capability_projection::insert_batch_support_fields(&mut fields, enabled, &caps);
        assert!(matches!(fields.get("batch_api"), Some(VmValue::Bool(value)) if *value == enabled));
        assert!(matches!(
            fields.get("batch_max_requests"),
            Some(VmValue::Nil)
        ));
        assert!(matches!(
            fields.get("batch_security_notes"),
            Some(VmValue::Nil)
        ));
        if enabled {
            assert!(matches!(
                fields.get("batch_discount_percent"),
                Some(VmValue::Int(0))
            ));
            assert!(matches!(
                fields.get("batch_max_input_bytes"),
                Some(VmValue::Int(i64::MAX))
            ));
            let nested = fields["batch"].as_dict().expect("supported batch record");
            for key in [
                "wire_format",
                "input_mode",
                "regions",
                "partial_failure",
                "discount_percent",
            ] {
                assert_eq!(
                    nested.get(key),
                    fields.get(format!("batch_{key}").as_str()),
                    "{key}"
                );
            }
        } else {
            assert!(fields
                .iter()
                .filter(|(key, _)| key.as_str() != "batch_api")
                .all(|(_, value)| matches!(value, VmValue::Nil)));
        }
    }
}
