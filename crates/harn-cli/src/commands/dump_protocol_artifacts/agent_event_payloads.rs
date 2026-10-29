//! `_harn/agentEvent` bindings projected from the adapter's typed event bodies.
//!
//! The ACP adapter serializes one Rust type per event kind; this module reads
//! the JSON schema derived from those types and emits a record per body plus a
//! kind-discriminated union for each host language.

use serde_json::{json, Map, Value};

use super::records::{FieldKind, Record, Target};
use super::schema_records::SchemaRecords;
use harn_serve::adapters::acp::agent_event_payloads::{
    agent_event_schema, AGENT_EVENT_SCHEMA_ARTIFACT,
};

pub(super) const SCHEMA_ARTIFACT: &str = AGENT_EVENT_SCHEMA_ARTIFACT;

/// One event kind and the binding type of its body.
struct Variant {
    kind: String,
    /// The Rust/Swift/TypeScript union case name.
    case: String,
    /// The generated record, or `None` for a body Harn does not type.
    record: Option<String>,
}

pub(super) struct AgentEventPayloads {
    schema: Value,
    variants: Vec<Variant>,
    records: Vec<Record>,
}

impl AgentEventPayloads {
    pub(super) fn load() -> Result<Self, String> {
        Self::from_schema(agent_event_schema())
    }

    fn from_schema(schema: Value) -> Result<Self, String> {
        let kinds = schema["x-harn-kinds"]
            .as_object()
            .filter(|kinds| !kinds.is_empty())
            .ok_or("agent event schema must map its kinds")?;
        let mut variants = Vec::new();
        let mut definitions = Map::new();
        let mut order = Vec::new();
        for (kind, definition) in kinds {
            let key = definition
                .as_str()
                .ok_or_else(|| format!("agent event {kind} must name its body"))?;
            let shape = schema["$defs"]
                .get(key)
                .ok_or_else(|| format!("agent event {kind} body {key} is missing"))?;
            let typed = shape["properties"]
                .as_object()
                .is_some_and(|properties| !properties.is_empty());
            if !typed && shape["additionalProperties"] != true {
                return Err(format!("agent event {kind} body {key} has no fields"));
            }
            let record = typed.then(|| format!("HarnAgentEvent{key}"));
            if let Some(name) = &record {
                lift_record(name, shape.clone(), &mut definitions, &mut order)?;
            }
            variants.push(Variant {
                kind: kind.clone(),
                case: key.to_owned(),
                record,
            });
        }
        let names = order
            .iter()
            .map(|key| (key.as_str(), key.clone()))
            .collect::<Vec<_>>();
        let normalized = json!({"$defs": definitions});
        let records = SchemaRecords {
            schema: &normalized,
            names: &names,
            label: "agent event",
            require_all: false,
            metadata: |_, _, shape| {
                // An untyped `serde_json::Value` field carries at most its doc.
                let annotation_only = shape.as_object().is_some_and(|shape| {
                    shape
                        .keys()
                        .all(|key| matches!(key.as_str(), "description" | "title"))
                });
                Ok(if shape == &Value::Bool(true) || annotation_only {
                    Some(FieldKind::Json)
                } else {
                    None
                })
            },
        }
        .load_extensible()?;
        let mut records = records;
        for field in records.iter_mut().flat_map(|record| &mut record.fields) {
            // `override` is a reserved Rust keyword.
            if field.rust_name == "override" {
                field.rust_name = "override_value".into();
            }
        }
        Ok(Self {
            schema,
            variants,
            records,
        })
    }

    pub(super) fn schema_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.schema).map_err(|error| error.to_string())
    }

    pub(super) fn append(&self, mut out: String, target: Target) -> String {
        out.truncate(out.trim_end().len());
        out.push_str("\n\n");
        for record in &self.records {
            record.append_mutable(&mut out, target, false);
        }
        match target {
            Target::Swift => self.append_swift(&mut out),
            Target::Rust => self.append_rust(&mut out),
            Target::Typescript => self.append_typescript(&mut out),
            Target::Python | Target::Go => {}
        }
        out.truncate(out.trim_end().len());
        out.push('\n');
        out
    }

    fn body_type(&self, variant: &Variant, target: Target) -> String {
        variant
            .record
            .clone()
            .unwrap_or_else(|| FieldKind::JsonObject.type_name(target))
    }

    fn append_swift(&self, out: &mut String) {
        let kinds: Vec<String> = self.variants.iter().map(|v| v.kind.clone()).collect();
        out.push_str(&super::swift::swift_enum("HarnAgentEventKind", &kinds));
        out.push_str("/// The typed body of one `_harn/agentEvent` notification.\npublic enum HarnAgentEventPayload: Sendable, Equatable {\n");
        for variant in &self.variants {
            out.push_str(&format!(
                "    case {}({})\n",
                super::swift::swift_case_name(&variant.kind),
                self.body_type(variant, Target::Swift)
            ));
        }
        out.push_str("}\n\n");
        out.push_str(
            "/// The params of one `_harn/agentEvent` notification. `fields` keeps every\n\
             /// params member, including those of a body Harn does not type. Decoding\n\
             /// fails for a kind this binding does not name.\n\
             public struct HarnAgentEvent: Codable, Sendable, Equatable {\n    \
             public let sessionId: String\n    \
             public let kind: HarnAgentEventKind\n    \
             public let payload: HarnAgentEventPayload\n    \
             public let fields: [String: HarnACPValue]\n\n    \
             public init(from decoder: Decoder) throws {\n        \
             let container = try decoder.singleValueContainer()\n        \
             let fields = try container.decode([String: HarnACPValue].self)\n        \
             guard case .string(let sessionId)? = fields[\"sessionId\"] else {\n            \
             throw DecodingError.dataCorruptedError(in: container, debugDescription: \"agent event without sessionId\")\n        \
             }\n        \
             guard case .string(let rawKind)? = fields[\"kind\"], let kind = HarnAgentEventKind(rawValue: rawKind) else {\n            \
             throw DecodingError.dataCorruptedError(in: container, debugDescription: \"unknown agent event kind\")\n        \
             }\n        \
             self.sessionId = sessionId\n        \
             self.kind = kind\n        \
             self.fields = fields\n        \
             switch kind {\n",
        );
        for variant in &self.variants {
            let case = super::swift::swift_case_name(&variant.kind);
            let body = match &variant.record {
                Some(record) => format!("try container.decode({record}.self)"),
                None => "fields".to_owned(),
            };
            out.push_str(&format!(
                "        case .{case}: self.payload = .{case}({body})\n"
            ));
        }
        out.push_str(
            "        }\n    }\n\n    \
             public func encode(to encoder: Encoder) throws {\n        \
             var container = encoder.singleValueContainer()\n        \
             try container.encode(fields)\n    }\n}\n\n",
        );
    }

    fn append_rust(&self, out: &mut String) {
        out.push_str("/// The typed body of one `_harn/agentEvent` notification, discriminated by\n/// its `kind`. The envelope's `sessionId` stays on [`HarnAgentEventParams`].\n#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]\n#[serde(tag = \"kind\")]\npub enum HarnAgentEventPayload {\n");
        for variant in &self.variants {
            out.push_str(&format!(
                "    #[serde(rename = {:?})]\n    {}({}),\n",
                variant.kind,
                variant.case,
                self.body_type(variant, Target::Rust)
            ));
        }
        out.push_str("}\n\n");
    }

    fn append_typescript(&self, out: &mut String) {
        out.push_str("/** The params of one `_harn/agentEvent` notification, discriminated by `kind`. */\nexport type HarnAgentEvent =\n");
        for variant in &self.variants {
            out.push_str(&format!(
                "  | ({{ sessionId: string; kind: {:?} }} & {})\n",
                variant.kind,
                self.body_type(variant, Target::Typescript)
            ));
        }
        out.push('\n');
    }
}

/// Name inline object fields before the shared schema reader resolves them.
/// Children are inserted first so Python annotations refer to defined classes.
fn lift_record(
    name: &str,
    mut shape: Value,
    definitions: &mut Map<String, Value>,
    order: &mut Vec<String>,
) -> Result<(), String> {
    if let Some(properties) = shape["properties"].as_object_mut() {
        for (field, child) in properties {
            let object = match child.get("anyOf").or_else(|| child.get("oneOf")) {
                Some(Value::Array(variants)) if variants.len() == 2 => variants
                    .iter()
                    .position(|variant| variant["properties"].is_object()),
                _ => None,
            };
            let mut chars = field.chars();
            let suffix = chars
                .next()
                .map(|ch| ch.to_uppercase().to_string())
                .unwrap_or_default()
                + chars.as_str();
            let child_name = format!("{name}{suffix}");
            if child["properties"].is_object() {
                let nullable = child["type"]
                    .as_array()
                    .is_some_and(|types| types.contains(&json!("null")));
                lift_record(&child_name, child.take(), definitions, order)?;
                let reference = json!({"$ref": format!("#/$defs/{child_name}")});
                *child = if nullable {
                    json!({"anyOf": [reference, {"type": "null"}]})
                } else {
                    reference
                };
            } else if let Some(index) = object {
                let union = if child.get("anyOf").is_some() {
                    "anyOf"
                } else {
                    "oneOf"
                };
                let nested = child[union][index].take();
                lift_record(&child_name, nested, definitions, order)?;
                *child = json!({"anyOf": [
                    {"$ref": format!("#/$defs/{child_name}")},
                    {"type": "null"},
                ]});
            }
        }
    }
    if definitions.insert(name.to_owned(), shape).is_some() {
        return Err(format!("duplicate agent event record {name}"));
    }
    order.push(name.to_owned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_kind_projects_from_its_emitted_rust_body() {
        let payloads = AgentEventPayloads::load().unwrap();
        let kinds: Vec<_> = payloads.variants.iter().map(|v| v.kind.as_str()).collect();
        assert_eq!(
            kinds,
            harn_serve::adapters::acp::HARN_AGENT_EVENT_KINDS,
            "the projection and the advertised kinds share one registry"
        );
        let judge = payloads
            .records
            .iter()
            .find(|record| record.name == "HarnAgentEventStepJudgeDecision")
            .unwrap();
        let verdict = judge
            .fields
            .iter()
            .find(|field| field.wire_name == "verdict")
            .unwrap();
        assert!(verdict.required);
        assert_eq!(verdict.kind, FieldKind::String);
        let wrapup = payloads
            .records
            .iter()
            .find(|record| record.name == "HarnAgentEventFinalWrapup")
            .unwrap();
        assert!(wrapup.fields.iter().any(|field| matches!(
            &field.kind,
            FieldKind::Nullable(inner)
                if **inner == FieldKind::Named("HarnAgentEventFinalWrapupUnconsumedToolCall".into())
        )));
        let open = payloads
            .variants
            .iter()
            .find(|variant| variant.kind == "reserved_terminal_verify")
            .unwrap();
        assert!(open.record.is_none(), "an untyped script record stays open");
    }

    #[test]
    fn a_body_without_fields_or_an_open_object_is_refused() {
        let mut schema = agent_event_schema();
        schema["$defs"]["IterationStart"] = json!({"type": "object"});
        let error = AgentEventPayloads::from_schema(schema).err().unwrap();
        assert!(error.contains("iteration_start"), "{error}");
    }
}
