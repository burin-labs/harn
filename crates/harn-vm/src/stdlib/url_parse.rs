//! URL parse / build / query builtins. The module is named `url_parse`
//! rather than `url` to avoid colliding with the `url` crate.

use std::collections::BTreeMap;
use std::rc::Rc;

use url::Url;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn dict_str<'a>(d: &'a BTreeMap<String, VmValue>, key: &str) -> Option<&'a str> {
    match d.get(key) {
        Some(VmValue::String(s)) => Some(s.as_ref()),
        _ => None,
    }
}

pub(crate) fn register_url_builtins(vm: &mut Vm) {
    vm.register_builtin("url_parse", |args, _out| {
        let raw = args.first().map(|a| a.display()).unwrap_or_default();
        let parsed = Url::parse(&raw)
            .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("url_parse: {e}")))))?;
        let mut dict = BTreeMap::new();
        dict.insert(
            "scheme".to_string(),
            VmValue::String(Rc::from(parsed.scheme())),
        );
        dict.insert(
            "host".to_string(),
            parsed
                .host_str()
                .map(|h| VmValue::String(Rc::from(h)))
                .unwrap_or(VmValue::Nil),
        );
        dict.insert(
            "port".to_string(),
            parsed
                .port()
                .map(|p| VmValue::Int(p as i64))
                .unwrap_or(VmValue::Nil),
        );
        dict.insert("path".to_string(), VmValue::String(Rc::from(parsed.path())));
        dict.insert(
            "query".to_string(),
            parsed
                .query()
                .map(|q| VmValue::String(Rc::from(q)))
                .unwrap_or(VmValue::Nil),
        );
        dict.insert(
            "fragment".to_string(),
            parsed
                .fragment()
                .map(|f| VmValue::String(Rc::from(f)))
                .unwrap_or(VmValue::Nil),
        );
        let username = parsed.username();
        dict.insert(
            "username".to_string(),
            if username.is_empty() {
                VmValue::Nil
            } else {
                VmValue::String(Rc::from(username))
            },
        );
        dict.insert(
            "password".to_string(),
            parsed
                .password()
                .map(|p| VmValue::String(Rc::from(p)))
                .unwrap_or(VmValue::Nil),
        );
        Ok(VmValue::Dict(Rc::new(dict)))
    });

    vm.register_builtin("url_build", |args, _out| {
        let Some(VmValue::Dict(parts)) = args.first() else {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "url_build: expected a dict of url parts",
            ))));
        };
        let scheme = dict_str(parts, "scheme").ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from("url_build: 'scheme' is required")))
        })?;
        let host = dict_str(parts, "host").unwrap_or("");
        let path = dict_str(parts, "path").unwrap_or("/");

        // Build the authority piece manually so we can inject userinfo and
        // port without re-parsing.
        let userinfo = match (
            dict_str(parts, "username").filter(|s| !s.is_empty()),
            dict_str(parts, "password"),
        ) {
            (Some(u), Some(p)) => format!("{u}:{p}@"),
            (Some(u), None) => format!("{u}@"),
            _ => String::new(),
        };
        let port = parts
            .get("port")
            .and_then(|v| v.as_int())
            .map(|p| format!(":{p}"))
            .unwrap_or_default();

        let authority = if host.is_empty() {
            String::new()
        } else {
            format!("//{userinfo}{host}{port}")
        };
        let mut composed = format!("{scheme}:{authority}{path}");
        if let Some(q) = dict_str(parts, "query") {
            if !q.is_empty() {
                composed.push('?');
                composed.push_str(q);
            }
        }
        if let Some(f) = dict_str(parts, "fragment") {
            if !f.is_empty() {
                composed.push('#');
                composed.push_str(f);
            }
        }
        // Round-trip through `Url` to canonicalize and surface errors.
        let parsed = Url::parse(&composed)
            .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("url_build: {e}")))))?;
        Ok(VmValue::String(Rc::from(parsed.as_str())))
    });

    vm.register_builtin("query_parse", |args, _out| {
        let raw = args.first().map(|a| a.display()).unwrap_or_default();
        let trimmed = raw.strip_prefix('?').unwrap_or(&raw);
        let pairs: Vec<VmValue> = url::form_urlencoded::parse(trimmed.as_bytes())
            .map(|(k, v)| {
                let mut row = BTreeMap::new();
                row.insert("key".to_string(), VmValue::String(Rc::from(k.into_owned())));
                row.insert(
                    "value".to_string(),
                    VmValue::String(Rc::from(v.into_owned())),
                );
                VmValue::Dict(Rc::new(row))
            })
            .collect();
        Ok(VmValue::List(Rc::new(pairs)))
    });

    vm.register_builtin("query_stringify", |args, _out| {
        let Some(VmValue::List(items)) = args.first() else {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "query_stringify: expected a list of {key, value} dicts",
            ))));
        };
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for item in items.iter() {
            let VmValue::Dict(pair) = item else {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "query_stringify: each item must be a {key, value} dict",
                ))));
            };
            let key = dict_str(pair, "key").unwrap_or("");
            let value = pair.get("value").map(|v| v.display()).unwrap_or_default();
            serializer.append_pair(key, &value);
        }
        Ok(VmValue::String(Rc::from(serializer.finish())))
    });
}
