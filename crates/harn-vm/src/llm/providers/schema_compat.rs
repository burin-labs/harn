//! Provider-facing JSON Schema compatibility transforms.
//!
//! Harn keeps the caller's original schema semantics in transcripts and local
//! validation. This module only shapes the copy that is sent to providers whose
//! strict request validators accept a narrower JSON Schema subset.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

const RECEIPT_SCHEMA: &str = "harn.llm.schema_compat_sanitization.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SchemaCompatProfile {
    OpenAiLenient,
    OpenAiStrict,
    AnthropicStrict,
    Google,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SchemaSurface {
    ToolParameters,
    StructuredOutput,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct SchemaSanitizationReceipt {
    schema: &'static str,
    provider: String,
    model: String,
    profile: &'static str,
    surface: SchemaSurface,
    path: String,
    keyword: String,
    action: &'static str,
    detail: String,
}

pub(crate) fn sanitize_schema_for_provider(
    provider: &str,
    model: &str,
    profile: SchemaCompatProfile,
    surface: SchemaSurface,
    schema: &Value,
) -> Value {
    let mut sanitizer = SchemaSanitizer {
        provider,
        model,
        profile,
        surface,
        receipts: Vec::new(),
    };
    let mut sanitized = schema.clone();
    sanitizer.sanitize_node(&mut sanitized, "#");
    sanitizer.emit_receipts();
    sanitized
}

impl SchemaCompatProfile {
    fn label(self) -> &'static str {
        match self {
            Self::OpenAiLenient => "openai_lenient",
            Self::OpenAiStrict => "openai_strict",
            Self::AnthropicStrict => "anthropic_strict",
            Self::Google => "google",
        }
    }

    fn unsupported_keywords(self) -> &'static [&'static str] {
        match self {
            Self::OpenAiLenient => &["$schema", "$id", "id", "default"],
            Self::OpenAiStrict => &[
                "$schema",
                "$id",
                "id",
                "default",
                "minimum",
                "maximum",
                "exclusiveMinimum",
                "exclusiveMaximum",
                "multipleOf",
                "minLength",
                "maxLength",
                "pattern",
                "format",
                "minItems",
                "maxItems",
                "uniqueItems",
                "minProperties",
                "maxProperties",
                "patternProperties",
                "propertyNames",
                "unevaluatedProperties",
                "unevaluatedItems",
                "contains",
                "minContains",
                "maxContains",
                "dependentRequired",
                "dependentSchemas",
                "if",
                "then",
                "else",
                "not",
                "allOf",
            ],
            Self::AnthropicStrict => &[
                "$schema",
                "$id",
                "id",
                "default",
                "minimum",
                "maximum",
                "exclusiveMinimum",
                "exclusiveMaximum",
                "multipleOf",
                "minLength",
                "maxLength",
                "pattern",
                "format",
                "minItems",
                "maxItems",
                "uniqueItems",
                "minProperties",
                "maxProperties",
                "patternProperties",
                "propertyNames",
                "unevaluatedProperties",
                "dependentRequired",
                "dependentSchemas",
                "if",
                "then",
                "else",
                "not",
                "allOf",
            ],
            Self::Google => &[
                "$schema",
                "$id",
                "id",
                "default",
                "title",
                "pattern",
                "patternProperties",
                "propertyNames",
                "unevaluatedProperties",
                "dependentRequired",
                "dependentSchemas",
                "examples",
                "readOnly",
                "writeOnly",
                "deprecated",
            ],
        }
    }

    fn additional_properties_policy(self) -> AdditionalPropertiesPolicy {
        match self {
            Self::OpenAiStrict | Self::AnthropicStrict => AdditionalPropertiesPolicy::Closed,
            Self::Google => AdditionalPropertiesPolicy::Remove,
            Self::OpenAiLenient => AdditionalPropertiesPolicy::Preserve,
        }
    }

    fn should_note_removed_keyword(self, keyword: &str) -> bool {
        !matches!(keyword, "$schema" | "$id" | "id" | "title")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdditionalPropertiesPolicy {
    Preserve,
    Closed,
    Remove,
}

struct SchemaSanitizer<'a> {
    provider: &'a str,
    model: &'a str,
    profile: SchemaCompatProfile,
    surface: SchemaSurface,
    receipts: Vec<SchemaSanitizationReceipt>,
}

impl SchemaSanitizer<'_> {
    fn sanitize_node(&mut self, value: &mut Value, path: &str) {
        match value {
            Value::Object(object) => {
                self.sanitize_object(object, path);
            }
            Value::Array(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    self.sanitize_node(item, &child_path(path, &index.to_string()));
                }
            }
            _ => {}
        }
    }

    fn sanitize_object(&mut self, object: &mut Map<String, Value>, path: &str) {
        for keyword in self.profile.unsupported_keywords() {
            if let Some(removed) = object.remove(*keyword) {
                if self.profile.should_note_removed_keyword(keyword) {
                    append_description_note(
                        object,
                        format!(
                            "Original JSON Schema `{keyword}` constraint omitted for {} provider compatibility: {}.",
                            self.profile.label(),
                            value_summary(&removed)
                        ),
                    );
                }
                self.record(
                    &child_path(path, keyword),
                    keyword,
                    "removed",
                    format!(
                        "keyword is not accepted by {} schemas",
                        self.profile.label()
                    ),
                );
            }
        }

        self.sanitize_additional_properties(object, path);
        self.sanitize_required_properties(object, path);

        if let Some(Value::Object(properties)) = object.get_mut("properties") {
            let properties_path = child_path(path, "properties");
            for (name, schema) in properties {
                self.sanitize_node(schema, &child_path(&properties_path, name));
            }
        }
        for map_keyword in ["$defs", "definitions", "patternProperties"] {
            if let Some(Value::Object(children)) = object.get_mut(map_keyword) {
                let map_path = child_path(path, map_keyword);
                for (name, schema) in children {
                    self.sanitize_node(schema, &child_path(&map_path, name));
                }
            }
        }
        for schema_keyword in [
            "items",
            "contains",
            "propertyNames",
            "additionalProperties",
            "not",
            "if",
            "then",
            "else",
            "unevaluatedProperties",
        ] {
            if let Some(schema) = object.get_mut(schema_keyword) {
                self.sanitize_node(schema, &child_path(path, schema_keyword));
            }
        }
        for array_keyword in ["anyOf", "oneOf", "allOf", "prefixItems"] {
            if let Some(Value::Array(children)) = object.get_mut(array_keyword) {
                let array_path = child_path(path, array_keyword);
                for (index, schema) in children.iter_mut().enumerate() {
                    self.sanitize_node(schema, &child_path(&array_path, &index.to_string()));
                }
            }
        }
    }

    fn sanitize_additional_properties(&mut self, object: &mut Map<String, Value>, path: &str) {
        let keyword_path = child_path(path, "additionalProperties");
        match self.profile.additional_properties_policy() {
            AdditionalPropertiesPolicy::Preserve => {}
            AdditionalPropertiesPolicy::Remove => {
                if let Some(removed) = object.remove("additionalProperties") {
                    append_description_note(
                        object,
                        format!(
                            "Original JSON Schema `additionalProperties` setting omitted for {} provider compatibility: {}.",
                            self.profile.label(),
                            value_summary(&removed)
                        ),
                    );
                    self.record(
                        &keyword_path,
                        "additionalProperties",
                        "removed",
                        format!(
                            "keyword is not accepted by {} schemas",
                            self.profile.label()
                        ),
                    );
                }
            }
            AdditionalPropertiesPolicy::Closed => {
                if !schema_is_object_like(object) {
                    return;
                }
                match object.get("additionalProperties") {
                    Some(Value::Bool(false)) => {}
                    Some(original) => {
                        let original = original.clone();
                        object.insert("additionalProperties".to_string(), Value::Bool(false));
                        append_description_note(
                            object,
                            format!(
                                "Original JSON Schema `additionalProperties` setting replaced with `false` for {} strict-schema compatibility: {}.",
                                self.profile.label(),
                                value_summary(&original)
                            ),
                        );
                        self.record(
                            &keyword_path,
                            "additionalProperties",
                            "set_false",
                            format!(
                                "strict {} schemas require closed objects",
                                self.profile.label()
                            ),
                        );
                    }
                    None => {
                        object.insert("additionalProperties".to_string(), Value::Bool(false));
                        self.record(
                            &keyword_path,
                            "additionalProperties",
                            "inserted_false",
                            format!(
                                "strict {} schemas require closed objects",
                                self.profile.label()
                            ),
                        );
                    }
                }
            }
        }
    }

    fn sanitize_required_properties(&mut self, object: &mut Map<String, Value>, path: &str) {
        if self.profile != SchemaCompatProfile::OpenAiStrict || !schema_is_object_like(object) {
            return;
        }
        let property_names = match object.get("properties") {
            Some(Value::Object(properties)) => properties.keys().cloned().collect::<Vec<_>>(),
            _ => return,
        };
        if property_names.is_empty() {
            return;
        }
        let mut existing = required_property_set(object.get("required"));
        let mut missing = Vec::new();
        for name in &property_names {
            if existing.insert(name.clone()) {
                missing.push(name.clone());
            }
        }
        if missing.is_empty() {
            return;
        }

        if let Some(Value::Object(properties)) = object.get_mut("properties") {
            for name in &missing {
                if let Some(Value::Object(property_schema)) = properties.get_mut(name) {
                    append_description_note(
                        property_schema,
                        "Originally optional in Harn; included in `required` because this provider's strict schema subset requires every object property to be present.".to_string(),
                    );
                }
            }
        }
        object.insert(
            "required".to_string(),
            Value::Array(property_names.iter().cloned().map(Value::String).collect()),
        );
        self.record(
            &child_path(path, "required"),
            "required",
            "required_all_properties",
            format!(
                "strict {} schemas require every declared property in `required`: {}",
                self.profile.label(),
                missing.join(", ")
            ),
        );
    }

    fn record(&mut self, path: &str, keyword: &str, action: &'static str, detail: String) {
        self.receipts.push(SchemaSanitizationReceipt {
            schema: RECEIPT_SCHEMA,
            provider: self.provider.to_string(),
            model: self.model.to_string(),
            profile: self.profile.label(),
            surface: self.surface,
            path: path.to_string(),
            keyword: keyword.to_string(),
            action,
            detail,
        });
    }

    fn emit_receipts(&self) {
        if self.receipts.is_empty() {
            return;
        }
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "schema".to_string(),
            Value::String(RECEIPT_SCHEMA.to_string()),
        );
        metadata.insert(
            "provider".to_string(),
            Value::String(self.provider.to_string()),
        );
        metadata.insert("model".to_string(), Value::String(self.model.to_string()));
        metadata.insert(
            "profile".to_string(),
            Value::String(self.profile.label().to_string()),
        );
        metadata.insert(
            "surface".to_string(),
            serde_json::to_value(self.surface).unwrap_or(Value::Null),
        );
        metadata.insert(
            "changes".to_string(),
            serde_json::to_value(&self.receipts).unwrap_or(Value::Null),
        );
        crate::events::log_debug_meta(
            "llm.schema_compat",
            "sanitized provider-facing JSON Schema",
            metadata,
        );
    }
}

fn append_description_note(object: &mut Map<String, Value>, note: String) {
    match object.get_mut("description") {
        Some(Value::String(description)) => {
            if description.is_empty() {
                *description = note;
            } else if !description.contains(&note) {
                description.push('\n');
                description.push_str(&note);
            }
        }
        Some(_) => {}
        None => {
            object.insert("description".to_string(), Value::String(note));
        }
    }
}

fn schema_is_object_like(object: &Map<String, Value>) -> bool {
    object.contains_key("properties") || object.get("type").is_some_and(type_includes_object)
}

fn required_property_set(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn type_includes_object(value: &Value) -> bool {
    match value {
        Value::String(kind) => kind == "object",
        Value::Array(kinds) => kinds
            .iter()
            .any(|kind| kind.as_str().is_some_and(|kind| kind == "object")),
        _ => false,
    }
}

fn value_summary(value: &Value) -> String {
    let mut text = match value {
        Value::String(value) => format!("{value:?}"),
        _ => value.to_string(),
    };
    const MAX_LEN: usize = 160;
    if text.len() > MAX_LEN {
        text.truncate(MAX_LEN);
        text.push_str("...");
    }
    text
}

fn child_path(parent: &str, child: &str) -> String {
    format!("{parent}/{}", escape_json_pointer(child))
}

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_profile_preserves_user_property_named_pattern() {
        let schema = serde_json::json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "pattern": {"type": "string"},
                "code": {
                    "type": "string",
                    "pattern": "^[A-Z]+$",
                    "default": "ABC"
                }
            }
        });

        let sanitized = sanitize_schema_for_provider(
            "gemini",
            "gemini-2.5-flash",
            SchemaCompatProfile::Google,
            SchemaSurface::ToolParameters,
            &schema,
        );

        assert_eq!(sanitized["properties"]["pattern"]["type"], "string");
        assert!(sanitized["properties"]["code"].get("pattern").is_none());
        assert!(sanitized["properties"]["code"].get("default").is_none());
        assert!(sanitized.get("additionalProperties").is_none());
    }

    #[test]
    fn strict_profile_closes_objects_and_removes_defaults() {
        let schema = serde_json::json!({
            "type": "object",
            "patternProperties": {
                "^x-": {"type": "string"}
            },
            "properties": {
                "pattern": {"type": "string"},
                "nested": {
                    "type": "object",
                    "default": {},
                    "minProperties": 1,
                    "properties": {
                        "value": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 64,
                            "pattern": "^[A-Z]+$",
                            "format": "email"
                        },
                        "count": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 10,
                            "multipleOf": 1
                        },
                        "items": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 3,
                            "uniqueItems": true,
                            "contains": {"type": "string"}
                        }
                    }
                }
            }
        });

        let sanitized = sanitize_schema_for_provider(
            "openai",
            "gpt-5.4",
            SchemaCompatProfile::OpenAiStrict,
            SchemaSurface::StructuredOutput,
            &schema,
        );

        assert_eq!(sanitized["additionalProperties"], false);
        assert_eq!(
            sanitized["required"],
            serde_json::json!(["nested", "pattern"])
        );
        assert!(sanitized.get("patternProperties").is_none());
        assert_eq!(sanitized["properties"]["pattern"]["type"], "string");
        assert_eq!(
            sanitized["properties"]["nested"]["additionalProperties"],
            false
        );
        assert_eq!(
            sanitized["properties"]["nested"]["required"],
            serde_json::json!(["count", "items", "value"])
        );
        assert!(sanitized["properties"]["nested"].get("default").is_none());
        assert!(sanitized["properties"]["nested"]
            .get("minProperties")
            .is_none());
        let value_schema = &sanitized["properties"]["nested"]["properties"]["value"];
        assert!(value_schema.get("minLength").is_none());
        assert!(value_schema.get("maxLength").is_none());
        assert!(value_schema.get("pattern").is_none());
        assert!(value_schema.get("format").is_none());
        let count_schema = &sanitized["properties"]["nested"]["properties"]["count"];
        assert!(count_schema.get("minimum").is_none());
        assert!(count_schema.get("maximum").is_none());
        assert!(count_schema.get("multipleOf").is_none());
        let items_schema = &sanitized["properties"]["nested"]["properties"]["items"];
        assert!(items_schema.get("minItems").is_none());
        assert!(items_schema.get("maxItems").is_none());
        assert!(items_schema.get("uniqueItems").is_none());
        assert!(items_schema.get("contains").is_none());
        assert!(value_schema["description"]
            .as_str()
            .expect("constraint note")
            .contains("Original JSON Schema `pattern` constraint omitted"));
    }

    #[test]
    fn strict_profile_sanitizes_defs_refs_and_combinators() {
        let schema = serde_json::json!({
            "type": "object",
            "$defs": {
                "Item": {
                    "type": "object",
                    "properties": {
                        "value": {"type": "string", "default": "x"}
                    }
                }
            },
            "properties": {
                "item": {"$ref": "#/$defs/Item"},
                "choice": {
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {"count": {"type": "integer", "default": 1}}
                        },
                        {"type": "null"}
                    ]
                }
            }
        });

        let sanitized = sanitize_schema_for_provider(
            "openai",
            "gpt-5.4",
            SchemaCompatProfile::OpenAiStrict,
            SchemaSurface::StructuredOutput,
            &schema,
        );

        let required = sanitized["required"]
            .as_array()
            .expect("root required")
            .iter()
            .filter_map(Value::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            required,
            std::collections::BTreeSet::from(["choice", "item"])
        );
        assert_eq!(sanitized["properties"]["item"]["$ref"], "#/$defs/Item");
        assert_eq!(sanitized["$defs"]["Item"]["additionalProperties"], false);
        assert_eq!(
            sanitized["$defs"]["Item"]["required"],
            serde_json::json!(["value"])
        );
        assert!(sanitized["$defs"]["Item"]["properties"]["value"]
            .get("default")
            .is_none());
        assert_eq!(
            sanitized["properties"]["choice"]["anyOf"][0]["additionalProperties"],
            false
        );
        assert_eq!(
            sanitized["properties"]["choice"]["anyOf"][0]["required"],
            serde_json::json!(["count"])
        );
        assert!(
            sanitized["properties"]["choice"]["anyOf"][0]["properties"]["count"]
                .get("default")
                .is_none()
        );
    }

    #[test]
    fn emits_structured_sanitization_receipt() {
        let sink = std::rc::Rc::new(crate::events::CollectorSink::new());
        crate::events::clear_event_sinks();
        crate::events::add_event_sink(sink.clone());

        let schema = serde_json::json!({
            "type": "object",
            "default": {},
            "properties": {"value": {"type": "string"}}
        });
        let _ = sanitize_schema_for_provider(
            "anthropic",
            "claude-sonnet-4-6",
            SchemaCompatProfile::AnthropicStrict,
            SchemaSurface::StructuredOutput,
            &schema,
        );

        let logs = sink.logs.borrow();
        let event = logs
            .iter()
            .find(|event| event.category == "llm.schema_compat")
            .expect("schema compat event");
        assert_eq!(
            event.metadata["schema"],
            Value::String(RECEIPT_SCHEMA.to_string())
        );
        assert_eq!(
            event.metadata["provider"],
            Value::String("anthropic".into())
        );
        assert!(event.metadata["changes"]
            .as_array()
            .expect("changes array")
            .iter()
            .any(|change| change["keyword"] == "default"));

        crate::events::reset_event_sinks();
    }
}
