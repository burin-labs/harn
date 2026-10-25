//! URL parse / build / query builtins. The module is named `url_parse`
//! rather than `url` to avoid colliding with the `url` crate.

use harn_vm_core::value::VmDictExt;
use std::collections::BTreeMap;

use url::Url;

use harn_vm::stdlib::macros::harn_builtin;
use harn_vm_core::value::{VmError, VmValue};

fn dict_str<'a>(d: &'a harn_vm_core::value::DictMap, key: &str) -> Option<&'a str> {
    match d.get(key) {
        Some(VmValue::String(s)) => Some(s.as_ref()),
        _ => None,
    }
}

harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_url_builtins);

#[harn_builtin(
    exposure = "pure",
    sig = "url_parse(url: string) -> dict",
    category = "url"
)]
fn url_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let raw = args.first().map(|a| a.display()).unwrap_or_default();
    let parsed = Url::parse(&raw)
        .map_err(|e| VmError::Thrown(VmValue::string(format!("url_parse: {e}"))))?;
    let mut dict = BTreeMap::new();
    dict.put_str("scheme", parsed.scheme());
    dict.put_nullable_str("host", parsed.host_str());
    dict.put_nullable("port", parsed.port().map(|p| VmValue::Int(p as i64)));
    dict.put_str("path", parsed.path());
    dict.put_nullable_str("query", parsed.query());
    dict.put_nullable_str("fragment", parsed.fragment());
    let username = parsed.username();
    dict.insert(
        "username".to_string(),
        if username.is_empty() {
            VmValue::Nil
        } else {
            VmValue::string(username)
        },
    );
    dict.put_nullable_str("password", parsed.password());
    Ok(VmValue::dict(dict))
}

#[harn_builtin(
    exposure = "pure",
    sig = "url_build(parts: dict) -> string",
    category = "url"
)]
fn url_build_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(VmValue::Dict(parts)) = args.first() else {
        return Err(VmError::Thrown(VmValue::string(
            "url_build: expected a dict of url parts",
        )));
    };
    let scheme = dict_str(parts, "scheme")
        .ok_or_else(|| VmError::Thrown(VmValue::string("url_build: 'scheme' is required")))?;
    let host = dict_str(parts, "host").unwrap_or("");
    let path = dict_str(parts, "path").unwrap_or("/");

    let mut parsed = if host.is_empty() {
        let base_path = if path.starts_with('/') { "/" } else { "" };
        Url::parse(&format!("{scheme}:{base_path}"))
            .map_err(|e| VmError::Thrown(VmValue::string(format!("url_build: {e}"))))?
    } else {
        let mut url = Url::parse(&format!("{scheme}://placeholder.invalid/"))
            .map_err(|e| VmError::Thrown(VmValue::string(format!("url_build: {e}"))))?;
        url.set_host(Some(host)).map_err(|_| {
            VmError::Thrown(VmValue::string(format!("url_build: invalid host '{host}'")))
        })?;
        if let Some(username) = dict_str(parts, "username").filter(|value| !value.is_empty()) {
            url.set_username(username).map_err(|_| {
                VmError::Thrown(VmValue::string(
                    "url_build: username is not allowed for this URL",
                ))
            })?;
        }
        if let Some(password) = dict_str(parts, "password") {
            url.set_password(Some(password)).map_err(|_| {
                VmError::Thrown(VmValue::string(
                    "url_build: password is not allowed for this URL",
                ))
            })?;
        }
        if let Some(port) = parts.get("port").and_then(|v| v.as_int()) {
            if !(0..=u16::MAX as i64).contains(&port) {
                return Err(VmError::Thrown(VmValue::string(format!(
                    "url_build: invalid port {port}"
                ))));
            }
            url.set_port(Some(port as u16)).map_err(|_| {
                VmError::Thrown(VmValue::string(
                    "url_build: port is not allowed for this URL",
                ))
            })?;
        }
        url
    };
    parsed.set_path(path);
    parsed.set_query(dict_str(parts, "query"));
    parsed.set_fragment(dict_str(parts, "fragment"));
    Ok(VmValue::string(parsed.as_str()))
}

#[harn_builtin(
    exposure = "pure",
    sig = "query_parse(query: string) -> list",
    category = "url"
)]
fn query_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let raw = args.first().map(|a| a.display()).unwrap_or_default();
    let trimmed = raw.strip_prefix('?').unwrap_or(&raw);
    let pairs: Vec<VmValue> = url::form_urlencoded::parse(trimmed.as_bytes())
        .map(|(k, v)| {
            let mut row = BTreeMap::new();
            row.put_str("key", k.as_ref());
            row.put_str("value", v.as_ref());
            VmValue::dict(row)
        })
        .collect();
    Ok(VmValue::list(pairs))
}

#[harn_builtin(
    exposure = "pure",
    sig = "query_stringify(pairs: list) -> string",
    category = "url"
)]
fn query_stringify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(VmValue::List(items)) = args.first() else {
        return Err(VmError::Thrown(VmValue::string(
            "query_stringify: expected a list of {key, value} dicts",
        )));
    };
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for item in items.iter() {
        let VmValue::Dict(pair) = item else {
            return Err(VmError::Thrown(VmValue::string(
                "query_stringify: each item must be a {key, value} dict",
            )));
        };
        let key = dict_str(pair, "key").unwrap_or("");
        let value = pair.get("value").map(|v| v.display()).unwrap_or_default();
        serializer.append_pair(key, &value);
    }
    Ok(VmValue::string(serializer.finish()))
}
