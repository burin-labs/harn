use super::*;
use crate::llm_config::{self, ModelRowKind};

#[test]
fn anthropic_sonnet_4_5_selector_and_snapshot_stay_distinct() {
    let overlay = llm_config::parse_config_toml(include_str!(
        "../llm/catalog_sources/60-models/00-anthropic.toml"
    ))
    .expect("anthropic catalog fragment parses");
    let catalog = artifact_embedded(Some(&overlay), None);
    let selector = catalog
        .models
        .iter()
        .find(|model| model.id == "claude-sonnet-4-5")
        .expect("selector row");
    let snapshot = catalog
        .models
        .iter()
        .find(|model| model.id == "claude-sonnet-4-5-20250929")
        .expect("snapshot row");
    assert_eq!(selector.name, snapshot.name);
    assert_eq!(selector.display_name, snapshot.display_name);
    assert_eq!(selector.row_kind, Some(ModelRowKind::Selector));
    assert_eq!(snapshot.row_kind, Some(ModelRowKind::Snapshot));
    assert_eq!(
        selector.current_snapshot.as_deref(),
        Some("claude-sonnet-4-5-20250929")
    );
    assert_eq!(snapshot.released.as_deref(), Some("2025-09-29"));
    assert!(selector.released.is_none());
}

#[test]
fn dated_sonnet_3_5_snapshots_order_by_released() {
    let overlay = llm_config::parse_config_toml(include_str!(
        "../llm/catalog_sources/60-models/00-anthropic.toml"
    ))
    .expect("anthropic catalog fragment parses");
    let catalog = artifact_embedded(Some(&overlay), None);
    let june = catalog
        .models
        .iter()
        .find(|model| model.id == "claude-3-5-sonnet-20240620")
        .expect("june snapshot");
    let october = catalog
        .models
        .iter()
        .find(|model| model.id == "claude-3-5-sonnet-20241022")
        .expect("october snapshot");
    assert_eq!(june.row_kind, Some(ModelRowKind::Snapshot));
    assert_eq!(october.row_kind, Some(ModelRowKind::Snapshot));
    assert_eq!(june.released.as_deref(), Some("2024-06-20"));
    assert_eq!(october.released.as_deref(), Some("2024-10-22"));
    assert!(june.released.as_deref().unwrap() < october.released.as_deref().unwrap());
}

#[test]
fn overlay_can_author_released_and_row_kind() {
    let overlay = llm_config::parse_config_toml(
        r#"
[models."fixture/dated-snapshot"]
name = "Fixture Twin"
provider = "openai"
context_window = 8192
released = "2026-01-15"
row_kind = "snapshot"

[models."fixture/moving-selector"]
name = "Fixture Twin"
provider = "openai"
context_window = 8192
row_kind = "selector"
current_snapshot = "fixture/dated-snapshot"
"#,
    )
    .expect("overlay parses");
    let catalog = artifact_embedded(Some(&overlay), None);
    let snapshot = catalog
        .models
        .iter()
        .find(|model| model.id == "fixture/dated-snapshot")
        .expect("overlay snapshot");
    let selector = catalog
        .models
        .iter()
        .find(|model| model.id == "fixture/moving-selector")
        .expect("overlay selector");
    assert_eq!(snapshot.name, selector.name);
    assert_eq!(snapshot.released.as_deref(), Some("2026-01-15"));
    assert_eq!(snapshot.row_kind, Some(ModelRowKind::Snapshot));
    assert_eq!(selector.row_kind, Some(ModelRowKind::Selector));
    assert_eq!(
        selector.current_snapshot.as_deref(),
        Some("fixture/dated-snapshot")
    );
}

#[test]
fn validation_rejects_invalid_released_and_dangling_current_snapshot() {
    let mut catalog = artifact();
    catalog.models[0].released = Some("not-a-date".to_string());
    catalog.models[0].current_snapshot = Some("no-such-model".to_string());
    catalog.models[0].row_kind = Some(ModelRowKind::Snapshot);
    let report = validate_artifact(&catalog);
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("released must be an ISO 8601 date")),
        "expected released date error, got {:?}",
        report.errors
    );
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("current_snapshot references unknown model")),
        "expected dangling snapshot error, got {:?}",
        report.errors
    );
    assert!(
        report
            .errors
            .iter()
            .any(|message| message.contains("is a snapshot and cannot name a current_snapshot")),
        "expected snapshot/current_snapshot conflict, got {:?}",
        report.errors
    );
}

#[test]
fn downstream_bindings_project_released_and_row_kind() {
    let schema = schema_value();
    assert_eq!(
        schema["$defs"]["model"]["properties"]["released"]["pattern"],
        "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"
    );
    assert_eq!(
        schema["$defs"]["model"]["properties"]["row_kind"]["enum"],
        serde_json::json!(["snapshot", "selector"])
    );
    assert!(schema["$defs"]["model"]["properties"]["current_snapshot"].is_object());

    let typescript = typescript_declarations();
    assert!(typescript.contains("released?: string"));
    assert!(typescript.contains(r#"row_kind?: "snapshot" | "selector""#));
    assert!(typescript.contains("current_snapshot?: string"));

    let swift = swift_binding().expect("swift binding renders");
    assert!(swift.contains("public let released: String?"));
    assert!(swift.contains("public let rowKind: String?"));
    assert!(swift.contains("public let currentSnapshot: String?"));
    assert!(swift.contains("case released"));
    assert!(swift.contains("case rowKind = \"row_kind\""));
    assert!(swift.contains("case currentSnapshot = \"current_snapshot\""));
}
