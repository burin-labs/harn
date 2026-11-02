//! Bundle bindings derived from the serialized manifest schema.

use super::records::{FieldKind, Record, Target};
use super::schema_records::SchemaRecords;
use serde_json::Value;

pub(super) struct BundleProjection {
    schema: Value,
    records: Vec<Record>,
    enums: Vec<(String, Vec<String>)>,
}

fn name(key: &str) -> String {
    let key = key
        .strip_prefix("Crystallization")
        .or_else(|| key.strip_prefix("Bundle"))
        .unwrap_or(key);
    format!("HarnCrystallization{key}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bundle_projection_carries_safety_and_provenance_fields() {
        let projection = BundleProjection::load().expect("bundle projection");
        let swift = projection.append(String::new(), Target::Swift);
        for field in [
            "sideEffectsKnown",
            "executionId",
            "replayOracle",
            "comparedReceipts",
            "divergenceHistory",
            "estimatedTimeTokenSavings",
            "skillCandidateId",
        ] {
            assert!(swift.contains(field), "missing bundle field {field}");
        }
        assert!(swift.contains("needsApproval = \"needs_approval\""));
        assert!(swift.contains("public static let schemaVersion = 2"));
        assert!(swift.contains("try values.encode(team, forKey: .team)"));
        assert!(!swift.contains("struct HarnCrystallizationBundleStep"));
        let version = &projection.schema["$defs"]["BundleGenerator"]["properties"]["version"];
        assert!(version.get("default").is_none());
        assert_eq!(version["x-harn-contextual-default"], true);
        assert!(projection.schema["properties"]["generator"]
            .get("default")
            .is_none());
        assert_eq!(
            projection.schema["$defs"]["BundleGenerator"]["properties"]["tool"]["default"],
            "harn"
        );
        assert_eq!(
            projection.schema["$defs"]["BundlePromotion"]["properties"]["rollout_policy"]
                ["default"],
            harn_vm::orchestration::BundlePromotion::default().rollout_policy
        );
    }

    #[test]
    fn bundle_projection_rejects_an_unprojectable_new_wire_shape() {
        let mut schema = harn_vm::orchestration::crystallization_bundle_json_schema();
        schema["properties"]["future_field"] =
            json!({"type": "object", "patternProperties": {"^x": {"type": "string"}}});
        assert!(BundleProjection::from_schema(schema).is_err());
    }
}

impl BundleProjection {
    pub(super) fn load() -> Result<Self, String> {
        Self::from_schema(harn_vm::orchestration::crystallization_bundle_json_schema())
    }

    fn from_schema(schema: Value) -> Result<Self, String> {
        let definitions = schema["$defs"]
            .as_object()
            .ok_or("bundle definitions missing")?;
        let mut names = vec![("", name("CandidateBundle"))];
        let mut enums = Vec::new();
        for (key, shape) in definitions {
            if shape["properties"].is_object() {
                names.push((key.as_str(), name(key)));
            } else {
                let values = shape["enum"]
                    .as_array()
                    .cloned()
                    .or_else(|| {
                        shape["oneOf"].as_array().map(|variants| {
                            variants
                                .iter()
                                .map(|variant| variant["const"].clone())
                                .collect()
                        })
                    })
                    .ok_or_else(|| format!("unsupported bundle definition {key}: {shape}"))?;
                if values.is_empty() {
                    return Err(format!("bundle enum {key} must not be empty"));
                }
                let values = values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or("bundle enum must contain strings")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                enums.push((name(key), values));
            }
        }
        let records = SchemaRecords {
            schema: &schema,
            names: &names,
            label: "crystallization bundle",
            require_all: false,
            metadata: |_, _, shape| {
                let annotation_only = shape.as_object().is_some_and(|object| {
                    object
                        .keys()
                        .all(|key| matches!(key.as_str(), "description" | "title" | "default"))
                });
                if shape == &Value::Bool(true) || annotation_only {
                    return Ok(Some(FieldKind::Json));
                }
                if let Some(key) = shape["$ref"]
                    .as_str()
                    .and_then(|reference| reference.strip_prefix("#/$defs/"))
                {
                    return Ok(Some(FieldKind::Named(name(key))));
                }
                Ok(None)
            },
        }
        .load_extensible()?;
        Ok(Self {
            schema,
            records,
            enums,
        })
    }

    pub(super) fn schema_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.schema).map_err(|error| error.to_string())
    }

    pub(super) fn append(&self, mut out: String, target: Target) -> String {
        out.push('\n');
        for (name, values) in &self.enums {
            super::records::append_string_enum(&mut out, target, name, values);
        }
        for record in &self.records {
            record.append_with_initializer(&mut out, target);
        }
        if matches!(target, Target::Swift) {
            out.push_str("public enum HarnCrystallizationBundleFormat {\n");
            for (key, value) in self.schema["x-harn-bundle"]
                .as_object()
                .expect("bundle layout")
                .iter()
            {
                out.push_str(&format!("    public static let {key} = {value}\n"));
            }
            out.push_str("}\n");
        }
        if matches!(target, Target::Python) {
            let names = self
                .records
                .iter()
                .map(|record| &record.name)
                .chain(self.enums.iter().map(|(name, _)| name))
                .map(|name| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("__all__.extend([{names}])\n"));
        }
        out.truncate(out.trim_end().len());
        out.push('\n');
        out
    }
}
