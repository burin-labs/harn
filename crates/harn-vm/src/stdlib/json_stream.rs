#![expect(
    clippy::string_slice,
    reason = "every slice offset here is accumulated from char_indices/len_utf8 \
              walks or ASCII token lengths over the same buffer being sliced"
)]

use crate::value::VmDictExt;
use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::error::Category;

use crate::schema;
use crate::stdlib::macros::harn_builtin;
use crate::value::{SchemaValidationReasonKind, VmError, VmResourceHandle, VmValue};

mod stream_schema;
pub(crate) use stream_schema::StreamSchemaValidator;
mod partial;
use partial::best_effort_partial;

#[cfg(test)]
mod path_tests;

#[derive(Clone)]
struct JsonStreamValidator {
    schema: VmValue,
    buffer: String,
    pending_utf8: Vec<u8>,
    scan: JsonStreamScan,
    status: JsonStreamStatus,
    value: Option<VmValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JsonStreamStatus {
    Pending,
    Valid,
    Invalid {
        reason_kind: SchemaValidationReasonKind,
        reason: String,
        path: String,
    },
}

#[derive(Clone, Debug, Default)]
struct JsonStreamScan {
    started: bool,
    complete: bool,
    root_scalar: bool,
    in_string: bool,
    escaped: bool,
    stack: Vec<char>,
    /// Cumulative byte offset of the next char to be consumed. The
    /// streaming validators feed the exact same text into the buffer and
    /// into the scan, so this offset lines up with byte indices in the
    /// validator buffer and lets [`Self::json_slice`] hand the parser the
    /// fenced-out JSON body without re-scanning.
    byte_pos: usize,
    /// Leading markdown code-fence detection (` ```json ` openers, etc.).
    lead_fence: LeadFence,
    /// Byte offset in the buffer where the JSON body begins. `0` unless a
    /// leading fence was stripped, in which case it points just past the
    /// opener line's newline.
    body_start: usize,
    /// Trailing markdown code-fence detection, active once the JSON value
    /// has completed.
    trail_fence: TrailFence,
    /// Byte offset just past the end of the JSON value. Set once trailing
    /// content (whitespace or a closing fence) is encountered so the
    /// parser stops before a closing ` ``` `. `None` means "to end of
    /// buffer".
    body_end: Option<usize>,
}

/// Leading code-fence state. Only a full triple backtick (optionally
/// followed by a language tag on the same line) counts as a fence; a stray
/// non-JSON, non-backtick lead still errors as before, and a run of one or
/// two backticks not completed to three before a non-backtick errors too.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum LeadFence {
    /// No leading fence seen (the common case: bare JSON or whitespace).
    #[default]
    None,
    /// Counting consecutive leading backticks; value is how many seen
    /// (always 1 or 2 — three flips us to `SkipLine`).
    Backticks(u8),
    /// A ` ``` ` opener was found; skipping the rest of the opener line
    /// (e.g. a `json` language tag) up to and including the next newline.
    SkipLine,
}

/// Trailing code-fence state, used once a JSON value has completed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum TrailFence {
    /// Only trailing whitespace seen so far; a backtick opens a closing
    /// fence.
    #[default]
    None,
    /// Counting consecutive closing backticks (1 or 2 so far).
    Backticks(u8),
    /// A closing ` ``` ` was consumed; only trailing whitespace is allowed
    /// from here.
    Closed,
}

#[derive(Clone, Debug)]
struct EarlyInvalid {
    reason_kind: SchemaValidationReasonKind,
    reason: String,
    path: String,
}

pub(crate) fn reset_json_stream_state() {
    // Validator state is owned by opaque VM resource values and is released
    // when those values leave the VM. There is no ambient registry to reset.
}

harn_vm_core::stdlib_state_reset!(reset_json_stream_state);

harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_json_stream_builtins);

fn create_validator(builtin: &str, args: &[VmValue]) -> Result<VmValue, VmError> {
    let schema = args
        .first()
        .ok_or_else(|| thrown(format!("{builtin}: requires a schema argument")))?;
    let schema = schema::schema_from_json_schema_value(schema)?;
    Ok(VmValue::resource(VmResourceHandle::new(
        "json_stream_validator",
        Mutex::new(JsonStreamValidator {
            schema,
            buffer: String::new(),
            pending_utf8: Vec::new(),
            scan: JsonStreamScan::default(),
            status: JsonStreamStatus::Pending,
            value: None,
        }),
    )))
}

#[harn_builtin(
    exposure = "harness.runtime.json_stream_validator",
    effects = ["state.mutate@dynamic"],
    sig = "__json_stream_validator(schema: dict) -> resource",
    category = "json_stream"
)]
fn json_stream_validator_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    create_validator("__json_stream_validator", args)
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["state.mutate@arg0"],
    sig = "__json_stream_validator_feed(handle: resource, chunk: string | bytes) -> any",
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
    exposure = "capability_arg:0",
    effects = ["state.observe@arg0"],
    sig = "__json_stream_validator_value(handle: resource) -> any",
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
    exposure = "capability_arg:0",
    effects = ["state.observe@arg0"],
    sig = "__json_stream_validator_status(handle: resource) -> any",
    category = "json_stream"
)]
fn json_stream_validator_status_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let handle = handle_arg(args, "__json_stream_validator_status")?;
    with_validator(&handle, |validator| Ok(status_value(&validator.status)))
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["state.observe@arg0"],
    sig = "__json_stream_validator_partial(handle: resource) -> any",
    category = "json_stream"
)]
fn json_stream_validator_partial_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let handle = handle_arg(args, "__json_stream_validator_partial")?;
    with_validator(&handle, |validator| Ok(validator.partial_value()))
}

// `std/json/stream_validate` (E5.1, #1773): plain-dict verdict API
// built on top of the same `JsonStreamValidator` storage that the
// `stream_validator` closure-bag uses. The verdict shape is
// intentionally provider-agnostic (`{verdict: "pending"|"valid"|"invalid",
// reason?, path?}`) so streaming agents can dispatch on it without
// pattern-matching enum variants.
#[harn_builtin(
    exposure = "harness.runtime.json_stream_validate_create",
    effects = ["state.mutate@dynamic"],
    sig = "__json_stream_validate_create(schema: dict) -> resource",
    category = "json_stream"
)]
fn json_stream_validate_create_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    create_validator("__json_stream_validate_create", args)
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["state.mutate@arg0"],
    sig = "__json_stream_validate_chunk(handle: resource, chunk: string | bytes) -> dict",
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
    exposure = "capability_arg:0",
    effects = ["state.mutate@arg0"],
    sig = "__json_stream_validate_finalize(handle: resource) -> dict",
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

impl JsonStreamValidator {
    fn feed(&mut self, chunk: &[u8]) {
        if matches!(self.status, JsonStreamStatus::Invalid { .. }) {
            return;
        }

        let mut buffered = std::mem::take(&mut self.pending_utf8);
        let chunk = if buffered.is_empty() {
            chunk
        } else {
            buffered.extend_from_slice(chunk);
            &buffered
        };
        let text = match std::str::from_utf8(chunk) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() => {
                let valid = error.valid_up_to();
                self.pending_utf8.extend_from_slice(&chunk[valid..]);
                std::str::from_utf8(&chunk[..valid]).expect("UTF-8 prefix was validated")
            }
            Err(error) => {
                self.invalidate(
                    SchemaValidationReasonKind::InvalidJson,
                    format!("chunk is not valid UTF-8: {error}"),
                    "$",
                );
                return;
            }
        };

        if let Err(error) = self.scan.feed(text) {
            self.buffer.push_str(text);
            self.invalidate(SchemaValidationReasonKind::InvalidJson, error, "$");
            return;
        }
        self.buffer.push_str(text);

        // Validate the fenced-out JSON body; a surrounding markdown code
        // fence only affects framing, not schema validation.
        let json = self.scan.json_slice(&self.buffer);
        if json.trim().is_empty() {
            self.status = JsonStreamStatus::Pending;
            self.value = None;
            return;
        }

        if let Some(invalid) = early_invalid(json, &self.schema) {
            self.invalidate(invalid.reason_kind, invalid.reason, invalid.path);
            return;
        }

        if !self.pending_utf8.is_empty() {
            self.status = JsonStreamStatus::Pending;
            self.value = None;
        } else if self.scan.complete || self.scan.root_scalar {
            self.parse_and_validate();
        } else {
            self.status = JsonStreamStatus::Pending;
            self.value = None;
        }
    }

    fn parse_and_validate(&mut self) {
        match parse_complete_buffer(
            self.scan.json_slice(&self.buffer),
            &self.schema,
            self.scan.complete,
        ) {
            ParseOutcome::Valid(value) => {
                self.status = JsonStreamStatus::Valid;
                self.value = Some(value);
            }
            ParseOutcome::Pending => {
                self.status = JsonStreamStatus::Pending;
                self.value = None;
            }
            ParseOutcome::Invalid {
                reason_kind,
                reason,
                path,
            } => self.invalidate(reason_kind, reason, path),
        }
    }

    fn invalidate(
        &mut self,
        reason_kind: SchemaValidationReasonKind,
        reason: String,
        path: impl Into<String>,
    ) {
        self.status = JsonStreamStatus::Invalid {
            reason_kind,
            reason,
            path: path.into(),
        };
        self.value = None;
    }

    /// Best-effort partially-filled value from the bytes seen so far — the
    /// engine behind Vercel `streamObject` / Instructor `Partial[T]`. While
    /// `Valid` this is the current value; while `Pending` it
    /// materializes the largest structurally-complete prefix by closing the
    /// open string/containers at the frontier and dropping any trailing
    /// incomplete member (a half-typed key or value). Structural only — the
    /// schema is not applied to a partial. Returns `Nil` when nothing
    /// parseable has arrived yet.
    fn partial_value(&self) -> VmValue {
        if let (JsonStreamStatus::Valid, Some(value)) = (&self.status, &self.value) {
            return value.clone();
        }
        best_effort_partial(self.scan.json_slice(&self.buffer))
    }

    /// Close out a stream. If the validator is still `Pending` and the
    /// buffer holds a partial document, transition to `Invalid` with an
    /// "incomplete JSON document" reason. An empty/whitespace-only
    /// buffer (nothing arrived) stays `Pending`. A root number ends here
    /// even when no delimiter arrived; incomplete UTF-8 is rejected.
    fn finalize(&mut self) {
        if matches!(self.status, JsonStreamStatus::Invalid { .. }) {
            return;
        }
        if !self.pending_utf8.is_empty() {
            self.invalidate(
                SchemaValidationReasonKind::InvalidJson,
                "incomplete UTF-8 character at end of stream".to_owned(),
                "$",
            );
            return;
        }
        if self.scan.root_scalar && !self.scan.complete {
            self.scan.complete = true;
            self.parse_and_validate();
        }
        if !matches!(self.status, JsonStreamStatus::Pending) {
            return;
        }
        // Use the fenced-out body: a buffer holding only a fence opener
        // (no JSON yet) counts as "nothing arrived", not a partial document.
        if self.scan.json_slice(&self.buffer).trim().is_empty() {
            // Exception: a fence opener that was seen but never closed its
            // opener line (e.g. a CR-only `` ```json\r{...} `` where the
            // expected `\n` never arrived) leaves us stuck mid-opener with
            // buffered content that never became a body. That is a malformed,
            // unrecoverable stream rather than "nothing arrived", so surface
            // Invalid instead of a permanent Pending.
            if self.scan.lead_fence == LeadFence::SkipLine && !self.buffer.trim().is_empty() {
                self.invalidate(
                    SchemaValidationReasonKind::InvalidJson,
                    "incomplete JSON document at end of stream".to_string(),
                    "$",
                );
            }
            return;
        }
        self.invalidate(
            SchemaValidationReasonKind::InvalidJson,
            "incomplete JSON document at end of stream".to_string(),
            "$",
        );
    }
}

/// Parse the buffered JSON and validate it against the canonical schema.
/// Shared by both the VmValue-backed `JsonStreamValidator` (script API)
/// and the Send-safe [`StreamSchemaValidator`] (LLM transport).
enum ParseOutcome {
    Valid(VmValue),
    Pending,
    Invalid {
        reason_kind: SchemaValidationReasonKind,
        reason: String,
        path: String,
    },
}

fn parse_complete_buffer(buffer: &str, schema: &VmValue, complete: bool) -> ParseOutcome {
    let number_can_grow = !complete
        && buffer
            .trim_start()
            .starts_with(|ch: char| ch == '-' || ch.is_ascii_digit());
    match serde_json::from_str::<serde_json::Value>(buffer) {
        Ok(json) => {
            let value = schema::json_to_vm_value(&json);
            match schema_validation_error(&value, schema, "") {
                Some(_) if number_can_grow => ParseOutcome::Pending,
                Some(invalid) => ParseOutcome::Invalid {
                    reason_kind: invalid.reason_kind,
                    reason: invalid.reason,
                    path: invalid.path,
                },
                None => ParseOutcome::Valid(value),
            }
        }
        Err(error) if error.classify() == Category::Eof => ParseOutcome::Pending,
        Err(_) if number_can_grow && super::json::validate_json_syntax(buffer).is_ok() => {
            ParseOutcome::Pending
        }
        Err(error) => ParseOutcome::Invalid {
            reason_kind: SchemaValidationReasonKind::InvalidJson,
            reason: format!("JSON parse error: {error}"),
            path: "$".to_string(),
        },
    }
}

impl JsonStreamScan {
    fn feed(&mut self, text: &str) -> Result<(), String> {
        for ch in text.chars() {
            let len = ch.len_utf8();
            self.feed_char(ch)?;
            self.byte_pos += len;
        }
        Ok(())
    }

    /// The fenced-out JSON body of `buffer`, i.e. `buffer` with any leading
    /// code-fence opener line and trailing closing fence removed. When no
    /// fence was seen this is `buffer` unchanged. The streaming validators
    /// feed the same text into `buffer` and into the scan, so `body_start`
    /// / `body_end` are valid byte indices into `buffer`.
    fn json_slice<'a>(&self, buffer: &'a str) -> &'a str {
        // Still consuming a leading fence opener (` ``` ` run or language
        // tag line): the JSON body hasn't begun, so there is nothing to
        // hand the parser yet.
        if self.lead_fence != LeadFence::None {
            return "";
        }
        let start = self.body_start.min(buffer.len());
        let end = self.body_end.unwrap_or(buffer.len()).min(buffer.len());
        if end < start {
            return "";
        }
        &buffer[start..end]
    }

    fn feed_char(&mut self, ch: char) -> Result<(), String> {
        // Trailing region: the JSON value is framed and we now only tolerate
        // whitespace plus an optional closing ` ``` ` fence. Root scalars set
        // `complete` the moment their value ends (see the `root_scalar` arm
        // below and the string closing-quote arm), so this single flag covers
        // every framed shape.
        if self.complete {
            return self.feed_trailing(ch);
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
                        self.body_end = Some(self.byte_pos + ch.len_utf8());
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
            return self.feed_lead(ch);
        }

        if self.root_scalar {
            // A root scalar (number / literal) has no closing delimiter, so
            // the first whitespace or backtick after it marks the end of the
            // value. Frame the value here exactly as a closing quote frames a
            // string root scalar: set `complete` and capture `body_end` once,
            // so every later char routes through `feed_trailing` (which
            // tolerates trailing whitespace + one closing ` ``` ` fence and
            // rejects any other junk). Without this, trailing garbage after a
            // scalar (e.g. `42 garbage`, `4 2`) would be silently dropped and
            // the stream wrongly accepted as Valid.
            if ch.is_whitespace() || ch == '`' {
                self.complete = true;
                if self.body_end.is_none() {
                    self.body_end = Some(self.byte_pos);
                }
                return self.feed_trailing(ch);
            }
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
                        self.body_end = Some(self.byte_pos + ch.len_utf8());
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

    /// Handle a char before any JSON value has started. Recognizes an
    /// optional leading ` ``` ` code fence (with an optional language tag on
    /// the opener line) and otherwise begins normal JSON parsing.
    ///
    /// DELIBERATE divergence from `extract_json_from_text` (json.rs:714):
    /// that helper scans for a fence anywhere in the text (`find("```")`), so
    /// it tolerates leading prose before the fence. This streaming validator
    /// only recognizes a fence as the FIRST non-whitespace content — any other
    /// leading byte starts (or fails) JSON parsing immediately. That is the
    /// stricter, safer choice for early-abort: a model that emits prose before
    /// a fence should abort promptly rather than have the validator hunt for a
    /// later fence. The two are different problems; do not unify them.
    fn feed_lead(&mut self, ch: char) -> Result<(), String> {
        match self.lead_fence {
            LeadFence::SkipLine => {
                // Inside the opener line (e.g. after ` ```json `). Drop
                // everything through the first newline, then resume on the
                // body.
                if ch == '\n' {
                    self.lead_fence = LeadFence::None;
                    self.body_start = self.byte_pos + ch.len_utf8();
                }
                Ok(())
            }
            LeadFence::Backticks(count) => {
                if ch == '`' {
                    let next = count + 1;
                    if next == 3 {
                        self.lead_fence = LeadFence::SkipLine;
                    } else {
                        self.lead_fence = LeadFence::Backticks(next);
                    }
                    Ok(())
                } else {
                    // One or two backticks not completed to a full triple
                    // fence: this was never a valid lead.
                    Err(format!("expected JSON value, got '{ch}'"))
                }
            }
            LeadFence::None => {
                if ch.is_whitespace() {
                    return Ok(());
                }
                if ch == '`' {
                    self.lead_fence = LeadFence::Backticks(1);
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
                Ok(())
            }
        }
    }

    /// Handle a char after the JSON value has been framed. Tolerates
    /// trailing whitespace and a single closing ` ``` ` fence (with trailing
    /// whitespace after it); anything else is genuine trailing garbage.
    fn feed_trailing(&mut self, ch: char) -> Result<(), String> {
        match self.trail_fence {
            TrailFence::None => {
                if ch.is_whitespace() {
                    Ok(())
                } else if ch == '`' {
                    self.trail_fence = TrailFence::Backticks(1);
                    Ok(())
                } else {
                    Err("trailing data after complete JSON value".to_string())
                }
            }
            TrailFence::Backticks(count) => {
                if ch == '`' {
                    let next = count + 1;
                    if next == 3 {
                        self.trail_fence = TrailFence::Closed;
                    } else {
                        self.trail_fence = TrailFence::Backticks(next);
                    }
                    Ok(())
                } else {
                    Err("trailing data after complete JSON value".to_string())
                }
            }
            TrailFence::Closed => {
                if ch.is_whitespace() {
                    Ok(())
                } else {
                    Err("trailing data after complete JSON value".to_string())
                }
            }
        }
    }
}

fn handle_arg(args: &[VmValue], builtin: &str) -> Result<Arc<Mutex<JsonStreamValidator>>, VmError> {
    match args.first() {
        Some(VmValue::Resource(handle)) if handle.label() == "json_stream_validator" => handle
            .downcast::<Mutex<JsonStreamValidator>>()
            .ok_or_else(|| thrown(format!("{builtin}: invalid validator authority"))),
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
    handle: &Arc<Mutex<JsonStreamValidator>>,
    f: impl FnOnce(&mut JsonStreamValidator) -> Result<T, VmError>,
) -> Result<T, VmError> {
    f(&mut handle.lock())
}

fn status_value(status: &JsonStreamStatus) -> VmValue {
    match status {
        JsonStreamStatus::Pending => VmValue::enum_variant("JsonStreamStatus", "Pending", vec![]),
        JsonStreamStatus::Valid => VmValue::enum_variant("JsonStreamStatus", "Valid", vec![]),
        JsonStreamStatus::Invalid {
            reason_kind,
            reason,
            path,
        } => {
            let payload = BTreeMap::from([
                (
                    "reason_kind".to_string(),
                    VmValue::string(reason_kind.as_str()),
                ),
                ("reason".to_string(), VmValue::string(reason.as_str())),
                ("path".to_string(), VmValue::string(path.as_str())),
            ]);
            VmValue::enum_variant("JsonStreamStatus", "Invalid", vec![VmValue::dict(payload)])
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
            dict.put_str("verdict", "pending");
        }
        JsonStreamStatus::Valid => {
            dict.put_str("verdict", "valid");
        }
        JsonStreamStatus::Invalid {
            reason_kind,
            reason,
            path,
        } => {
            dict.put_str("verdict", "invalid");
            dict.put_str("reason_kind", reason_kind.as_str());
            dict.put_str("reason", reason.as_str());
            dict.put_str("path", path.as_str());
        }
    }
    VmValue::dict(dict)
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
    schema: &crate::value::DictMap,
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
        if let Some(child_schema) = properties.get(key.as_str()).and_then(VmValue::as_dict) {
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
                        &VmValue::dict(child_schema.clone()),
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
    schema: &crate::value::DictMap,
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
            reason_kind: SchemaValidationReasonKind::WrongType,
            reason: format!(
                "expected type '{expected}', got JSON {}",
                token_label(first)
            ),
            path: path.to_string(),
        })
    }
}

fn schema_validation_error(
    value: &VmValue,
    schema: &VmValue,
    path_prefix: &str,
) -> Option<EarlyInvalid> {
    schema::first_schema_validation_issue(value, schema)
        .map(|issue| project_schema_validation_issue(issue, path_prefix))
}

fn project_schema_validation_issue(
    issue: schema::SchemaValidationIssue,
    path_prefix: &str,
) -> EarlyInvalid {
    let path = if path_prefix.is_empty() {
        if issue.path == "root" {
            "$".to_string()
        } else {
            issue.path
        }
    } else if issue.path == "root" {
        path_prefix.to_string()
    } else if issue.path.starts_with('[') {
        format!("{path_prefix}{}", issue.path)
    } else {
        format!("{path_prefix}.{}", issue.path)
    };
    EarlyInvalid {
        reason_kind: issue.kind,
        reason: issue.detail,
        path,
    }
}

fn expected_type(schema: &crate::value::DictMap) -> Option<&str> {
    if schema_is_object_like(schema) {
        return Some("dict");
    }
    match schema.get("type") {
        Some(VmValue::String(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn schema_is_object_like(schema: &crate::value::DictMap) -> bool {
    matches!(schema.get("type"), Some(VmValue::String(value)) if value.as_str() == "dict")
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
    VmError::Thrown(VmValue::string(message))
}

#[cfg(test)]
mod tests;
