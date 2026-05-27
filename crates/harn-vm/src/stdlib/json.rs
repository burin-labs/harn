use std::rc::Rc;
use std::{cell::RefCell, collections::BTreeMap, thread_local};

use crate::runtime_limits::RuntimeLimits;
use crate::schema;
use crate::stdlib::json_query;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Cap on memoized parses. Each entry holds the original source string as
/// its key plus the parsed value tree, so the cache can grow large quickly
/// when a script feeds varied JSON. Mirror the regex-cache bound so the
/// VM's per-thread parse caches share a predictable ceiling.
const JSON_PARSE_CACHE_LIMIT: usize = RuntimeLimits::DEFAULT.max_json_parse_cache_entries;

thread_local! {
    static JSON_PARSE_CACHE: RefCell<BTreeMap<String, VmValue>> = const { RefCell::new(BTreeMap::new()) };
}

pub(crate) fn reset_json_state() {
    JSON_PARSE_CACHE.with(|cache| cache.borrow_mut().clear());
}

fn require_args(args: &[VmValue], min: usize, name: &str) -> Result<(), VmError> {
    if args.len() < min {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{name} requires {min} arguments"
        )))));
    }
    Ok(())
}

fn schema_key_list(value: &VmValue, builtin_name: &str) -> Result<Vec<String>, VmError> {
    let list = match value {
        VmValue::List(list) => list,
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{builtin_name}: keys must be a list"
            )))));
        }
    };
    Ok(list.iter().map(VmValue::display).collect())
}

pub(crate) fn register_json_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "json_stringify(value: any) -> string", category = "json")]
fn json_stringify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    Ok(VmValue::String(Rc::from(vm_value_to_json(val))))
}

#[harn_builtin(sig = "json_stringify_pretty(value: any) -> string", category = "json")]
fn json_stringify_pretty_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    serde_json::to_string_pretty(&vm_value_to_data_value(val))
        .map(|text| VmValue::String(Rc::from(text)))
        .map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "json_stringify_pretty: {error}"
            ))))
        })
}

#[harn_builtin(sig = "json_parse(text: string) -> any", category = "json")]
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
        Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "JSON parse error: {e}"
        ))))),
    }
}

#[harn_builtin(sig = "yaml_parse(text: string) -> any", category = "json")]
fn yaml_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    match serde_yml::from_str::<serde_yml::Value>(&text) {
        Ok(value) => match serde_json::to_value(value) {
            Ok(json_value) => Ok(schema::json_to_vm_value(&json_value)),
            Err(error) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "yaml_parse: {error}"
            ))))),
        },
        Err(error) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "YAML parse error: {error}"
        ))))),
    }
}

#[harn_builtin(sig = "yaml_stringify(value: any) -> string", category = "json")]
fn yaml_stringify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let value = args.first().unwrap_or(&VmValue::Nil);
    let data_value = vm_value_to_data_value(value);
    serde_yml::to_string(&data_value)
        .map(|text| VmValue::String(Rc::from(text)))
        .map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "yaml_stringify: {error}"
            ))))
        })
}

#[harn_builtin(sig = "toml_parse(text: string) -> any", category = "json")]
fn toml_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    match toml::from_str::<toml::Value>(&text) {
        Ok(value) => match serde_json::to_value(value) {
            Ok(json_value) => Ok(schema::json_to_vm_value(&json_value)),
            Err(error) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "toml_parse: {error}"
            ))))),
        },
        Err(error) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "TOML parse error: {error}"
        ))))),
    }
}

#[harn_builtin(sig = "toml_stringify(value: any) -> string", category = "json")]
fn toml_stringify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let value = args.first().unwrap_or(&VmValue::Nil);
    let data_value = vm_value_to_data_value(value);
    let toml_value = toml::Value::try_from(data_value).map_err(|error| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "toml_stringify: {error}"
        ))))
    })?;
    toml::to_string(&toml_value)
        .map(|text| VmValue::String(Rc::from(text)))
        .map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
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

#[harn_builtin(sig = "schema_is(value: any, schema: any) -> bool", category = "json")]
fn schema_is_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 2, "schema_is")?;
    Ok(VmValue::Bool(schema::schema_is_value(&args[0], &args[1])?))
}

// `schema_of(T)` is primarily a compile-time intrinsic: the compiler
// rewrites `schema_of(TypeAlias)` to the alias's JSON-Schema dict
// constant. This runtime fallback accepts an already-built schema dict
// and returns it unchanged, keeping `schema_of` useful in pipelines
// that pass schemas around at runtime (e.g. `let s = schema_of(T); ...`).
#[harn_builtin(sig = "schema_of(type_alias: any) -> dict", category = "json")]
fn schema_of_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    require_args(args, 1, "schema_of")?;
    match &args[0] {
        VmValue::Dict(_) => Ok(args[0].clone()),
        other => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "schema_of: expected a type alias or schema dict, got {}",
            other.type_name()
        ))))),
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
    sig = "json_extract(text: string, key?: string) -> any",
    category = "json"
)]
fn json_extract_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.is_empty() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(
            "json_extract requires at least 1 argument: text",
        ))));
    }
    let text = args[0].display();
    let key = args.get(1).map(|a| a.display());

    let json_str = extract_json_from_text(&text);
    let parsed = match serde_json::from_str::<serde_json::Value>(&json_str) {
        Ok(jv) => schema::json_to_vm_value(&jv),
        Err(e) => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "json_extract: failed to parse JSON: {e}"
            )))));
        }
    };

    match key {
        Some(k) => match &parsed {
            VmValue::Dict(map) => match map.get(&k) {
                Some(val) => Ok(val.clone()),
                None => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "json_extract: key '{k}' not found"
                ))))),
            },
            _ => Err(VmError::Thrown(VmValue::String(Rc::from(
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
        .map(|values| VmValue::List(Rc::new(values)))
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))
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
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))
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
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{builtin}: pointer must be empty or start with '/'"
        )))));
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
                            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                                "{builtin}: invalid escape '~{other}' in pointer"
                            )))));
                        }
                        None => {
                            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                                "{builtin}: dangling '~' in pointer"
                            )))));
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
                let Some(next) = map.get(&token) else {
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
                next.insert(head.clone(), replacement);
                VmValue::Dict(Rc::new(next))
            } else if let Some(child) = map.get(head) {
                next.insert(head.clone(), pointer_set_at(child, tail, replacement));
                VmValue::Dict(Rc::new(next))
            } else {
                value.clone()
            }
        }
        VmValue::List(items) => {
            let mut next = items.as_ref().clone();
            if tail.is_empty() {
                if head == "-" || parse_pointer_index(head) == Some(next.len()) {
                    next.push(replacement);
                    return VmValue::List(Rc::new(next));
                }
                if let Some(index) = parse_pointer_index(head) {
                    if let Some(slot) = next.get_mut(index) {
                        *slot = replacement;
                        return VmValue::List(Rc::new(next));
                    }
                }
                value.clone()
            } else if let Some(index) = parse_pointer_index(head) {
                if let Some(child) = items.get(index) {
                    next[index] = pointer_set_at(child, tail, replacement);
                    VmValue::List(Rc::new(next))
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
                next.remove(head);
                VmValue::Dict(Rc::new(next))
            } else if let Some(child) = map.get(head) {
                next.insert(head.clone(), pointer_delete_at(child, tail));
                VmValue::Dict(Rc::new(next))
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
                VmValue::List(Rc::new(next))
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
        VmValue::String(s) => serde_json::json!(s.as_ref()),
        VmValue::Bool(b) => serde_json::json!(b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(items) | VmValue::Set(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_data_value).collect())
        }
        VmValue::Dict(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), vm_value_to_data_value(value)))
                .collect(),
        ),
        VmValue::StructInstance { .. } => serde_json::Value::Object(
            value
                .struct_fields_map()
                .unwrap_or_default()
                .iter()
                .map(|(key, value)| (key.clone(), vm_value_to_data_value(value)))
                .collect(),
        ),
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
        VmValue::Float(n) if n.is_finite() => out.push_str(&n.to_string()),
        VmValue::Float(_) => out.push_str("null"),
        VmValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        VmValue::Nil => out.push_str("null"),
        VmValue::List(items) | VmValue::Set(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_vm_value_to_json(item, out);
            }
            out.push(']');
        }
        VmValue::Dict(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&escape_json_string_vm(k));
                out.push(':');
                write_vm_value_to_json(v, out);
            }
            out.push('}');
        }
        VmValue::StructInstance { .. } => {
            out.push('{');
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
        let value = VmValue::List(Rc::new(vec![
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
