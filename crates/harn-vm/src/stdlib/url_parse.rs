//! URL parse / build / query builtins. The module is named `url_parse`
//! rather than `url` to avoid colliding with the `url` crate.

use crate::value::VmDictExt;
use std::collections::BTreeMap;

use url::Url;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn dict_str<'a>(d: &'a crate::value::DictMap, key: &str) -> Option<&'a str> {
    match d.get(key) {
        Some(VmValue::String(s)) => Some(s.as_ref()),
        _ => None,
    }
}

pub(crate) fn register_url_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &URL_PARSE_IMPL_DEF,
    &URL_BUILD_IMPL_DEF,
    &QUERY_PARSE_IMPL_DEF,
    &QUERY_STRINGIFY_IMPL_DEF,
];

#[harn_builtin(sig = "url_parse(url: string) -> dict", category = "url")]
fn url_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let raw = args.first().map(|a| a.display()).unwrap_or_default();
    let parsed = Url::parse(&raw).map_err(|e| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "url_parse: {e}"
        ))))
    })?;
    let mut dict = BTreeMap::new();
    dict.put_str("scheme", parsed.scheme());
    dict.insert(
        "host".to_string(),
        parsed
            .host_str()
            .map(|h| VmValue::String(std::sync::Arc::from(h)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "port".to_string(),
        parsed
            .port()
            .map(|p| VmValue::Int(p as i64))
            .unwrap_or(VmValue::Nil),
    );
    dict.put_str("path", parsed.path());
    dict.insert(
        "query".to_string(),
        parsed
            .query()
            .map(|q| VmValue::String(std::sync::Arc::from(q)))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "fragment".to_string(),
        parsed
            .fragment()
            .map(|f| VmValue::String(std::sync::Arc::from(f)))
            .unwrap_or(VmValue::Nil),
    );
    let username = parsed.username();
    dict.insert(
        "username".to_string(),
        if username.is_empty() {
            VmValue::Nil
        } else {
            VmValue::String(std::sync::Arc::from(username))
        },
    );
    dict.insert(
        "password".to_string(),
        parsed
            .password()
            .map(|p| VmValue::String(std::sync::Arc::from(p)))
            .unwrap_or(VmValue::Nil),
    );
    Ok(VmValue::dict(dict))
}

#[harn_builtin(sig = "url_build(parts: dict) -> string", category = "url")]
fn url_build_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(VmValue::Dict(parts)) = args.first() else {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "url_build: expected a dict of url parts",
        ))));
    };
    let scheme = dict_str(parts, "scheme").ok_or_else(|| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "url_build: 'scheme' is required",
        )))
    })?;
    let host = dict_str(parts, "host").unwrap_or("");
    let path = dict_str(parts, "path").unwrap_or("/");

    let mut parsed = if host.is_empty() {
        Url::parse(&format!("{scheme}:{path}")).map_err(|e| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "url_build: {e}"
            ))))
        })?
    } else {
        let mut url = Url::parse(&format!("{scheme}://placeholder.invalid/")).map_err(|e| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "url_build: {e}"
            ))))
        })?;
        url.set_host(Some(host)).map_err(|_| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "url_build: invalid host '{host}'"
            ))))
        })?;
        if let Some(username) = dict_str(parts, "username").filter(|value| !value.is_empty()) {
            url.set_username(username).map_err(|_| {
                VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "url_build: username is not allowed for this URL",
                )))
            })?;
        }
        if let Some(password) = dict_str(parts, "password") {
            url.set_password(Some(password)).map_err(|_| {
                VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "url_build: password is not allowed for this URL",
                )))
            })?;
        }
        if let Some(port) = parts.get("port").and_then(|v| v.as_int()) {
            if !(0..=u16::MAX as i64).contains(&port) {
                return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    format!("url_build: invalid port {port}"),
                ))));
            }
            url.set_port(Some(port as u16)).map_err(|_| {
                VmError::Thrown(VmValue::String(std::sync::Arc::from(
                    "url_build: port is not allowed for this URL",
                )))
            })?;
        }
        url.set_path(path);
        url
    };

    if let Some(q) = dict_str(parts, "query").filter(|value| !value.is_empty()) {
        parsed.set_query(Some(q));
    }
    if let Some(f) = dict_str(parts, "fragment").filter(|value| !value.is_empty()) {
        parsed.set_fragment(Some(f));
    }
    Ok(VmValue::String(std::sync::Arc::from(parsed.as_str())))
}

#[harn_builtin(sig = "query_parse(query: string) -> list", category = "url")]
fn query_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let raw = args.first().map(|a| a.display()).unwrap_or_default();
    let trimmed = raw.strip_prefix('?').unwrap_or(&raw);
    let pairs: Vec<VmValue> = url::form_urlencoded::parse(trimmed.as_bytes())
        .map(|(k, v)| {
            let mut row = BTreeMap::new();
            row.put_str("key", &k);
            row.put_str("value", &v);
            VmValue::dict(row)
        })
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(pairs)))
}

#[harn_builtin(sig = "query_stringify(pairs: list) -> string", category = "url")]
fn query_stringify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(VmValue::List(items)) = args.first() else {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "query_stringify: expected a list of {key, value} dicts",
        ))));
    };
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for item in items.iter() {
        let VmValue::Dict(pair) = item else {
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
                "query_stringify: each item must be a {key, value} dict",
            ))));
        };
        let key = dict_str(pair, "key").unwrap_or("");
        let value = pair.get("value").map(|v| v.display()).unwrap_or_default();
        serializer.append_pair(key, &value);
    }
    Ok(VmValue::String(std::sync::Arc::from(serializer.finish())))
}
