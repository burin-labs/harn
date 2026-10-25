use super::*;
use serde_json::json;

fn config() -> toml::Value {
    toml::from_str(r#"[models.demo]
provider = "private"
models_dev = { provider = "upstream", model = "exact-id", fields = ["name", "context_window", "pricing"] }
"#).unwrap()
}

fn upstream() -> Value {
    json!({"upstream": {"models": {"exact-id": {
        "name": "A model", "limit": {"context": 128000},
        "cost": {"text": {"input": {"price": 1.0, "cache": {"read": 0.1, "write": {"5m": 1.25, "1h": 2.0}}}, "output": {"price": 5.0}}}
    }}}})
}

#[test]
fn captures_exact_routes_and_resolves_selected_facts_without_runtime_policy() {
    let mut config = config();
    let snapshot = Snapshot::capture(&upstream(), &config, "a".repeat(64)).unwrap();
    let encoded = serde_json::to_string(&snapshot).unwrap();
    let snapshot = Snapshot::parse(&encoded).unwrap();
    assert_eq!(snapshot.apply(&mut config).unwrap(), 3);
    let model = &config["models"]["demo"];
    assert_eq!(model["provider"].as_str(), Some("private"));
    assert_eq!(model["name"].as_str(), Some("A model"));
    assert_eq!(model["context_window"].as_integer(), Some(128000));
    assert_eq!(
        model["pricing"]["cache_write_1h_per_mtok"].as_float(),
        Some(2.0)
    );
    assert!(model.get("models_dev").is_none());
    assert!(model.get("capabilities").is_none());
}

#[test]
fn refuses_invented_routes_and_disappearing_upstream_facts() {
    let config = config();
    let mut missing = upstream();
    missing["upstream"]["models"] = json!({"exact-id-neighbour": {"name": "wrong"}});
    assert!(Snapshot::capture(&missing, &config, "a".repeat(64))
        .unwrap_err()
        .contains("required by demo is missing"));
    let mut missing = upstream();
    missing["upstream"]["models"]["exact-id"]["limit"] = json!({});
    assert!(Snapshot::capture(&missing, &config, "a".repeat(64))
        .unwrap_err()
        .contains("positive integer"));
}

#[test]
fn refuses_double_ownership_and_unknown_source_fields() {
    let mut config = config();
    config["models"]["demo"]
        .as_table_mut()
        .unwrap()
        .insert("context_window".to_string(), toml::Value::Integer(128000));
    assert!(Snapshot::capture(&upstream(), &config, "a".repeat(64))
        .unwrap_err()
        .contains("both locally"));
    config["models"]["demo"]
        .as_table_mut()
        .unwrap()
        .remove("context_window");
    config["models"]["demo"]["models_dev"]["fields"] =
        toml::Value::Array(vec![toml::Value::String("native_tools".into())]);
    assert!(has_bindings(&config)
        .unwrap_err()
        .contains("invalid models_dev binding"));
}

#[test]
fn refuses_conditional_prices_and_unknown_cost_semantics_instead_of_flattening() {
    for extra in ["multipliers", "audio", "web_search", "new_cost_rule"] {
        let mut upstream = upstream();
        upstream["upstream"]["models"]["exact-id"]["cost"][extra] = json!([]);
        let error = Snapshot::capture(&upstream, &config(), "a".repeat(64)).unwrap_err();
        assert!(error.contains("explicit Harn card"), "{extra}: {error}");
    }
    let mut upstream = upstream();
    upstream["upstream"]["models"]["exact-id"]["cost"]["text"]["input"]["overrides"] = json!([
        {"when": {"context": {"gt": 100000}}, "price": 2.0}
    ]);
    assert!(Snapshot::capture(&upstream, &config(), "a".repeat(64))
        .unwrap_err()
        .contains("explicit Harn card"));
}

#[test]
fn accepts_generic_cache_writes_but_refuses_distinct_reasoning_rates() {
    let mut upstream = upstream();
    upstream["upstream"]["models"]["exact-id"]["cost"]["text"]["input"]["cache"]["write"] =
        json!(1.25);
    let snapshot = Snapshot::capture(&upstream, &config(), "a".repeat(64)).unwrap();
    let mut normalized = config();
    snapshot.apply(&mut normalized).unwrap();
    assert_eq!(
        normalized["models"]["demo"]["pricing"]["cache_write_per_mtok"].as_float(),
        Some(1.25)
    );
    assert!(normalized["models"]["demo"]["pricing"]
        .get("cache_write_1h_per_mtok")
        .is_none());
    upstream["upstream"]["models"]["exact-id"]["cost"]["text"]["output"]["reasoning"] = json!(10.0);
    assert!(Snapshot::capture(&upstream, &config(), "a".repeat(64))
        .unwrap_err()
        .contains("reasoning rates"));
}

#[test]
fn refuses_unsupported_snapshot_versions() {
    let snapshot = Snapshot::capture(&upstream(), &config(), "a".repeat(64)).unwrap();
    let mut value = serde_json::to_value(snapshot).unwrap();
    value["schema"] = json!("harn.models-dev.v999");
    assert!(Snapshot::parse(&value.to_string())
        .unwrap_err()
        .contains("unsupported"));
}
