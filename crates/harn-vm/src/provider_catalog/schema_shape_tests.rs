//! Pin the serialized field sets of the catalog's deny-unknown-fields records
//! to the schema version. The handwritten JSON Schema alone cannot detect a
//! new required Rust field that its author forgot to project.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::{artifact_embedded, schema_value, ProviderCatalogArtifact};

const ROOT_FIELDS_V12: &str = "
    aliases families generated_by models providers qc_defaults routing_routes
    schema schema_version variants
";

const MODEL_FIELDS_V12: &str = "
    aliases api_dialect architecture availability avoid_as_reviewer_for batch
    benchmarks blurb capability_tags complementary_with completion_review
    context_window current_snapshot data_controls deprecation display_name
    embedding_dim embedding_max_tokens equivalence_group family format_preferences
    id lineage local_memory logical_model modalities name open_weight operations
    performance pricing prompt_cache provider quality_tags rate_limits reasoning
    reasoning_modes released row_kind runtime_context_window served_variant
    serving_tiers stream_timeout strengths structured_output tier tool_support
    wire_model
";

fn fields(value: &Value) -> Result<BTreeSet<String>, String> {
    Ok(value
        .as_object()
        .ok_or_else(|| "expected an object while checking catalog fields".to_string())?
        .keys()
        .cloned()
        .collect())
}

fn pinned_fields(version: u64) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let (root, model) = match version {
        12 => (ROOT_FIELDS_V12, MODEL_FIELDS_V12),
        other => {
            return Err(format!(
                "catalog schema version {other} has no field-set pin"
            ))
        }
    };
    let parse = |names: &str| names.split_whitespace().map(str::to_string).collect();
    Ok((parse(root), parse(model)))
}

fn check_shape(artifact: &Value, schema: &Value) -> Result<(), String> {
    let version = artifact["schema_version"]
        .as_u64()
        .ok_or_else(|| "catalog has no numeric schema_version".to_string())?;
    let (expected_root, expected_model) = pinned_fields(version)?;
    if schema["properties"]["schema_version"]["const"] != version {
        return Err(format!(
            "schema declaration disagrees with version {version}"
        ));
    }

    for (name, properties, expected) in [
        ("root", &schema["properties"], &expected_root),
        (
            "model",
            &schema["$defs"]["model"]["properties"],
            &expected_model,
        ),
    ] {
        let actual = fields(properties)?;
        if &actual != expected {
            return Err(format!(
                "schema version {version} {name} fields changed: added {:?}, removed {:?}",
                actual.difference(expected).collect::<Vec<_>>(),
                expected.difference(&actual).collect::<Vec<_>>()
            ));
        }
    }

    let models = artifact["models"]
        .as_array()
        .ok_or_else(|| "catalog models are not an array".to_string())?;
    if models.len() < 200 {
        return Err(format!("measured only {} catalog models", models.len()));
    }
    for (name, rows, schema_record, allowed) in [
        (
            "root",
            std::slice::from_ref(artifact),
            schema,
            &expected_root,
        ),
        (
            "model",
            models.as_slice(),
            &schema["$defs"]["model"],
            &expected_model,
        ),
    ] {
        let required = schema_record["required"]
            .as_array()
            .ok_or_else(|| format!("{name} schema has no required field list"))?;
        for (index, row) in rows.iter().enumerate() {
            let serialized = fields(row)?;
            if let Some(extra) = serialized.difference(allowed).next() {
                return Err(format!(
                    "{name} row {index} serialized undeclared field {extra}"
                ));
            }
            for field in required {
                let field = field
                    .as_str()
                    .ok_or_else(|| format!("{name} schema has a non-string required field"))?;
                if !serialized.contains(field) {
                    return Err(format!("{name} row {index} omitted required field {field}"));
                }
            }
        }
    }
    Ok(())
}

#[test]
fn serialized_catalog_fields_are_pinned_to_schema_version() {
    let artifact = serde_json::to_value(artifact_embedded(None, None)).expect("serialize catalog");
    let schema = schema_value();
    check_shape(&artifact, &schema).expect("current catalog shape matches version 12");

    let mut new_rust_field = artifact.clone();
    new_rust_field["models"][0]["future_required"] = json!(true);
    let error = check_shape(&new_rust_field, &schema).unwrap_err();
    assert!(
        error.contains("undeclared field future_required"),
        "{error}"
    );

    let mut new_schema_field = schema;
    new_schema_field["$defs"]["model"]["properties"]["future_required"] =
        json!({"type": "boolean"});
    let error = check_shape(&artifact, &new_schema_field).unwrap_err();
    assert!(
        error.contains("fields changed") && error.contains("future_required"),
        "{error}"
    );

    let mut old_model = artifact;
    old_model["models"][0]
        .as_object_mut()
        .expect("serialized model")
        .remove("operations");
    let error = serde_json::from_value::<ProviderCatalogArtifact>(old_model).unwrap_err();
    assert!(error.to_string().contains("operations"), "{error}");
}
