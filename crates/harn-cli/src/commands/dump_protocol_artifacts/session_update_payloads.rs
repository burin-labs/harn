//! Session-update bindings projected from the adapter's owning JSON schema.

use super::records::{FieldKind, Record, Target};
use super::schema_records::SchemaRecords;
use super::support::ProtocolArtifactSource;
use serde_json::{json, Map, Value};

pub(super) const SOURCE: &str = "conformance/protocols/schemas/acp-session-update.schema.json";

pub(super) struct SessionUpdatePayloads {
    pub variants: Vec<(String, String)>,
    pub records: Vec<Record>,
    schemas: Vec<Value>,
}

impl SessionUpdatePayloads {
    pub(super) fn load(source: &ProtocolArtifactSource) -> Result<Self, String> {
        Self::parse(&source.read_text(SOURCE)?)
    }

    pub(super) fn parse(text: &str) -> Result<Self, String> {
        let schema: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
        let variants = schema["$defs"]["HarnExtensionUpdate"]["oneOf"]
            .as_array()
            .ok_or("HarnExtensionUpdate.oneOf must enumerate the typed payloads")?;
        if variants.is_empty() {
            return Err("HarnExtensionUpdate must not be empty".into());
        }
        let mut payloads = Vec::new();
        let mut definitions = Map::new();
        let mut order = Vec::new();
        let mut schemas = Vec::new();
        for variant in variants {
            let reference = variant["$ref"]
                .as_str()
                .ok_or("payload must reference a definition")?;
            let key = reference
                .strip_prefix("#/$defs/")
                .ok_or("payload reference must be local")?;
            let mut shape = schema["$defs"]
                .get(key)
                .ok_or_else(|| format!("missing payload {key}"))?
                .clone();
            merge_properties(&mut shape, &schema["$defs"]["HarnExtensionUpdate"])?;
            let discriminator = shape["properties"]["sessionUpdate"]["const"]
                .as_str()
                .ok_or_else(|| format!("{key} must pin sessionUpdate"))?;
            if payloads.iter().any(|(kind, _)| kind == discriminator) {
                return Err(format!("duplicate session update {discriminator}"));
            }
            let stem = key.strip_suffix("Update").unwrap_or(key);
            let name = format!("ACP{stem}Update");
            lift_record(&name, shape.clone(), &mut definitions, &mut order)?;
            payloads.push((discriminator.to_owned(), name));
            schemas.push(shape.clone());
        }
        let names = order
            .iter()
            .map(|key: &String| (key.as_str(), key.clone()))
            .collect::<Vec<_>>();
        let normalized = json!({"$defs": definitions});
        let records = SchemaRecords {
            schema: &normalized,
            names: &names,
            label: "session update",
            require_all: false,
            metadata: |_, _, shape| {
                Ok(if shape.as_object().is_some_and(Map::is_empty) {
                    Some(FieldKind::Json)
                } else {
                    None
                })
            },
        }
        .load_extensible()?;
        Ok(Self {
            variants: payloads,
            records,
            schemas,
        })
    }

    #[cfg(test)]
    pub(super) fn load_for_tests() -> Self {
        Self::load(
            &ProtocolArtifactSource::from_anchor(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
                .expect("workspace source"),
        )
        .expect("session-update schema")
    }

    pub(super) fn append(&self, out: &mut String, target: Target) {
        out.push('\n');
        for record in &self.records {
            let mut record = record.clone();
            if matches!(target, Target::Swift) {
                record.name = format!("Harn{}", record.name);
                for field in &mut record.fields {
                    swift_names(&mut field.kind);
                }
            }
            record.append_mutable(out, target, true);
        }
        match target {
            Target::Rust => self.append_rust_union(out),
            Target::Swift => self.append_swift_union(out),
            Target::Typescript => {
                out.push_str("export type ACPTypedSessionUpdate =\n");
                out.push_str(&self.typescript_union_members());
                out.push('\n');
            }
            _ => {}
        }
    }

    pub(super) fn typescript_union_members(&self) -> String {
        self.variants
            .iter()
            .map(|(_, name)| format!("  | {name}\n"))
            .collect()
    }

    fn append_rust_union(&self, out: &mut String) {
        out.push_str("fn deserialize_present_session_update_value<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {\n    Value::deserialize(deserializer).map(Some)\n}\n\n");
        out.push_str("#[derive(Clone, Debug, PartialEq, Eq, Serialize)]\n#[serde(untagged)]\npub enum ACPTypedSessionUpdate {\n");
        for (_, name) in &self.variants {
            out.push_str(&format!("    {}({name}),\n", &name[3..]));
        }
        out.push_str("}\n\nimpl<'de> Deserialize<'de> for ACPTypedSessionUpdate {\n    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {\n        let value = Value::deserialize(deserializer)?;\n        match value.get(\"sessionUpdate\").and_then(Value::as_str) {\n");
        for ((kind, name), schema) in self.variants.iter().zip(&self.schemas) {
            out.push_str(&format!("            Some({kind:?}) => {{\n"));
            super::session_update_validation::append_checks(out, schema, Target::Rust);
            out.push_str(&format!("                serde_json::from_value(value).map(Self::{}).map_err(serde::de::Error::custom)\n            }},\n", &name[3..]));
        }
        out.push_str("            _ => Err(serde::de::Error::custom(\"unknown typed session update\")),\n        }\n    }\n}\n\n");
    }

    fn append_swift_union(&self, out: &mut String) {
        out.push_str("public enum HarnACPTypedSessionUpdate: Codable, Sendable, Equatable {\n");
        for (kind, name) in &self.variants {
            out.push_str(&format!("    case {kind}(Harn{name})\n"));
        }
        out.push_str("\n    public init(from decoder: Decoder) throws {\n        let container = try decoder.singleValueContainer()\n        let value = try container.decode(HarnACPValue.self)\n        switch value[\"sessionUpdate\"]?.stringValue {\n");
        for ((kind, name), schema) in self.variants.iter().zip(&self.schemas) {
            out.push_str(&format!("        case {kind:?}:\n"));
            super::session_update_validation::append_checks(out, schema, Target::Swift);
            out.push_str(&format!(
                "            self = .{kind}(try container.decode(Harn{name}.self))\n"
            ));
        }
        out.push_str("        default: throw DecodingError.dataCorruptedError(in: container, debugDescription: \"unknown typed session update\")\n        }\n    }\n\n    public func encode(to encoder: Encoder) throws {\n        switch self {\n");
        for (kind, _) in &self.variants {
            out.push_str(&format!(
                "        case .{kind}(let update): try update.encode(to: encoder)\n"
            ));
        }
        out.push_str("        }\n    }\n}\n\n");
    }
}

fn merge_properties(shape: &mut Value, common: &Value) -> Result<(), String> {
    let Some(properties) = common["properties"].as_object() else {
        return Ok(());
    };
    let target = shape["properties"]
        .as_object_mut()
        .ok_or("shared metadata needs an object")?;
    for (name, field) in properties {
        if let Some(existing) = target.get_mut(name) {
            if field["properties"].is_object() {
                merge_properties(existing, field)?;
            } else if existing != field {
                return Err(format!("conflicting shared metadata field {name}"));
            }
        } else {
            target.insert(name.clone(), field.clone());
        }
    }
    Ok(())
}

/// Name inline records before the shared schema reader resolves their fields.
/// Children are inserted first so Python annotations refer to existing classes.
fn lift_record(
    name: &str,
    mut shape: Value,
    definitions: &mut Map<String, Value>,
    order: &mut Vec<String>,
) -> Result<(), String> {
    let properties = shape["properties"]
        .as_object_mut()
        .ok_or_else(|| format!("{name} requires properties"))?;
    for (field, child) in properties {
        if child["type"] == "object" && child["properties"].is_object() {
            let suffix = field.trim_start_matches('_');
            let mut chars = suffix.chars();
            let suffix = chars
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default()
                + chars.as_str();
            let child_name = format!("{name}{suffix}");
            lift_record(&child_name, child.take(), definitions, order)?;
            *child = json!({"$ref": format!("#/$defs/{child_name}")});
        }
    }
    if definitions.insert(name.to_owned(), shape).is_some() {
        return Err(format!("duplicate session-update record {name}"));
    }
    order.push(name.to_owned());
    Ok(())
}

fn swift_names(kind: &mut FieldKind) {
    match kind {
        FieldKind::Named(name) => *name = format!("Harn{name}"),
        FieldKind::List(inner) | FieldKind::Nullable(inner) | FieldKind::DefaultList(inner) => {
            swift_names(inner)
        }
        _ => {}
    }
}
