//! Lower catalog schemas using the runtime's prepared JSON Schema resources.

use std::collections::BTreeMap;

use referencing::{Draft, Resolver, ResourceRef};
use serde_json::Value;

use super::*;

mod nominal;
mod runtime;

#[cfg(test)]
mod runtime_tests;

pub(crate) use runtime::{wire_annotations, RuntimeJsonCodec};

pub(crate) struct SchemaWireBuilder<'r> {
    types: BTreeMap<WireTypeId, Option<WireType>>,
    locations: BTreeMap<String, WireTypeId>,
    repeated: Vec<WireType>,
    object_refs: Vec<WireTypeId>,
    runtime_registry: Option<jsonschema::Registry<'r>>,
    runtime_json: BTreeMap<WireTypeId, RuntimeJsonCodec>,
}

impl<'r> SchemaWireBuilder<'r> {
    pub(crate) fn new() -> Self {
        Self {
            types: BTreeMap::new(),
            locations: BTreeMap::new(),
            repeated: Vec::new(),
            object_refs: Vec::new(),
            runtime_registry: None,
            runtime_json: BTreeMap::new(),
        }
    }

    pub(crate) fn runtime(registry: jsonschema::Registry<'r>) -> Self {
        Self {
            runtime_registry: Some(registry),
            ..Self::new()
        }
    }

    pub(crate) fn finish(self) -> Result<PreparedWireGraph, WireFailure> {
        for repeated in &self.repeated {
            let original = self.types[&repeated.id].as_ref().expect("lowered node");
            if semantic_definition(original) != semantic_definition(repeated) {
                return Err(super::graph::failure(
                    repeated,
                    WireFailureCode::InvalidContract,
                ));
            }
        }
        let types = self
            .types
            .into_values()
            .map(|ty| ty.expect("successful lowering fills every reserved node"));
        let graph = if self.runtime_registry.is_some() {
            PreparedWireGraph::prepare_runtime(types, self.runtime_json)
        } else {
            PreparedWireGraph::prepare(types)
        }?;
        for id in &self.object_refs {
            let ty = graph.unaliased(id)?;
            if !matches!(
                ty.shape,
                WireShape::Record { .. }
                    | WireShape::Map { .. }
                    | WireShape::Enum { .. }
                    | WireShape::Bytes { .. }
                    | WireShape::RuntimeJson
            ) {
                return Err(super::graph::failure(ty, WireFailureCode::InvalidContract));
            }
        }
        Ok(graph)
    }

    pub(crate) fn lower(
        &mut self,
        schema: &Value,
        resolver: Resolver<'_>,
        location: &str,
    ) -> Result<WireTypeId, WireFailure> {
        match self.lower_closed(schema, resolver.clone(), location) {
            Err(failure)
                if self.runtime_registry.is_some()
                    && failure.code == WireFailureCode::UnsupportedType
                    && wire_annotations(schema, resolver.clone()).allows_opaque_json() =>
            {
                let id = failure
                    .type_id
                    .expect("schema failures carry their type ID");
                let codec = RuntimeJsonCodec::compile(
                    schema,
                    self.runtime_registry
                        .as_ref()
                        .expect("runtime schema registry"),
                    &resolver,
                    location,
                )
                .map_err(|()| error(&id, location, WireFailureCode::InvalidContract))?;
                self.runtime_json.insert(id.clone(), codec);
                self.locations.insert(location.into(), id.clone());
                self.types.insert(
                    id.clone(),
                    Some(WireType {
                        id: id.clone(),
                        name: None,
                        source: failure.source,
                        documentation: schema
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        shape: WireShape::RuntimeJson,
                    }),
                );
                Ok(id)
            }
            result => result,
        }
    }

    fn lower_closed(
        &mut self,
        schema: &Value,
        resolver: Resolver<'_>,
        location: &str,
    ) -> Result<WireTypeId, WireFailure> {
        if let Some(id) = self.locations.get(location) {
            return Ok(id.clone());
        }
        let id = match schema.get("x-harn-wire-id") {
            Some(Value::String(id)) if !id.is_empty() => WireTypeId(id.clone()),
            Some(_) => {
                return Err(error(
                    &WireTypeId(location.into()),
                    location,
                    WireFailureCode::InvalidContract,
                ))
            }
            None => WireTypeId(super::identity::digest(
                &serde_json::json!({"schema":location}),
            )),
        };
        let declaration_source = schema
            .get("x-harn-source")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| error(&id, location, WireFailureCode::InvalidContract))?
            .unwrap_or_else(|| source(location));
        let object = schema
            .as_object()
            .ok_or_else(|| error(&id, location, WireFailureCode::UnsupportedType))?;
        if !supported_keyword_combination(object) {
            return Err(error(&id, location, WireFailureCode::UnsupportedType));
        }
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "$schema"
                    | "$id"
                    | "$anchor"
                    | "$defs"
                    | "$ref"
                    | "title"
                    | "description"
                    | "examples"
                    | "$comment"
                    | "type"
                    | "const"
                    | "oneOf"
                    | "anyOf"
                    | "properties"
                    | "required"
                    | "additionalProperties"
                    | "items"
                    | "prefixItems"
                    | "minItems"
                    | "maxItems"
                    | "uniqueItems"
                    | "minimum"
                    | "maximum"
                    | "x-harn-type"
                    | "x-harn-source"
                    | "x-harn-field"
                    | "x-harn-nominal"
                    | "x-harn-alias"
                    | "x-harn-wire-id"
                    | "format"
            )
        }) {
            return Err(error(&id, location, WireFailureCode::UnsupportedType));
        }
        if object
            .get("format")
            .is_some_and(|value| value != "harn-decimal")
            || object
                .get("uniqueItems")
                .is_some_and(|value| !value.is_boolean())
        {
            return Err(error(&id, location, WireFailureCode::UnsupportedType));
        }
        if object.get("x-harn-type") == Some(&Value::String("set".into()))
            && (object.get("type") != Some(&Value::String("array".into()))
                || object.contains_key("prefixItems"))
        {
            return Err(error(&id, location, WireFailureCode::InvalidContract));
        }
        self.locations.insert(location.into(), id.clone());
        self.types.entry(id.clone()).or_insert(None);
        let shape = if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            if object.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "$ref"
                        | "$schema"
                        | "$id"
                        | "$defs"
                        | "title"
                        | "description"
                        | "examples"
                        | "$comment"
                        | "x-harn-source"
                        | "x-harn-field"
                        | "x-harn-alias"
                        | "x-harn-wire-id"
                        | "type"
                )
            }) {
                return Err(error(&id, location, WireFailureCode::UnsupportedType));
            }
            if let Some(kind) = object.get("type") {
                if kind != "object" {
                    return Err(error(&id, location, WireFailureCode::UnsupportedType));
                }
            }
            let target = resolver
                .lookup(reference)
                .map_err(|_| error(&id, location, WireFailureCode::UnresolvedReference))?;
            let absolute = resolver
                .resolve_uri(&resolver.base_uri().as_ref().borrow(), reference)
                .map_err(|_| error(&id, location, WireFailureCode::UnresolvedReference))?;
            let (schema, scope, _) = target.into_inner();
            let target_id = self.lower(schema, scope, absolute.as_str().trim_end_matches('#'))?;
            if !object.contains_key("x-harn-alias") {
                if object.contains_key("x-harn-wire-id") || target_id == id {
                    return Err(error(&id, location, WireFailureCode::InvalidContract));
                }
                self.locations.insert(location.into(), target_id.clone());
                if self.types.get(&id).is_some_and(Option::is_none) {
                    self.types.remove(&id);
                }
                if object.contains_key("type") {
                    self.object_refs.push(target_id.clone());
                }
                return Ok(target_id);
            }
            if object.contains_key("type") {
                self.object_refs.push(id.clone());
            }
            WireShape::Alias {
                target: target_id,
                declaration: match object.get("x-harn-alias") {
                    Some(value) => self.nominal_identity(value, resolver.clone(), &id, location)?,
                    None => WireDeclarationIdentity {
                        declaration_id: location.into(),
                        name: location.into(),
                        arguments: Vec::new(),
                    },
                },
            }
        } else if let Some(literal) = object.get("const") {
            WireShape::Literal {
                value: match literal {
                    Value::Null => WireLiteral::Null,
                    Value::Bool(value) => WireLiteral::Boolean { value: *value },
                    Value::String(value) => WireLiteral::String {
                        value: value.clone(),
                    },
                    Value::Number(value) if value.is_i64() => WireLiteral::Integer {
                        value: value.to_string(),
                    },
                    Value::Number(value) if value.is_u64() => {
                        return Err(error(&id, location, WireFailureCode::InvalidRange));
                    }
                    Value::Number(value) => WireLiteral::Float {
                        value: value.to_string(),
                    },
                    _ => return Err(error(&id, location, WireFailureCode::UnsupportedType)),
                },
            }
        } else if let Some(nominal) = object
            .get("x-harn-nominal")
            .filter(|_| object.contains_key("oneOf"))
        {
            self.nominal_enum(schema, nominal, resolver.clone(), &id, location)?
        } else if let Some(members) = object.get("oneOf").or_else(|| object.get("anyOf")) {
            let keyword = if object.contains_key("oneOf") {
                "oneOf"
            } else {
                "anyOf"
            };
            let members = members
                .as_array()
                .ok_or_else(|| error(&id, location, WireFailureCode::InvalidContract))?;
            WireShape::Union {
                members: members
                    .iter()
                    .enumerate()
                    .map(|(index, schema)| {
                        self.lower_child(
                            schema,
                            resolver.clone(),
                            &child(location, &format!("{keyword}/{index}")),
                        )
                    })
                    .collect::<Result<_, _>>()?,
            }
        } else {
            match object.get("x-harn-type").and_then(Value::as_str) {
                Some("bytes") => {
                    let expected = crate::value::json::bytes_json_schema(false);
                    if expected
                        .iter()
                        .any(|(key, value)| object.get(key) != Some(value))
                    {
                        return Err(error(&id, location, WireFailureCode::InvalidContract));
                    }
                    WireShape::Bytes {
                        encoding: WireBytesEncoding::Base64Envelope,
                    }
                }
                Some("decimal") => {
                    if object.get("type") != Some(&Value::String("string".into()))
                        || object.get("format") != Some(&Value::String("harn-decimal".into()))
                    {
                        return Err(error(&id, location, WireFailureCode::InvalidContract));
                    }
                    WireShape::Decimal {
                        encoding: WireDecimalEncoding::CanonicalString,
                    }
                }
                Some(value) if value != "set" => {
                    return Err(error(&id, location, WireFailureCode::UnsupportedType))
                }
                _ => match object.get("type").and_then(Value::as_str) {
                    Some("null") => WireShape::Null,
                    Some("boolean") => WireShape::Boolean,
                    Some("integer") => WireShape::Integer {
                        encoding: WireIntegerEncoding::JsonNumber,
                        minimum: object
                            .get("minimum")
                            .map_or(Some(i64::MIN), Value::as_i64)
                            .ok_or_else(|| error(&id, location, WireFailureCode::InvalidRange))?
                            .to_string(),
                        maximum: object
                            .get("maximum")
                            .map_or(Some(i64::MAX), Value::as_i64)
                            .ok_or_else(|| error(&id, location, WireFailureCode::InvalidRange))?
                            .to_string(),
                    },
                    Some("number") => WireShape::Float,
                    Some("string")
                        if object.get("format") == Some(&Value::String("harn-decimal".into())) =>
                    {
                        WireShape::Decimal {
                            encoding: WireDecimalEncoding::CanonicalString,
                        }
                    }
                    Some("string") => WireShape::String,
                    Some("array") => {
                        if object.contains_key("prefixItems")
                            || object.get("items") == Some(&Value::Bool(false))
                        {
                            let items = fixed_tuple_items(schema).ok_or_else(|| {
                                error(&id, location, WireFailureCode::UnsupportedType)
                            })?;
                            WireShape::Tuple {
                                items: items
                                    .iter()
                                    .enumerate()
                                    .map(|(index, schema)| {
                                        self.lower_child(
                                            schema,
                                            resolver.clone(),
                                            &child(location, &format!("prefixItems/{index}")),
                                        )
                                    })
                                    .collect::<Result<_, _>>()?,
                            }
                        } else {
                            if object.contains_key("minItems") || object.contains_key("maxItems") {
                                return Err(error(&id, location, WireFailureCode::UnsupportedType));
                            }
                            let item = object.get("items").ok_or_else(|| {
                                error(&id, location, WireFailureCode::UnsupportedType)
                            })?;
                            let item = self.lower_child(
                                item,
                                resolver.clone(),
                                &child(location, "items"),
                            )?;
                            if object.get("x-harn-type") == Some(&Value::String("set".into())) {
                                if object.get("uniqueItems") != Some(&Value::Bool(true)) {
                                    return Err(error(
                                        &id,
                                        location,
                                        WireFailureCode::InvalidContract,
                                    ));
                                }
                                WireShape::Set { item }
                            } else if object.get("uniqueItems") == Some(&Value::Bool(true)) {
                                return Err(error(&id, location, WireFailureCode::UnsupportedType));
                            } else {
                                WireShape::List { item }
                            }
                        }
                    }
                    Some("object") => {
                        let additional = match object.get("additionalProperties") {
                            Some(Value::Bool(false)) => None,
                            Some(schema @ Value::Object(_)) => Some(self.lower_child(
                                schema,
                                resolver.clone(),
                                &child(location, "additionalProperties"),
                            )?),
                            _ => {
                                return Err(error(&id, location, WireFailureCode::UnsupportedType))
                            }
                        };
                        let required: Vec<&str> = match object.get("required") {
                            Some(Value::Array(values)) => values
                                .iter()
                                .map(|value| {
                                    value.as_str().ok_or_else(|| {
                                        error(&id, location, WireFailureCode::InvalidContract)
                                    })
                                })
                                .collect::<Result<_, _>>()?,
                            None => Vec::new(),
                            _ => {
                                return Err(error(&id, location, WireFailureCode::InvalidContract))
                            }
                        };
                        let mut fields = Vec::new();
                        if let Some(properties) = object.get("properties") {
                            let properties = properties.as_object().ok_or_else(|| {
                                error(&id, location, WireFailureCode::InvalidContract)
                            })?;
                            if required.iter().any(|name| !properties.contains_key(*name)) {
                                return Err(error(&id, location, WireFailureCode::InvalidContract));
                            }
                            for (name, schema) in properties {
                                let field_location =
                                    child(location, &format!("properties/{}", escape(name)));
                                let origin =
                                    self.field_origin(schema, &id, name, &field_location)?;
                                fields.push(WireField {
                                    id: origin.id,
                                    name: name.clone(),
                                    type_id: self.lower_child(
                                        schema,
                                        resolver.clone(),
                                        &field_location,
                                    )?,
                                    optional: !required.contains(&name.as_str()),
                                    source: origin.source,
                                    documentation: origin.documentation,
                                });
                            }
                        } else if !required.is_empty() {
                            return Err(error(&id, location, WireFailureCode::InvalidContract));
                        }
                        let nominal = object
                            .get("x-harn-nominal")
                            .map(|value| {
                                self.nominal_identity(value, resolver.clone(), &id, location)
                            })
                            .transpose()?;
                        if nominal.is_some() && additional.is_some() {
                            return Err(error(&id, location, WireFailureCode::InvalidContract));
                        }
                        if fields.is_empty() && nominal.is_none() {
                            if let Some(value) = additional {
                                WireShape::Map { value }
                            } else {
                                WireShape::Record {
                                    fields,
                                    additional: None,
                                    nominal: None,
                                }
                            }
                        } else {
                            WireShape::Record {
                                fields,
                                additional,
                                nominal,
                            }
                        }
                    }
                    _ => return Err(error(&id, location, WireFailureCode::UnsupportedType)),
                },
            }
        };
        let ty = WireType {
            id: id.clone(),
            name: None,
            source: declaration_source,
            documentation: object
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            shape,
        };
        if self.types[&id].is_some() {
            self.repeated.push(ty);
        } else {
            self.types.insert(id.clone(), Some(ty));
        }
        Ok(id)
    }

    fn lower_child(
        &mut self,
        schema: &Value,
        resolver: Resolver<'_>,
        location: &str,
    ) -> Result<WireTypeId, WireFailure> {
        let resolver = resolver
            .in_subresource(ResourceRef::new(schema, Draft::Draft202012))
            .map_err(|_| {
                error(
                    &WireTypeId(location.into()),
                    location,
                    WireFailureCode::UnresolvedReference,
                )
            })?;
        self.lower(schema, resolver, location)
    }
}

fn semantic_definition(ty: &WireType) -> WireType {
    let mut ty = ty.clone();
    ty.name = None;
    ty.documentation = None;
    ty.source = WireSourceRef::Synthetic {
        owner: "semantic".into(),
    };
    let fields = |fields: &mut Vec<WireField>| {
        for field in fields {
            field.source = WireSourceRef::Synthetic {
                owner: "semantic".into(),
            };
            field.documentation = None;
        }
    };
    match &mut ty.shape {
        WireShape::Record { fields: record, .. } => fields(record),
        WireShape::Enum { variants, .. } => {
            for variant in variants {
                variant.source = WireSourceRef::Synthetic {
                    owner: "semantic".into(),
                };
                variant.documentation = None;
                fields(&mut variant.fields);
            }
        }
        _ => {}
    }
    ty
}

fn fixed_tuple_items(schema: &Value) -> Option<&[Value]> {
    let items = match schema.get("prefixItems") {
        Some(Value::Array(items)) if !items.is_empty() => items.as_slice(),
        None => &[],
        _ => return None,
    };
    (schema.get("type")?.as_str()? == "array"
        && schema.get("items")? == &Value::Bool(false)
        && schema.get("minItems")?.as_u64()? == items.len() as u64
        && schema.get("maxItems")?.as_u64()? == items.len() as u64)
        .then_some(items)
}

// A recognized keyword must constrain the selected shape, rather than being
// silently ignored because another keyword chose an earlier lowering branch.
fn supported_keyword_combination(object: &serde_json::Map<String, Value>) -> bool {
    let shape: &[&str] = if object.contains_key("$ref") {
        &["$ref", "type", "x-harn-alias"]
    } else if object.contains_key("const") {
        &["const"]
    } else if object.contains_key("oneOf") || object.contains_key("anyOf") {
        if object.contains_key("oneOf") && object.contains_key("anyOf") {
            return false;
        }
        &["oneOf", "anyOf", "x-harn-nominal"]
    } else {
        match object.get("x-harn-type").and_then(Value::as_str) {
            Some("bytes") => &[
                "type",
                "x-harn-type",
                "properties",
                "required",
                "additionalProperties",
            ],
            Some("decimal") => &["type", "x-harn-type", "format"],
            Some("set") => &["type", "x-harn-type", "items", "uniqueItems"],
            Some(_) => return false,
            None => match object.get("type").and_then(Value::as_str) {
                Some("integer") => &["type", "minimum", "maximum"],
                Some("array") => &[
                    "type",
                    "items",
                    "prefixItems",
                    "minItems",
                    "maxItems",
                    "uniqueItems",
                ],
                Some("object") => &[
                    "type",
                    "properties",
                    "required",
                    "additionalProperties",
                    "x-harn-nominal",
                ],
                Some("string") => &["type", "format"],
                Some("null" | "boolean" | "number") => &["type"],
                _ => return false,
            },
        }
    };
    object.keys().all(|key| {
        shape.contains(&key.as_str())
            || matches!(
                key.as_str(),
                "$schema"
                    | "$id"
                    | "$anchor"
                    | "$defs"
                    | "title"
                    | "description"
                    | "examples"
                    | "$comment"
                    | "x-harn-source"
                    | "x-harn-field"
                    | "x-harn-wire-id"
            )
    })
}

fn source(location: &str) -> WireSourceRef {
    let (document, pointer) = location
        .split_once('#')
        .filter(|(_, fragment)| fragment.is_empty() || fragment.starts_with('/'))
        .unwrap_or((location, ""));
    WireSourceRef::DocumentPointer {
        document: document.into(),
        pointer: pointer.into(),
    }
}

fn child(location: &str, segment: &str) -> String {
    if location.contains('#') {
        format!("{location}/{segment}")
    } else {
        format!("{location}#/{segment}")
    }
}

fn escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn error(id: &WireTypeId, location: &str, code: WireFailureCode) -> WireFailure {
    WireFailure {
        code,
        type_id: Some(id.clone()),
        source: source(location),
        structural_path: String::new(),
    }
}
