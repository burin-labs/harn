//! Small stream projections of the owning Agents API OpenAPI document.

use serde_json::{json, Map, Value};

use super::records::{FieldKind, Record, Target};
use super::schema_records::SchemaRecords;
use super::support::ProtocolArtifactSource;

pub(super) const SOURCE: &str = "crates/harn-serve/openapi.yaml";

#[derive(Debug)]
pub(super) struct AgentsApiProjection {
    records: Vec<Record>,
    statuses: Vec<String>,
    terminal_statuses: Vec<String>,
}

impl AgentsApiProjection {
    pub(super) fn load(source: &ProtocolArtifactSource) -> Result<Self, String> {
        let document: Value = harn_vm::yaml::from_str(&source.read_text(SOURCE)?)
            .map_err(|error| format!("invalid Agents API contract: {error}"))?;
        Self::from_document(&document)
    }

    fn from_document(document: &Value) -> Result<Self, String> {
        let mut properties = Map::new();
        let mut required = Vec::new();
        collect_fields(
            document,
            &document["components"]["schemas"]["Event"],
            &mut properties,
            &mut required,
            0,
        )?;
        let projected = ["id", "object", "event", "session_id", "task_id", "payload"];
        let mut fields = Map::new();
        for field in projected {
            fields.insert(
                field.into(),
                properties
                    .remove(field)
                    .ok_or_else(|| format!("Agents Event is missing {field}"))?,
            );
        }
        let required: Vec<_> = required
            .into_iter()
            .filter(|field| projected.contains(&field.as_str()))
            .collect();
        let schema = json!({"type":"object", "properties":fields, "required":required});
        let names = [("", "HarnAgentsEventProjection".to_owned())];
        let records = SchemaRecords {
            schema: &schema,
            names: &names,
            label: "Agents event projection",
            require_all: false,
            metadata: |_, field, shape| {
                Ok(match field {
                    "payload" if shape["$ref"] == "#/components/schemas/JsonObject" => {
                        Some(FieldKind::JsonObject)
                    }
                    "object" if shape["enum"] == json!(["event"]) => Some(FieldKind::Literal {
                        value: "event".into(),
                        wire_type: None,
                    }),
                    _ => None,
                })
            },
        }
        .load_extensible()?;
        let statuses = document["components"]["schemas"]["TaskStatus"]["enum"]
            .as_array()
            .filter(|values| !values.is_empty())
            .ok_or("Agents TaskStatus must name its states")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("Agents task states must be strings")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut terminal_statuses = Vec::new();
        for status in &statuses {
            let native: harn_serve::adapters::api::ApiTaskStatus =
                serde_json::from_value(json!(status)).map_err(|error| {
                    format!("Agents TaskStatus {status} is not served: {error}")
                })?;
            if !native.is_active() {
                terminal_statuses.push(status.clone());
            }
        }
        Ok(Self {
            records,
            statuses,
            terminal_statuses,
        })
    }

    pub(super) fn append(&self, mut out: String, target: Target) -> String {
        let name = "HarnAgentsTaskStatus";
        match target {
            Target::Swift => {
                out.push_str(&format!(
                    "public enum {name}: String, Codable, Sendable, CaseIterable {{\n"
                ));
                for value in &self.statuses {
                    out.push_str(&format!(
                        "    case {} = {value:?}\n",
                        super::swift::swift_case_name(&value.to_lowercase())
                    ));
                }
                out.push_str("    public var isTerminal: Bool {\n        switch self {\n");
                out.push_str(&format!("        case {}: return true\n", self.terminal_statuses.iter().map(|value| format!(".{}", super::swift::swift_case_name(&value.to_lowercase()))).collect::<Vec<_>>().join(", ")));
                out.push_str("        default: return false\n        }\n    }\n}\n\n");
            }
            Target::Rust => {
                out.push_str(&format!("#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\n#[serde(rename_all = \"SCREAMING_SNAKE_CASE\")]\npub enum {name} {{ {} }}\n\n", self.statuses.iter().map(|value| super::rust::rust_type_name(&value.to_lowercase())).collect::<Vec<_>>().join(", ")));
                out.push_str(&format!("impl {name} {{ pub fn is_terminal(self) -> bool {{ matches!(self, {}) }} }}\n\n", self.terminal_statuses.iter().map(|value| format!("Self::{}", super::rust::rust_type_name(&value.to_lowercase()))).collect::<Vec<_>>().join(" | ")));
            }
            Target::Typescript => out.push_str(&format!(
                "export type {name} = {}\nexport const HARN_AGENTS_TERMINAL_TASK_STATUSES: ReadonlyArray<{name}> = {:?}\n\n",
                self.statuses
                    .iter()
                    .map(|value| format!("{value:?}"))
                    .collect::<Vec<_>>()
                    .join(" | "), self.terminal_statuses,
            )),
            Target::Python => {
                out.push_str(&super::python::py_str_enum_owned(name, &self.statuses));
                out.push_str(&format!("    @property\n    def is_terminal(self) -> bool:\n        return self.value in {:?}\n\n", self.terminal_statuses));
            }
            Target::Go => out.push_str(&format!("type {name} string\nfunc (status {name}) IsTerminal() bool {{\n switch status {{\n case {}: return true\n default: return false\n }}\n}}\n\n", self.terminal_statuses.iter().map(|value| format!("{value:?}")).collect::<Vec<_>>().join(", "))),
        }
        for record in &self.records {
            record.append(&mut out, target);
        }
        out.truncate(out.trim_end().len());
        out.push('\n');
        out
    }
}

fn collect_fields(
    document: &Value,
    shape: &Value,
    properties: &mut Map<String, Value>,
    required: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    if depth > 16 {
        return Err("Agents event schema has cyclic inheritance".into());
    }
    if let Some(reference) = shape["$ref"].as_str() {
        let pointer = reference
            .strip_prefix('#')
            .ok_or("Agents schema references must be local")?;
        return collect_fields(
            document,
            document
                .pointer(pointer)
                .ok_or("missing Agents schema reference")?,
            properties,
            required,
            depth + 1,
        );
    }
    if let Some(parents) = shape["allOf"].as_array() {
        for parent in parents {
            collect_fields(document, parent, properties, required, depth + 1)?;
        }
    }
    if let Some(fields) = shape["properties"].as_object() {
        properties.extend(fields.clone());
    }
    if let Some(fields) = shape["required"].as_array() {
        for field in fields {
            let field = field
                .as_str()
                .ok_or("Agents required fields must be strings")?
                .to_owned();
            if !required.contains(&field) {
                required.push(field);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_is_derived_from_the_owning_event_contract() {
        let source =
            ProtocolArtifactSource::from_anchor(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
                .unwrap();
        let mut document: Value =
            harn_vm::yaml::from_str(&source.read_text(SOURCE).unwrap()).unwrap();
        let projection = AgentsApiProjection::from_document(&document).unwrap();
        assert_eq!(projection.records[0].fields.len(), 6);
        assert!(
            projection.records[0]
                .fields
                .iter()
                .find(|field| field.wire_name == "id")
                .unwrap()
                .required
        );
        assert!(
            projection.records[0]
                .fields
                .iter()
                .find(|field| field.wire_name == "payload")
                .unwrap()
                .required
        );
        assert!(projection.statuses.contains(&"AUTH_REQUIRED".to_owned()));
        document["components"]["schemas"]["Event"]["allOf"][1]["properties"]
            .as_object_mut()
            .unwrap()
            .remove("payload");
        assert!(AgentsApiProjection::from_document(&document)
            .unwrap_err()
            .contains("payload"));
    }
}
