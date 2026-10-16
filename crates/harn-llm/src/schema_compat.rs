//! Provider-facing JSON Schema compatibility transforms.
//!
//! Harn keeps the caller's original schema in transcripts and retry guidance.
//! It projects one provider-compatible schema for both the wire request and
//! local response validation, so Harn never rejects a response for a rule the
//! provider did not receive.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

const RECEIPT_SCHEMA: &str = "harn.llm.schema_compat_sanitization.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaCompatProfile {
    OpenAiLenient,
    OpenAiStrict,
    AnthropicStrict,
    Google,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaSurface {
    ToolParameters,
    StructuredOutput,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SchemaSanitizationReceipt {
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

#[cfg(test)]
fn sanitize_schema_for_provider(
    provider: &str,
    model: &str,
    profile: SchemaCompatProfile,
    surface: SchemaSurface,
    schema: &Value,
) -> Value {
    sanitize_schema(provider, model, profile, surface, schema).schema
}

/// Project a structured-output schema onto the dialect selected for one route.
///
/// The result is the effective contract for that call: request builders send
/// it and response validators enforce it. The original schema remains on
/// the host's call options for transcript and corrective-nudge context.
pub fn project_output_schema(
    provider: &str,
    model: &str,
    dialect: harn_provider_catalog::capabilities::WireDialect,
    strict: bool,
    schema: &Value,
) -> SchemaProjection {
    use harn_provider_catalog::capabilities::WireDialect;

    let profile = match dialect {
        WireDialect::Anthropic => Some(SchemaCompatProfile::AnthropicStrict),
        WireDialect::Gemini => Some(SchemaCompatProfile::Google),
        WireDialect::Ollama => None,
        WireDialect::OpenAiCompat => Some(if strict {
            SchemaCompatProfile::OpenAiStrict
        } else {
            SchemaCompatProfile::OpenAiLenient
        }),
    };
    match profile {
        Some(profile) => sanitize_schema(
            provider,
            model,
            profile,
            SchemaSurface::StructuredOutput,
            schema,
        ),
        None => SchemaProjection {
            schema: schema.clone(),
            receipts: Vec::new(),
        },
    }
}

/// A provider-facing contract and the changes needed to produce it.
#[derive(Clone, Debug)]
pub struct SchemaProjection {
    pub schema: Value,
    pub receipts: Vec<SchemaSanitizationReceipt>,
}

/// Normalize once for both wire serialization and local response validation.
pub fn sanitize_schema(
    provider: &str,
    model: &str,
    profile: SchemaCompatProfile,
    surface: SchemaSurface,
    schema: &Value,
) -> SchemaProjection {
    let mut sanitizer = SchemaSanitizer {
        provider,
        model,
        profile,
        surface,
        receipts: Vec::new(),
    };
    let mut sanitized = schema.clone();
    sanitizer.sanitize_node(&mut sanitized, "#");
    SchemaProjection {
        schema: sanitized,
        receipts: sanitizer.receipts,
    }
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
                "const",
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
                "oneOf",
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
        self.rewrite_openai_strict_discriminated_union(object, path);
        self.rewrite_openai_strict_const(object, path);

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

    fn rewrite_openai_strict_discriminated_union(
        &mut self,
        object: &mut Map<String, Value>,
        path: &str,
    ) {
        if self.profile != SchemaCompatProfile::OpenAiStrict
            || path == "#"
            || object.contains_key("anyOf")
        {
            return;
        }
        let Some(discriminator) = object
            .get("oneOf")
            .and_then(Value::as_array)
            .and_then(|branches| closed_union_discriminator(branches))
        else {
            return;
        };
        let Some(branches) = object.remove("oneOf") else {
            return;
        };
        object.insert("anyOf".to_string(), branches);
        self.record(
            &child_path(path, "oneOf"),
            "oneOf",
            "rewritten_to_any_of",
            format!(
                "closed branches with distinct `{discriminator}` literals preserve one-of semantics as anyOf in {} schemas",
                self.profile.label()
            ),
        );
    }

    fn rewrite_openai_strict_const(&mut self, object: &mut Map<String, Value>, path: &str) {
        if self.profile != SchemaCompatProfile::OpenAiStrict {
            return;
        }
        let Some(value) = object.remove("const") else {
            return;
        };
        let (values, action, detail) = match object.remove("enum") {
            None => (
                vec![value],
                "rewritten_to_single_value_enum",
                "single-value enum preserves const semantics in openai_strict schemas",
            ),
            Some(Value::Array(values))
                if value.as_str().is_some_and(|constant| {
                    values
                        .iter()
                        .any(|candidate| candidate.as_str() == Some(constant))
                }) => (
                vec![value],
                "intersected_with_const",
                "single-value enum preserves the string const and enum intersection in openai_strict schemas",
            ),
            Some(_) => (
                Vec::new(),
                "rewritten_to_empty_enum",
                "empty enum fails closed when const and enum have no safely supported intersection in openai_strict schemas",
            ),
        };
        object.insert("enum".to_string(), Value::Array(values));
        self.record(
            &child_path(path, "const"),
            "const",
            action,
            detail.to_string(),
        );
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

        // Strict mode has no way to leave a property out. For tool parameters,
        // optionality moves into the value: an originally optional property
        // also accepts `null`, and tool dispatch reads that `null` back as
        // absent (`llm::tools::compat::drop_unused_optional_args`). Without
        // this the model must invent a value (`""`, `0`, `false`) that the
        // tool then executes as if it were meant. Structured output keeps the
        // plain note: completed responses are validated against the caller's
        // original schema, which has no inverse step to turn `null` back into
        // absence.
        if let Some(Value::Object(properties)) = object.get_mut("properties") {
            for name in &missing {
                match properties.get_mut(name) {
                    Some(property_schema) if self.surface == SchemaSurface::ToolParameters => {
                        admit_null_for_unused_optional(property_schema);
                    }
                    Some(Value::Object(property_schema)) => append_description_note(
                        property_schema,
                        "Originally optional in Harn; included in `required` because this provider's strict schema subset requires every object property to be present.".to_string(),
                    ),
                    _ => {}
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
}

impl SchemaProjection {
    /// Existing audit-event metadata, absent when no transformation occurred.
    pub fn audit_metadata(&self) -> Option<BTreeMap<String, Value>> {
        let first = self.receipts.first()?;
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "schema".to_string(),
            Value::String(RECEIPT_SCHEMA.to_string()),
        );
        metadata.insert(
            "provider".to_string(),
            Value::String(first.provider.clone()),
        );
        metadata.insert("model".to_string(), Value::String(first.model.clone()));
        metadata.insert(
            "profile".to_string(),
            Value::String(first.profile.to_string()),
        );
        metadata.insert(
            "surface".to_string(),
            serde_json::to_value(first.surface).unwrap_or(Value::Null),
        );
        metadata.insert(
            "changes".to_string(),
            serde_json::to_value(&self.receipts).unwrap_or(Value::Null),
        );
        Some(metadata)
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

const UNUSED_OPTIONAL_NOTE: &str = "Optional in Harn: send null when not used. Listed in `required` only because this provider's strict schema subset requires every object property to be present.";

/// Let an originally optional property accept `null` so a strict-mode model
/// can say "not used" without inventing a value.
///
/// A plain scalar or array `type` widens in place (`"string"` →
/// `["string", "null"]`), the spelling models follow most reliably. An object,
/// or anything carrying a value set or a combinator, is wrapped as
/// `anyOf: [original, {"type": "null"}]` instead, so the object is still
/// closed in its own branch and later rewrites of that schema (`const` →
/// `enum`) cannot silently drop the `null` again.
fn admit_null_for_unused_optional(schema: &mut Value) {
    let Value::Object(object) = schema else {
        return;
    };
    if schema_admits_null(object) {
        append_description_note(object, UNUSED_OPTIONAL_NOTE.to_string());
        return;
    }
    let has_value_set_or_combinator = ["enum", "const", "anyOf", "oneOf", "allOf", "$ref"]
        .iter()
        .any(|keyword| object.contains_key(*keyword))
        || schema_is_object_like(object);
    match object.get_mut("type") {
        Some(Value::String(kind)) if !has_value_set_or_combinator => {
            let kind = std::mem::take(kind);
            object.insert(
                "type".to_string(),
                Value::Array(vec![Value::String(kind), Value::String("null".to_string())]),
            );
            append_description_note(object, UNUSED_OPTIONAL_NOTE.to_string());
        }
        Some(Value::Array(kinds)) if !has_value_set_or_combinator => {
            kinds.push(Value::String("null".to_string()));
            append_description_note(object, UNUSED_OPTIONAL_NOTE.to_string());
        }
        _ => {
            let description = object.remove("description");
            let original = Value::Object(std::mem::take(object));
            object.insert(
                "anyOf".to_string(),
                Value::Array(vec![original, serde_json::json!({"type": "null"})]),
            );
            if let Some(description) = description {
                object.insert("description".to_string(), description);
            }
            append_description_note(object, UNUSED_OPTIONAL_NOTE.to_string());
        }
    }
}

fn schema_admits_null(object: &Map<String, Value>) -> bool {
    let type_admits_null = match object.get("type") {
        Some(Value::String(kind)) => kind == "null",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind.as_str() == Some("null")),
        _ => false,
    };
    // Only `anyOf` survives the strict subset; a `null` branch under `oneOf`
    // is removed along with the keyword, so it cannot count here.
    let union_admits_null = object
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|branches| {
            branches
                .iter()
                .filter_map(Value::as_object)
                .any(schema_admits_null)
        });
    type_admits_null || union_admits_null
}

fn schema_is_object_like(object: &Map<String, Value>) -> bool {
    object.contains_key("properties") || object.get("type").is_some_and(type_includes_object)
}

fn closed_union_discriminator(branches: &[Value]) -> Option<String> {
    if branches.len() < 2 {
        return None;
    }
    let first_properties = branches
        .first()?
        .as_object()?
        .get("properties")?
        .as_object()?;
    for name in first_properties.keys() {
        let Some(values) = branches
            .iter()
            .map(|branch| closed_object_discriminator_value(branch, name))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let distinct = values.iter().copied().collect::<BTreeSet<_>>();
        if distinct.len() == branches.len() {
            return Some(name.clone());
        }
    }
    None
}

fn closed_object_discriminator_value<'a>(branch: &'a Value, name: &str) -> Option<&'a str> {
    let object = branch.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("object")
        || object.get("additionalProperties").and_then(Value::as_bool) != Some(false)
        || !object
            .get("required")?
            .as_array()?
            .iter()
            .any(|required| required.as_str() == Some(name))
    {
        return None;
    }
    object
        .get("properties")?
        .as_object()?
        .get(name)?
        .as_object()?
        .get("const")?
        .as_str()
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
    let text = match value {
        Value::String(value) => format!("{value:?}"),
        _ => value.to_string(),
    };
    const MAX_LEN: usize = 160;
    harn_vm_core::text::truncate::truncate_end_bytes(&text, MAX_LEN)
}

fn child_path(parent: &str, child: &str) -> String {
    format!("{parent}/{}", escape_json_pointer(child))
}

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn value_summary_truncates_multibyte_text_on_a_char_boundary() {
        // The leading quote puts every `é` on an odd byte, so byte 160 lands
        // mid-character; a raw byte truncate panicked here.
        let summary = value_summary(&Value::String("é".repeat(120)));
        assert!(summary.len() <= 160, "{summary}");
        assert!(summary.ends_with('…'), "{summary}");
    }

    use super::test_support::closed_discriminated_union_schema;

    fn openai_receipt_change(path: &str, keyword: &str, action: &str, detail: &str) -> Value {
        serde_json::json!({
            "schema": RECEIPT_SCHEMA,
            "provider": "openai",
            "model": "gpt-5.4",
            "profile": "openai_strict",
            "surface": "structured_output",
            "path": path,
            "keyword": keyword,
            "action": action,
            "detail": detail
        })
    }

    fn sanitize_openai_with_changes(schema: &Value) -> (Value, Value) {
        let mut sanitizer = SchemaSanitizer {
            provider: "openai",
            model: "gpt-5.4",
            profile: SchemaCompatProfile::OpenAiStrict,
            surface: SchemaSurface::StructuredOutput,
            receipts: Vec::new(),
        };
        let mut sanitized = schema.clone();
        sanitizer.sanitize_node(&mut sanitized, "#");
        let changes = serde_json::to_value(sanitizer.receipts).expect("serialize schema changes");
        (sanitized, changes)
    }

    fn omitted_one_of_note(branches: &Value) -> String {
        format!(
            "Original JSON Schema `oneOf` constraint omitted for openai_strict provider compatibility: {}.",
            value_summary(branches)
        )
    }

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
                        "variant": {
                            "oneOf": [
                                {"type": "string"},
                                {"type": "integer"}
                            ]
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
            serde_json::json!(["count", "items", "value", "variant"])
        );
        assert!(sanitized["properties"]["nested"].get("default").is_none());
        assert!(sanitized["properties"]["nested"]
            .get("minProperties")
            .is_none());
        let variant_schema = &sanitized["properties"]["nested"]["properties"]["variant"];
        assert!(variant_schema.get("oneOf").is_none());
        assert!(variant_schema["description"]
            .as_str()
            .expect("oneOf note")
            .contains("Original JSON Schema `oneOf` constraint omitted"));
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
    fn openai_strict_rewrites_closed_discriminated_union_without_losing_branches() {
        let schema = closed_discriminated_union_schema("oneOf", "const");
        let sanitized = sanitize_schema_for_provider(
            "openai",
            "gpt-5.4",
            SchemaCompatProfile::OpenAiStrict,
            SchemaSurface::StructuredOutput,
            &schema,
        );

        assert_eq!(
            sanitized,
            closed_discriminated_union_schema("anyOf", "enum")
        );
    }

    #[test]
    fn openai_strict_does_not_rewrite_overlapping_discriminator_branches() {
        let mut schema = closed_discriminated_union_schema("oneOf", "const");
        schema["properties"]["source"]["oneOf"][1]["properties"]["kind"]["const"] =
            Value::String("cron".to_string());
        let note = omitted_one_of_note(&schema["properties"]["source"]["oneOf"]);

        let (sanitized, changes) = sanitize_openai_with_changes(&schema);
        let mut expected = schema;
        expected["properties"]["schema_version"] = serde_json::json!({"enum": ["1"]});
        expected["properties"]["source"] = serde_json::json!({"description": note});

        assert_eq!(sanitized, expected);
        assert_eq!(
            changes,
            Value::Array(vec![
                openai_receipt_change(
                    "#/properties/schema_version/const",
                    "const",
                    "rewritten_to_single_value_enum",
                    "single-value enum preserves const semantics in openai_strict schemas",
                ),
                openai_receipt_change(
                    "#/properties/source/oneOf",
                    "oneOf",
                    "removed",
                    "keyword is not accepted by openai_strict schemas",
                ),
            ])
        );
    }

    #[test]
    fn openai_strict_does_not_rewrite_json_schema_equal_numeric_discriminators() {
        let mut schema = closed_discriminated_union_schema("oneOf", "const");
        schema["properties"]["source"]["oneOf"][0]["properties"]["kind"]["const"] =
            serde_json::json!(1);
        schema["properties"]["source"]["oneOf"][1]["properties"]["kind"]["const"] =
            serde_json::json!(1.0);
        let note = omitted_one_of_note(&schema["properties"]["source"]["oneOf"]);

        let (sanitized, changes) = sanitize_openai_with_changes(&schema);
        let mut expected = schema;
        expected["properties"]["schema_version"] = serde_json::json!({"enum": ["1"]});
        expected["properties"]["source"] = serde_json::json!({"description": note});

        assert_eq!(sanitized, expected);
        assert_eq!(
            changes,
            Value::Array(vec![
                openai_receipt_change(
                    "#/properties/schema_version/const",
                    "const",
                    "rewritten_to_single_value_enum",
                    "single-value enum preserves const semantics in openai_strict schemas",
                ),
                openai_receipt_change(
                    "#/properties/source/oneOf",
                    "oneOf",
                    "removed",
                    "keyword is not accepted by openai_strict schemas",
                ),
            ])
        );
    }

    #[test]
    fn openai_strict_does_not_rewrite_root_discriminated_union() {
        let schema =
            closed_discriminated_union_schema("oneOf", "const")["properties"]["source"].clone();
        let note = omitted_one_of_note(&schema["oneOf"]);

        let (sanitized, changes) = sanitize_openai_with_changes(&schema);

        assert_eq!(sanitized, serde_json::json!({"description": note}));
        assert_eq!(
            changes,
            Value::Array(vec![openai_receipt_change(
                "#/oneOf",
                "oneOf",
                "removed",
                "keyword is not accepted by openai_strict schemas",
            )])
        );
    }

    #[test]
    fn openai_strict_intersects_string_const_with_enum() {
        let schema = serde_json::json!({"type": "string", "const": "a", "enum": ["a", "b"]});

        let (sanitized, changes) = sanitize_openai_with_changes(&schema);

        assert_eq!(
            sanitized,
            serde_json::json!({"type": "string", "enum": ["a"]})
        );
        assert_eq!(
            changes,
            Value::Array(vec![openai_receipt_change(
                "#/const",
                "const",
                "intersected_with_const",
                "single-value enum preserves the string const and enum intersection in openai_strict schemas",
            )])
        );
    }

    #[test]
    fn openai_strict_fails_closed_for_empty_const_enum_intersection() {
        let schema = serde_json::json!({"type": "string", "const": "a", "enum": ["b"]});

        let (sanitized, changes) = sanitize_openai_with_changes(&schema);

        assert_eq!(sanitized, serde_json::json!({"type": "string", "enum": []}));
        assert_eq!(
            changes,
            Value::Array(vec![openai_receipt_change(
                "#/const",
                "const",
                "rewritten_to_empty_enum",
                "empty enum fails closed when const and enum have no safely supported intersection in openai_strict schemas",
            )])
        );
    }

    #[test]
    fn projects_structured_sanitization_receipt() {
        let schema = closed_discriminated_union_schema("oneOf", "const");
        let projection = sanitize_schema(
            "openai",
            "gpt-5.4",
            SchemaCompatProfile::OpenAiStrict,
            SchemaSurface::StructuredOutput,
            &schema,
        );

        let metadata = projection
            .audit_metadata()
            .expect("schema transformation receipt");
        assert_eq!(
            metadata["schema"],
            Value::String(RECEIPT_SCHEMA.to_string())
        );
        assert_eq!(metadata["provider"], Value::String("openai".into()));
        assert_eq!(
            metadata["changes"],
            Value::Array(vec![
                openai_receipt_change(
                    "#/properties/schema_version/const",
                    "const",
                    "rewritten_to_single_value_enum",
                    "single-value enum preserves const semantics in openai_strict schemas",
                ),
                openai_receipt_change(
                    "#/properties/source/oneOf",
                    "oneOf",
                    "rewritten_to_any_of",
                    "closed branches with distinct `kind` literals preserve one-of semantics as anyOf in openai_strict schemas",
                ),
                openai_receipt_change(
                    "#/properties/source/anyOf/0/properties/kind/const",
                    "const",
                    "rewritten_to_single_value_enum",
                    "single-value enum preserves const semantics in openai_strict schemas",
                ),
                openai_receipt_change(
                    "#/properties/source/anyOf/1/properties/kind/const",
                    "const",
                    "rewritten_to_single_value_enum",
                    "single-value enum preserves const semantics in openai_strict schemas",
                ),
            ])
        );
    }
}
