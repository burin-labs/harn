use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tool_annotations::{SideEffectLevel, ToolAnnotations, ToolKind};
use crate::value::{VmDictExt, VmError, VmValue};

use super::manifest::{BindingManifestEntry, BindingPolicyStatus};

pub const COMPOSITION_STATE_SCHEMA_VERSION: u32 = 1;
pub const COMPOSITION_STATE_CAPABILITY: &str = "composition_state";

const STATE_BINDING_NAME: &str = "state";
const DEFAULT_MAX_VALUE_BYTES: u64 = 16 * 1024;
const DEFAULT_MAX_TOTAL_BYTES: u64 = 64 * 1024;
const DEFAULT_MAX_KEYS: u64 = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompositionStateBinding {
    pub schema_version: u32,
    pub max_value_bytes: u64,
    pub max_total_bytes: u64,
    pub max_keys: u64,
}

impl Default for CompositionStateBinding {
    fn default() -> Self {
        Self {
            schema_version: COMPOSITION_STATE_SCHEMA_VERSION,
            max_value_bytes: DEFAULT_MAX_VALUE_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            max_keys: DEFAULT_MAX_KEYS,
        }
    }
}

impl CompositionStateBinding {
    pub fn validate(&self) -> Result<(), CompositionStateError> {
        if self.schema_version != COMPOSITION_STATE_SCHEMA_VERSION {
            return Err(CompositionStateError::invalid_limits(format!(
                "unsupported state schema_version={} (expected {})",
                self.schema_version, COMPOSITION_STATE_SCHEMA_VERSION
            )));
        }
        if self.max_value_bytes == 0 {
            return Err(CompositionStateError::invalid_limits(
                "max_value_bytes must be greater than zero",
            ));
        }
        if self.max_total_bytes == 0 {
            return Err(CompositionStateError::invalid_limits(
                "max_total_bytes must be greater than zero",
            ));
        }
        if self.max_keys == 0 {
            return Err(CompositionStateError::invalid_limits(
                "max_keys must be greater than zero",
            ));
        }
        if self.max_value_bytes > self.max_total_bytes {
            return Err(CompositionStateError::invalid_limits(
                "max_value_bytes cannot exceed max_total_bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompositionStateErrorCode {
    InvalidLimits,
    SessionRequired,
    InvalidKey,
    NonJsonValue,
    ValueTooLarge,
    TotalTooLarge,
    TooManyKeys,
}

impl CompositionStateErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::SessionRequired => "session_required",
            Self::InvalidKey => "invalid_key",
            Self::NonJsonValue => "non_json_value",
            Self::ValueTooLarge => "value_too_large",
            Self::TotalTooLarge => "total_too_large",
            Self::TooManyKeys => "too_many_keys",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompositionStateError {
    pub code: CompositionStateErrorCode,
    pub operation: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<u64>,
}

impl CompositionStateError {
    pub fn invalid_limits(message: impl Into<String>) -> Self {
        Self {
            code: CompositionStateErrorCode::InvalidLimits,
            operation: "configure".to_string(),
            message: message.into(),
            key: None,
            limit: None,
            actual: None,
        }
    }

    pub fn session_required(operation: &str) -> Self {
        Self {
            code: CompositionStateErrorCode::SessionRequired,
            operation: operation.to_string(),
            message: "state operations require a non-empty composition session_id".to_string(),
            key: None,
            limit: None,
            actual: None,
        }
    }

    pub fn invalid_key(operation: &str, message: impl Into<String>) -> Self {
        Self {
            code: CompositionStateErrorCode::InvalidKey,
            operation: operation.to_string(),
            message: message.into(),
            key: None,
            limit: None,
            actual: None,
        }
    }

    pub fn non_json(operation: &str, key: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code: CompositionStateErrorCode::NonJsonValue,
            operation: operation.to_string(),
            message: message.into(),
            key: key.map(ToOwned::to_owned),
            limit: None,
            actual: None,
        }
    }

    fn for_key(
        code: CompositionStateErrorCode,
        operation: &str,
        key: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            operation: operation.to_string(),
            message: message.into(),
            key: Some(key.to_string()),
            limit: None,
            actual: None,
        }
    }

    fn with_bound(mut self, limit: u64, actual: u64) -> Self {
        self.limit = Some(limit);
        self.actual = Some(actual);
        self
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            serde_json::json!({
                "code": self.code.as_str(),
                "operation": self.operation,
                "message": self.message,
            })
        })
    }

    pub fn into_vm_error(self) -> VmError {
        let mut fields = crate::value::DictMap::new();
        fields.put_str("type", "composition_state_error");
        fields.put_str("code", self.code.as_str());
        fields.put_str("operation", &self.operation);
        fields.put_str("message", &self.message);
        if let Some(key) = self.key {
            fields.put_str("key", key);
        }
        if let Some(limit) = self.limit {
            fields.put(
                "limit",
                VmValue::Int(i64::try_from(limit).unwrap_or(i64::MAX)),
            );
        }
        if let Some(actual) = self.actual {
            fields.put(
                "actual",
                VmValue::Int(i64::try_from(actual).unwrap_or(i64::MAX)),
            );
        }
        VmError::Thrown(VmValue::dict(fields))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct CompositionStateScope {
    session_id: String,
    tool_window_id: String,
}

impl CompositionStateScope {
    pub fn new(session_id: String, tool_window_id: String) -> Self {
        Self {
            session_id,
            tool_window_id,
        }
    }

    pub fn tool_window_id(&self) -> &str {
        &self.tool_window_id
    }
}

#[derive(Default)]
struct StateWindow {
    values: BTreeMap<String, Value>,
    total_bytes: u64,
}

static STATE_WINDOWS: LazyLock<parking_lot::Mutex<BTreeMap<CompositionStateScope, StateWindow>>> =
    LazyLock::new(|| parking_lot::Mutex::new(BTreeMap::new()));

fn ensure_cleanup_hook() {
    static REGISTRATION: OnceLock<crate::llm::SessionCloseHookRegistration> = OnceLock::new();
    REGISTRATION.get_or_init(|| {
        crate::llm::register_session_close_hook(Arc::new(|session_id| {
            STATE_WINDOWS
                .lock()
                .retain(|scope, _| scope.session_id != session_id);
        }))
    });
}

pub fn execute(
    scope: &CompositionStateScope,
    limits: &CompositionStateBinding,
    operation: &str,
    key: Option<&str>,
    value: Option<Value>,
) -> Result<Value, CompositionStateError> {
    ensure_cleanup_hook();
    limits.validate()?;
    match operation {
        "get" => {
            let key = validate_key(operation, key)?;
            Ok(STATE_WINDOWS
                .lock()
                .get(scope)
                .and_then(|window| window.values.get(key))
                .cloned()
                .unwrap_or(Value::Null))
        }
        "list" => Ok(Value::Array(
            STATE_WINDOWS
                .lock()
                .get(scope)
                .map(|window| window.values.keys().cloned().map(Value::String).collect())
                .unwrap_or_default(),
        )),
        "put" => {
            let key = validate_key(operation, key)?;
            let value = value.ok_or_else(|| {
                CompositionStateError::for_key(
                    CompositionStateErrorCode::NonJsonValue,
                    operation,
                    key,
                    "state.put requires a JSON value",
                )
            })?;
            put(scope, limits, key, value)?;
            Ok(Value::Null)
        }
        "delete" => {
            let key = validate_key(operation, key)?;
            Ok(Value::Bool(delete(scope, key)))
        }
        _ => Err(CompositionStateError::invalid_key(
            operation,
            format!("unknown state operation '{operation}'"),
        )),
    }
}

fn validate_key<'a>(
    operation: &str,
    key: Option<&'a str>,
) -> Result<&'a str, CompositionStateError> {
    let key = key.unwrap_or_default();
    if key.is_empty() {
        return Err(CompositionStateError::for_key(
            CompositionStateErrorCode::InvalidKey,
            operation,
            key,
            "state keys must be non-empty strings",
        ));
    }
    Ok(key)
}

fn put(
    scope: &CompositionStateScope,
    limits: &CompositionStateBinding,
    key: &str,
    value: Value,
) -> Result<(), CompositionStateError> {
    let value_bytes = u64::try_from(
        serde_json::to_vec(&value)
            .map_err(|error| {
                CompositionStateError::for_key(
                    CompositionStateErrorCode::NonJsonValue,
                    "put",
                    key,
                    format!("state value is not valid JSON: {error}"),
                )
            })?
            .len(),
    )
    .unwrap_or(u64::MAX);
    if value_bytes > limits.max_value_bytes {
        return Err(CompositionStateError::for_key(
            CompositionStateErrorCode::ValueTooLarge,
            "put",
            key,
            format!(
                "state value exceeds max_value_bytes={}",
                limits.max_value_bytes
            ),
        )
        .with_bound(limits.max_value_bytes, value_bytes));
    }

    let key_bytes = u64::try_from(key.len()).unwrap_or(u64::MAX);
    let mut windows = STATE_WINDOWS.lock();
    let window = windows.get(scope);
    let old_value = window.and_then(|window| window.values.get(key));
    let old_bytes = old_value
        .and_then(|old| serde_json::to_vec(old).ok())
        .and_then(|old| u64::try_from(old.len()).ok())
        .map(|old| old.saturating_add(key_bytes))
        .unwrap_or(0);
    let key_count = window
        .map(|window| u64::try_from(window.values.len()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    if old_value.is_none() && key_count >= limits.max_keys {
        return Err(CompositionStateError::for_key(
            CompositionStateErrorCode::TooManyKeys,
            "put",
            key,
            format!("state store exceeds max_keys={}", limits.max_keys),
        )
        .with_bound(limits.max_keys, key_count.saturating_add(1)));
    }
    let new_total = window
        .map(|window| window.total_bytes)
        .unwrap_or(0)
        .saturating_sub(old_bytes)
        .saturating_add(key_bytes)
        .saturating_add(value_bytes);
    if new_total > limits.max_total_bytes {
        return Err(CompositionStateError::for_key(
            CompositionStateErrorCode::TotalTooLarge,
            "put",
            key,
            format!(
                "state store exceeds max_total_bytes={}",
                limits.max_total_bytes
            ),
        )
        .with_bound(limits.max_total_bytes, new_total));
    }
    let window = windows.entry(scope.clone()).or_default();
    window.values.insert(key.to_string(), value);
    window.total_bytes = new_total;
    Ok(())
}

fn delete(scope: &CompositionStateScope, key: &str) -> bool {
    let mut windows = STATE_WINDOWS.lock();
    let Some(window) = windows.get_mut(scope) else {
        return false;
    };
    let Some(value) = window.values.remove(key) else {
        return false;
    };
    let value_bytes = serde_json::to_vec(&value)
        .ok()
        .and_then(|bytes| u64::try_from(bytes.len()).ok())
        .unwrap_or(0);
    window.total_bytes = window
        .total_bytes
        .saturating_sub(u64::try_from(key.len()).unwrap_or(u64::MAX))
        .saturating_sub(value_bytes);
    if window.values.is_empty() {
        windows.remove(scope);
    }
    true
}

pub fn binding_entry(operation: &str) -> BindingManifestEntry {
    let writes = matches!(operation, "put" | "delete");
    let capability = if writes { "write" } else { "read" };
    let mut capabilities = BTreeMap::new();
    capabilities.insert(
        COMPOSITION_STATE_CAPABILITY.to_string(),
        vec![capability.to_string()],
    );
    let annotations = ToolAnnotations {
        kind: if writes {
            ToolKind::Edit
        } else {
            ToolKind::Read
        },
        side_effect_level: if writes {
            SideEffectLevel::WorkspaceWrite
        } else {
            SideEffectLevel::ReadOnly
        },
        capabilities: capabilities.clone(),
        inline_result: true,
        ..ToolAnnotations::default()
    };
    BindingManifestEntry {
        name: format!("{STATE_BINDING_NAME}.{operation}"),
        binding: format!("{STATE_BINDING_NAME}.{operation}"),
        namespace: Some(STATE_BINDING_NAME.to_string()),
        description: Some(format!("Session-scoped composition state {operation}")),
        input_schema: state_input_schema(operation),
        output_schema: Some(state_output_schema(operation)),
        annotations,
        side_effect_level: if writes {
            SideEffectLevel::WorkspaceWrite
        } else {
            SideEffectLevel::ReadOnly
        },
        capabilities,
        source: COMPOSITION_STATE_CAPABILITY.to_string(),
        policy: BindingPolicyStatus::default(),
        metadata: serde_json::json!({
            "internal": true,
            "state_operation": operation,
        }),
        ..BindingManifestEntry::default()
    }
}

fn state_input_schema(operation: &str) -> Value {
    match operation {
        "put" => serde_json::json!({
            "type": "object",
            "required": ["key", "value"],
            "properties": {
                "key": {"type": "string"},
                "value": {},
            },
        }),
        "get" | "delete" => serde_json::json!({
            "type": "object",
            "required": ["key"],
            "properties": {"key": {"type": "string"}},
        }),
        _ => serde_json::json!({"type": "object", "properties": {}}),
    }
}

fn state_output_schema(operation: &str) -> Value {
    match operation {
        "list" => serde_json::json!({"type": "array", "items": {"type": "string"}}),
        "delete" => serde_json::json!({"type": "boolean"}),
        _ => serde_json::json!({}),
    }
}

pub fn operation_names() -> BTreeSet<String> {
    ["get", "put", "list", "delete"]
        .into_iter()
        .map(|operation| format!("{STATE_BINDING_NAME}.{operation}"))
        .collect()
}

pub fn harn_runtime_source() -> &'static str {
    "const state = {\n\
       _namespace: \"composition_state\",\n\
       get: { key -> __composition_state(\"get\", key) },\n\
       put: { key, value -> __composition_state(\"put\", key, value) },\n\
       list: { -> __composition_state(\"list\") },\n\
       delete: { key -> __composition_state(\"delete\", key) },\n\
     }\n"
}

pub fn harn_api_source() -> &'static str {
    "type CompositionState = {\n\
       _namespace: string,\n\
       get: fn(string) -> JsonValue,\n\
       put: fn(string, JsonValue) -> nil,\n\
       list: fn() -> list<string>,\n\
       delete: fn(string) -> bool,\n\
     }\n\
     const state: CompositionState = {\n\
       _namespace: \"composition_state\",\n\
       get: { key -> __composition_state(\"get\", key) },\n\
       put: { key, value -> __composition_state(\"put\", key, value) },\n\
       list: { -> __composition_state(\"list\") },\n\
       delete: { key -> __composition_state(\"delete\", key) },\n\
     }\n\n"
}

pub fn typescript_api_source() -> &'static str {
    "export interface CompositionState {\n\
       get(key: string): Promise<JsonValue>;\n\
       put(key: string, value: JsonValue): Promise<void>;\n\
       list(): Promise<string[]>;\n\
       delete(key: string): Promise<boolean>;\n\
     }\n\
     export declare const state: CompositionState;\n\n"
}
