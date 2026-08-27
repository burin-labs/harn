use super::{JsonStreamStatus, StreamSchemaValidator};
use crate::value::SchemaValidationReasonKind;

#[test]
fn nested_max_length_preserves_schema_issue_kind_detail_and_path() {
    let schema = serde_json::json!({
        "type": "array",
        "items": {
            "type": "object",
            "required": ["summary"],
            "properties": {
                "summary": {"type": "string", "maxLength": 5}
            }
        }
    });

    let mut validator = StreamSchemaValidator::from_json_schema(&schema).expect("schema");
    assert_eq!(
        validator.feed(r#"[{"summary":"too "#).clone(),
        JsonStreamStatus::Pending
    );
    let status = validator.feed(r#"long"}]"#).clone();
    match status {
        JsonStreamStatus::Invalid {
            reason_kind,
            reason,
            path,
        } => {
            assert_eq!(reason_kind, SchemaValidationReasonKind::MaxLength);
            assert_eq!(path, "[0].summary");
            assert!(
                reason.contains("longer than 5"),
                "validator detail lost the maxLength fact: {reason:?}"
            );
        }
        other => panic!("expected nested maxLength failure, got {other:?}"),
    }
}

#[test]
fn root_schema_issue_preserves_public_json_path() {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["a"],
        "properties": { "a": { "type": "integer" } },
    });
    let mut validator = StreamSchemaValidator::from_json_schema(&schema).expect("schema");
    match validator.feed("{}").clone() {
        JsonStreamStatus::Invalid {
            reason_kind, path, ..
        } => {
            assert_eq!(reason_kind, SchemaValidationReasonKind::MissingRequired);
            assert_eq!(path, "$");
        }
        other => panic!("expected missing-required failure, got {other:?}"),
    }
}
