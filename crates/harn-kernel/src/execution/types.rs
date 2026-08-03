use std::collections::{BTreeMap, BTreeSet};

use harn_builtin_meta::Ty;
use serde::{Deserialize, Serialize};

use super::diagnostic;
use super::resource::{validate_data_value, validate_json_value};
use crate::{type_contract::manifest_signature_is_portable, Diagnostic};

const MAX_JSON_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq)]
pub enum DataValue {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<DataValue>),
    Record(BTreeMap<String, DataValue>),
}

// DataValue's serde representation is intentionally identical to the public
// JSON seam. Deriving serde here would turn non-finite floats into null and
// would create a second, incompatible tagged representation for snapshots and
// capability requests.
impl Serialize for DataValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_json().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DataValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        Self::from_json(value).map_err(|diagnostic| {
            serde::de::Error::custom(format!("{}: {}", diagnostic.code, diagnostic.message))
        })
    }
}

impl DataValue {
    pub fn from_json(value: serde_json::Value) -> Result<Self, Diagnostic> {
        validate_json_value(&value)?;
        Self::from_json_validated(value)
    }

    fn from_json_validated(value: serde_json::Value) -> Result<Self, Diagnostic> {
        Ok(match value {
            serde_json::Value::Null => Self::Nil,
            serde_json::Value::Bool(value) => Self::Bool(value),
            serde_json::Value::Number(value) => {
                if let Some(value) = value.as_i64() {
                    Self::Int(value)
                } else {
                    Self::Float(value.as_f64().ok_or_else(|| {
                        diagnostic(
                            "input_number",
                            "JSON number is outside Harn's numeric range",
                        )
                    })?)
                }
            }
            serde_json::Value::String(value) => Self::String(value),
            serde_json::Value::Array(values) => Self::List(
                values
                    .into_iter()
                    .map(Self::from_json_validated)
                    .collect::<Result<_, _>>()?,
            ),
            serde_json::Value::Object(entries) => {
                if entries.len() == 1 {
                    let (tag, tagged) = entries.iter().next().expect("single entry");
                    match (tag.as_str(), tagged) {
                        ("$int", serde_json::Value::String(value)) => {
                            return value.parse::<i64>().map(Self::Int).map_err(|_| {
                                diagnostic(
                                    "input_integer",
                                    "tagged integer is outside the i64 range",
                                )
                            });
                        }
                        ("$int", _) => {
                            return Err(diagnostic(
                                "input_integer",
                                "tagged integer must contain a decimal string",
                            ));
                        }
                        ("$float", serde_json::Value::String(value)) => {
                            return match value.as_str() {
                                "nan" => Ok(Self::Float(f64::NAN)),
                                "infinity" => Ok(Self::Float(f64::INFINITY)),
                                "-infinity" => Ok(Self::Float(f64::NEG_INFINITY)),
                                _ => Err(diagnostic(
                                    "input_float",
                                    "tagged float must be nan, infinity, or -infinity",
                                )),
                            };
                        }
                        ("$float", _) => {
                            return Err(diagnostic(
                                "input_float",
                                "tagged float must contain a string",
                            ));
                        }
                        ("$bytes", serde_json::Value::Array(values)) => {
                            let mut bytes = Vec::with_capacity(values.len());
                            for value in values {
                                let Some(value) =
                                    value.as_u64().and_then(|value| u8::try_from(value).ok())
                                else {
                                    return Err(diagnostic(
                                        "input_bytes",
                                        "tagged bytes must contain integers from 0 through 255",
                                    ));
                                };
                                bytes.push(value);
                            }
                            return Ok(Self::Bytes(bytes));
                        }
                        ("$bytes", _) => {
                            return Err(diagnostic(
                                "input_bytes",
                                "tagged bytes must contain an integer array",
                            ));
                        }
                        _ => {}
                    }
                }
                Self::Record(
                    entries
                        .into_iter()
                        .map(|(key, value)| Ok((key, Self::from_json_validated(value)?)))
                        .collect::<Result<_, Diagnostic>>()?,
                )
            }
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Nil => serde_json::Value::Null,
            Self::Bool(value) => (*value).into(),
            Self::Int(value) if value.unsigned_abs() <= MAX_JSON_SAFE_INTEGER as u64 => {
                (*value).into()
            }
            Self::Int(value) => serde_json::json!({"$int": value.to_string()}),
            Self::Float(value) if value.is_nan() => serde_json::json!({"$float": "nan"}),
            Self::Float(value) if *value == f64::INFINITY => {
                serde_json::json!({"$float": "infinity"})
            }
            Self::Float(value) if *value == f64::NEG_INFINITY => {
                serde_json::json!({"$float": "-infinity"})
            }
            Self::Float(value) => serde_json::Number::from_f64(*value)
                .map(serde_json::Value::Number)
                .expect("finite floats are representable as JSON numbers"),
            Self::String(value) => value.clone().into(),
            Self::Bytes(value) => serde_json::json!({"$bytes": value}),
            Self::List(values) => {
                serde_json::Value::Array(values.iter().map(Self::to_json).collect())
            }
            Self::Record(entries) => serde_json::Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_json()))
                    .collect(),
            ),
        }
    }

    pub(super) fn validate(&self) -> Result<(), Diagnostic> {
        validate_data_value(self)
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct GrantSet {
    grants: BTreeSet<String>,
    snapshot_key: Option<[u8; 32]>,
}

impl GrantSet {
    pub fn pure() -> Self {
        Self::default()
    }

    pub fn from_names(names: impl IntoIterator<Item = String>) -> Result<Self, Diagnostic> {
        let grants = names.into_iter().collect::<BTreeSet<_>>();
        for grant in &grants {
            let Some((capability, operation)) = grant.split_once('.') else {
                return Err(diagnostic(
                    "invalid_capability_grant",
                    format!("grant `{grant}` must name one exact capability operation"),
                ));
            };
            let Some(contract) =
                harn_capability_contracts::capability_method_entry(capability, operation)
            else {
                return Err(diagnostic(
                    "unknown_capability_grant",
                    format!("grant `{grant}` is not in the canonical capability registry"),
                ));
            };
            if !manifest_signature_is_portable(contract.signature) {
                return Err(diagnostic(
                    "unsupported_portable_capability_type",
                    format!(
                        "grant `{grant}` uses a capability type outside the portable value contract"
                    ),
                ));
            }
        }
        Ok(Self {
            grants,
            snapshot_key: None,
        })
    }

    /// Decode the shared host grant contract used by native and Wasm adapters.
    ///
    /// A string list grants exact operations for a terminal execution. An
    /// object additionally carries the host-owned snapshot authentication key
    /// required for suspend/resume.
    pub fn from_host_json(json: &str) -> Result<Self, Diagnostic> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct HostGrants {
            capabilities: Vec<String>,
            snapshot_key: Option<Vec<u8>>,
        }

        let input: HostGrants = serde_json::from_str(json).map_err(|error| {
            diagnostic(
                "invalid_capability_grants",
                format!("invalid grants JSON: {error}"),
            )
        })?;
        let grants = Self::from_names(input.capabilities)?;
        match input.snapshot_key {
            Some(snapshot_key) => {
                let key = decode_snapshot_key(&snapshot_key)?;
                Ok(grants.with_snapshot_key(key))
            }
            None => Ok(grants),
        }
    }

    /// Install a host-owned key that authenticates resumable snapshots.
    ///
    /// The key is never serialized by the kernel. A host that grants a
    /// suspendable capability must retain the same key until execution ends.
    pub fn with_snapshot_key(mut self, key: [u8; 32]) -> Self {
        self.snapshot_key = Some(key);
        self
    }

    pub(super) fn snapshot_key(&self) -> Option<&[u8; 32]> {
        self.snapshot_key.as_ref()
    }

    pub(super) fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        for grant in &self.grants {
            hasher.update(grant.as_bytes());
            hasher.update(&[0]);
        }
        *hasher.finalize().as_bytes()
    }

    pub fn allows(&self, capability: &str, operation: &str) -> bool {
        self.grants.contains(&format!("{capability}.{operation}"))
    }
}

fn decode_snapshot_key(encoded: &[u8]) -> Result<[u8; 32], Diagnostic> {
    if encoded.len() != 32 {
        return Err(diagnostic(
            "invalid_snapshot_key",
            "snapshotKey must contain exactly 32 bytes",
        ));
    }
    let mut key = [0_u8; 32];
    key.copy_from_slice(encoded);
    Ok(key)
}

impl std::fmt::Debug for GrantSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GrantSet")
            .field("grants", &self.grants)
            .field(
                "snapshot_key",
                &self.snapshot_key.is_some().then_some("<redacted>"),
            )
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityRequest {
    pub id: String,
    pub capability: String,
    pub operation: String,
    pub arguments: DataValue,
    pub expected: ValueShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueShape {
    Any,
    Nil,
    Bool,
    Int,
    Float,
    String,
    Bytes,
    List,
    Record,
}

impl ValueShape {
    pub(super) fn from_type(ty: Ty) -> Self {
        match ty {
            Ty::Named("nil") | Ty::Never => Self::Nil,
            Ty::Named("bool") => Self::Bool,
            Ty::Named("int") | Ty::LitInt(_) => Self::Int,
            Ty::Named("float") => Self::Float,
            Ty::Named("string") | Ty::LitString(_) => Self::String,
            Ty::Named("bytes") => Self::Bytes,
            Ty::Named("list") | Ty::Apply("list" | "List", _) => Self::List,
            Ty::Named("dict" | "record") | Ty::Shape(_) => Self::Record,
            Ty::Optional(_)
            | Ty::Any
            | Ty::Generic(_)
            | Ty::Named(_)
            | Ty::Apply(_, _)
            | Ty::Union(_)
            | Ty::Fn(_, _)
            | Ty::SchemaOf(_) => Self::Any,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CapabilityResult {
    Ok {
        request_id: String,
        value: DataValue,
    },
    Err {
        request_id: String,
        code: String,
        message: String,
    },
}

impl CapabilityResult {
    pub(super) fn request_id(&self) -> &str {
        match self {
            Self::Ok { request_id, .. } | Self::Err { request_id, .. } => request_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Execution {
    Completed {
        value: DataValue,
    },
    Suspended {
        request: CapabilityRequest,
        snapshot: Vec<u8>,
    },
    Failed {
        diagnostic: Diagnostic,
    },
}

pub(super) fn value_kind(value: &DataValue) -> &'static str {
    match value {
        DataValue::Nil => "nil",
        DataValue::Bool(_) => "bool",
        DataValue::Int(_) => "int",
        DataValue::Float(_) => "float",
        DataValue::String(_) => "string",
        DataValue::Bytes(_) => "bytes",
        DataValue::List(_) => "list",
        DataValue::Record(_) => "record",
    }
}
