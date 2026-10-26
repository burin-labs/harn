//! Conversion between `VmValue` and `serde_json::Value`.
//!
//! These conversions need nothing above the value layer, so they live here
//! rather than in `schema`, `stdlib`, or `llm`, which re-export them. The VM
//! core reaches them without depending upward (#9112).
use super::VmValue;

impl From<harn_kernel::DataValue> for VmValue {
    fn from(value: harn_kernel::DataValue) -> Self {
        use harn_kernel::DataValue;
        use std::sync::Arc;
        match value {
            DataValue::Nil => Self::Nil,
            DataValue::Bool(value) => Self::Bool(value),
            DataValue::Int(value) => Self::Int(value),
            DataValue::Float(value) => Self::Float(value),
            DataValue::String(value) => Self::String(value.into()),
            DataValue::Bytes(value) => Self::Bytes(Arc::new(value)),
            DataValue::List(values) => {
                Self::List(Arc::new(values.into_iter().map(Self::from).collect()))
            }
            DataValue::Record(entries) => Self::dict(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, Self::from(value))),
            ),
        }
    }
}

pub const BYTES_B64_TAG: &str = "$bytes_b64";

pub fn tagged_bytes_json(bytes: &[u8]) -> serde_json::Value {
    use base64::Engine;

    serde_json::json!({
        BYTES_B64_TAG: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

pub(crate) fn bytes_json_schema(openapi_style: bool) -> serde_json::Map<String, serde_json::Value> {
    let encoded = if openapi_style {
        serde_json::json!({"type": "string", "format": "byte"})
    } else {
        serde_json::json!({"type": "string", "contentEncoding": "base64"})
    };
    [
        ("type".into(), serde_json::json!("object")),
        ("x-harn-type".into(), serde_json::json!("bytes")),
        (
            "properties".into(),
            serde_json::json!({BYTES_B64_TAG: encoded}),
        ),
        ("required".into(), serde_json::json!([BYTES_B64_TAG])),
        ("additionalProperties".into(), serde_json::json!(false)),
    ]
    .into_iter()
    .collect()
}

pub(crate) fn decode_tagged_bytes(value: &VmValue) -> Option<VmValue> {
    use base64::Engine;

    let fields = value.as_dict()?;
    if fields.len() != 1 {
        return None;
    }
    let VmValue::String(encoded) = fields.get(BYTES_B64_TAG)? else {
        return None;
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .ok()?;
    Some(VmValue::Bytes(std::sync::Arc::new(bytes)))
}

pub fn json_to_vm_value(jv: &serde_json::Value) -> VmValue {
    match jv {
        serde_json::Value::Null => VmValue::Nil,
        serde_json::Value::Bool(b) => VmValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                VmValue::Int(i)
            } else {
                VmValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => VmValue::string(s.as_str()),
        serde_json::Value::Array(arr) => VmValue::list(arr.iter().map(json_to_vm_value).collect()),
        serde_json::Value::Object(map) => {
            let mut m = super::DictMap::new();
            for (k, v) in map {
                m.insert(super::intern_key(k), json_to_vm_value(v));
            }
            VmValue::dict(m)
        }
    }
}

/// Convert a VmValue dict to serde_json::Value for API payloads.
pub fn vm_value_dict_to_json(dict: &super::DictMap) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in dict {
        map.insert(k.to_string(), vm_value_to_json(v));
    }
    serde_json::Value::Object(map)
}

pub fn vm_value_to_json(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::Int(i) => serde_json::json!(i),
        VmValue::Float(f) => serde_json::json!(f),
        // Decimal crosses the host bridge as a string to preserve exact
        // precision (binary-float JSON numbers would corrupt money values).
        VmValue::Decimal(d) => serde_json::json!(d.to_string()),
        VmValue::String(s) => serde_json::json!(s.as_str()),
        VmValue::Bytes(bytes) => tagged_bytes_json(bytes),
        VmValue::Bool(b) => serde_json::json!(b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(list) => {
            serde_json::Value::Array(list.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(d) => vm_value_dict_to_json(d),
        VmValue::StructInstance(_) => {
            vm_value_dict_to_json(&val.struct_fields_map().unwrap_or_default())
        }
        _ => serde_json::json!(val.display()),
    }
}
