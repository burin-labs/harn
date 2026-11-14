//! Resolved wire types shared by schemas, codecs and portable code generation.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::{
    WireFailure, WireFailureCode, WireLiteral, WireShape, WireSourceRef, WireType, WireTypeId,
};

/// JSON integer tokens accepted by the signed integer codec, before its bounds.
pub const WIRE_INTEGER_TOKEN_PATTERN: &str = "^(0|-?[1-9][0-9]*)$";

/// Immutable graph. Every edge and union discriminator is checked at preparation.
#[derive(Clone, Debug)]
pub struct PreparedWireGraph {
    types: BTreeMap<WireTypeId, WireType>,
    alias_targets: BTreeMap<WireTypeId, WireTypeId>,
}

impl PreparedWireGraph {
    pub fn prepare(types: impl IntoIterator<Item = WireType>) -> Result<Self, WireFailure> {
        let mut entries = BTreeMap::new();
        for ty in types {
            if ty.id.0.is_empty() || entries.contains_key(&ty.id) {
                return Err(failure(&ty, WireFailureCode::InvalidContract));
            }
            validate_source(&ty.source)
                .map_err(|()| failure(&ty, WireFailureCode::InvalidContract))?;
            entries.insert(ty.id.clone(), ty);
        }
        let mut graph = Self {
            types: entries,
            alias_targets: BTreeMap::new(),
        };
        for ty in graph.types.values() {
            for reference in references(&ty.shape) {
                if !graph.types.contains_key(reference) {
                    return Err(failure(ty, WireFailureCode::UnresolvedReference));
                }
            }
            match &ty.shape {
                WireShape::Integer {
                    minimum, maximum, ..
                } => {
                    let low = canonical_integer(minimum);
                    let high = canonical_integer(maximum);
                    if !matches!((low, high), (Some(low), Some(high)) if low <= high) {
                        return Err(failure(ty, WireFailureCode::InvalidRange));
                    }
                }
                WireShape::Literal { value } => validate_literal(value)
                    .map_err(|()| failure(ty, WireFailureCode::InvalidContract))?,
                WireShape::Record {
                    fields,
                    nominal,
                    additional,
                } => {
                    validate_fields(fields)
                        .map_err(|()| failure(ty, WireFailureCode::InvalidContract))?;
                    if nominal.as_ref().is_some_and(|nominal| {
                        nominal.name.is_empty()
                            || nominal.declaration_id.is_empty()
                            || additional.is_some()
                    }) {
                        return Err(failure(ty, WireFailureCode::InvalidContract));
                    }
                }
                WireShape::Enum { nominal, variants } => {
                    let mut names = BTreeSet::new();
                    let mut identities = BTreeSet::new();
                    if nominal.name.is_empty()
                        || nominal.declaration_id.is_empty()
                        || variants.is_empty()
                    {
                        return Err(failure(ty, WireFailureCode::InvalidContract));
                    }
                    for variant in variants {
                        if variant.name.is_empty()
                            || variant.id.is_empty()
                            || !names.insert(&variant.name)
                            || !identities.insert(&variant.id)
                            || validate_source(&variant.source).is_err()
                            || validate_fields(&variant.fields).is_err()
                            || variant.fields.iter().any(|field| field.optional)
                        {
                            return Err(failure(ty, WireFailureCode::InvalidContract));
                        }
                    }
                }
                WireShape::Alias { declaration, .. } => {
                    if declaration.name.is_empty() || declaration.declaration_id.is_empty() {
                        return Err(failure(ty, WireFailureCode::InvalidContract));
                    }
                }
                WireShape::Union { members } => {
                    if members.len() < 2
                        || members.iter().collect::<BTreeSet<_>>().len() != members.len()
                    {
                        return Err(failure(ty, WireFailureCode::InvalidContract));
                    }
                }
                _ => {}
            }
        }
        graph.resolve_alias_targets()?;
        // All primitive bounds and references are valid before disjointness reads them.
        for ty in graph.types.values() {
            if let WireShape::Union { members } = &ty.shape {
                for (index, left) in members.iter().enumerate() {
                    for right in &members[index + 1..] {
                        if !graph.disjoint(left, right, &mut BTreeSet::new())? {
                            return Err(failure(ty, WireFailureCode::AmbiguousUnion));
                        }
                    }
                }
            }
        }
        Ok(graph)
    }

    pub fn get(&self, id: &WireTypeId) -> Option<&WireType> {
        self.types.get(id)
    }

    pub fn types(&self) -> impl Iterator<Item = &WireType> {
        self.types.values()
    }

    pub(crate) fn has_recursive_types(&self) -> bool {
        let mut active = BTreeSet::new();
        let mut complete = BTreeSet::new();
        for root in self.types.keys() {
            let mut pending = vec![(root, false)];
            while let Some((id, leaving)) = pending.pop() {
                if leaving {
                    active.remove(id);
                    complete.insert(id);
                    continue;
                }
                if complete.contains(id) {
                    continue;
                }
                if !active.insert(id) {
                    return true;
                }
                pending.push((id, true));
                pending.extend(
                    references(&self.types[id].shape)
                        .into_iter()
                        .map(|edge| (edge, false)),
                );
            }
        }
        false
    }

    pub(crate) fn unaliased(&self, id: &WireTypeId) -> Result<&WireType, WireFailure> {
        let target = self.alias_targets.get(id).unwrap_or(id);
        self.types.get(target).ok_or_else(|| WireFailure {
            code: WireFailureCode::UnresolvedReference,
            type_id: Some(id.clone()),
            source: WireSourceRef::Synthetic {
                owner: "wire".into(),
            },
            structural_path: String::new(),
        })
    }

    fn resolve_alias_targets(&mut self) -> Result<(), WireFailure> {
        for root in self.types.keys() {
            let mut trail = Vec::new();
            let mut visiting = BTreeSet::new();
            let mut current = root;
            let target = loop {
                if let Some(target) = self.alias_targets.get(current) {
                    break target.clone();
                }
                let ty = &self.types[current];
                match &ty.shape {
                    WireShape::Alias { target, .. } => {
                        if !visiting.insert(current) {
                            return Err(failure(ty, WireFailureCode::RecursiveExpansion));
                        }
                        trail.push(current);
                        current = target;
                    }
                    _ => break current.clone(),
                }
            };
            for alias in trail {
                self.alias_targets.insert(alias.clone(), target.clone());
            }
        }
        Ok(())
    }

    /// Draft 2020-12 projection uses graph references, including recursive edges.
    pub fn json_schema(&self, root: &WireTypeId) -> Result<Value, WireFailure> {
        let Some(ty) = self.get(root) else {
            return Err(WireFailure {
                code: WireFailureCode::UnresolvedReference,
                type_id: Some(root.clone()),
                source: WireSourceRef::Synthetic {
                    owner: "wire".into(),
                },
                structural_path: String::new(),
            });
        };
        let mut definitions = serde_json::Map::new();
        let mut pending = vec![ty.id.clone()];
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let entry = &self.types[&id];
            pending.extend(references(&entry.shape).into_iter().cloned());
            definitions.insert(id.0, self.schema_node(entry));
        }
        let mut schema = self.schema_node(ty);
        let object = schema.as_object_mut().expect("schema node is an object");
        object.insert(
            "$schema".into(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        object.insert("$defs".into(), Value::Object(definitions));
        if matches!(self.unaliased(root)?.shape, WireShape::Record { .. }) {
            object.insert("type".into(), json!("object"));
        }
        Ok(schema)
    }

    fn schema_node(&self, ty: &WireType) -> Value {
        let field_record = |fields: &[super::WireField], additional: &Option<WireTypeId>| {
            let properties: serde_json::Map<String, Value> = fields
                .iter()
                .map(|field| (field.name.clone(), field_schema(field)))
                .collect();
            let required: Vec<&str> = fields
                .iter()
                .filter(|field| !field.optional)
                .map(|field| field.name.as_str())
                .collect();
            json!({"type":"object", "properties":properties, "required":required,
                "additionalProperties":additional.as_ref().map_or(json!(false), |id| json!({"$ref":pointer(id)}))})
        };
        let mut result = match &ty.shape {
            WireShape::Null => json!({"type":"null"}),
            WireShape::Boolean => json!({"type":"boolean"}),
            WireShape::Integer {
                minimum, maximum, ..
            } => json!({"type":"integer",
                "minimum": canonical_integer(minimum).expect("validated bound"),
                "maximum": canonical_integer(maximum).expect("validated bound")}),
            WireShape::Float => json!({"type":"number"}),
            WireShape::String => json!({"type":"string"}),
            WireShape::Bytes { .. } => Value::Object(crate::value::json::bytes_json_schema(false)),
            WireShape::Decimal { .. } => {
                json!({"type":"string", "format":"harn-decimal", "x-harn-type":"decimal"})
            }
            WireShape::Literal { value } => json!({"const":literal_json(value)}),
            WireShape::List { item } => json!({"type":"array","items":{"$ref":pointer(item)}}),
            WireShape::Set { item } => {
                json!({"type":"array","items":{"$ref":pointer(item)},"uniqueItems":true,"x-harn-type":"set"})
            }
            WireShape::Tuple { items } => {
                tuple_schema(items.iter().map(|id| json!({"$ref":pointer(id)})).collect())
            }
            WireShape::Map { value } => {
                json!({"type":"object","additionalProperties":{"$ref":pointer(value)}})
            }
            WireShape::Record {
                fields, additional, ..
            } => field_record(fields, additional),
            WireShape::Enum { nominal, variants } => {
                json!({"oneOf": variants.iter().map(|variant| json!({
                "x-harn-variant":super::metadata::VariantOrigin { id:variant.id.clone(),name:variant.name.clone(),source:variant.source.clone(),documentation:variant.documentation.clone() },
                "type":"object", "properties":{"enum":{"const":nominal.name},"variant":{"const":variant.name},
                    "fields":tuple_schema(variant.fields.iter().map(field_schema).collect())},
                "required":["enum","variant","fields"],"additionalProperties":false})).collect::<Vec<_>>()})
            }
            WireShape::Union { members } => {
                json!({"oneOf":members.iter().map(|id| json!({"$ref":pointer(id)})).collect::<Vec<_>>()})
            }
            WireShape::Alias { target, .. } => json!({"$ref":pointer(target)}),
        };
        let object = result.as_object_mut().expect("schema nodes are objects");
        object.insert("x-harn-wire-id".into(), json!(ty.id));
        match &ty.shape {
            WireShape::Record {
                nominal: Some(nominal),
                ..
            }
            | WireShape::Enum { nominal, .. } => {
                object.insert(
                    "x-harn-nominal".into(),
                    serde_json::to_value(nominal).expect("nominal identity serializes"),
                );
            }
            WireShape::Alias { declaration, .. } => {
                object.insert(
                    "x-harn-alias".into(),
                    serde_json::to_value(declaration).expect("alias identity serializes"),
                );
            }
            _ => {}
        }
        object.insert(
            "x-harn-source".into(),
            serde_json::to_value(&ty.source).expect("source serializes"),
        );
        if let Some(documentation) = &ty.documentation {
            object.insert("description".into(), json!(documentation));
        }
        if let Some(name) = &ty.name {
            object.insert("title".into(), json!(name));
        }
        result
    }

    fn disjoint(
        &self,
        left: &WireTypeId,
        right: &WireTypeId,
        visited: &mut BTreeSet<(WireTypeId, WireTypeId)>,
    ) -> Result<bool, WireFailure> {
        if !visited.insert((left.clone(), right.clone())) {
            return Ok(false);
        }
        let left = self.unaliased(left)?;
        let right = self.unaliased(right)?;
        if let WireShape::Union { members } = &left.shape {
            for member in members {
                if !self.disjoint(member, &right.id, &mut visited.clone())? {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        if matches!(right.shape, WireShape::Union { .. }) {
            return self.disjoint(&right.id, &left.id, visited);
        }
        if let (WireShape::Literal { value: a }, WireShape::Literal { value: b }) =
            (&left.shape, &right.shape)
        {
            return Ok(literal_json(a) != literal_json(b));
        }
        if category(&left.shape) & category(&right.shape) == 0 {
            return Ok(true);
        }
        if let (
            WireShape::Integer {
                minimum: a,
                maximum: b,
                ..
            },
            WireShape::Integer {
                minimum: c,
                maximum: d,
                ..
            },
        ) = (&left.shape, &right.shape)
        {
            return Ok(
                canonical_integer(b).unwrap() < canonical_integer(c).unwrap()
                    || canonical_integer(d).unwrap() < canonical_integer(a).unwrap(),
            );
        }
        if let (WireShape::Tuple { items: a }, WireShape::Tuple { items: b }) =
            (&left.shape, &right.shape)
        {
            if a.len() != b.len() {
                return Ok(true);
            }
        }
        if let (
            WireShape::Enum {
                nominal: a,
                variants: av,
            },
            WireShape::Enum {
                nominal: b,
                variants: bv,
            },
        ) = (&left.shape, &right.shape)
        {
            return Ok(a.name != b.name || av.iter().all(|a| bv.iter().all(|b| a.name != b.name)));
        }
        if let (WireShape::Record { fields: a, .. }, WireShape::Record { fields: b, .. }) =
            (&left.shape, &right.shape)
        {
            for field in a.iter().filter(|field| !field.optional) {
                if let Some(other) = b
                    .iter()
                    .find(|other| other.name == field.name && !other.optional)
                {
                    if self.disjoint(&field.type_id, &other.type_id, &mut visited.clone())? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}

fn tuple_schema(items: Vec<Value>) -> Value {
    let mut schema =
        json!({"type":"array","items":false,"minItems":items.len(),"maxItems":items.len()});
    // Draft 2020-12 requires prefixItems itself to contain at least one item.
    if !items.is_empty() {
        schema["prefixItems"] = Value::Array(items);
    }
    schema
}

pub(crate) fn references(shape: &WireShape) -> Vec<&WireTypeId> {
    match shape {
        WireShape::List { item } | WireShape::Set { item } => vec![item],
        WireShape::Tuple { items } | WireShape::Union { members: items } => items.iter().collect(),
        WireShape::Map { value } => vec![value],
        WireShape::Record {
            fields,
            additional,
            nominal,
        } => fields
            .iter()
            .map(|field| &field.type_id)
            .chain(additional.iter())
            .chain(nominal.iter().flat_map(|value| &value.arguments))
            .collect(),
        WireShape::Enum { nominal, variants } => variants
            .iter()
            .flat_map(|variant| variant.fields.iter().map(|field| &field.type_id))
            .chain(&nominal.arguments)
            .collect(),
        WireShape::Alias {
            target,
            declaration,
        } => std::iter::once(target)
            .chain(&declaration.arguments)
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn validate_source(source: &WireSourceRef) -> Result<(), ()> {
    match source {
        WireSourceRef::TextSpan {
            document,
            start,
            end,
        } if !document.is_empty() && start <= end => Ok(()),
        WireSourceRef::DocumentPointer { document, pointer }
            if !document.is_empty() && (pointer.is_empty() || pointer.starts_with('/')) =>
        {
            Ok(())
        }
        WireSourceRef::Synthetic { owner } if !owner.is_empty() => Ok(()),
        _ => Err(()),
    }
}

fn validate_fields(fields: &[super::WireField]) -> Result<(), ()> {
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for field in fields {
        if field.id.is_empty() || !ids.insert(&field.id) || !names.insert(&field.name) {
            return Err(());
        }
        validate_source(&field.source)?;
    }
    Ok(())
}

pub(crate) fn canonical_integer(value: &str) -> Option<i64> {
    value
        .parse::<i64>()
        .ok()
        .filter(|parsed| parsed.to_string() == value)
}

fn validate_literal(value: &WireLiteral) -> Result<(), ()> {
    match value {
        WireLiteral::Integer { value } if canonical_integer(value).is_none() => Err(()),
        WireLiteral::Float { value } if !value.parse::<f64>().is_ok_and(f64::is_finite) => Err(()),
        _ => Ok(()),
    }
}

pub(crate) fn literal_json(value: &WireLiteral) -> Value {
    match value {
        WireLiteral::Null => Value::Null,
        WireLiteral::Boolean { value } => json!(value),
        WireLiteral::String { value } => json!(value),
        WireLiteral::Integer { value } => {
            json!(canonical_integer(value).expect("validated integer"))
        }
        WireLiteral::Float { value } => json!(value.parse::<f64>().expect("validated float")),
    }
}

fn category(shape: &WireShape) -> u8 {
    match shape {
        WireShape::Null
        | WireShape::Literal {
            value: WireLiteral::Null,
        } => 1,
        WireShape::Boolean
        | WireShape::Literal {
            value: WireLiteral::Boolean { .. },
        } => 2,
        WireShape::Integer { .. }
        | WireShape::Float
        | WireShape::Literal {
            value: WireLiteral::Integer { .. } | WireLiteral::Float { .. },
        } => 4,
        WireShape::String
        | WireShape::Decimal { .. }
        | WireShape::Literal {
            value: WireLiteral::String { .. },
        } => 8,
        WireShape::List { .. } | WireShape::Set { .. } | WireShape::Tuple { .. } => 16,
        WireShape::Map { .. }
        | WireShape::Record { .. }
        | WireShape::Enum { .. }
        | WireShape::Bytes { .. } => 32,
        WireShape::Union { .. } | WireShape::Alias { .. } => 63,
    }
}

pub(crate) fn failure(ty: &WireType, code: WireFailureCode) -> WireFailure {
    WireFailure {
        code,
        type_id: Some(ty.id.clone()),
        source: ty.source.clone(),
        structural_path: String::new(),
    }
}

fn pointer(id: &WireTypeId) -> String {
    format!("#/$defs/{}", id.0.replace('~', "~0").replace('/', "~1"))
}

fn field_schema(field: &super::WireField) -> Value {
    json!({"$ref":pointer(&field.type_id),"x-harn-field":super::metadata::FieldOrigin {
        id:field.id.clone(),name:field.name.clone(),source:field.source.clone(),documentation:field.documentation.clone() }})
}
