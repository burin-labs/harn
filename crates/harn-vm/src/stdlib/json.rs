use std::{cell::RefCell, thread_local};

use serde::Deserialize;

use crate::runtime_limits::RuntimeLimits;
use crate::schema;
use crate::stdlib::json_query;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmDictExt, VmError, VmValue};
use crate::vm::Vm;

/// Cap on memoized parses. Each entry holds the original source string as
/// its key plus the parsed value tree, so the cache can grow large quickly
/// when a script feeds varied JSON. Mirror the regex-cache bound so the
/// VM's per-thread parse caches share a predictable ceiling.
const JSON_PARSE_CACHE_LIMIT: usize = RuntimeLimits::DEFAULT.max_json_parse_cache_entries;

/// Serde JSON's default container recursion boundary. Detect it before serde
/// so callers receive a stable structural kind instead of parser prose.
const JSON_PARSE_MAX_CONTAINER_DEPTH: usize = 127;

/// Deepest `VmValue` nesting we will hand to a third-party recursive encoder
/// (pretty JSON via `serde_json`, YAML via `serde_yaml_ng`). Our own JSON writer
/// grows the stack on demand, but those library serializers recurse without a
/// hook we can guard, so a value past this depth is rejected with a catchable
/// error rather than aborting the process. See `value::recursion`.
const MAX_SERIALIZE_DEPTH: usize = RuntimeLimits::DEFAULT.max_value_depth;

/// Reject values nested too deep for the external (serde) encoders to walk
/// without overflowing the native stack.
fn ensure_serializable_depth(value: &VmValue, builtin: &str) -> Result<(), VmError> {
    if crate::value::recursion::depth_within(value, MAX_SERIALIZE_DEPTH) {
        Ok(())
    } else {
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!(
                "{builtin}: value nesting exceeds the maximum serialization depth ({MAX_SERIALIZE_DEPTH})"
            ),
        ))))
    }
}

thread_local! {
    // Internal parse cache (source -> parsed value), not a Dict payload, so it
    // stays a plain `BTreeMap`: it is mutated in place and needs a `const` init.
    static JSON_PARSE_CACHE: RefCell<std::collections::BTreeMap<String, VmValue>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

pub(crate) fn reset_json_state() {
    JSON_PARSE_CACHE.with(|cache| cache.borrow_mut().clear());
}

fn require_args(args: &[VmValue], min: usize, name: &str) -> Result<(), VmError> {
    if args.len() < min {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{name} requires {min} arguments"),
        ))));
    }
    Ok(())
}

fn schema_key_list(value: &VmValue, builtin_name: &str) -> Result<Vec<String>, VmError> {
    let list = match value {
        VmValue::List(list) => list,
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("{builtin_name}: keys must be a list"),
            ))));
        }
    };
    Ok(list.iter().map(VmValue::display).collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JsonDepthViolation {
    line: usize,
    column: usize,
}

/// Find the first container that exceeds the supported JSON nesting depth.
/// Quotes and escapes are tracked so braces inside strings never affect the
/// structural count. This scanner classifies no errors by itself: over-depth
/// input is subsequently validated by serde's iterative ignored-value path.
fn json_depth_violation(text: &str) -> Option<JsonDepthViolation> {
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut line = 1;
    let mut column = 0;

    for character in text.chars() {
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            // serde_json reports byte-oriented columns for string/slice input.
            // Keep recursion failures in that same coordinate system.
            column += character.len_utf8();
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' | '[' => {
                stack.push(character);
                if stack.len() > JSON_PARSE_MAX_CONTAINER_DEPTH {
                    return Some(JsonDepthViolation { line, column });
                }
            }
            '}' => match stack.pop() {
                Some('{') => {}
                _ => return None,
            },
            ']' => match stack.pop() {
                Some('[') => {}
                _ => return None,
            },
            _ => {}
        }
    }
    None
}

/// Validate over-depth input without materializing its value tree. Serde's
/// ignored-value path uses an explicit stack, so disabling its recursive depth
/// guard here is safe and lets syntax errors keep their authoritative parser
/// location instead of being mislabeled as recursion failures.
fn validate_deep_json(text: &str) -> Result<(), serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    deserializer.disable_recursion_limit();
    serde::de::IgnoredAny::deserialize(&mut deserializer)?;
    deserializer.end()
}

fn structured_parse_error_value(
    format: &'static str,
    kind: &'static str,
    message: impl AsRef<str>,
    location: Option<(usize, usize)>,
) -> VmValue {
    let mut fields = crate::value::DictMap::new();
    fields.put_str("error", "structured_parse_error");
    fields.put_str("format", format);
    fields.put_str("kind", kind);
    fields.put_str("message", message);
    if let Some((line, column)) = location {
        fields.put_int("line", line as i64);
        fields.put_int("column", column as i64);
    }
    VmValue::dict(fields)
}

fn malformed_json_error(error: serde_json::Error) -> VmError {
    VmError::Thrown(structured_parse_error_value(
        "json",
        "malformed",
        format!("JSON parse error: {error}"),
        Some((error.line(), error.column())),
    ))
}

fn byte_offset_location(text: &str, byte_offset: usize) -> (usize, usize) {
    let mut offset = byte_offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let prefix = &text[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    let column = text[line_start..offset].chars().count() + 1;
    (line, column)
}

pub(crate) fn register_json_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "json_stringify(value: any) -> string", category = "json")]
fn json_stringify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    Ok(VmValue::String(arcstr::ArcStr::from(vm_value_to_json(val))))
}

#[harn_builtin(sig = "json_stringify_pretty(value: any) -> string", category = "json")]
fn json_stringify_pretty_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    ensure_serializable_depth(val, "json_stringify_pretty")?;
    serde_json::to_string_pretty(&vm_value_to_data_value(val))
        .map(|text| VmValue::String(arcstr::ArcStr::from(text)))
        .map_err(|error| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "json_stringify_pretty: {error}"
            ))))
        })
}

#[harn_builtin(sig = "json_parse(text: string?) -> any", category = "json")]
fn json_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    if let Some(cached) = JSON_PARSE_CACHE.with(|cache| cache.borrow().get(&text).cloned()) {
        return Ok(cached);
    }
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(jv) => {
            let parsed = schema::json_to_vm_value(&jv);
            JSON_PARSE_CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                if cache.len() >= JSON_PARSE_CACHE_LIMIT {
                    cache.clear();
                }
                cache.insert(text, parsed.clone());
            });
            Ok(parsed)
        }
        Err(error) => {
            if let Some(violation) = json_depth_violation(&text) {
                return match validate_deep_json(&text) {
                    Ok(()) => Err(VmError::Thrown(structured_parse_error_value(
                        "json",
                        "recursion_limit",
                        format!(
                            "JSON nesting exceeds the maximum container depth ({JSON_PARSE_MAX_CONTAINER_DEPTH})"
                        ),
                        Some((violation.line, violation.column)),
                    ))),
                    Err(syntax_error) => Err(malformed_json_error(syntax_error)),
                };
            }
            Err(malformed_json_error(error))
        }
    }
}

#[harn_builtin(sig = "yaml_parse(text: string?) -> any", category = "json")]
fn yaml_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) {
        Ok(value) => match serde_json::to_value(value) {
            Ok(json_value) => Ok(schema::json_to_vm_value(&json_value)),
            Err(error) => Err(VmError::Thrown(structured_parse_error_value(
                "yaml",
                "malformed",
                format!("YAML parse error: {error}"),
                None,
            ))),
        },
        Err(error) => {
            let location = error
                .location()
                .map(|location| (location.line(), location.column()));
            Err(VmError::Thrown(structured_parse_error_value(
                "yaml",
                "malformed",
                format!("YAML parse error: {error}"),
                location,
            )))
        }
    }
}

#[harn_builtin(sig = "yaml_stringify(value: any) -> string", category = "json")]
fn yaml_stringify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let value = args.first().unwrap_or(&VmValue::Nil);
    ensure_serializable_depth(value, "yaml_stringify")?;
    let data_value = vm_value_to_data_value(value);
    serde_yaml_ng::to_string(&data_value)
        .map(|text| VmValue::String(arcstr::ArcStr::from(text)))
        .map_err(|error| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "yaml_stringify: {error}"
            ))))
        })
}

#[harn_builtin(sig = "toml_parse(text: string?) -> any", category = "json")]
fn toml_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    match toml::from_str::<toml::Value>(&text) {
        Ok(value) => match serde_json::to_value(value) {
            Ok(json_value) => Ok(schema::json_to_vm_value(&json_value)),
            Err(error) => Err(VmError::Thrown(structured_parse_error_value(
                "toml",
                "malformed",
                format!("TOML parse error: {error}"),
                None,
            ))),
        },
        Err(error) => {
            let location = error
                .span()
                .map(|span| byte_offset_location(&text, span.start));
            Err(VmError::Thrown(structured_parse_error_value(
                "toml",
                "malformed",
                format!("TOML parse error: {}", error.message()),
                location,
            )))
        }
    }
}

#[harn_builtin(sig = "toml_stringify(value: any) -> string", category = "json")]
fn toml_stringify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let value = args.first().unwrap_or(&VmValue::Nil);
    let data_value = vm_value_to_data_value(value);
    let toml_value = toml::Value::try_from(data_value).map_err(|error| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "toml_stringify: {error}"
        ))))
    })?;
    toml::to_string(&toml_value)
        .map(|text| VmValue::String(arcstr::ArcStr::from(text)))
        .map_err(|error| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "toml_stringify: {error}"
            ))))
        })
}

#[harn_builtin(
    sig = "json_validate(value: any, schema: dict) -> bool",
    category = "json"
)]
fn json_validate_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "json_validate")?;
    let result = schema::schema_expect_value(&args[0], &args[1], false);
    match result {
        Ok(_) => Ok(VmValue::Bool(true)),
        Err(error) => Err(error),
    }
}

#[harn_builtin(
    sig = "schema_check(value: any, schema: any) -> any",
    category = "json"
)]
fn schema_check_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_check")?;
    Ok(schema::schema_result_value(&args[0], &args[1], false))
}

#[harn_builtin(
    sig = "schema_parse(value: any, schema: any) -> any",
    category = "json"
)]
fn schema_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_parse")?;
    Ok(schema::schema_result_value(&args[0], &args[1], true))
}

#[harn_builtin(
    sig = "schema_report(value: any, schema: any, apply_defaults?: bool) -> dict",
    category = "json"
)]
fn schema_report_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_report")?;
    let apply_defaults = args.get(2).is_some_and(|value| value.is_truthy());
    Ok(schema::schema_report_value(
        &args[0],
        &args[1],
        apply_defaults,
    ))
}

#[harn_builtin(sig = "schema_is(value: any, schema: any) -> bool", category = "json")]
fn schema_is_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_is")?;
    Ok(VmValue::Bool(schema::schema_is_value(&args[0], &args[1])?))
}

// The compiler rewrites `schema_of(TypeAlias)` to a JSON-Schema expression.
// This runtime fallback accepts an already-built schema dict and returns it
// unchanged, keeping `schema_of` useful in pipelines that pass schemas around
// at runtime (e.g. `let s = schema_of(T); ...`).
#[harn_builtin(sig = "schema_of(type_alias: any) -> dict", category = "json")]
fn schema_of_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 1, "schema_of")?;
    match &args[0] {
        VmValue::Dict(_) => Ok(args[0].clone()),
        other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!(
                "schema_of: expected a type alias or schema dict, got {}",
                other.type_name()
            ),
        )))),
    }
}

#[harn_builtin(sig = "is_type(value: any, schema: any) -> bool", category = "json")]
fn is_type_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "is_type")?;
    Ok(VmValue::Bool(schema::schema_is_value(&args[0], &args[1])?))
}

#[harn_builtin(
    sig = "schema_expect(value: any, schema: any, apply_defaults?: bool) -> any",
    category = "json"
)]
fn schema_expect_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_expect")?;
    let apply_defaults = args.get(2).is_some_and(|value| value.is_truthy());
    schema::schema_expect_value(&args[0], &args[1], apply_defaults)
}

#[harn_builtin(sig = "schema_to_json_schema(schema: any) -> dict", category = "json")]
fn schema_to_json_schema_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 1, "schema_to_json_schema")?;
    schema::schema_to_json_schema_value(&args[0])
}

#[harn_builtin(
    sig = "schema_from_json_schema(json_schema: dict) -> dict",
    category = "json"
)]
fn schema_from_json_schema_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 1, "schema_from_json_schema")?;
    schema::schema_from_json_schema_value(&args[0])
}

#[harn_builtin(
    sig = "schema_to_openapi_schema(schema: any) -> dict",
    category = "json"
)]
fn schema_to_openapi_schema_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 1, "schema_to_openapi_schema")?;
    schema::schema_to_openapi_schema_value(&args[0])
}

#[harn_builtin(
    sig = "schema_from_openapi_schema(openapi_schema: dict) -> dict",
    category = "json"
)]
fn schema_from_openapi_schema_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    require_args(args, 1, "schema_from_openapi_schema")?;
    schema::schema_from_openapi_schema_value(&args[0])
}

#[harn_builtin(
    sig = "schema_extend(base: dict, overrides: dict) -> dict",
    category = "json"
)]
fn schema_extend_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_extend")?;
    schema::schema_extend_value(&args[0], &args[1])
}

#[harn_builtin(sig = "schema_partial(schema: any) -> dict", category = "json")]
fn schema_partial_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 1, "schema_partial")?;
    schema::schema_partial_value(&args[0])
}

#[harn_builtin(
    sig = "schema_pick(schema: any, keys: list) -> dict",
    category = "json"
)]
fn schema_pick_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_pick")?;
    let keys = schema_key_list(&args[1], "schema_pick")?;
    schema::schema_pick_value(&args[0], &keys)
}

#[harn_builtin(
    sig = "schema_omit(schema: any, keys: list) -> dict",
    category = "json"
)]
fn schema_omit_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_omit")?;
    let keys = schema_key_list(&args[1], "schema_omit")?;
    schema::schema_omit_value(&args[0], &keys)
}

#[harn_builtin(
    sig = "json_extract(text: string?, key?: string) -> any",
    category = "json"
)]
fn json_extract_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "json_extract requires at least 1 argument: text",
        ))));
    }
    let text = args[0].display();
    let key = args.get(1).map(|a| a.display());

    let json_str = extract_json_from_text(&text);
    let parsed = match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(jv) => schema::json_to_vm_value(&jv),
        Err(e) => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("json_extract: failed to parse JSON: {e}"),
            ))));
        }
    };

    match key {
        Some(k) => match &parsed {
            VmValue::Dict(map) => match map.get(k.as_str()) {
                Some(val) => Ok(val.clone()),
                None => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!("json_extract: key '{k}' not found"),
                )))),
            },
            _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "json_extract: parsed value is not a dict, cannot extract key",
            )))),
        },
        None => Ok(parsed),
    }
}

#[harn_builtin(
    sig = "json_pointer(value: any, pointer: string) -> any",
    category = "json"
)]
fn json_pointer_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "json_pointer")?;
    let ptr = args[1].display();
    json_pointer_get(&args[0], &ptr)
}

#[harn_builtin(
    sig = "json_pointer_set(value: any, pointer: string, replacement: any) -> any",
    category = "json"
)]
fn json_pointer_set_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 3, "json_pointer_set")?;
    let ptr = args[1].display();
    json_pointer_set(&args[0], &ptr, args[2].clone())
}

#[harn_builtin(
    sig = "json_pointer_delete(value: any, pointer: string) -> any",
    category = "json"
)]
fn json_pointer_delete_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "json_pointer_delete")?;
    let ptr = args[1].display();
    json_pointer_delete(&args[0], &ptr)
}

#[harn_builtin(sig = "jq(value: any, expression: string) -> list", category = "json")]
fn jq_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "jq")?;
    let expr = args[1].display();
    json_query::eval_jq(&args[0], &expr)
        .map(|values| VmValue::List(std::sync::Arc::new(values)))
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))
}

#[harn_builtin(
    sig = "jq_first(value: any, expression: string) -> any",
    category = "json"
)]
fn jq_first_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "jq_first")?;
    let expr = args[1].display();
    json_query::eval_jq(&args[0], &expr)
        .map(|values| values.into_iter().next().unwrap_or(VmValue::Nil))
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &JSON_STRINGIFY_IMPL_DEF,
    &JSON_STRINGIFY_PRETTY_IMPL_DEF,
    &JSON_PARSE_IMPL_DEF,
    &YAML_PARSE_IMPL_DEF,
    &YAML_STRINGIFY_IMPL_DEF,
    &TOML_PARSE_IMPL_DEF,
    &TOML_STRINGIFY_IMPL_DEF,
    &JSON_VALIDATE_IMPL_DEF,
    &SCHEMA_CHECK_IMPL_DEF,
    &SCHEMA_PARSE_IMPL_DEF,
    &SCHEMA_REPORT_IMPL_DEF,
    &SCHEMA_IS_IMPL_DEF,
    &SCHEMA_OF_IMPL_DEF,
    &IS_TYPE_IMPL_DEF,
    &SCHEMA_EXPECT_IMPL_DEF,
    &SCHEMA_TO_JSON_SCHEMA_IMPL_DEF,
    &SCHEMA_FROM_JSON_SCHEMA_IMPL_DEF,
    &SCHEMA_TO_OPENAPI_SCHEMA_IMPL_DEF,
    &SCHEMA_FROM_OPENAPI_SCHEMA_IMPL_DEF,
    &SCHEMA_EXTEND_IMPL_DEF,
    &SCHEMA_PARTIAL_IMPL_DEF,
    &SCHEMA_PICK_IMPL_DEF,
    &SCHEMA_OMIT_IMPL_DEF,
    &JSON_EXTRACT_IMPL_DEF,
    &JSON_POINTER_IMPL_DEF,
    &JSON_POINTER_SET_IMPL_DEF,
    &JSON_POINTER_DELETE_IMPL_DEF,
    &JQ_IMPL_DEF,
    &JQ_FIRST_IMPL_DEF,
];

fn json_pointer_tokens(ptr: &str, builtin: &str) -> Result<Vec<String>, VmError> {
    if ptr.is_empty() {
        return Ok(Vec::new());
    }
    if !ptr.starts_with('/') {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("{builtin}: pointer must be empty or start with '/'"),
        ))));
    }
    ptr[1..]
        .split('/')
        .map(|segment| {
            let mut decoded = String::with_capacity(segment.len());
            let mut chars = segment.chars();
            while let Some(ch) = chars.next() {
                if ch == '~' {
                    match chars.next() {
                        Some('0') => decoded.push('~'),
                        Some('1') => decoded.push('/'),
                        Some(other) => {
                            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                                format!("{builtin}: invalid escape '~{other}' in pointer"),
                            ))));
                        }
                        None => {
                            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                                format!("{builtin}: dangling '~' in pointer"),
                            ))));
                        }
                    }
                } else {
                    decoded.push(ch);
                }
            }
            Ok(decoded)
        })
        .collect()
}

fn json_pointer_get(value: &VmValue, ptr: &str) -> Result<VmValue, VmError> {
    let tokens = json_pointer_tokens(ptr, "json_pointer")?;
    let mut current = value;
    for token in tokens {
        match current {
            VmValue::Dict(map) => {
                let Some(next) = map.get(token.as_str()) else {
                    return Ok(VmValue::Nil);
                };
                current = next;
            }
            VmValue::List(items) => {
                let Some(index) = parse_pointer_index(&token) else {
                    return Ok(VmValue::Nil);
                };
                let Some(next) = items.get(index) else {
                    return Ok(VmValue::Nil);
                };
                current = next;
            }
            _ => return Ok(VmValue::Nil),
        }
    }
    Ok(current.clone())
}

fn json_pointer_set(value: &VmValue, ptr: &str, replacement: VmValue) -> Result<VmValue, VmError> {
    let tokens = json_pointer_tokens(ptr, "json_pointer_set")?;
    if tokens.is_empty() {
        return Ok(replacement);
    }
    Ok(pointer_set_at(value, &tokens, replacement))
}

fn pointer_set_at(value: &VmValue, tokens: &[String], replacement: VmValue) -> VmValue {
    let Some((head, tail)) = tokens.split_first() else {
        return replacement;
    };
    match value {
        VmValue::Dict(map) => {
            let mut next = map.as_ref().clone();
            if tail.is_empty() {
                next.insert(crate::value::intern_key(head), replacement);
                VmValue::dict(next)
            } else if let Some(child) = map.get(head.as_str()) {
                next.insert(
                    crate::value::intern_key(head),
                    pointer_set_at(child, tail, replacement),
                );
                VmValue::dict(next)
            } else {
                value.clone()
            }
        }
        VmValue::List(items) => {
            let mut next = items.as_ref().clone();
            if tail.is_empty() {
                if head == "-" || parse_pointer_index(head) == Some(next.len()) {
                    next.push(replacement);
                    return VmValue::List(std::sync::Arc::new(next));
                }
                if let Some(index) = parse_pointer_index(head) {
                    if let Some(slot) = next.get_mut(index) {
                        *slot = replacement;
                        return VmValue::List(std::sync::Arc::new(next));
                    }
                }
                value.clone()
            } else if let Some(index) = parse_pointer_index(head) {
                if let Some(child) = items.get(index) {
                    next[index] = pointer_set_at(child, tail, replacement);
                    VmValue::List(std::sync::Arc::new(next))
                } else {
                    value.clone()
                }
            } else {
                value.clone()
            }
        }
        _ => value.clone(),
    }
}

fn json_pointer_delete(value: &VmValue, ptr: &str) -> Result<VmValue, VmError> {
    let tokens = json_pointer_tokens(ptr, "json_pointer_delete")?;
    if tokens.is_empty() {
        return Ok(VmValue::Nil);
    }
    Ok(pointer_delete_at(value, &tokens))
}

fn pointer_delete_at(value: &VmValue, tokens: &[String]) -> VmValue {
    let Some((head, tail)) = tokens.split_first() else {
        return value.clone();
    };
    match value {
        VmValue::Dict(map) => {
            let mut next = map.as_ref().clone();
            if tail.is_empty() {
                next.remove(head.as_str());
                VmValue::dict(next)
            } else if let Some(child) = map.get(head.as_str()) {
                next.insert(
                    crate::value::intern_key(head),
                    pointer_delete_at(child, tail),
                );
                VmValue::dict(next)
            } else {
                value.clone()
            }
        }
        VmValue::List(items) => {
            let mut next = items.as_ref().clone();
            if let Some(index) = parse_pointer_index(head) {
                if index >= next.len() {
                    return value.clone();
                }
                if tail.is_empty() {
                    next.remove(index);
                } else {
                    next[index] = pointer_delete_at(&next[index], tail);
                }
                VmValue::List(std::sync::Arc::new(next))
            } else {
                value.clone()
            }
        }
        _ => value.clone(),
    }
}

fn parse_pointer_index(token: &str) -> Option<usize> {
    if token.is_empty()
        || !token.bytes().all(|byte| byte.is_ascii_digit())
        || (token.len() > 1 && token.starts_with('0'))
    {
        return None;
    }
    token.parse::<usize>().ok()
}

pub(crate) fn escape_json_string_vm(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub(crate) fn vm_value_to_data_value(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Int(i) => serde_json::json!(i),
        VmValue::Float(f) if f.is_finite() => serde_json::json!(f),
        VmValue::Float(_) => serde_json::Value::Null,
        // Decimal serializes as a string to preserve exact precision.
        VmValue::Decimal(d) => serde_json::json!(d.to_string()),
        VmValue::String(s) => serde_json::json!(s.as_str()),
        VmValue::Bool(b) => serde_json::json!(b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(items) => crate::value::recursion::guard_recursion(|| {
            serde_json::Value::Array(items.iter().map(vm_value_to_data_value).collect())
        }),
        VmValue::Set(set) => crate::value::recursion::guard_recursion(|| {
            serde_json::Value::Array(set.iter().map(vm_value_to_data_value).collect())
        }),
        VmValue::Dict(map) => crate::value::recursion::guard_recursion(|| {
            serde_json::Value::Object(
                map.iter()
                    .map(|(key, value)| (key.to_string(), vm_value_to_data_value(value)))
                    .collect(),
            )
        }),
        VmValue::StructInstance(_) => crate::value::recursion::guard_recursion(|| {
            serde_json::Value::Object(
                value
                    .struct_fields_map()
                    .unwrap_or_default()
                    .iter()
                    .map(|(key, value)| (key.to_string(), vm_value_to_data_value(value)))
                    .collect(),
            )
        }),
        // Ranges stringify like Display (`"1 to 5"`); use `.to_list()` in Harn
        // to materialise an int array.
        VmValue::Range(_) => serde_json::json!(value.display()),
        _ => serde_json::json!(value.display()),
    }
}

pub(crate) fn vm_value_to_json(val: &VmValue) -> String {
    let mut out = String::new();
    write_vm_value_to_json(val, &mut out);
    out
}

fn write_vm_value_to_json(val: &VmValue, out: &mut String) {
    match val {
        VmValue::String(s) => out.push_str(&escape_json_string_vm(s)),
        VmValue::Bytes(bytes) => {
            use base64::Engine;

            out.push('{');
            out.push_str(&escape_json_string_vm(crate::schema::BYTES_B64_TAG));
            out.push(':');
            out.push_str(&escape_json_string_vm(
                &base64::engine::general_purpose::STANDARD.encode(bytes.as_slice()),
            ));
            out.push('}');
        }
        VmValue::Int(n) => out.push_str(&n.to_string()),
        // Render finite floats through serde's `Number` so the compact output
        // matches `json_stringify_pretty` (which goes through serde) byte for
        // byte: a whole-number float keeps its `.0` and round-trips back as a
        // `float` instead of `f64::to_string()` collapsing `2.0` to `"2"`
        // (which `json_parse` would then read as an `int`).
        VmValue::Float(n) if n.is_finite() => match serde_json::Number::from_f64(*n) {
            Some(num) => out.push_str(&num.to_string()),
            None => out.push_str("null"),
        },
        VmValue::Float(_) => out.push_str("null"),
        // Decimal serializes as a JSON string to preserve exact precision.
        VmValue::Decimal(d) => out.push_str(&escape_json_string_vm(&d.to_string())),
        VmValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        VmValue::Nil => out.push_str("null"),
        VmValue::List(_) | VmValue::Set(_) => {
            let items: &[VmValue] = match val {
                VmValue::Set(set) => set.items(),
                VmValue::List(items) => items,
                _ => &[],
            };
            out.push('[');
            crate::value::recursion::guard_recursion(|| {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_vm_value_to_json(item, out);
                }
            });
            out.push(']');
        }
        VmValue::Dict(map) => {
            out.push('{');
            crate::value::recursion::guard_recursion(|| {
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&escape_json_string_vm(k));
                    out.push(':');
                    write_vm_value_to_json(v, out);
                }
            });
            out.push('}');
        }
        VmValue::StructInstance(_) => {
            out.push('{');
            crate::value::recursion::guard_recursion(|| {
                for (i, (k, v)) in val
                    .struct_fields_map()
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&escape_json_string_vm(k));
                    out.push(':');
                    write_vm_value_to_json(v, out);
                }
            });
            out.push('}');
        }
        VmValue::Range(_) => out.push_str(&escape_json_string_vm(&val.display())),
        _ => out.push_str("null"),
    }
}

pub(crate) fn extract_json_from_text(text: &str) -> String {
    let trimmed = text.trim();

    if let Some(start) = trimmed.find("```") {
        let after_backticks = &trimmed[start + 3..];
        let content_start = if let Some(nl) = after_backticks.find('\n') {
            nl + 1
        } else {
            0
        };
        let content = &after_backticks[content_start..];
        if let Some(end) = content.find("```") {
            return content[..end].trim().to_string();
        }
    }

    if let Some(result) = find_balanced_json(trimmed, b'{', b'}') {
        return result;
    }
    if let Some(result) = find_balanced_json(trimmed, b'[', b']') {
        return result;
    }

    trimmed.to_string()
}

fn find_balanced_json(text: &str, open: u8, close: u8) -> Option<String> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|&b| b == open)?;

    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let mut i = start;

    while i < bytes.len() {
        let b = bytes[i];
        if escape {
            if b == b'u' && i + 4 < bytes.len() {
                i += 5;
            } else {
                i += 1;
            }
            escape = false;
            continue;
        }
        if b == b'\\' && in_string {
            escape = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = !in_string;
        } else if !in_string {
            if b == open {
                depth += 1;
            } else if b == close {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=i].to_string());
                }
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_from_code_fence() {
        let text = "Here is the result:\n```json\n{\"key\": \"value\"}\n```\nDone.";
        assert_eq!(extract_json_from_text(text), "{\"key\": \"value\"}");
    }

    #[test]
    fn whole_number_float_keeps_decimal_point() {
        // A `float` with no fractional part must serialize as `2.0`, not `2`,
        // so it round-trips as a float and matches the pretty printer.
        assert_eq!(vm_value_to_json(&VmValue::Float(2.0)), "2.0");
        assert_eq!(vm_value_to_json(&VmValue::Float(-5.0)), "-5.0");
        assert_eq!(vm_value_to_json(&VmValue::Float(2.5)), "2.5");
        // Ints are still bare.
        assert_eq!(vm_value_to_json(&VmValue::Int(2)), "2");
        // Inside a container, too.
        let list = VmValue::List(std::sync::Arc::new(vec![VmValue::Float(1.0)]));
        assert_eq!(vm_value_to_json(&list), "[1.0]");
    }

    fn deep_list(depth: usize) -> VmValue {
        let mut v = VmValue::Int(0);
        for _ in 0..depth {
            v = VmValue::List(std::sync::Arc::new(vec![v]));
        }
        v
    }

    fn nested_json(depth: usize, leaf: &str) -> String {
        format!("{}{}{}", "[".repeat(depth), leaf, "]".repeat(depth))
    }

    fn parse_failure(
        parse: fn(&[VmValue], &mut String) -> Result<VmValue, VmError>,
        text: &str,
    ) -> VmValue {
        let mut output = String::new();
        match parse(&[VmValue::String(arcstr::ArcStr::from(text))], &mut output)
            .expect_err("invalid structured data must throw")
        {
            VmError::Thrown(failure) => failure,
            other => panic!("expected thrown structured-data failure, got {other:?}"),
        }
    }

    fn json_parse_failure(text: &str) -> VmValue {
        parse_failure(json_parse_impl, text)
    }

    fn failure_field<'a>(failure: &'a VmValue, key: &str) -> &'a VmValue {
        match failure {
            VmValue::Dict(fields) => fields
                .get(key)
                .unwrap_or_else(|| panic!("missing structured-data failure field {key}")),
            other => panic!("expected structured-data failure dict, got {other:?}"),
        }
    }

    #[test]
    fn depth_boundary_distinguishes_supported_and_over_limit_json() {
        let supported = nested_json(JSON_PARSE_MAX_CONTAINER_DEPTH, "null");
        assert_eq!(json_depth_violation(&supported), None);
        assert!(serde_json::from_str::<serde_json::Value>(&supported).is_ok());

        let over_limit = nested_json(JSON_PARSE_MAX_CONTAINER_DEPTH + 1, "null");
        assert_eq!(
            json_depth_violation(&over_limit),
            Some(JsonDepthViolation {
                line: 1,
                column: JSON_PARSE_MAX_CONTAINER_DEPTH + 1,
            })
        );
        assert!(validate_deep_json(&over_limit).is_ok());
    }

    #[test]
    fn deep_malformed_json_is_not_a_recursion_failure() {
        let malformed = nested_json(JSON_PARSE_MAX_CONTAINER_DEPTH + 1, "x");
        assert!(json_depth_violation(&malformed).is_some());
        let error = validate_deep_json(&malformed).expect_err("bare x is invalid JSON");
        assert_eq!(error.line(), 1);
        assert_eq!(error.column(), JSON_PARSE_MAX_CONTAINER_DEPTH + 2);
    }

    #[test]
    fn json_parse_throws_closed_malformed_failure() {
        let failure = json_parse_failure("{\n  bad");
        assert_eq!(
            failure_field(&failure, "error").display(),
            "structured_parse_error"
        );
        assert_eq!(failure_field(&failure, "format").display(), "json");
        assert_eq!(failure_field(&failure, "kind").display(), "malformed");
        assert_eq!(failure_field(&failure, "line").as_int(), Some(2));
        assert!(failure_field(&failure, "column")
            .as_int()
            .is_some_and(|column| column > 0));
        assert!(!failure_field(&failure, "message").as_str_cow().is_empty());
    }

    #[test]
    fn json_parse_throws_closed_recursion_failure() {
        let over_limit = nested_json(JSON_PARSE_MAX_CONTAINER_DEPTH + 1, "null");
        let failure = json_parse_failure(&over_limit);
        assert_eq!(
            failure_field(&failure, "error").display(),
            "structured_parse_error"
        );
        assert_eq!(failure_field(&failure, "format").display(), "json");
        assert_eq!(failure_field(&failure, "kind").display(), "recursion_limit");
        assert_eq!(failure_field(&failure, "line").as_int(), Some(1));
        assert_eq!(
            failure_field(&failure, "column").as_int(),
            Some((JSON_PARSE_MAX_CONTAINER_DEPTH + 1) as i64)
        );
    }

    #[test]
    fn yaml_parse_throws_closed_malformed_failure() {
        let failure = parse_failure(yaml_parse_impl, "name: [\n");
        assert_eq!(
            failure_field(&failure, "error").display(),
            "structured_parse_error"
        );
        assert_eq!(failure_field(&failure, "format").display(), "yaml");
        assert_eq!(failure_field(&failure, "kind").display(), "malformed");
        assert!(failure_field(&failure, "line")
            .as_int()
            .is_some_and(|line| line > 0));
        assert!(failure_field(&failure, "column")
            .as_int()
            .is_some_and(|column| column > 0));
        assert!(!failure_field(&failure, "message").as_str_cow().is_empty());
    }

    #[test]
    fn toml_parse_throws_closed_malformed_failure() {
        let failure = parse_failure(toml_parse_impl, "name = \"Ada\"\nitems = [\n");
        assert_eq!(
            failure_field(&failure, "error").display(),
            "structured_parse_error"
        );
        assert_eq!(failure_field(&failure, "format").display(), "toml");
        assert_eq!(failure_field(&failure, "kind").display(), "malformed");
        assert!(failure_field(&failure, "line")
            .as_int()
            .is_some_and(|line| line > 0));
        assert!(failure_field(&failure, "column")
            .as_int()
            .is_some_and(|column| column > 0));
        assert!(!failure_field(&failure, "message").as_str_cow().is_empty());
    }

    #[test]
    fn byte_offsets_become_one_based_unicode_locations() {
        assert_eq!(byte_offset_location("é\nvalue", 3), (2, 1));
        assert_eq!(byte_offset_location("é\nvalue", 4), (2, 2));
        assert_eq!(byte_offset_location("é\nvalue", 1), (1, 1));
    }

    #[test]
    fn depth_scanner_ignores_brackets_in_strings_and_counts_utf8_bytes() {
        let prefix = "[\"é\\\"{}[]\",";
        let text = format!("{prefix}{}{}", "[".repeat(126), "]".repeat(127));
        assert_eq!(json_depth_violation(&text), None);
        assert!(validate_deep_json(&text).is_ok());

        let over_limit = format!("{prefix}{}null{}", "[".repeat(127), "]".repeat(128));
        assert_eq!(
            json_depth_violation(&over_limit),
            Some(JsonDepthViolation {
                line: 1,
                column: prefix.len() + 127,
            })
        );
        assert!(validate_deep_json(&over_limit).is_ok());
    }

    #[test]
    fn compact_json_handles_deeply_nested_value_without_overflow() {
        // `json_stringify` writes JSON directly with a stack-growing walk, so a
        // value far deeper than any frame budget serializes instead of
        // aborting the process.
        let deep = deep_list(200_000);
        let json = vm_value_to_json(&deep);
        assert!(json.starts_with("[[") || json.starts_with('['));
        assert!(json.ends_with("0]") || json.ends_with(']'));
        crate::value::recursion::dismantle(deep);
    }

    #[test]
    fn pretty_serializer_rejects_values_too_deep_for_serde() {
        // The serde-backed pretty/YAML encoders recurse without a hook we can
        // guard, so values past `max_value_depth` are rejected with a
        // catchable error rather than overflowing the stack.
        let deep = deep_list(MAX_SERIALIZE_DEPTH + 10);
        let err = ensure_serializable_depth(&deep, "json_stringify_pretty")
            .expect_err("value deeper than the limit must be rejected");
        assert!(matches!(err, VmError::Thrown(_)));
        // A value within the limit is accepted.
        let shallow = deep_list(8);
        assert!(ensure_serializable_depth(&shallow, "json_stringify_pretty").is_ok());
        crate::value::recursion::dismantle(deep);
    }

    #[test]
    fn extract_from_code_fence_no_language() {
        let text = "```\n[1, 2, 3]\n```";
        assert_eq!(extract_json_from_text(text), "[1, 2, 3]");
    }

    #[test]
    fn extract_balanced_object() {
        let text = "prefix {\"a\": 1, \"b\": {\"c\": 2}} suffix";
        assert_eq!(
            extract_json_from_text(text),
            "{\"a\": 1, \"b\": {\"c\": 2}}"
        );
    }

    #[test]
    fn extract_balanced_array() {
        let text = "result: [1, [2, 3], 4] end";
        assert_eq!(extract_json_from_text(text), "[1, [2, 3], 4]");
    }

    #[test]
    fn extract_plain_text_fallback() {
        let text = "just plain text";
        assert_eq!(extract_json_from_text(text), "just plain text");
    }

    #[test]
    fn extract_respects_string_brackets() {
        let text = r#"{"msg": "hello {world} [test]"}"#;
        assert_eq!(extract_json_from_text(text), text);
    }

    #[test]
    fn extract_handles_escaped_quotes() {
        let text = r#"{"key": "value with \" quote"}"#;
        assert_eq!(extract_json_from_text(text), text);
    }

    #[test]
    fn pointer_indices_reject_signed_or_non_digit_tokens() {
        assert_eq!(parse_pointer_index("0"), Some(0));
        assert_eq!(parse_pointer_index("10"), Some(10));
        assert_eq!(parse_pointer_index("+1"), None);
        assert_eq!(parse_pointer_index("-1"), None);
        assert_eq!(parse_pointer_index("01"), None);
        assert_eq!(parse_pointer_index("1.0"), None);
    }

    #[test]
    fn stringify_non_finite_floats_as_json_null() {
        let value = VmValue::List(std::sync::Arc::new(vec![
            VmValue::Float(f64::NAN),
            VmValue::Float(f64::INFINITY),
            VmValue::Float(f64::NEG_INFINITY),
            VmValue::Float(1.5),
        ]));

        let compact = vm_value_to_json(&value);
        assert_eq!(compact, "[null,null,null,1.5]");
        serde_json::from_str::<serde_json::Value>(&compact).expect("compact JSON parses");

        let pretty = serde_json::to_string_pretty(&vm_value_to_data_value(&value)).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pretty).unwrap(),
            serde_json::json!([null, null, null, 1.5])
        );
    }
}
