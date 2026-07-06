//! Render parsed Harn type declarations into Rust event-payload structs.
//!
//! This is the reverse of `pg_codegen`: instead of SQL -> Harn types, it walks
//! the Harn type AST (`TypeExpr`) from the canonical schema module and emits the
//! Rust structs the runtime deserializes webhook JSON into. The mapping mirrors
//! the serde conventions of the hand-written `GitHubEventPayload` family in
//! `crates/harn-vm/src/triggers/event/payloads.rs` so the same JSON deserializes
//! identically into either copy.

use harn_parser::{Node, ShapeField, TypeExpr};

/// A record type declaration lifted from the schema AST.
struct Record {
    name: String,
    fields: Vec<ShapeField>,
}

/// A union type declaration lifted from the schema AST. Members are the named
/// record types in declaration order; the union becomes a Rust enum.
struct Union {
    name: String,
    members: Vec<String>,
}

/// The set of declarations extracted from one schema module.
struct Schema {
    records: Vec<Record>,
    unions: Vec<Union>,
}

/// Render the full generated Rust file from the schema source.
///
/// Returns an error string when the schema cannot be parsed or contains a type
/// form the generator does not support (so a typo in the schema fails loudly
/// rather than silently dropping a field).
pub(crate) fn render(source: &str, header: &str) -> Result<String, String> {
    let schema = extract_schema(source)?;
    let mut out = String::new();
    out.push_str(header);
    out.push_str(&preamble(&schema));

    for record in &schema.records {
        out.push('\n');
        out.push_str(&render_record(record, &schema)?);
    }
    for union in &schema.unions {
        out.push('\n');
        out.push_str(&render_union(union, &schema)?);
    }
    Ok(out)
}

/// The import preamble. Imports are emitted only when the schema actually uses
/// them so the generated file stays free of `unused_imports` warnings under
/// `-D warnings`: `BTreeMap` only when a `dict<K, V>` field exists, and the
/// `Deserializer` trait only when a union (which gets a manual `Deserialize`)
/// exists.
fn preamble(schema: &Schema) -> String {
    let uses_btreemap = schema
        .records
        .iter()
        .flat_map(|record| &record.fields)
        .any(|field| field_uses_dict(&field.type_expr));
    let has_union = !schema.unions.is_empty();

    let mut out = String::new();
    if uses_btreemap {
        out.push_str("use std::collections::BTreeMap;\n\n");
    }
    if has_union {
        out.push_str("use serde::{Deserialize, Deserializer, Serialize};\n");
    } else {
        out.push_str("use serde::{Deserialize, Serialize};\n");
    }
    out.push_str("use serde_json::Value as JsonValue;\n");
    out
}

/// Whether a field's type expression contains a `dict<K, V>` anywhere (so the
/// `BTreeMap` import is required).
fn field_uses_dict(type_expr: &TypeExpr) -> bool {
    match type_expr {
        TypeExpr::DictType(_, _) => true,
        TypeExpr::List(inner) => field_uses_dict(inner),
        _ => false,
    }
}

/// Parse the schema source via the Harn compiler front-end and lift every
/// top-level `type` declaration into a [`Record`] or [`Union`].
fn extract_schema(source: &str) -> Result<Schema, String> {
    let program = harn_parser::parse_source(source)
        .map_err(|error| format!("failed to parse connector schema module: {error:?}"))?;

    let mut records = Vec::new();
    let mut unions = Vec::new();
    for snode in &program {
        let Node::TypeDecl {
            name, type_expr, ..
        } = &snode.node
        else {
            continue;
        };
        match type_expr {
            TypeExpr::Shape(fields) => records.push(Record {
                name: name.clone(),
                fields: fields.clone(),
            }),
            TypeExpr::Union(members) => {
                let mut member_names = Vec::with_capacity(members.len());
                for member in members {
                    let TypeExpr::Named(member_name) = member else {
                        return Err(format!(
                            "union `{name}` has a non-named member; only named record \
                             types are supported as union members"
                        ));
                    };
                    member_names.push(member_name.clone());
                }
                unions.push(Union {
                    name: name.clone(),
                    members: member_names,
                });
            }
            other => {
                return Err(format!(
                    "type `{name}` is a {} which the connector-schema generator does \
                     not support (expected a record `{{...}}` or a union `A | B`)",
                    type_expr_kind(other)
                ));
            }
        }
    }
    Ok(Schema { records, unions })
}

fn type_expr_kind(type_expr: &TypeExpr) -> &'static str {
    match type_expr {
        TypeExpr::Named(_) => "type alias",
        TypeExpr::Union(_) => "union",
        TypeExpr::Intersection(_) => "intersection",
        TypeExpr::Shape(_) => "record",
        TypeExpr::List(_) => "list",
        TypeExpr::DictType(_, _) => "dict",
        TypeExpr::Applied { .. } => "generic application",
        _ => "unsupported type",
    }
}

/// Render one record type as a `#[derive(...)]`-annotated Rust struct.
///
/// `Eq` is derived alongside `PartialEq` whenever the struct can support it,
/// matching the hand-written structs (which derive `Eq`) and satisfying
/// clippy's `derive_partial_eq_without_eq`. A struct is `Eq` unless it
/// transitively contains a `float` field (`f64` is `PartialEq` but not `Eq`);
/// `serde_json::Value` does implement `Eq`, so `any` fields are fine.
fn render_record(record: &Record, schema: &Schema) -> Result<String, String> {
    let mut out = String::new();
    if record_is_eq(record, schema) {
        out.push_str("#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]\n");
    } else {
        out.push_str("#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]\n");
    }
    out.push_str(&format!("pub struct {} {{\n", record.name));
    for field in &record.fields {
        out.push_str(&render_field(&record.name, field)?);
    }
    out.push_str("}\n");
    Ok(out)
}

/// Whether a record can derive `Eq`: true unless any field transitively
/// resolves to a `float` (`f64`).
fn record_is_eq(record: &Record, schema: &Schema) -> bool {
    record
        .fields
        .iter()
        .all(|field| type_is_eq(&field.type_expr, schema))
}

fn type_is_eq(type_expr: &TypeExpr, schema: &Schema) -> bool {
    match type_expr {
        TypeExpr::Named(name) => match name.as_str() {
            "float" => false,
            "string" | "int" | "bool" | "any" => true,
            other => schema
                .records
                .iter()
                .find(|record| record.name == other)
                // A reference to another record is `Eq` iff that record is;
                // an unknown name (shouldn't occur in a valid schema) is
                // conservatively treated as non-`Eq`.
                .is_some_and(|referenced| record_is_eq(referenced, schema)),
        },
        TypeExpr::List(inner) => type_is_eq(inner, schema),
        TypeExpr::DictType(key, value) => type_is_eq(key, schema) && type_is_eq(value, schema),
        _ => false,
    }
}

/// Render one struct field, including its serde attribute line(s).
fn render_field(record_name: &str, field: &ShapeField) -> Result<String, String> {
    // A field named `common` whose type is a named record is the flattened
    // common block, matching the hand-written `#[serde(flatten)] pub common`.
    if field.name == "common" && !field.optional {
        if let TypeExpr::Named(type_name) = &field.type_expr {
            return Ok(format!(
                "    #[serde(flatten)]\n    pub common: {type_name},\n"
            ));
        }
    }

    let rust_type = rust_type_for(record_name, &field.name, &field.type_expr)?;
    let mut out = String::new();

    if field.optional {
        // Optional field: `Option<T>` that defaults to `None` on absence and
        // is omitted from serialized output when `None`.
        out.push_str("    #[serde(default, skip_serializing_if = \"Option::is_none\")]\n");
        out.push_str(&format!("    pub {}: Option<{rust_type}>,\n", field.name));
    } else if is_list(&field.type_expr) || is_dict(&field.type_expr) {
        // Required collection: default to empty so an absent key deserializes
        // to an empty collection (matching the hand-written `#[serde(default)]`
        // on `Vec`/`BTreeMap` fields).
        out.push_str("    #[serde(default)]\n");
        out.push_str(&format!("    pub {}: {rust_type},\n", field.name));
    } else {
        out.push_str(&format!("    pub {}: {rust_type},\n", field.name));
    }
    Ok(out)
}

fn is_list(type_expr: &TypeExpr) -> bool {
    matches!(type_expr, TypeExpr::List(_))
}

fn is_dict(type_expr: &TypeExpr) -> bool {
    matches!(type_expr, TypeExpr::DictType(_, _))
}

/// Map a Harn type expression to its Rust type spelling.
fn rust_type_for(
    record_name: &str,
    field_name: &str,
    type_expr: &TypeExpr,
) -> Result<String, String> {
    match type_expr {
        TypeExpr::Named(name) => Ok(named_rust_type(name)),
        TypeExpr::List(inner) => {
            let inner = rust_type_for(record_name, field_name, inner)?;
            Ok(format!("Vec<{inner}>"))
        }
        TypeExpr::DictType(key, value) => {
            let key = rust_type_for(record_name, field_name, key)?;
            let value = rust_type_for(record_name, field_name, value)?;
            Ok(format!("BTreeMap<{key}, {value}>"))
        }
        other => Err(format!(
            "field `{record_name}.{field_name}` uses an unsupported type form ({}); \
             the connector-schema generator supports named scalars, `any`, \
             `list<T>`, `dict<K, V>`, and named record references only",
            type_expr_kind(other)
        )),
    }
}

/// Map a named Harn type to its Rust spelling. Scalars and `any` map to
/// primitives / `serde_json::Value`; any other name is a reference to another
/// generated record type and is emitted verbatim.
fn named_rust_type(name: &str) -> String {
    match name {
        "string" => "String".to_string(),
        "int" => "i64".to_string(),
        "float" => "f64".to_string(),
        "bool" => "bool".to_string(),
        "any" => "JsonValue".to_string(),
        other => other.to_string(),
    }
}

/// Render a union as an `event`-dispatched Rust enum with a manual
/// `Deserialize`, mirroring the hand-written `GitHubEventPayload`.
///
/// Each member maps to a variant; the trailing member that is a *common* record
/// (no event-specific fields, named `...Common`) is the `Other` fallback the
/// dispatcher selects when the `event` discriminator is unrecognized.
fn render_union(union: &Union, schema: &Schema) -> Result<String, String> {
    let mut variants = Vec::with_capacity(union.members.len());
    for member in &union.members {
        if !schema.records.iter().any(|record| &record.name == member) {
            return Err(format!(
                "union `{}` references `{member}`, which is not a record type in the \
                 same schema module",
                union.name
            ));
        }
        let variant = variant_name(member);
        let event = event_discriminator(member);
        variants.push((variant, member.clone(), event));
    }

    // The enum is `Eq` iff every member record is `Eq` (matching the
    // hand-written `GitHubEventPayload`, which derives `Eq`).
    let all_members_eq = union.members.iter().all(|member| {
        schema
            .records
            .iter()
            .find(|record| &record.name == member)
            .is_some_and(|record| record_is_eq(record, schema))
    });

    let mut out = String::new();
    if all_members_eq {
        out.push_str("#[derive(Clone, Debug, PartialEq, Eq, Serialize)]\n");
    } else {
        out.push_str("#[derive(Clone, Debug, PartialEq, Serialize)]\n");
    }
    out.push_str("#[serde(untagged)]\n");
    out.push_str(&format!("pub enum {} {{\n", union.name));
    for (variant, payload, _event) in &variants {
        out.push_str(&format!("    {variant}({payload}),\n"));
    }
    out.push_str("}\n");

    // Manual `Deserialize` that dispatches on the `event` field — an untagged
    // enum cannot reliably round-trip the all-optional variants, so we mirror
    // the hand-written dispatch table exactly.
    out.push_str(&format!(
        "\nimpl<'de> Deserialize<'de> for {} {{\n",
        union.name
    ));
    out.push_str("    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>\n");
    out.push_str("    where\n        D: Deserializer<'de>,\n    {\n");
    out.push_str("        const value = JsonValue::deserialize(deserializer)?;\n");
    out.push_str("        const kind = value\n");
    out.push_str("            .get(\"event\")\n");
    out.push_str("            .and_then(JsonValue::as_str)\n");
    out.push_str("            .unwrap_or(\"\")\n");
    out.push_str("            .to_string();\n");
    out.push_str("        const payload = match kind.as_str() {\n");
    for (variant, _payload, event) in &variants {
        let Some(event) = event else { continue };
        out.push_str(&format!(
            "            {event:?} => {}::{variant}(\n",
            union.name
        ));
        out.push_str(
            "                serde_json::from_value(value).map_err(serde::de::Error::custom)?,\n",
        );
        out.push_str("            ),\n");
    }
    // The fallback variant is the one whose discriminator is `None` (a common
    // record with no event-specific promoted fields).
    let fallback = variants
        .iter()
        .find(|(_, _, event)| event.is_none())
        .ok_or_else(|| {
            format!(
                "union `{}` has no common-record fallback member (expected one member \
                 named like `...Common`)",
                union.name
            )
        })?;
    out.push_str(&format!(
        "            _ => {}::{}(\n",
        union.name, fallback.0
    ));
    out.push_str(
        "                serde_json::from_value(value).map_err(serde::de::Error::custom)?,\n",
    );
    out.push_str("            ),\n");
    out.push_str("        };\n");
    out.push_str("        Ok(payload)\n");
    out.push_str("    }\n}\n");
    Ok(out)
}

/// Derive the enum variant name for a payload record by stripping the connector
/// prefix and the `EventPayload` / `EventCommon` suffix.
///
/// `GitHubIssuesEventPayload` -> `Issues`; `GitHubEventCommon` -> `Other`.
fn variant_name(record_name: &str) -> String {
    let stripped = strip_connector_prefix(record_name);
    if let Some(rest) = stripped.strip_suffix("EventCommon") {
        if rest.is_empty() {
            return "Other".to_string();
        }
    }
    stripped
        .strip_suffix("EventPayload")
        .unwrap_or(stripped)
        .to_string()
}

/// The connector prefix (e.g. `GitHub`) is the leading run of capitalized
/// segments before the first event-name word. We strip the known connector
/// prefixes; everything after is the variant stem.
fn strip_connector_prefix(record_name: &str) -> &str {
    for prefix in CONNECTOR_PREFIXES {
        if let Some(rest) = record_name.strip_prefix(prefix) {
            return rest;
        }
    }
    record_name
}

const CONNECTOR_PREFIXES: &[&str] = &["GitHub", "Slack", "Linear", "Notion"];

/// The wire `event` discriminator a payload record dispatches on, derived from
/// the variant stem by snake-casing it. The common-record fallback has no
/// discriminator (`None`).
fn event_discriminator(record_name: &str) -> Option<String> {
    let variant = variant_name(record_name);
    if variant == "Other" {
        return None;
    }
    Some(pascal_to_snake(&variant))
}

fn pascal_to_snake(value: &str) -> String {
    let mut out = String::new();
    for (index, ch) in value.char_indices() {
        if ch.is_ascii_uppercase() {
            if index != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}
