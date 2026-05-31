use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use serde_json::error::Category;

use crate::schema;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static JSON_STREAM_VALIDATORS: RefCell<BTreeMap<String, JsonStreamValidator>> =
        const { RefCell::new(BTreeMap::new()) };
    static NEXT_JSON_STREAM_VALIDATOR_ID: Cell<u64> = const { Cell::new(1) };
}

#[derive(Clone)]
struct JsonStreamValidator {
    schema: VmValue,
    buffer: String,
    scan: JsonStreamScan,
    status: JsonStreamStatus,
    value: Option<VmValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JsonStreamStatus {
    Pending,
    Valid,
    Invalid { reason: String, path: String },
}

/// Send-safe Rust API for the incremental JSON validator. Mirrors the
/// `std/json/stream` script builtin but uses `serde_json::Value` as the
/// schema storage so the LLM streaming transport can hold it across
/// off-thread awaits without depending on VM heap identity.
///
/// The canonicalized schema is rebuilt per feed inside [`Self::feed`];
/// canonicalization is a cheap tree walk so callers pay nothing
/// observable for the indirection.
pub(crate) struct StreamSchemaValidator {
    schema_json: serde_json::Value,
    buffer: String,
    scan: JsonStreamScan,
    status: JsonStreamStatus,
}

impl StreamSchemaValidator {
    /// Build a validator from a JSON Schema-shaped `serde_json::Value`.
    /// Returns the canonicalization error string on malformed schemas;
    /// callers typically log and skip rather than aborting the request.
    pub(crate) fn from_json_schema(schema: &serde_json::Value) -> Result<Self, String> {
        let schema_vm = schema::json_to_vm_value(schema);
        // Validate that the schema canonicalizes; we don't keep the
        // VmValue around because the streaming transport stores a JSON copy,
        // but failing here gives callers an early malformed-schema signal.
        schema::schema_from_json_schema_value(&schema_vm).map_err(|err| err.to_string())?;
        Ok(Self {
            schema_json: schema.clone(),
            buffer: String::new(),
            scan: JsonStreamScan::default(),
            status: JsonStreamStatus::Pending,
        })
    }

    /// Feed a text chunk into the validator. The returned status is the
    /// validator's *new* status after consuming `chunk` — `Pending` while
    /// the JSON is incomplete, `Valid` once a complete and schema-conforming
    /// document is in the buffer, `Invalid` the first time the partial
    /// content cannot conceivably satisfy the schema.
    pub(crate) fn feed(&mut self, chunk: &str) -> &JsonStreamStatus {
        if matches!(self.status, JsonStreamStatus::Invalid { .. }) {
            return &self.status;
        }
        if let Err(err) = self.scan.feed(chunk) {
            self.buffer.push_str(chunk);
            self.status = JsonStreamStatus::Invalid {
                reason: err,
                path: "$".to_string(),
            };
            return &self.status;
        }
        self.buffer.push_str(chunk);

        if self.buffer.trim().is_empty() {
            self.status = JsonStreamStatus::Pending;
            return &self.status;
        }

        let schema_vm = schema::json_to_vm_value(&self.schema_json);
        // `from_json_schema` validates that the schema canonicalizes at
        // construction, so this call cannot fail by the time we get
        // here. Fall back to the raw VmValue defensively rather than
        // panicking — the early-invalid + parse-and-validate paths will
        // simply produce a less-precise diagnostic instead of misbehaving.
        let canonical = match schema::schema_from_json_schema_value(&schema_vm) {
            Ok(c) => c,
            Err(_) => schema_vm,
        };

        if let Some(invalid) = early_invalid(&self.buffer, &canonical) {
            self.status = JsonStreamStatus::Invalid {
                reason: invalid.reason,
                path: invalid.path,
            };
            return &self.status;
        }

        if self.scan.complete || self.scan.root_scalar {
            self.status = match parse_complete_buffer(&self.buffer, &canonical) {
                ParseOutcome::Valid => {
                    self.scan.complete = true;
                    JsonStreamStatus::Valid
                }
                ParseOutcome::Pending => JsonStreamStatus::Pending,
                ParseOutcome::Invalid { reason, path } => {
                    JsonStreamStatus::Invalid { reason, path }
                }
            };
        } else {
            self.status = JsonStreamStatus::Pending;
        }
        &self.status
    }
}

#[derive(Clone, Debug, Default)]
struct JsonStreamScan {
    started: bool,
    complete: bool,
    root_scalar: bool,
    in_string: bool,
    escaped: bool,
    stack: Vec<char>,
}

#[derive(Clone, Debug)]
struct EarlyInvalid {
    reason: String,
    path: String,
}

pub(crate) fn reset_json_stream_state() {
    JSON_STREAM_VALIDATORS.with(|validators| validators.borrow_mut().clear());
    NEXT_JSON_STREAM_VALIDATOR_ID.with(|next| next.set(1));
}

pub(crate) fn register_json_stream_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

fn create_validator(builtin: &str, args: &[VmValue]) -> Result<VmValue, VmError> {
    let schema = args
        .first()
        .ok_or_else(|| thrown(format!("{builtin}: requires a schema argument")))?;
    let schema = schema::schema_from_json_schema_value(schema)?;
    let handle = next_handle();
    JSON_STREAM_VALIDATORS.with(|validators| {
        validators.borrow_mut().insert(
            handle.clone(),
            JsonStreamValidator {
                schema,
                buffer: String::new(),
                scan: JsonStreamScan::default(),
                status: JsonStreamStatus::Pending,
                value: None,
            },
        );
    });
    Ok(VmValue::String(std::sync::Arc::from(handle)))
}

#[harn_builtin(
    sig = "__json_stream_validator(schema: dict) -> string",
    category = "json_stream"
)]
fn json_stream_validator_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    create_validator("__json_stream_validator", args)
}

#[harn_builtin(
    sig = "__json_stream_validator_feed(handle: string, chunk: string | bytes) -> any",
    category = "json_stream"
)]
fn json_stream_validator_feed_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let handle = handle_arg(args, "__json_stream_validator_feed")?;
    let chunk = chunk_arg(args, "__json_stream_validator_feed")?;
    with_validator(&handle, |validator| {
        validator.feed(&chunk);
        Ok(status_value(&validator.status))
    })
}

#[harn_builtin(
    sig = "__json_stream_validator_value(handle: string) -> any",
    category = "json_stream"
)]
fn json_stream_validator_value_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let handle = handle_arg(args, "__json_stream_validator_value")?;
    with_validator(&handle, |validator| {
        Ok(match &validator.status {
            JsonStreamStatus::Valid => validator.value.clone().unwrap_or(VmValue::Nil),
            JsonStreamStatus::Pending | JsonStreamStatus::Invalid { .. } => VmValue::Nil,
        })
    })
}

#[harn_builtin(
    sig = "__json_stream_validator_status(handle: string) -> any",
    category = "json_stream"
)]
fn json_stream_validator_status_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let handle = handle_arg(args, "__json_stream_validator_status")?;
    with_validator(&handle, |validator| Ok(status_value(&validator.status)))
}

// `std/json/stream_validate` (E5.1, #1773): plain-dict verdict API
// built on top of the same `JsonStreamValidator` storage that the
// `stream_validator` closure-bag uses. The verdict shape is
// intentionally provider-agnostic (`{verdict: "pending"|"valid"|"invalid",
// reason?, path?}`) so streaming agents can dispatch on it without
// pattern-matching enum variants.
#[harn_builtin(
    sig = "__json_stream_validate_create(schema: dict) -> string",
    category = "json_stream"
)]
fn json_stream_validate_create_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    create_validator("__json_stream_validate_create", args)
}

#[harn_builtin(
    sig = "__json_stream_validate_chunk(handle: string, chunk: string | bytes) -> dict",
    category = "json_stream"
)]
fn json_stream_validate_chunk_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let handle = handle_arg(args, "__json_stream_validate_chunk")?;
    let chunk = chunk_arg(args, "__json_stream_validate_chunk")?;
    with_validator(&handle, |validator| {
        validator.feed(&chunk);
        Ok(verdict_value(&validator.status))
    })
}

#[harn_builtin(
    sig = "__json_stream_validate_finalize(handle: string) -> dict",
    category = "json_stream"
)]
fn json_stream_validate_finalize_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let handle = handle_arg(args, "__json_stream_validate_finalize")?;
    with_validator(&handle, |validator| {
        validator.finalize();
        Ok(verdict_value(&validator.status))
    })
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &JSON_STREAM_VALIDATOR_IMPL_DEF,
    &JSON_STREAM_VALIDATOR_FEED_IMPL_DEF,
    &JSON_STREAM_VALIDATOR_VALUE_IMPL_DEF,
    &JSON_STREAM_VALIDATOR_STATUS_IMPL_DEF,
    &JSON_STREAM_VALIDATE_CREATE_IMPL_DEF,
    &JSON_STREAM_VALIDATE_CHUNK_IMPL_DEF,
    &JSON_STREAM_VALIDATE_FINALIZE_IMPL_DEF,
];

impl JsonStreamValidator {
    fn feed(&mut self, chunk: &[u8]) {
        if matches!(self.status, JsonStreamStatus::Invalid { .. }) {
            return;
        }

        let text = match std::str::from_utf8(chunk) {
            Ok(text) => text,
            Err(error) => {
                self.invalidate(format!("chunk is not valid UTF-8: {error}"), "$");
                return;
            }
        };

        if let Err(error) = self.scan.feed(text) {
            self.buffer.push_str(text);
            self.invalidate(error, "$");
            return;
        }
        self.buffer.push_str(text);

        if self.buffer.trim().is_empty() {
            self.status = JsonStreamStatus::Pending;
            self.value = None;
            return;
        }

        if let Some(invalid) = early_invalid(&self.buffer, &self.schema) {
            self.invalidate(invalid.reason, invalid.path);
            return;
        }

        if self.scan.complete || self.scan.root_scalar {
            self.parse_and_validate();
        } else {
            self.status = JsonStreamStatus::Pending;
            self.value = None;
        }
    }

    fn parse_and_validate(&mut self) {
        match serde_json::from_str::<serde_json::Value>(&self.buffer) {
            Ok(json) => {
                let value = schema::json_to_vm_value(&json);
                match schema_validation_error(&value, &self.schema, "$") {
                    Some(invalid) => self.invalidate(invalid.reason, invalid.path),
                    None => {
                        self.status = JsonStreamStatus::Valid;
                        self.value = Some(value);
                        self.scan.complete = true;
                    }
                }
            }
            Err(error) if error.classify() == Category::Eof => {
                self.status = JsonStreamStatus::Pending;
                self.value = None;
            }
            Err(error) => self.invalidate(format!("JSON parse error: {error}"), "$"),
        }
    }

    fn invalidate(&mut self, reason: String, path: impl Into<String>) {
        self.status = JsonStreamStatus::Invalid {
            reason,
            path: path.into(),
        };
        self.value = None;
    }

    /// Close out a stream. If the validator is still `Pending` and the
    /// buffer holds a partial document, transition to `Invalid` with an
    /// "incomplete JSON document" reason. An empty/whitespace-only
    /// buffer (nothing arrived) stays `Pending` so callers can tell
    /// "no document yet" apart from "partial document arrived"; an
    /// already `Valid` or `Invalid` status is left alone.
    fn finalize(&mut self) {
        if !matches!(self.status, JsonStreamStatus::Pending) {
            return;
        }
        if self.buffer.trim().is_empty() {
            return;
        }
        self.invalidate("incomplete JSON document at end of stream".to_string(), "$");
    }
}

/// Parse the buffered JSON and validate it against the canonical schema.
/// Shared by both the VmValue-backed `JsonStreamValidator` (script API)
/// and the Send-safe [`StreamSchemaValidator`] (LLM transport).
enum ParseOutcome {
    Valid,
    Pending,
    Invalid { reason: String, path: String },
}

fn parse_complete_buffer(buffer: &str, schema: &VmValue) -> ParseOutcome {
    match serde_json::from_str::<serde_json::Value>(buffer) {
        Ok(json) => {
            let value = schema::json_to_vm_value(&json);
            match schema_validation_error(&value, schema, "$") {
                Some(invalid) => ParseOutcome::Invalid {
                    reason: invalid.reason,
                    path: invalid.path,
                },
                None => ParseOutcome::Valid,
            }
        }
        Err(error) if error.classify() == Category::Eof => ParseOutcome::Pending,
        Err(error) => ParseOutcome::Invalid {
            reason: format!("JSON parse error: {error}"),
            path: "$".to_string(),
        },
    }
}

impl JsonStreamScan {
    fn feed(&mut self, text: &str) -> Result<(), String> {
        for ch in text.chars() {
            self.feed_char(ch)?;
        }
        Ok(())
    }

    fn feed_char(&mut self, ch: char) -> Result<(), String> {
        if self.complete {
            if ch.is_whitespace() {
                return Ok(());
            }
            return Err("trailing data after complete JSON value".to_string());
        }

        if self.in_string {
            if self.escaped {
                self.escaped = false;
                return Ok(());
            }
            match ch {
                '\\' => self.escaped = true,
                '"' => {
                    self.in_string = false;
                    if self.root_scalar && self.stack.is_empty() {
                        self.complete = true;
                    }
                }
                c if c.is_control() => {
                    return Err("unescaped control character in JSON string".to_string());
                }
                _ => {}
            }
            return Ok(());
        }

        if !self.started {
            if ch.is_whitespace() {
                return Ok(());
            }
            self.started = true;
            match ch {
                '{' => self.stack.push('}'),
                '[' => self.stack.push(']'),
                '"' => {
                    self.root_scalar = true;
                    self.in_string = true;
                }
                '-' | '0'..='9' | 't' | 'f' | 'n' => self.root_scalar = true,
                _ => return Err(format!("expected JSON value, got '{ch}'")),
            }
            return Ok(());
        }

        if self.root_scalar {
            return Ok(());
        }

        match ch {
            '"' => self.in_string = true,
            '{' => self.stack.push('}'),
            '[' => self.stack.push(']'),
            '}' | ']' => match self.stack.pop() {
                Some(expected) if expected == ch => {
                    if self.stack.is_empty() {
                        self.complete = true;
                    }
                }
                Some(expected) => {
                    return Err(format!(
                        "mismatched JSON delimiter: expected '{expected}', got '{ch}'"
                    ));
                }
                None => return Err(format!("unexpected JSON delimiter '{ch}'")),
            },
            _ => {}
        }
        Ok(())
    }
}

fn next_handle() -> String {
    NEXT_JSON_STREAM_VALIDATOR_ID.with(|next| {
        let id = next.get();
        next.set(id.saturating_add(1));
        format!("json_stream_validator:{id}")
    })
}

fn handle_arg(args: &[VmValue], builtin: &str) -> Result<String, VmError> {
    match args.first() {
        Some(VmValue::String(handle)) => Ok(handle.to_string()),
        Some(other) => Err(thrown(format!(
            "{builtin}: expected validator handle, got {}",
            other.type_name()
        ))),
        None => Err(thrown(format!("{builtin}: missing validator handle"))),
    }
}

fn chunk_arg(args: &[VmValue], builtin: &str) -> Result<Vec<u8>, VmError> {
    match args.get(1) {
        Some(VmValue::Bytes(bytes)) => Ok(bytes.as_ref().clone()),
        Some(VmValue::String(text)) => Ok(text.as_bytes().to_vec()),
        Some(other) => Err(thrown(format!(
            "{builtin}: expected string or bytes chunk, got {}",
            other.type_name()
        ))),
        None => Err(thrown(format!("{builtin}: missing chunk argument"))),
    }
}

fn with_validator<T>(
    handle: &str,
    f: impl FnOnce(&mut JsonStreamValidator) -> Result<T, VmError>,
) -> Result<T, VmError> {
    JSON_STREAM_VALIDATORS.with(|validators| {
        let mut validators = validators.borrow_mut();
        let validator = validators.get_mut(handle).ok_or_else(|| {
            thrown(format!(
                "json stream validator handle not found or expired: {handle}"
            ))
        })?;
        f(validator)
    })
}

fn status_value(status: &JsonStreamStatus) -> VmValue {
    match status {
        JsonStreamStatus::Pending => VmValue::enum_variant("JsonStreamStatus", "Pending", vec![]),
        JsonStreamStatus::Valid => VmValue::enum_variant("JsonStreamStatus", "Valid", vec![]),
        JsonStreamStatus::Invalid { reason, path } => {
            let payload = BTreeMap::from([
                (
                    "reason".to_string(),
                    VmValue::String(std::sync::Arc::from(reason.as_str())),
                ),
                (
                    "path".to_string(),
                    VmValue::String(std::sync::Arc::from(path.as_str())),
                ),
            ]);
            VmValue::enum_variant(
                "JsonStreamStatus",
                "Invalid",
                vec![VmValue::Dict(std::sync::Arc::new(payload))],
            )
        }
    }
}

/// Plain-dict verdict shape used by the `std/json/stream_validate.*`
/// builtins. Agents dispatch on the string `verdict` field rather than
/// pattern-matching an enum variant, which keeps the surface stable for
/// downstream consumers that build on top of it (SSE adapters,
/// WebSocket frame validators, etc.).
fn verdict_value(status: &JsonStreamStatus) -> VmValue {
    let mut dict = BTreeMap::new();
    match status {
        JsonStreamStatus::Pending => {
            dict.insert(
                "verdict".to_string(),
                VmValue::String(std::sync::Arc::from("pending")),
            );
        }
        JsonStreamStatus::Valid => {
            dict.insert(
                "verdict".to_string(),
                VmValue::String(std::sync::Arc::from("valid")),
            );
        }
        JsonStreamStatus::Invalid { reason, path } => {
            dict.insert(
                "verdict".to_string(),
                VmValue::String(std::sync::Arc::from("invalid")),
            );
            dict.insert(
                "reason".to_string(),
                VmValue::String(std::sync::Arc::from(reason.as_str())),
            );
            dict.insert(
                "path".to_string(),
                VmValue::String(std::sync::Arc::from(path.as_str())),
            );
        }
    }
    VmValue::Dict(std::sync::Arc::new(dict))
}

fn early_invalid(buffer: &str, schema: &VmValue) -> Option<EarlyInvalid> {
    let schema_dict = schema.as_dict()?;
    let start = first_non_ws(buffer)?;
    if let Some(invalid) = invalid_type_start(buffer[start..].chars().next()?, schema_dict, "$") {
        return Some(invalid);
    }
    if buffer[start..].starts_with('{') {
        return early_invalid_object(&buffer[start..], schema_dict, "$");
    }
    None
}

fn early_invalid_object(
    buffer: &str,
    schema: &BTreeMap<String, VmValue>,
    path: &str,
) -> Option<EarlyInvalid> {
    if !schema_is_object_like(schema) {
        return None;
    }
    let properties = schema.get("properties").and_then(VmValue::as_dict)?;
    let mut index = 1;

    loop {
        index = skip_ws(buffer, index);
        let ch = char_at(buffer, index)?;
        if ch == '}' {
            return None;
        }
        if ch == ',' {
            index += ch.len_utf8();
            continue;
        }
        if ch != '"' {
            return None;
        }

        let (key, key_end) = parse_json_string_at(buffer, index)?;
        index = skip_ws(buffer, key_end);
        if char_at(buffer, index)? != ':' {
            return None;
        }
        index += 1;
        index = skip_ws(buffer, index);

        let child_path = child_json_path(path, &key);
        if let Some(child_schema) = properties.get(&key).and_then(VmValue::as_dict) {
            let first = char_at(buffer, index)?;
            if let Some(invalid) = invalid_type_start(first, child_schema, &child_path) {
                return Some(invalid);
            }
            if first == '{' {
                if let Some(invalid) =
                    early_invalid_object(&buffer[index..], child_schema, &child_path)
                {
                    return Some(invalid);
                }
            }
            if let Some(value_end) = complete_value_end(buffer, index) {
                if let Ok(json) =
                    serde_json::from_str::<serde_json::Value>(&buffer[index..value_end])
                {
                    let value = schema::json_to_vm_value(&json);
                    if let Some(invalid) = schema_validation_error(
                        &value,
                        &VmValue::Dict(std::sync::Arc::new(child_schema.clone())),
                        &child_path,
                    ) {
                        return Some(invalid);
                    }
                }
                index = value_end;
                continue;
            }
        } else if let Some(value_end) = complete_value_end(buffer, index) {
            index = value_end;
            continue;
        }

        return None;
    }
}

fn invalid_type_start(
    first: char,
    schema: &BTreeMap<String, VmValue>,
    path: &str,
) -> Option<EarlyInvalid> {
    let expected = expected_type(schema)?;
    if expected == "any" {
        return None;
    }
    let starts_ok = match expected {
        "dict" => first == '{',
        "list" => first == '[',
        "string" => first == '"',
        "bool" => matches!(first, 't' | 'f'),
        "nil" => first == 'n',
        "int" => matches!(first, '-' | '0'..='9'),
        "float" => matches!(first, '-' | '0'..='9'),
        _ => true,
    };
    if starts_ok {
        None
    } else {
        Some(EarlyInvalid {
            reason: format!(
                "expected type '{expected}', got JSON {}",
                token_label(first)
            ),
            path: path.to_string(),
        })
    }
}

fn schema_validation_error(value: &VmValue, schema: &VmValue, path: &str) -> Option<EarlyInvalid> {
    match schema::schema_result_value(value, schema, false) {
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Err") => {
            let reason = enum_variant
                .fields
                .first()
                .and_then(VmValue::as_dict)
                .and_then(|payload| payload.get("message"))
                .map(VmValue::display)
                .unwrap_or_else(|| "schema validation failed".to_string());
            Some(EarlyInvalid {
                reason,
                path: path.to_string(),
            })
        }
        _ => None,
    }
}

fn expected_type(schema: &BTreeMap<String, VmValue>) -> Option<&str> {
    if schema_is_object_like(schema) {
        return Some("dict");
    }
    match schema.get("type") {
        Some(VmValue::String(value)) => Some(value.as_ref()),
        _ => None,
    }
}

fn schema_is_object_like(schema: &BTreeMap<String, VmValue>) -> bool {
    matches!(schema.get("type"), Some(VmValue::String(value)) if value.as_ref() == "dict")
        || schema.contains_key("properties")
        || schema.contains_key("required")
        || schema.contains_key("additional_properties")
}

fn token_label(first: char) -> &'static str {
    match first {
        '{' => "object",
        '[' => "array",
        '"' => "string",
        't' | 'f' => "boolean",
        'n' => "null",
        '-' | '0'..='9' => "number",
        _ => "token",
    }
}

fn complete_value_end(buffer: &str, start: usize) -> Option<usize> {
    let start = skip_ws(buffer, start);
    let first = char_at(buffer, start)?;
    match first {
        '"' => parse_json_string_at(buffer, start).map(|(_, end)| end),
        '{' | '[' => complete_container_end(buffer, start),
        't' if buffer[start..].starts_with("true") => Some(start + 4),
        'f' if buffer[start..].starts_with("false") => Some(start + 5),
        'n' if buffer[start..].starts_with("null") => Some(start + 4),
        '-' | '0'..='9' => complete_number_end(buffer, start),
        _ => None,
    }
}

fn complete_container_end(buffer: &str, start: usize) -> Option<usize> {
    let first = char_at(buffer, start)?;
    let expected = match first {
        '{' => '}',
        '[' => ']',
        _ => return None,
    };
    let mut stack = vec![expected];
    let mut in_string = false;
    let mut escaped = false;
    let mut index = start + first.len_utf8();

    while index < buffer.len() {
        let ch = char_at(buffer, index)?;
        index += ch.len_utf8();
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop()? != ch {
                    return None;
                }
                if stack.is_empty() {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn complete_number_end(buffer: &str, start: usize) -> Option<usize> {
    let mut index = start;
    while index < buffer.len() {
        let ch = char_at(buffer, index)?;
        if matches!(ch, '-' | '+' | '.' | 'e' | 'E' | '0'..='9') {
            index += ch.len_utf8();
        } else {
            break;
        }
    }
    if index == buffer.len() {
        None
    } else {
        Some(index)
    }
}

fn parse_json_string_at(buffer: &str, start: usize) -> Option<(String, usize)> {
    let mut escaped = false;
    let mut index = start + 1;
    while index < buffer.len() {
        let ch = char_at(buffer, index)?;
        index += ch.len_utf8();
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => {
                let raw = &buffer[start..index];
                let value = serde_json::from_str::<String>(raw).ok()?;
                return Some((value, index));
            }
            _ => {}
        }
    }
    None
}

fn child_json_path(parent: &str, key: &str) -> String {
    if key
        .chars()
        .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        format!("{parent}.{key}")
    } else {
        format!(
            "{parent}[{}]",
            serde_json::to_string(key).unwrap_or_default()
        )
    }
}

fn first_non_ws(buffer: &str) -> Option<usize> {
    buffer
        .char_indices()
        .find_map(|(index, ch)| (!ch.is_whitespace()).then_some(index))
}

fn skip_ws(buffer: &str, mut index: usize) -> usize {
    while index < buffer.len() {
        let Some(ch) = char_at(buffer, index) else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        index += ch.len_utf8();
    }
    index
}

fn char_at(buffer: &str, index: usize) -> Option<char> {
    buffer.get(index..)?.chars().next()
}

fn thrown(message: String) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(message)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string(value: &str) -> VmValue {
        VmValue::String(std::sync::Arc::from(value))
    }

    fn dict(entries: impl IntoIterator<Item = (&'static str, VmValue)>) -> VmValue {
        VmValue::Dict(std::sync::Arc::new(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        ))
    }

    fn list(items: Vec<VmValue>) -> VmValue {
        VmValue::List(std::sync::Arc::new(items))
    }

    fn status_variant(value: &VmValue) -> String {
        match value {
            VmValue::EnumVariant(enum_variant) => enum_variant.variant.to_string(),
            other => panic!("expected enum status, got {other:?}"),
        }
    }

    fn call(name: &str, args: Vec<VmValue>) -> VmValue {
        call_result(name, args).unwrap()
    }

    fn call_result(name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let mut vm = Vm::new();
        register_json_stream_builtins(&mut vm);
        let builtin = vm.builtins.get(name).unwrap().clone();
        builtin(&args, &mut String::new())
    }

    #[test]
    fn stream_validator_rejects_cyclic_schema_at_create() {
        reset_json_stream_state();
        let schema = dict([("$ref", string("#"))]);
        let error = call_result("__json_stream_validator", vec![schema])
            .expect_err("cyclic schema must be rejected");

        assert!(
            error
                .to_string()
                .contains("cyclic schema reference: # -> #"),
            "expected cyclic schema reference error, got {error:?}"
        );
    }

    #[test]
    fn feed_reaches_valid_once_document_completes() {
        reset_json_stream_state();
        let schema = dict([
            ("type", string("object")),
            ("required", list(vec![string("name")])),
            (
                "properties",
                dict([("name", dict([("type", string("string"))]))]),
            ),
        ]);
        let handle = call("__json_stream_validator", vec![schema]);
        let pending = call(
            "__json_stream_validator_feed",
            vec![handle.clone(), string("{\"name\"")],
        );
        assert_eq!(status_variant(&pending), "Pending");
        let valid = call(
            "__json_stream_validator_feed",
            vec![handle.clone(), string(":\"Ada\"}")],
        );
        assert_eq!(status_variant(&valid), "Valid");
        let value = call("__json_stream_validator_value", vec![handle]);
        assert!(matches!(value, VmValue::Dict(_)));
    }

    #[test]
    fn detects_impossible_property_type_before_object_closes() {
        reset_json_stream_state();
        let schema = dict([
            ("type", string("object")),
            ("required", list(vec![string("age")])),
            (
                "properties",
                dict([("age", dict([("type", string("int"))]))]),
            ),
        ]);
        let handle = call("__json_stream_validator", vec![schema]);
        let invalid = call(
            "__json_stream_validator_feed",
            vec![handle, string("{\"age\":\"")],
        );
        assert_eq!(status_variant(&invalid), "Invalid");
    }

    #[test]
    fn feed_after_valid_non_whitespace_invalidates_stream() {
        reset_json_stream_state();
        let schema = dict([("type", string("int"))]);
        let handle = call("__json_stream_validator", vec![schema]);
        let valid = call(
            "__json_stream_validator_feed",
            vec![handle.clone(), string("1")],
        );
        assert_eq!(status_variant(&valid), "Valid");
        let invalid = call("__json_stream_validator_feed", vec![handle, string("2")]);
        assert_eq!(status_variant(&invalid), "Invalid");
    }

    #[test]
    fn stream_validate_verdict_progresses_pending_to_valid() {
        reset_json_stream_state();
        let schema = dict([
            ("type", string("dict")),
            ("required", list(vec![string("name")])),
            (
                "properties",
                dict([("name", dict([("type", string("string"))]))]),
            ),
        ]);
        let handle = call("__json_stream_validate_create", vec![schema]);
        let pending = call(
            "__json_stream_validate_chunk",
            vec![handle.clone(), string("{\"name\":")],
        );
        let pending_map = pending.as_dict().expect("pending dict");
        assert_eq!(
            pending_map.get("verdict").map(VmValue::display).as_deref(),
            Some("pending")
        );
        let valid = call(
            "__json_stream_validate_chunk",
            vec![handle, string("\"Ada\"}")],
        );
        let valid_map = valid.as_dict().expect("valid dict");
        assert_eq!(
            valid_map.get("verdict").map(VmValue::display).as_deref(),
            Some("valid")
        );
    }

    #[test]
    fn stream_validate_chunk_surfaces_invalid_reason_and_path() {
        reset_json_stream_state();
        let schema = dict([
            ("type", string("dict")),
            ("required", list(vec![string("age")])),
            (
                "properties",
                dict([("age", dict([("type", string("int"))]))]),
            ),
        ]);
        let handle = call("__json_stream_validate_create", vec![schema]);
        let invalid = call(
            "__json_stream_validate_chunk",
            vec![handle, string("{\"age\":\"")],
        );
        let invalid_map = invalid.as_dict().expect("invalid dict");
        assert_eq!(
            invalid_map.get("verdict").map(VmValue::display).as_deref(),
            Some("invalid")
        );
        assert!(invalid_map.contains_key("reason"));
        assert!(invalid_map.contains_key("path"));
    }

    #[test]
    fn stream_validate_finalize_invalidates_incomplete_partial() {
        reset_json_stream_state();
        let schema = dict([("type", string("dict"))]);
        let handle = call("__json_stream_validate_create", vec![schema]);
        let pending = call(
            "__json_stream_validate_chunk",
            vec![handle.clone(), string("{\"a\":")],
        );
        assert_eq!(
            pending
                .as_dict()
                .and_then(|d| d.get("verdict"))
                .map(VmValue::display)
                .as_deref(),
            Some("pending")
        );
        let finalized = call("__json_stream_validate_finalize", vec![handle]);
        let finalized_map = finalized.as_dict().expect("finalize dict");
        assert_eq!(
            finalized_map
                .get("verdict")
                .map(VmValue::display)
                .as_deref(),
            Some("invalid")
        );
        assert_eq!(
            finalized_map.get("reason").map(VmValue::display).as_deref(),
            Some("incomplete JSON document at end of stream")
        );
    }

    #[test]
    fn stream_validate_finalize_keeps_valid_intact() {
        reset_json_stream_state();
        let schema = dict([("type", string("int"))]);
        let handle = call("__json_stream_validate_create", vec![schema]);
        let valid = call(
            "__json_stream_validate_chunk",
            vec![handle.clone(), string("42")],
        );
        assert_eq!(
            valid
                .as_dict()
                .and_then(|d| d.get("verdict"))
                .map(VmValue::display)
                .as_deref(),
            Some("valid")
        );
        let finalized = call("__json_stream_validate_finalize", vec![handle]);
        assert_eq!(
            finalized
                .as_dict()
                .and_then(|d| d.get("verdict"))
                .map(VmValue::display)
                .as_deref(),
            Some("valid")
        );
    }

    #[test]
    fn one_byte_chunks_keep_large_container_linear() {
        let schema = schema::schema_from_json_schema_value(&dict([
            ("type", string("list")),
            ("items", dict([("type", string("int"))])),
        ]))
        .unwrap();
        let mut validator = JsonStreamValidator {
            schema,
            buffer: String::new(),
            scan: JsonStreamScan::default(),
            status: JsonStreamStatus::Pending,
            value: None,
        };
        let payload = format!("[{}]", vec!["1"; 6_000].join(","));

        for byte in payload.as_bytes() {
            validator.feed(std::slice::from_ref(byte));
        }

        assert_eq!(validator.status, JsonStreamStatus::Valid);
        assert!(matches!(validator.value, Some(VmValue::List(_))));
    }
}
