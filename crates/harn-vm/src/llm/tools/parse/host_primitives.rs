//! Host primitives the Harn dialect composition is built from.
//!
//! Each one answers a question whose cost is proportional to the *bytes* of a
//! response — delimit the units, find the end of a balanced object, parse a TS
//! call expression, decode a string channel. None of them answers a question
//! about what a dialect means; `std/llm/tool_parse` owns that, and owns the
//! order these are called in.
//!
//! Two conventions run through the module. Fallible primitives return a tagged
//! dict — `{ok: true, …}` or `{ok: false, error}` — rather than raising, because
//! a parse failure is model-facing feedback the composition classifies, not a
//! runtime fault. And any vocabulary a primitive needs (the dialect spec, the
//! HTML entity table) arrives as an argument: a primitive that restated a
//! dialect list would put policy back in Rust.

use crate::llm::helpers::vm_value_to_json;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};

use super::bare::{bare_tool_names, parse_bare_calls_in_body, parse_bare_calls_in_body_with_known};
use super::scan::{call_head, scan_units, ScanSpec};
use super::syntax::{
    balanced_json_object_len, parse_object_literal_from, parse_ts_call_from, render_canonical_call,
};

/// The primitives, in the order the composition reaches for them. Registered
/// from `llm::register_llm_builtins` alongside the other agent-host slices.
pub(crate) const PARSE_HOST_PRIMITIVE_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_TOOL_SCAN_UNITS_BUILTIN_DEF,
    &HOST_TOOL_PARSE_CALL_EXPR_BUILTIN_DEF,
    &HOST_TOOL_PARSE_OBJECT_LITERAL_BUILTIN_DEF,
    &HOST_TOOL_CALL_HEAD_BUILTIN_DEF,
    &HOST_TOOL_BALANCED_JSON_LEN_BUILTIN_DEF,
    &HOST_TOOL_JSON_STREAM_BUILTIN_DEF,
    &HOST_TOOL_DECODE_ENTITIES_BUILTIN_DEF,
    &HOST_TOOL_RENDER_CALL_BUILTIN_DEF,
    &HOST_TOOL_RENDER_PARTS_BUILTIN_DEF,
    &HOST_TOOL_SCAN_BARE_CALLS_BUILTIN_DEF,
    &HOST_TOOL_SCAN_BARE_UNITS_BUILTIN_DEF,
];

/// Delimit the top-level structural units of a model response.
///
/// `spec` carries the dialect vocabulary from `std/llm/dialects`; the scanner
/// holds none of its own, so a missing field is an error rather than a
/// fallback. Returns `{source, units}` where `source` is the
/// post-thinking-strip text every unit offset indexes, and each unit ships the
/// bytes it covers pre-sliced so the Harn walk never re-slices the source.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_tool_scan_units(text: string, spec: dict) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_tool_scan_units_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    const LABEL: &str = "__host_tool_scan_units(text, spec)";
    let text = string_arg(args, 0, "text", LABEL)?;
    let spec_value = match args.get(1) {
        Some(value @ VmValue::Dict(_)) => vm_value_to_json(value),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{LABEL}: spec must be a dialect dict; got {}",
                other.type_name()
            )))
        }
        None => return Err(VmError::Runtime(format!("{LABEL}: missing spec"))),
    };
    let spec = ScanSpec::from_json(&spec_value)
        .map_err(|error| VmError::Runtime(format!("{LABEL}: {error}")))?;
    Ok(json_to_vm_value(&scan_units(&text, &spec).to_json()))
}

/// Parse a complete `name(args)` TS call expression at the start of `text`.
///
/// `text` must begin at the identifier. Returns `{ok: true, arguments,
/// consumed}` with the byte length through the closing paren, or `{ok: false,
/// error}` carrying the model-facing diagnostic.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_tool_parse_call_expr(text: string, name: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_tool_parse_call_expr_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    const LABEL: &str = "__host_tool_parse_call_expr(text, name)";
    let text = string_arg(args, 0, "text", LABEL)?;
    let name = string_arg(args, 1, "name", LABEL)?;
    Ok(json_to_vm_value(&match parse_ts_call_from(&text, name) {
        Ok((arguments, consumed)) => {
            serde_json::json!({"ok": true, "arguments": arguments, "consumed": consumed})
        }
        Err(error) => serde_json::json!({"ok": false, "error": error}),
    }))
}

/// Parse a TypeScript object literal at the start of `text`.
///
/// `name` names the tool the literal belongs to and appears only in the error
/// message. Returns `{ok: true, value, consumed}` or `{ok: false, error}`.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_tool_parse_object_literal(text: string, name: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_tool_parse_object_literal_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    const LABEL: &str = "__host_tool_parse_object_literal(text, name)";
    let text = string_arg(args, 0, "text", LABEL)?;
    let name = string_arg(args, 1, "name", LABEL)?;
    Ok(json_to_vm_value(
        &match parse_object_literal_from(&text, &name) {
            Ok((value, consumed)) => {
                serde_json::json!({"ok": true, "value": value, "consumed": consumed})
            }
            Err(error) => serde_json::json!({"ok": false, "error": error}),
        },
    ))
}

/// The leading call head of `text`: `{name, sep}` when it opens with an
/// identifier followed by `(` or `{`, otherwise an empty dict.
///
/// The scanner already reports this on every unit that carries a body. This
/// primitive is for the text the composition holds *after* peeling something
/// off — a narration block, a line prefix — where no unit boundary exists.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_tool_call_head(text: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_tool_call_head_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = string_arg(args, 0, "text", "__host_tool_call_head(text)")?;
    Ok(json_to_vm_value(&match call_head(&text) {
        Some(head) => serde_json::json!({"name": head.name, "sep": head.sep.to_string()}),
        None => serde_json::json!({}),
    }))
}

/// Byte length of the balanced `{ … }` object at the start of `text`, counting
/// braces while stepping over string spans. `0` when `text` does not open with
/// `{` or the object is never closed — a balanced object is at least two bytes,
/// so zero is unambiguous.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_tool_balanced_json_len(text: string) -> int",
    category = "agent.host",
    runtime_only = true
)]
fn host_tool_balanced_json_len_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let text = string_arg(args, 0, "text", "__host_tool_balanced_json_len(text)")?;
    let len = balanced_json_object_len(&text).unwrap_or(0);
    Ok(VmValue::Int(len as i64))
}

/// Read the consecutive top-level JSON values at the start of `text`.
///
/// Returns `{values: [{value, start, end}], end, eof, error?}`. `eof` is true
/// when the scan stopped because a value was cut off mid-way — the truncation
/// signature — as opposed to `error`, which means the bytes were present and
/// invalid. Keeping those apart is what lets the composition tell a truncated
/// response from a malformed one instead of reporting both the same way.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_tool_json_stream(text: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_tool_json_stream_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = string_arg(args, 0, "text", "__host_tool_json_stream(text)")?;
    Ok(json_to_vm_value(&json_stream(&text)))
}

#[expect(
    clippy::string_slice,
    reason = "consumed is serde_json's byte_offset just past a parsed value, a char boundary"
)]
fn json_stream(text: &str) -> serde_json::Value {
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>();
    let mut values: Vec<serde_json::Value> = Vec::new();
    let mut consumed = 0usize;
    let mut eof = false;
    let mut error: Option<String> = None;
    loop {
        // `byte_offset` sits just past the previous value, so the next value
        // starts after whatever whitespace separates them.
        let gap = &text[consumed..];
        let start = consumed + (gap.len() - gap.trim_start().len());
        match stream.next() {
            None => break,
            Some(Ok(value)) => {
                let end = stream.byte_offset();
                values.push(serde_json::json!({
                    "value": value,
                    "start": start,
                    "end": end,
                }));
                consumed = end;
            }
            Some(Err(failure)) => {
                if failure.is_eof() {
                    eof = true;
                } else {
                    error = Some(failure.to_string());
                }
                break;
            }
        }
    }
    let mut out = serde_json::Map::new();
    out.insert("values".to_string(), serde_json::Value::Array(values));
    out.insert("end".to_string(), consumed.into());
    out.insert("eof".to_string(), eof.into());
    if let Some(error) = error {
        out.insert("error".to_string(), error.into());
    }
    serde_json::Value::Object(out)
}

/// Decode HTML character references in every string reachable in `value`,
/// using the `entities` table from `std/llm/dialects`.
///
/// Keys are the reference body *without* the leading `&` (`"lt;"`), values are
/// the replacement. Matching is longest-key-first so the table's iteration
/// order cannot change the result. The scan resolves each reference exactly
/// once and never rescans what it produced, so a double-escaped `&amp;lt;`
/// decodes to the literal `&lt;` rather than collapsing to `<`. Callers must
/// invoke this at most once per value; a second pass is not idempotent.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_tool_decode_entities(value: any, entities: dict) -> any",
    category = "agent.host",
    runtime_only = true
)]
fn host_tool_decode_entities_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    const LABEL: &str = "__host_tool_decode_entities(value, entities)";
    let mut value = match args.first() {
        Some(value) => vm_value_to_json(value),
        None => return Err(VmError::Runtime(format!("{LABEL}: missing value"))),
    };
    let table = match args.get(1) {
        Some(entities @ VmValue::Dict(_)) => entity_table(&vm_value_to_json(entities), LABEL)?,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{LABEL}: entities must be a dict of reference body to replacement; got {}",
                other.type_name()
            )))
        }
        None => return Err(VmError::Runtime(format!("{LABEL}: missing entities"))),
    };
    decode_entities_in(&mut value, &table);
    Ok(json_to_vm_value(&value))
}

/// The entity table, ordered longest key first so matching is independent of
/// the caller's dict iteration order.
fn entity_table(
    entities: &serde_json::Value,
    label: &str,
) -> Result<Vec<(String, String)>, VmError> {
    let map: &serde_json::Map<String, serde_json::Value> = entities
        .as_object()
        .ok_or_else(|| VmError::Runtime(format!("{label}: entities must be a dict")))?;
    let mut table: Vec<(String, String)> = map
        .iter()
        .map(|(token, replacement)| match replacement.as_str() {
            Some(text) => Ok((token.clone(), text.to_string())),
            None => Err(VmError::Runtime(format!(
                "{label}: entity `{token}` must map to a string replacement"
            ))),
        })
        .collect::<Result<_, _>>()?;
    table.sort_by(|left, right| right.0.len().cmp(&left.0.len()).then(left.0.cmp(&right.0)));
    Ok(table)
}

fn decode_entities_in(value: &mut serde_json::Value, table: &[(String, String)]) {
    match value {
        serde_json::Value::String(text) => {
            let decoded = decode_entities(text, table);
            if decoded != *text {
                *text = decoded;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                decode_entities_in(item, table);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values_mut() {
                decode_entities_in(item, table);
            }
        }
        _ => {}
    }
}

fn decode_entities(raw: &str, table: &[(String, String)]) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some((before, after)) = rest.split_once('&') {
        out.push_str(before);
        let matched = table.iter().find_map(|(token, replacement)| {
            after
                .strip_prefix(token.as_str())
                .map(|tail| (tail, replacement))
        });
        match matched {
            Some((tail, replacement)) => {
                out.push_str(replacement);
                rest = tail;
            }
            // Not a recognized reference: emit the `&` and keep scanning.
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Render a call back to the bare TS syntax used inside call tags. This is
/// what the canonical history entry replays, so a turn with leading raw code
/// does not become "what the agent said" on the next turn.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_tool_render_call(name: string, arguments: any) -> string",
    category = "agent.host",
    runtime_only = true
)]
fn host_tool_render_call_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    const LABEL: &str = "__host_tool_render_call(name, arguments)";
    let name = string_arg(args, 0, "name", LABEL)?;
    let arguments = args.get(1).map_or(serde_json::json!({}), vm_value_to_json);
    Ok(VmValue::String(
        render_canonical_call(&name, &arguments).into(),
    ))
}

/// Render Harn-selected canonical response parts with one VM crossing.
///
/// Policy and ordering stay in `std/llm/tool_parse`: each dict is an explicit
/// `{kind, ...}` projection, while strings preserve uncommon paths that were
/// already rendered there.
#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "__host_tool_render_parts(parts: list) -> string",
    category = "agent.host"
)]
fn host_tool_render_parts_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    const LABEL: &str = "__host_tool_render_parts(parts)";
    let parts = match args.first() {
        Some(VmValue::List(parts)) => parts,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{LABEL}: parts must be a list; got {}",
                other.type_name()
            )))
        }
        None => return Err(VmError::Runtime(format!("{LABEL}: missing parts"))),
    };
    let mut rendered = Vec::with_capacity(parts.len());
    for part in parts.iter() {
        if let VmValue::String(text) = part {
            rendered.push(text.to_string());
            continue;
        }
        let VmValue::Dict(part) = part else {
            return Err(VmError::Runtime(format!(
                "{LABEL}: each part must be a string or dict; got {}",
                part.type_name()
            )));
        };
        let Some(VmValue::String(kind)) = part.get("kind") else {
            return Err(VmError::Runtime(format!(
                "{LABEL}: dict part is missing kind"
            )));
        };
        let text = match kind.as_str() {
            "call" => {
                let name = match part.get("name") {
                    Some(VmValue::String(name)) => name.as_str(),
                    _ => "",
                };
                let arguments = part
                    .get("arguments")
                    .map_or_else(|| serde_json::json!({}), vm_value_to_json);
                format!(
                    "<tool_call>\n{}\n</tool_call>",
                    render_canonical_call(name, &arguments)
                )
            }
            "bare" => {
                let Some(VmValue::List(calls)) = part.get("calls") else {
                    return Err(VmError::Runtime(format!(
                        "{LABEL}: bare part is missing its calls list"
                    )));
                };
                let mut blocks = Vec::with_capacity(calls.len() + 1);
                for call in calls.iter() {
                    let VmValue::Dict(call) = call else {
                        return Err(VmError::Runtime(format!(
                            "{LABEL}: canonical call must be a dict"
                        )));
                    };
                    let name = match call.get("name") {
                        Some(VmValue::String(name)) => name.as_str(),
                        _ => "",
                    };
                    let arguments = call
                        .get("arguments")
                        .map_or_else(|| serde_json::json!({}), vm_value_to_json);
                    blocks.push(format!(
                        "<tool_call>\n{}\n</tool_call>",
                        render_canonical_call(name, &arguments)
                    ));
                }
                if let Some(VmValue::String(prose)) = part.get("prose") {
                    let prose = prose.trim();
                    if !prose.is_empty() {
                        blocks.push(format!("<assistant_prose>\n{prose}\n</assistant_prose>"));
                    }
                }
                blocks.join("\n\n")
            }
            "prose" => format!(
                "<assistant_prose>\n{}\n</assistant_prose>",
                part.get("text")
                    .map_or_else(String::new, VmValue::display)
                    .trim()
            ),
            "answer" => format!(
                "<user_response>\n{}\n</user_response>",
                part.get("text")
                    .map_or_else(String::new, VmValue::display)
                    .trim()
            ),
            "done" => format!(
                "<done>{}</done>",
                part.get("text")
                    .map_or_else(String::new, VmValue::display)
                    .trim()
            ),
            other => {
                return Err(VmError::Runtime(format!(
                    "{LABEL}: unsupported part kind {other:?}"
                )))
            }
        };
        rendered.push(text);
    }
    Ok(VmValue::String(rendered.join("\n\n").into()))
}

/// Scan a text body for bare `name({ … })` calls, the way the stray sniffer
/// does. Returns `{calls, errors, prose}`.
///
/// This is the primitive behind an invariant worth restating: the sniffer runs
/// over stray *and fenced* text and dispatches what it finds. Markdown fencing
/// does not make a call inert — a composition that treats the fenced branch as
/// prose-only silently stops dispatching real calls.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_tool_scan_bare_calls(text: string, tools?: dict|nil) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_tool_scan_bare_calls_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    const LABEL: &str = "__host_tool_scan_bare_calls(text, tools?)";
    let text = string_arg(args, 0, "text", LABEL)?;
    let tools = match args.get(1) {
        Some(VmValue::Nil) | None => crate::stdlib::tools::current_tool_registry(),
        Some(value @ VmValue::Dict(_)) => Some(value.clone()),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{LABEL}: tools must be a tool registry dict or nil; got {}",
                other.type_name()
            )))
        }
    };
    let parsed = parse_bare_calls_in_body(&text, tools.as_ref());
    Ok(json_to_vm_value(&serde_json::json!({
        "calls": parsed.calls,
        "errors": parsed.errors,
        "prose": parsed.prose,
    })))
}

/// Parse every text-bearing structural unit with one registry projection and
/// one VM boundary crossing. Results align by index with `units`; non-text
/// units carry `nil` because Harn still owns which unit kinds are meaningful.
#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "__host_tool_scan_bare_units(units: list<dict>, tools?: dict|nil) -> list<dict|nil>",
    category = "agent.host"
)]
fn host_tool_scan_bare_units_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    const LABEL: &str = "__host_tool_scan_bare_units(units, tools?)";
    let units = match args.first() {
        Some(VmValue::List(units)) => units,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{LABEL}: units must be a list; got {}",
                other.type_name()
            )))
        }
        None => return Err(VmError::Runtime(format!("{LABEL}: missing units"))),
    };
    let tools = match args.get(1) {
        Some(VmValue::Nil) | None => crate::stdlib::tools::current_tool_registry(),
        Some(value @ VmValue::Dict(_)) => Some(value.clone()),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{LABEL}: tools must be a tool registry dict or nil; got {}",
                other.type_name()
            )))
        }
    };
    let known = bare_tool_names(tools.as_ref());
    let results = units
        .iter()
        .map(|unit| {
            let VmValue::Dict(unit) = unit else {
                return VmValue::Nil;
            };
            let Some(VmValue::String(kind)) = unit.get("kind") else {
                return VmValue::Nil;
            };
            if matches!(kind.as_str(), "text" | "fenced_line" | "harmony_line") {
                let Some(VmValue::String(text)) = unit.get("text") else {
                    return VmValue::Nil;
                };
                // Match the Harn composition boundary exactly. It trims each
                // structural text unit before invoking the single-unit parser;
                // retaining separator whitespace here changes model-facing
                // error excerpts even though the parse decision is identical.
                let parsed = parse_bare_calls_in_body_with_known(text.trim(), &known);
                return json_to_vm_value(&serde_json::json!({"bare": {
                    "calls": parsed.calls,
                    "errors": parsed.errors,
                    "prose": parsed.prose,
                }}));
            }
            if matches!(kind.as_str(), "block" | "unclosed_block" | "reserved_block") {
                let (
                    Some(VmValue::String(body)),
                    Some(VmValue::String(name)),
                    Some(VmValue::String(sep)),
                ) = (
                    unit.get("body"),
                    unit.get("head_name"),
                    unit.get("head_sep"),
                )
                else {
                    return VmValue::Nil;
                };
                let body = body.trim();
                let direct = if sep.as_str() == "(" {
                    match parse_ts_call_from(body, name.to_string()) {
                        Ok((arguments, consumed)) => {
                            serde_json::json!({"ok": true, "arguments": arguments, "consumed": consumed})
                        }
                        Err(error) => serde_json::json!({"ok": false, "error": error}),
                    }
                } else {
                    #[expect(
                        clippy::string_slice,
                        reason = "head_name is an ASCII ident prefix of the scanned unit body"
                    )]
                    let argument = &body[name.len()..];
                    match parse_object_literal_from(argument, name) {
                        Ok((value, consumed)) => {
                            serde_json::json!({"ok": true, "value": value, "consumed": name.len() + consumed})
                        }
                        Err(error) => serde_json::json!({"ok": false, "error": error}),
                    }
                };
                return json_to_vm_value(&serde_json::json!({"direct": direct}));
            }
            VmValue::Nil
        })
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(results)))
}

fn string_arg(args: &[VmValue], index: usize, field: &str, label: &str) -> Result<String, VmError> {
    match args.get(index) {
        Some(VmValue::String(text)) => Ok(text.to_string()),
        Some(other) => Err(VmError::Runtime(format!(
            "{label}: {field} must be a string; got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!("{label}: missing {field}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::helpers::vm_value_to_json;

    fn text(value: &str) -> VmValue {
        VmValue::String(value.into())
    }

    fn call(
        builtin: fn(&[VmValue], &mut String) -> Result<VmValue, VmError>,
        args: &[VmValue],
    ) -> serde_json::Value {
        let mut out = String::new();
        vm_value_to_json(&builtin(args, &mut out).expect("builtin"))
    }

    fn entities() -> VmValue {
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "amp;": "&",
            "lt;": "<",
            "gt;": ">",
            "quot;": "\"",
        }))
    }

    #[test]
    fn scan_units_reads_its_vocabulary_from_the_spec() {
        let spec = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "call_tags": ["action"],
            "block_tags": [],
            "wrapper_tags": [],
            "markup_openers": [],
            "reserved_openers": [],
            "harmony": {
                "header_markers": [],
                "standalone_markers": [],
                "frame_prefix": "<|",
                "frame_suffix": "|>",
                "message_marker": "<|message|>",
                "tool_call_header_prefix": "tool_call to=",
                "corrupted_openers": [],
            },
            "known_tools": [],
        }));
        let result = call(
            host_tool_scan_units_builtin,
            &[text("<action>look({})</action>"), spec],
        );
        assert_eq!(result["units"][0]["kind"], "block");
        assert_eq!(result["units"][0]["tag"], "action");
        assert_eq!(result["units"][0]["head_name"], "look");
    }

    #[test]
    fn scan_units_rejects_a_spec_with_no_vocabulary() {
        let mut out = String::new();
        let error = host_tool_scan_units_builtin(
            &[
                text("hi"),
                crate::stdlib::json_to_vm_value(&serde_json::json!({})),
            ],
            &mut out,
        )
        .expect_err("an absent field is an error");
        assert!(format!("{error:?}").contains("spec.call_tags"), "{error:?}");
    }

    #[test]
    #[expect(clippy::string_slice, reason = "test input is ASCII")]
    fn parse_call_expr_reports_arguments_and_the_bytes_it_consumed() {
        let source = "look({ path: \"a.rs\" }) trailing prose";
        let result = call(
            host_tool_parse_call_expr_builtin,
            &[text(source), text("look")],
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["arguments"], serde_json::json!({"path": "a.rs"}));
        assert_eq!(
            &source[..result["consumed"].as_u64().expect("consumed") as usize],
            "look({ path: \"a.rs\" })"
        );
    }

    #[test]
    fn parse_call_expr_surfaces_the_model_facing_diagnostic() {
        let result = call(
            host_tool_parse_call_expr_builtin,
            &[text("look({ path: )"), text("look")],
        );
        assert_eq!(result["ok"], false);
        assert!(
            result["error"]
                .as_str()
                .expect("error")
                .contains("TOOL CALL PARSE ERROR"),
            "{result}"
        );
    }

    #[test]
    fn parse_object_literal_stops_at_the_closing_brace() {
        let result = call(
            host_tool_parse_object_literal_builtin,
            &[text("{ path: \"a.rs\" } and then prose"), text("look")],
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["value"], serde_json::json!({"path": "a.rs"}));
        assert_eq!(result["consumed"], "{ path: \"a.rs\" }".len());
    }

    #[test]
    fn call_head_answers_both_separators_and_declines_bare_words() {
        let paren = call(host_tool_call_head_builtin, &[text("  look({})")]);
        assert_eq!(paren, serde_json::json!({"name": "look", "sep": "("}));
        let brace = call(host_tool_call_head_builtin, &[text("edit{ a: 1 }")]);
        assert_eq!(brace, serde_json::json!({"name": "edit", "sep": "{"}));
        let prose = call(host_tool_call_head_builtin, &[text("Reading the file.")]);
        assert_eq!(prose, serde_json::json!({}));
    }

    #[test]
    fn balanced_json_len_ignores_braces_inside_strings() {
        let object = "{\"a\": \"}{\"}";
        let result = call(
            host_tool_balanced_json_len_builtin,
            &[text(&format!("{object} trailing"))],
        );
        assert_eq!(result, serde_json::json!(object.len()));
        let unclosed = call(host_tool_balanced_json_len_builtin, &[text("{\"a\": 1")]);
        assert_eq!(unclosed, serde_json::json!(0));
        let not_json = call(host_tool_balanced_json_len_builtin, &[text("prose")]);
        assert_eq!(not_json, serde_json::json!(0));
    }

    #[test]
    fn json_stream_reports_consecutive_values_with_offsets() {
        let source = "{\"a\": 1} {\"b\": 2}";
        let result = call(host_tool_json_stream_builtin, &[text(source)]);
        assert_eq!(result["values"].as_array().expect("values").len(), 2);
        assert_eq!(result["values"][1]["start"], 9);
        assert_eq!(result["values"][1]["end"], source.len());
        assert_eq!(result["values"][1]["value"], serde_json::json!({"b": 2}));
        assert_eq!(result["eof"], false);
        assert!(result.get("error").is_none(), "{result}");
    }

    #[test]
    fn json_stream_tells_truncation_apart_from_invalid_bytes() {
        let truncated = call(host_tool_json_stream_builtin, &[text("{\"a\": 1")]);
        assert_eq!(truncated["eof"], true);
        assert!(truncated.get("error").is_none(), "{truncated}");
        assert_eq!(truncated["values"].as_array().expect("values").len(), 0);

        let invalid = call(host_tool_json_stream_builtin, &[text("{\"a\": 1} nope")]);
        assert_eq!(invalid["eof"], false);
        assert!(invalid.get("error").is_some(), "{invalid}");
        // The value that did parse is still reported, with the offset the
        // caller needs to say where the trouble starts.
        assert_eq!(invalid["values"].as_array().expect("values").len(), 1);
        assert_eq!(invalid["end"], 8);
    }

    #[test]
    fn decode_entities_resolves_each_reference_exactly_once() {
        let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "content": "if (a &lt;= b) { xs.map(x =&gt; x) }",
            "nested": ["a &amp;&amp; b"],
        }));
        let result = call(host_tool_decode_entities_builtin, &[value, entities()]);
        assert_eq!(result["content"], "if (a <= b) { xs.map(x => x) }");
        assert_eq!(result["nested"][0], "a && b");
    }

    #[test]
    fn decode_entities_is_not_a_second_pass_on_double_escaped_input() {
        // `&amp;lt;` is the wire form of a literal `&lt;`, so it must decode to
        // that literal rather than collapsing to `<`.
        let value = text("&amp;lt; and a bare R&D");
        let result = call(host_tool_decode_entities_builtin, &[value, entities()]);
        assert_eq!(result, serde_json::json!("&lt; and a bare R&D"));
    }

    #[test]
    fn decode_entities_matches_longest_key_first() {
        // Iteration order of the caller's dict must not change the result.
        let table = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "lt": "L",
            "lt;": "<",
        }));
        let result = call(host_tool_decode_entities_builtin, &[text("&lt;"), table]);
        assert_eq!(result, serde_json::json!("<"));
    }

    #[test]
    fn render_call_round_trips_through_the_call_parser() {
        let arguments = crate::stdlib::json_to_vm_value(&serde_json::json!({"path": "a.rs"}));
        let rendered = call(host_tool_render_call_builtin, &[text("look"), arguments]);
        let rendered = rendered.as_str().expect("rendered call");
        assert!(rendered.starts_with("look({"), "{rendered}");
        let reparsed = call(
            host_tool_parse_call_expr_builtin,
            &[text(rendered), text("look")],
        );
        assert_eq!(reparsed["arguments"], serde_json::json!({"path": "a.rs"}));
    }

    #[test]
    fn scan_bare_calls_recovers_a_call_and_leaves_the_prose() {
        let registry = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "_type": "tool_registry",
            "tools": [{"name": "look", "description": "read a file", "params": []}],
        }));
        let result = call(
            host_tool_scan_bare_calls_builtin,
            &[text("Checking now.\nlook({ path: \"a.rs\" })\n"), registry],
        );
        let calls = result["calls"].as_array().expect("calls");
        assert_eq!(calls.len(), 1, "{result}");
        assert_eq!(calls[0]["name"], "look");
        assert_eq!(calls[0]["arguments"], serde_json::json!({"path": "a.rs"}));
        assert!(
            result["prose"]
                .as_str()
                .expect("prose")
                .contains("Checking now."),
            "{result}"
        );
    }

    #[test]
    fn scan_bare_units_matches_the_trimmed_single_unit_boundary() {
        let registry = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "_type": "tool_registry",
            "tools": [{"name": "edit", "description": "edit a file", "params": []}],
        }));
        let source = "edit({ path: \"main.go\", content: \n";
        let single = call(
            host_tool_scan_bare_calls_builtin,
            &[text(source.trim()), registry.clone()],
        );
        let units = crate::stdlib::json_to_vm_value(&serde_json::json!([
            {"kind": "text", "text": source}
        ]));
        let batched = call(host_tool_scan_bare_units_builtin, &[units, registry]);

        assert_eq!(batched[0]["bare"], single);
    }
}
