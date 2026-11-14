use std::borrow::Cow;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::{DictMap, VmSet, VmValue};

use super::graph::{canonical_integer, failure, literal_json};
use super::{PreparedWireGraph, WireFailure, WireFailureCode, WireShape, WireType, WireTypeId};

/// Public boundaries use a finite depth independently of the VM's stack budget.
pub const MAX_WIRE_VALUE_DEPTH: usize = 128;
pub const ENUM_NAME_KEY: &str = "enum";
pub const ENUM_VARIANT_KEY: &str = "variant";
pub const ENUM_FIELDS_KEY: &str = "fields";

pub fn decimal_wire_pattern() -> String {
    format!(
        r"^-?(0|[1-9][0-9]*)(\.[0-9]{{1,{}}})?$",
        rust_decimal::Decimal::MAX_SCALE
    )
}

impl PreparedWireGraph {
    pub fn decode(&self, root: &WireTypeId, value: &Value) -> Result<VmValue, WireFailure> {
        self.decode_inner(root, value, 0, "")
    }

    pub fn encode(&self, root: &WireTypeId, value: &VmValue) -> Result<Value, WireFailure> {
        self.encode_inner(root, value, 0, "")
    }

    fn decode_inner(
        &self,
        id: &WireTypeId,
        value: &Value,
        depth: usize,
        path: &str,
    ) -> Result<VmValue, WireFailure> {
        let ty = self.codec_type(id, depth, path)?;
        let fail = |code| at(ty, code, path);
        match &ty.shape {
            WireShape::Null if value.is_null() => Ok(VmValue::Nil),
            WireShape::Boolean => value
                .as_bool()
                .map(VmValue::Bool)
                .ok_or_else(|| fail(WireFailureCode::InvalidValue)),
            WireShape::Integer {
                minimum, maximum, ..
            } => value
                .as_i64()
                .filter(|value| {
                    *value >= canonical_integer(minimum).unwrap()
                        && *value <= canonical_integer(maximum).unwrap()
                })
                .map(VmValue::Int)
                .ok_or_else(|| fail(WireFailureCode::InvalidValue)),
            WireShape::Float => value
                .as_f64()
                .filter(|value| value.is_finite())
                .map(VmValue::Float)
                .ok_or_else(|| fail(WireFailureCode::NonFiniteFloat)),
            WireShape::String => value
                .as_str()
                .map(VmValue::string)
                .ok_or_else(|| fail(WireFailureCode::InvalidValue)),
            WireShape::Bytes { .. } => {
                let encoded = value
                    .as_object()
                    .filter(|object| object.len() == 1)
                    .and_then(|object| object.get(crate::value::json::BYTES_B64_TAG))
                    .and_then(Value::as_str)
                    .ok_or_else(|| fail(WireFailureCode::InvalidBase64))?;
                let bytes = crate::value::json::decode_canonical_base64(encoded)
                    .ok_or_else(|| fail(WireFailureCode::InvalidBase64))?;
                Ok(VmValue::Bytes(Arc::new(bytes)))
            }
            WireShape::Decimal { .. } => {
                let spelling = value
                    .as_str()
                    .ok_or_else(|| fail(WireFailureCode::InvalidDecimal))?;
                let decimal = spelling
                    .parse::<rust_decimal::Decimal>()
                    .map_err(|_| fail(WireFailureCode::InvalidDecimal))?;
                if decimal.to_string() != spelling {
                    return Err(fail(WireFailureCode::InvalidDecimal));
                }
                Ok(VmValue::decimal(decimal))
            }
            WireShape::Literal { value: literal } if literal_json(literal) == *value => {
                Ok(crate::value::json::json_to_vm_value(value))
            }
            WireShape::List { item } | WireShape::Set { item } => {
                let items = value
                    .as_array()
                    .ok_or_else(|| fail(WireFailureCode::InvalidValue))?;
                let decoded = items
                    .iter()
                    .map(|value| self.decode_inner(item, value, depth + 1, &format!("{path}/[]")))
                    .collect::<Result<Vec<_>, _>>()?;
                if matches!(ty.shape, WireShape::Set { .. }) {
                    let mut set = VmSet::with_capacity(decoded.len());
                    for value in decoded {
                        if !set.insert(value) {
                            return Err(fail(WireFailureCode::DuplicateSetMember));
                        }
                    }
                    Ok(VmValue::set_value(set))
                } else {
                    Ok(VmValue::list(decoded))
                }
            }
            WireShape::Tuple { items } => {
                let values = value
                    .as_array()
                    .filter(|values| values.len() == items.len())
                    .ok_or_else(|| fail(WireFailureCode::InvalidValue))?;
                Ok(VmValue::list(
                    items
                        .iter()
                        .zip(values)
                        .map(|(id, value)| {
                            self.decode_inner(id, value, depth + 1, &format!("{path}/[]"))
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            }
            WireShape::Map { value: item } => {
                let values = value
                    .as_object()
                    .ok_or_else(|| fail(WireFailureCode::InvalidValue))?;
                let mut decoded = DictMap::new();
                for (key, value) in values {
                    decoded.insert(
                        key.as_str().into(),
                        self.decode_inner(item, value, depth + 1, &format!("{path}/*"))?,
                    );
                }
                Ok(VmValue::dict(decoded))
            }
            WireShape::Record {
                fields,
                additional,
                nominal,
            } => {
                let values = value
                    .as_object()
                    .ok_or_else(|| fail(WireFailureCode::InvalidValue))?;
                let mut decoded = DictMap::new();
                for field in fields {
                    match values.get(&field.name) {
                        Some(value) => {
                            decoded.insert(
                                field.name.as_str().into(),
                                self.decode_inner(
                                    &field.type_id,
                                    value,
                                    depth + 1,
                                    &format!("{path}/*"),
                                )?,
                            );
                        }
                        None if field.optional => {}
                        None => return Err(fail(WireFailureCode::MissingField)),
                    }
                }
                for (key, value) in values {
                    if fields.iter().any(|field| field.name == *key) {
                        continue;
                    }
                    let Some(id) = additional else {
                        return Err(fail(WireFailureCode::UnknownField));
                    };
                    decoded.insert(
                        key.as_str().into(),
                        self.decode_inner(id, value, depth + 1, &format!("{path}/*"))?,
                    );
                }
                Ok(match nominal {
                    Some(nominal) => VmValue::struct_instance_with_layout(
                        nominal.name.clone(),
                        fields.iter().map(|field| field.name.clone()).collect(),
                        decoded,
                    ),
                    None => VmValue::dict(decoded),
                })
            }
            WireShape::Enum { nominal, variants } => {
                let object = value
                    .as_object()
                    .filter(|object| object.len() == 3)
                    .ok_or_else(|| fail(WireFailureCode::InvalidNominalDiscriminator))?;
                if object.get(ENUM_NAME_KEY).and_then(Value::as_str) != Some(&nominal.name) {
                    return Err(fail(WireFailureCode::InvalidNominalDiscriminator));
                }
                let variant = object
                    .get(ENUM_VARIANT_KEY)
                    .and_then(Value::as_str)
                    .and_then(|name| variants.iter().find(|variant| variant.name == name))
                    .ok_or_else(|| fail(WireFailureCode::InvalidNominalDiscriminator))?;
                let values = object
                    .get(ENUM_FIELDS_KEY)
                    .and_then(Value::as_array)
                    .filter(|values| values.len() == variant.fields.len())
                    .ok_or_else(|| fail(WireFailureCode::InvalidValue))?;
                let fields = variant
                    .fields
                    .iter()
                    .zip(values)
                    .map(|(field, value)| {
                        self.decode_inner(&field.type_id, value, depth + 1, &format!("{path}/*/[]"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(VmValue::enum_variant(
                    nominal.name.as_str(),
                    variant.name.as_str(),
                    fields,
                ))
            }
            WireShape::Union { members } => exactly_one(
                members
                    .iter()
                    .filter_map(|id| self.decode_inner(id, value, depth + 1, path).ok()),
            )
            .ok_or_else(|| fail(WireFailureCode::InvalidValue)),
            WireShape::Alias { .. } => unreachable!("codec_type resolves aliases"),
            _ => Err(fail(WireFailureCode::InvalidValue)),
        }
    }

    fn encode_inner(
        &self,
        id: &WireTypeId,
        value: &VmValue,
        depth: usize,
        path: &str,
    ) -> Result<Value, WireFailure> {
        let ty = self.codec_type(id, depth, path)?;
        let fail = |code| at(ty, code, path);
        match (&ty.shape, value) {
            (WireShape::Null, VmValue::Nil) => Ok(Value::Null),
            (WireShape::Boolean, VmValue::Bool(value)) => Ok(json!(value)),
            (
                WireShape::Integer {
                    minimum, maximum, ..
                },
                VmValue::Int(value),
            ) if *value >= canonical_integer(minimum).unwrap()
                && *value <= canonical_integer(maximum).unwrap() =>
            {
                Ok(json!(value))
            }
            (WireShape::Float, VmValue::Float(value)) if value.is_finite() => Ok(json!(value)),
            (WireShape::Float, VmValue::Int(value)) => Ok(json!(value)),
            (WireShape::String, VmValue::String(value)) => Ok(json!(value.as_str())),
            (WireShape::Bytes { .. }, VmValue::Bytes(bytes)) => {
                Ok(crate::value::json::tagged_bytes_json(bytes))
            }
            (WireShape::Decimal { .. }, VmValue::Decimal(value)) => Ok(json!(value.to_string())),
            (WireShape::Literal { value: literal }, _) => {
                let encoded = match (literal, value) {
                    (super::WireLiteral::Null, VmValue::Nil) => Value::Null,
                    (super::WireLiteral::Boolean { .. }, VmValue::Bool(value)) => json!(value),
                    (super::WireLiteral::String { .. }, VmValue::String(value)) => {
                        json!(value.as_str())
                    }
                    (super::WireLiteral::Integer { .. }, VmValue::Int(value)) => json!(value),
                    (super::WireLiteral::Float { .. }, VmValue::Float(value))
                        if value.is_finite() =>
                    {
                        json!(value)
                    }
                    _ => return Err(fail(WireFailureCode::WrongRuntimeType)),
                };
                if encoded == literal_json(literal) {
                    Ok(encoded)
                } else {
                    Err(fail(WireFailureCode::WrongRuntimeType))
                }
            }
            (WireShape::List { item }, VmValue::List(values)) => {
                self.encode_sequence(item, values.iter(), depth, path)
            }
            (WireShape::Set { item }, VmValue::Set(values)) => {
                self.encode_sequence(item, values.iter(), depth, path)
            }
            (WireShape::Tuple { items }, VmValue::List(values)) if items.len() == values.len() => {
                Ok(Value::Array(
                    items
                        .iter()
                        .zip(values.iter())
                        .map(|(id, value)| {
                            self.encode_inner(id, value, depth + 1, &format!("{path}/[]"))
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            }
            (WireShape::Map { value: item }, _) => {
                let values =
                    object_fields(value).ok_or_else(|| fail(WireFailureCode::WrongRuntimeType))?;
                let mut encoded = serde_json::Map::new();
                for (key, value) in values.iter() {
                    encoded.insert(
                        key.to_string(),
                        self.encode_inner(item, value, depth + 1, &format!("{path}/*"))?,
                    );
                }
                Ok(Value::Object(encoded))
            }
            (
                WireShape::Record {
                    fields,
                    additional,
                    nominal,
                },
                _,
            ) => {
                if nominal
                    .as_ref()
                    .is_some_and(|nominal| value.struct_name() != Some(&nominal.name))
                {
                    return Err(fail(WireFailureCode::WrongRuntimeType));
                }
                let values =
                    object_fields(value).ok_or_else(|| fail(WireFailureCode::WrongRuntimeType))?;
                let mut encoded = serde_json::Map::new();
                for field in fields {
                    match values.get(field.name.as_str()) {
                        Some(value) => {
                            encoded.insert(
                                field.name.clone(),
                                self.encode_inner(
                                    &field.type_id,
                                    value,
                                    depth + 1,
                                    &format!("{path}/*"),
                                )?,
                            );
                        }
                        None if field.optional => {}
                        None => return Err(fail(WireFailureCode::MissingField)),
                    }
                }
                for (key, value) in values.iter() {
                    if fields.iter().any(|field| field.name == key.as_str()) {
                        continue;
                    }
                    let Some(id) = additional else {
                        return Err(fail(WireFailureCode::UnknownField));
                    };
                    encoded.insert(
                        key.to_string(),
                        self.encode_inner(id, value, depth + 1, &format!("{path}/*"))?,
                    );
                }
                Ok(Value::Object(encoded))
            }
            (WireShape::Enum { nominal, variants }, VmValue::EnumVariant(value))
                if value.enum_name.as_str() == nominal.name =>
            {
                let variant = variants
                    .iter()
                    .find(|variant| {
                        variant.name == value.variant.as_str()
                            && variant.fields.len() == value.fields.len()
                    })
                    .ok_or_else(|| fail(WireFailureCode::InvalidNominalDiscriminator))?;
                let fields = variant
                    .fields
                    .iter()
                    .zip(value.fields.iter())
                    .map(|(field, value)| {
                        self.encode_inner(&field.type_id, value, depth + 1, &format!("{path}/*/[]"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(
                    json!({(ENUM_NAME_KEY):nominal.name,(ENUM_VARIANT_KEY):variant.name,(ENUM_FIELDS_KEY):fields}),
                )
            }
            (WireShape::Union { members }, _) => exactly_one(
                members
                    .iter()
                    .filter_map(|id| self.encode_inner(id, value, depth + 1, path).ok()),
            )
            .ok_or_else(|| fail(WireFailureCode::WrongRuntimeType)),
            _ => Err(fail(WireFailureCode::WrongRuntimeType)),
        }
    }

    fn codec_type(
        &self,
        id: &WireTypeId,
        depth: usize,
        path: &str,
    ) -> Result<&WireType, WireFailure> {
        let ty = self.unaliased(id)?;
        if depth > MAX_WIRE_VALUE_DEPTH {
            Err(at(ty, WireFailureCode::MaximumDepth, path))
        } else {
            Ok(ty)
        }
    }

    fn encode_sequence<'a>(
        &self,
        item: &WireTypeId,
        values: impl Iterator<Item = &'a VmValue>,
        depth: usize,
        path: &str,
    ) -> Result<Value, WireFailure> {
        Ok(Value::Array(
            values
                .map(|value| self.encode_inner(item, value, depth + 1, &format!("{path}/[]")))
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }
}

fn object_fields(value: &VmValue) -> Option<Cow<'_, DictMap>> {
    match value {
        VmValue::Dict(values) => Some(Cow::Borrowed(values.as_ref())),
        VmValue::StructInstance(_) => value.struct_fields_map().map(Cow::Owned),
        _ => None,
    }
}

fn exactly_one<T>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let first = values.next()?;
    if values.next().is_none() {
        Some(first)
    } else {
        None
    }
}

fn at(ty: &WireType, code: WireFailureCode, path: &str) -> WireFailure {
    let mut failure = failure(ty, code);
    failure.structural_path = path.into();
    failure
}
