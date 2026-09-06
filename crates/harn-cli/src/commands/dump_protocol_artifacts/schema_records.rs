//! Normalize the supported record shapes from owning wire schemas once.

use super::records::{snake_ident, Field, FieldKind, Record};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) struct SchemaRecords<'a> {
    pub schema: &'a Value,
    pub names: &'a [(&'a str, String)],
    pub label: &'a str,
    pub require_all: bool,
    pub metadata: fn(&str, &str, &Value) -> Result<Option<FieldKind>, String>,
}

impl SchemaRecords<'_> {
    pub(super) fn load(&self) -> Result<Vec<Record>, String> {
        self.names
            .iter()
            .map(|(key, name)| {
                let object = if key.is_empty() {
                    self.schema
                } else {
                    self.schema
                        .pointer(&format!("/$defs/{key}"))
                        .ok_or_else(|| format!("missing {} record {key}", self.label))?
                };
                if object["additionalProperties"] != false {
                    return Err(format!("{} {key} must be closed", self.label));
                }
                let properties = object["properties"]
                    .as_object()
                    .ok_or_else(|| format!("missing {} {key} properties", self.label))?;
                let required = match object.get("required") {
                    Some(value) => value
                        .as_array()
                        .ok_or("required fields must be an array")?
                        .iter()
                        .map(|value| value.as_str().ok_or("field name must be a string"))
                        .collect::<Result<Vec<_>, _>>()?,
                    None => Vec::new(),
                };
                let names: BTreeSet<_> = required.iter().copied().collect();
                if names.len() != required.len() {
                    return Err(format!("{} {key} repeats a required field", self.label));
                }
                if self.require_all && names != properties.keys().map(String::as_str).collect() {
                    return Err(format!(
                        "{} {key} requires explicit presence for every field",
                        self.label
                    ));
                }
                let fields = required
                    .iter()
                    .copied()
                    .chain(
                        properties
                            .keys()
                            .map(String::as_str)
                            .filter(|name| !names.contains(name)),
                    )
                    .map(|field| {
                        let shape = properties
                            .get(field)
                            .ok_or_else(|| format!("missing {} {key}.{field}", self.label))?;
                        Ok(Field {
                            wire_name: field.to_owned().into(),
                            rust_name: if field == "_type" {
                                "type_name".into()
                            } else {
                                snake_ident(field).into()
                            },
                            kind: self.kind(key, field, shape, &mut BTreeSet::new())?,
                            required: names.contains(field),
                            identity: false,
                        })
                    })
                    .collect::<Result<_, String>>()?;
                Ok(Record {
                    name: name.clone(),
                    fields,
                })
            })
            .collect()
    }

    fn kind(
        &self,
        owner: &str,
        field: &str,
        schema: &Value,
        references: &mut BTreeSet<String>,
    ) -> Result<FieldKind, String> {
        if let Some(kind) = (self.metadata)(owner, field, schema)? {
            return Ok(kind);
        }
        if let Some(reference) = schema["$ref"].as_str() {
            let key = reference
                .strip_prefix("#/$defs/")
                .ok_or_else(|| format!("unsupported {} reference {reference}", self.label))?;
            let definition = self
                .schema
                .pointer(&format!("/$defs/{key}"))
                .ok_or_else(|| format!("missing {} reference {reference}", self.label))?;
            if let Some((_, name)) = self.names.iter().find(|(candidate, _)| *candidate == key) {
                return Ok(FieldKind::Named(name.clone()));
            }
            if !references.insert(key.to_owned()) {
                return Err(format!("cyclic {} reference {reference}", self.label));
            }
            let result = self.kind(owner, field, definition, references);
            references.remove(key);
            return result;
        }
        if let Some(variants) = schema["oneOf"].as_array() {
            if variants.len() == 2 && variants[1]["type"] == "null" {
                return Ok(FieldKind::Nullable(Box::new(self.kind(
                    owner,
                    field,
                    &variants[0],
                    references,
                )?)));
            }
        }
        if let Some(types) = schema["type"].as_array() {
            if types.len() == 2 && types[1] == "null" {
                let mut inner = schema.clone();
                inner["type"] = types[0].clone();
                return Ok(FieldKind::Nullable(Box::new(
                    self.kind(owner, field, &inner, references)?,
                )));
            }
        }
        if let Some(value) = schema["const"].as_str() {
            return Ok(FieldKind::Literal {
                value: value.into(),
                wire_type: None,
            });
        }
        Ok(match schema["type"].as_str() {
            Some("string") => FieldKind::String,
            Some("boolean") => FieldKind::Bool,
            Some("array") => FieldKind::List(Box::new(self.kind(
                owner,
                field,
                &schema["items"],
                references,
            )?)),
            Some("object") if schema["additionalProperties"] == true => FieldKind::JsonObject,
            None if schema.as_object().is_some_and(|object| object.is_empty()) => {
                FieldKind::Nullable(Box::new(FieldKind::Json))
            }
            _ => {
                return Err(format!(
                    "unsupported {} shape {owner}.{field}: {schema}",
                    self.label
                ))
            }
        })
    }
}
