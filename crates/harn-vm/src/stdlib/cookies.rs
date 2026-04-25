use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::SystemTime;

use base64::Engine;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn cookie_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("cookie: {}", message.into()))
}

fn dict(fields: BTreeMap<String, VmValue>) -> VmValue {
    VmValue::Dict(Rc::new(fields))
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(Rc::new(items))
}

fn string(value: impl Into<Rc<str>>) -> VmValue {
    VmValue::String(value.into())
}

fn bool_value(value: bool) -> VmValue {
    VmValue::Bool(value)
}

fn nil() -> VmValue {
    VmValue::Nil
}

fn require_args(args: &[VmValue], count: usize, name: &str) -> Result<(), VmError> {
    if args.len() < count {
        return Err(cookie_error(format!("{name} requires {count} arguments")));
    }
    Ok(())
}

fn option_map<'a>(
    args: &'a [VmValue],
    index: usize,
    name: &str,
) -> Result<Option<&'a BTreeMap<String, VmValue>>, VmError> {
    match args.get(index) {
        Some(VmValue::Dict(map)) => Ok(Some(map)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(cookie_error(format!(
            "{name}: options must be a dict, got {}",
            other.type_name()
        ))),
    }
}

fn option_value<'a>(
    options: Option<&'a BTreeMap<String, VmValue>>,
    names: &[&str],
) -> Option<&'a VmValue> {
    let options = options?;
    for name in names {
        if let Some(value) = options.get(*name) {
            return Some(value);
        }
    }
    None
}

fn option_bool(options: Option<&BTreeMap<String, VmValue>>, names: &[&str], default: bool) -> bool {
    option_value(options, names).map_or(default, VmValue::is_truthy)
}

fn option_string(options: Option<&BTreeMap<String, VmValue>>, names: &[&str]) -> Option<String> {
    option_value(options, names).and_then(|value| match value {
        VmValue::Nil => None,
        other => Some(other.display()),
    })
}

fn option_i64(
    options: Option<&BTreeMap<String, VmValue>>,
    names: &[&str],
) -> Result<Option<i64>, VmError> {
    match option_value(options, names) {
        Some(VmValue::Int(value)) => Ok(Some(*value)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(cookie_error(format!(
            "option {} must be an int, got {}",
            names[0],
            other.type_name()
        ))),
    }
}

fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'a'..=b'z'
                    | b'|'
                    | b'~'
            )
        })
}

fn valid_cookie_value(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| !byte.is_ascii_control() && !matches!(byte, b';' | b',' | b'\\' | b'"' | b' '))
}

fn validate_attr_value(label: &str, value: &str) -> Result<(), VmError> {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b';')
    {
        return Err(cookie_error(format!(
            "{label} contains an invalid cookie attribute value"
        )));
    }
    Ok(())
}

fn normalize_same_site(raw: &str) -> Result<&'static str, VmError> {
    match raw.to_ascii_lowercase().as_str() {
        "lax" => Ok("Lax"),
        "strict" => Ok("Strict"),
        "none" => Ok("None"),
        _ => Err(cookie_error(
            "same_site must be one of Lax, Strict, or None",
        )),
    }
}

fn raw_cookie_headers(value: &VmValue) -> Result<Vec<String>, VmError> {
    match value {
        VmValue::Nil => Ok(Vec::new()),
        VmValue::String(text) => Ok(vec![text.to_string()]),
        VmValue::List(items) => items
            .iter()
            .map(|item| match item {
                VmValue::String(text) => Ok(text.to_string()),
                other => Err(cookie_error(format!(
                    "cookie header list entries must be strings, got {}",
                    other.type_name()
                ))),
            })
            .collect(),
        VmValue::Dict(headers) => {
            let mut out = Vec::new();
            for (name, value) in headers.iter() {
                if name.eq_ignore_ascii_case("cookie") {
                    match value {
                        VmValue::String(text) => out.push(text.to_string()),
                        VmValue::List(items) => {
                            for item in items.iter() {
                                match item {
                                    VmValue::String(text) => out.push(text.to_string()),
                                    other => {
                                        return Err(cookie_error(format!(
                                            "Cookie header values must be strings, got {}",
                                            other.type_name()
                                        )));
                                    }
                                }
                            }
                        }
                        other => {
                            return Err(cookie_error(format!(
                                "Cookie header must be a string or list, got {}",
                                other.type_name()
                            )));
                        }
                    }
                }
            }
            Ok(out)
        }
        other => Err(cookie_error(format!(
            "cookie headers must be a string, list, dict, or nil; got {}",
            other.type_name()
        ))),
    }
}

fn invalid_segment(segment: &str, reason: &str) -> VmValue {
    dict(BTreeMap::from([
        ("segment".to_string(), string(segment.to_string())),
        ("reason".to_string(), string(reason.to_string())),
    ]))
}

struct ParsedCookieHeader {
    cookies: BTreeMap<String, VmValue>,
    pairs: Vec<VmValue>,
    values: BTreeMap<String, Vec<String>>,
    invalid: Vec<VmValue>,
}

fn parse_cookie_header_value(raw: &str) -> ParsedCookieHeader {
    let mut cookies = BTreeMap::new();
    let mut pairs = Vec::new();
    let mut all_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut invalid = Vec::new();

    for segment in raw.split(';') {
        let trimmed = segment.trim_matches(|ch| ch == ' ' || ch == '\t');
        if trimmed.is_empty() {
            continue;
        }
        let Some((name, value)) = trimmed.split_once('=') else {
            invalid.push(invalid_segment(trimmed, "missing '='"));
            continue;
        };
        let name = name.trim_matches(|ch| ch == ' ' || ch == '\t');
        if !valid_cookie_name(name) {
            invalid.push(invalid_segment(trimmed, "invalid name"));
            continue;
        }
        let mut value = value.trim_matches(|ch| ch == ' ' || ch == '\t').to_string();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = value[1..value.len() - 1].to_string();
        }
        if value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b';')
        {
            invalid.push(invalid_segment(trimmed, "invalid value"));
            continue;
        }
        cookies
            .entry(name.to_string())
            .or_insert_with(|| string(value.clone()));
        all_values
            .entry(name.to_string())
            .or_default()
            .push(value.clone());
        pairs.push(dict(BTreeMap::from([
            ("name".to_string(), string(name.to_string())),
            ("value".to_string(), string(value)),
        ])));
    }

    ParsedCookieHeader {
        cookies,
        pairs,
        values: all_values,
        invalid,
    }
}

fn parse_cookie_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 1, "cookie_parse")?;
    let mut cookies = BTreeMap::new();
    let mut pairs = Vec::new();
    let mut all_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut invalid = Vec::new();

    for header in raw_cookie_headers(&args[0])? {
        let parsed = parse_cookie_header_value(&header);
        for (name, value) in parsed.cookies {
            cookies.entry(name).or_insert(value);
        }
        pairs.extend(parsed.pairs);
        invalid.extend(parsed.invalid);
        for (name, values) in parsed.values {
            all_values.entry(name).or_default().extend(values);
        }
    }

    let duplicates = all_values
        .into_iter()
        .filter_map(|(name, values)| {
            if values.len() > 1 {
                Some((
                    name,
                    list(values.into_iter().map(string).collect::<Vec<_>>()),
                ))
            } else {
                None
            }
        })
        .collect();

    Ok(dict(BTreeMap::from([
        ("cookies".to_string(), dict(cookies)),
        ("pairs".to_string(), list(pairs)),
        ("duplicates".to_string(), dict(duplicates)),
        ("invalid".to_string(), list(invalid)),
    ])))
}

fn serialize_cookie_with_defaults(
    name: &str,
    value: &str,
    options: Option<&BTreeMap<String, VmValue>>,
    defaults: CookieDefaults,
) -> Result<String, VmError> {
    if !valid_cookie_name(name) {
        return Err(cookie_error(
            "cookie name is empty or contains invalid characters",
        ));
    }
    if !valid_cookie_value(value) {
        return Err(cookie_error(
            "cookie value contains spaces, quotes, separators, or control characters; encode it first",
        ));
    }

    let mut out = format!("{name}={value}");
    let path =
        option_string(options, &["path", "Path"]).or_else(|| defaults.path.map(str::to_string));
    if let Some(path) = path {
        validate_attr_value("Path", &path)?;
        out.push_str("; Path=");
        out.push_str(&path);
    }

    if let Some(domain) = option_string(options, &["domain", "Domain"]) {
        validate_attr_value("Domain", &domain)?;
        out.push_str("; Domain=");
        out.push_str(&domain);
    }

    if let Some(max_age) = option_i64(options, &["max_age", "Max-Age", "maxAge"])? {
        out.push_str("; Max-Age=");
        out.push_str(&max_age.to_string());
    } else if let Some(max_age) = defaults.max_age {
        out.push_str("; Max-Age=");
        out.push_str(&max_age.to_string());
    }

    let expires = option_string(options, &["expires", "Expires"])
        .or_else(|| defaults.expires.map(str::to_string));
    if let Some(expires) = expires {
        validate_attr_value("Expires", &expires)?;
        out.push_str("; Expires=");
        out.push_str(&expires);
    }

    let http_only = option_bool(
        options,
        &["http_only", "HttpOnly", "httponly"],
        defaults.http_only,
    );
    let secure = option_bool(options, &["secure", "Secure"], defaults.secure);
    let same_site = option_string(options, &["same_site", "SameSite", "sameSite"])
        .or_else(|| defaults.same_site.map(str::to_string));
    let same_site = same_site.as_deref().map(normalize_same_site).transpose()?;
    if same_site == Some("None") && !secure {
        return Err(cookie_error("SameSite=None requires Secure"));
    }

    if http_only {
        out.push_str("; HttpOnly");
    }
    if secure {
        out.push_str("; Secure");
    }
    if let Some(same_site) = same_site {
        out.push_str("; SameSite=");
        out.push_str(same_site);
    }

    Ok(out)
}

#[derive(Clone, Copy)]
struct CookieDefaults {
    path: Option<&'static str>,
    http_only: bool,
    secure: bool,
    same_site: Option<&'static str>,
    max_age: Option<i64>,
    expires: Option<&'static str>,
}

impl CookieDefaults {
    const NONE: Self = Self {
        path: None,
        http_only: false,
        secure: false,
        same_site: None,
        max_age: None,
        expires: None,
    };

    const SESSION: Self = Self {
        path: Some("/"),
        http_only: true,
        secure: true,
        same_site: Some("Lax"),
        max_age: None,
        expires: None,
    };

    const DELETE: Self = Self {
        path: Some("/"),
        http_only: true,
        secure: true,
        same_site: Some("Lax"),
        max_age: Some(0),
        expires: Some("Thu, 01 Jan 1970 00:00:00 GMT"),
    };
}

fn cookie_serialize_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 2, "cookie_serialize")?;
    let name = args[0].display();
    let value = args[1].display();
    let options = option_map(args, 2, "cookie_serialize")?;
    Ok(string(serialize_cookie_with_defaults(
        &name,
        &value,
        options,
        CookieDefaults::NONE,
    )?))
}

fn cookie_delete_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 1, "cookie_delete")?;
    let name = args[0].display();
    let options = option_map(args, 1, "cookie_delete")?;
    Ok(string(serialize_cookie_with_defaults(
        &name,
        "",
        options,
        CookieDefaults::DELETE,
    )?))
}

fn hmac_sha256_base64url(key: &str, message: &str) -> String {
    let mac = crate::connectors::hmac::hmac_sha256(key.as_bytes(), message.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac)
}

fn cookie_sign_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 2, "cookie_sign")?;
    let value = args[0].display();
    let secret = args[1].display();
    if value.contains(';') || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(cookie_error(
            "cookie_sign value must not contain separators or control characters",
        ));
    }
    let signature = hmac_sha256_base64url(&secret, &value);
    Ok(string(format!("{value}.{signature}")))
}

fn cookie_verify_result(ok: bool, value: Option<String>, error: Option<&str>) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("ok".to_string(), bool_value(ok));
    result.insert("value".to_string(), value.map(string).unwrap_or_else(nil));
    result.insert("error".to_string(), error.map(string).unwrap_or_else(nil));
    dict(result)
}

fn cookie_verify_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 2, "cookie_verify")?;
    let signed = args[0].display();
    let secret = args[1].display();
    let Some((value, signature)) = signed.rsplit_once('.') else {
        return Ok(cookie_verify_result(false, None, Some("malformed")));
    };
    let expected = hmac_sha256_base64url(&secret, value);
    if crate::connectors::hmac::secure_eq(signature.as_bytes(), expected.as_bytes()) {
        Ok(cookie_verify_result(true, Some(value.to_string()), None))
    } else {
        Ok(cookie_verify_result(false, None, Some("invalid_signature")))
    }
}

fn session_sign_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 2, "session_sign")?;
    let payload_json = super::json::vm_value_to_json(&args[0]);
    let secret = args[1].display();
    let encoded_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_json);
    let signature = hmac_sha256_base64url(&secret, &encoded_payload);
    Ok(string(format!("v1.{encoded_payload}.{signature}")))
}

fn session_verify_result(ok: bool, payload: VmValue, error: Option<&str>) -> VmValue {
    dict(BTreeMap::from([
        ("ok".to_string(), bool_value(ok)),
        ("payload".to_string(), payload),
        ("error".to_string(), error.map(string).unwrap_or_else(nil)),
    ]))
}

fn session_verify_token(token: &str, secret: &str) -> VmValue {
    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 || parts[0] != "v1" {
        return session_verify_result(false, nil(), Some("malformed"));
    }

    let expected = hmac_sha256_base64url(secret, parts[1]);
    if !crate::connectors::hmac::secure_eq(parts[2].as_bytes(), expected.as_bytes()) {
        return session_verify_result(false, nil(), Some("invalid_signature"));
    }

    let payload_bytes = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]) {
        Ok(bytes) => bytes,
        Err(_) => return session_verify_result(false, nil(), Some("malformed_payload")),
    };
    let payload_json = match String::from_utf8(payload_bytes) {
        Ok(text) => text,
        Err(_) => return session_verify_result(false, nil(), Some("malformed_payload")),
    };
    let payload = match serde_json::from_str::<serde_json::Value>(&payload_json) {
        Ok(value) => crate::schema::json_to_vm_value(&value),
        Err(_) => return session_verify_result(false, nil(), Some("malformed_payload")),
    };
    session_verify_result(true, payload, None)
}

fn session_verify_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 2, "session_verify")?;
    let token = args[0].display();
    let secret = args[1].display();
    Ok(session_verify_token(&token, &secret))
}

fn session_cookie_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 3, "session_cookie")?;
    let name = args[0].display();
    let secret = args[2].display();
    let token = session_sign_builtin(&[args[1].clone(), string(secret)])?.display();
    let options = option_map(args, 3, "session_cookie")?;
    Ok(string(serialize_cookie_with_defaults(
        &name,
        &token,
        options,
        CookieDefaults::SESSION,
    )?))
}

fn session_from_cookies_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 3, "session_from_cookies")?;
    let parsed = parse_cookie_builtin(&[args[0].clone()])?;
    let name = args[1].display();
    let secret = args[2].display();
    let token = match &parsed {
        VmValue::Dict(result) => match result.get("cookies") {
            Some(VmValue::Dict(cookies)) => cookies.get(&name).map(VmValue::display),
            _ => None,
        },
        _ => None,
    };
    match token {
        Some(token) => Ok(session_verify_token(&token, &secret)),
        None => Ok(session_verify_result(false, nil(), Some("missing_cookie"))),
    }
}

fn raw_set_cookie_headers(value: &VmValue) -> Result<Vec<String>, VmError> {
    match value {
        VmValue::Nil => Ok(Vec::new()),
        VmValue::String(text) => Ok(vec![text.to_string()]),
        VmValue::List(items) => items
            .iter()
            .map(|item| match item {
                VmValue::String(text) => Ok(text.to_string()),
                other => Err(cookie_error(format!(
                    "Set-Cookie list entries must be strings, got {}",
                    other.type_name()
                ))),
            })
            .collect(),
        VmValue::Dict(headers) => {
            let mut out = Vec::new();
            for (name, value) in headers.iter() {
                if name.eq_ignore_ascii_case("set-cookie") {
                    out.extend(raw_set_cookie_headers(value)?);
                }
            }
            Ok(out)
        }
        other => Err(cookie_error(format!(
            "Set-Cookie headers must be a string, list, dict, or nil; got {}",
            other.type_name()
        ))),
    }
}

fn parse_set_cookie(header: &str) -> Option<(String, String, bool)> {
    let mut segments = header.split(';');
    let first = segments.next()?.trim_matches(|ch| ch == ' ' || ch == '\t');
    let (name, value) = first.split_once('=')?;
    if !valid_cookie_name(name) {
        return None;
    }

    let mut delete = false;
    for attr in segments {
        let attr = attr.trim_matches(|ch| ch == ' ' || ch == '\t');
        let (attr_name, attr_value) = attr.split_once('=').unwrap_or((attr, ""));
        if attr_name.eq_ignore_ascii_case("Max-Age") {
            if attr_value.trim().parse::<i64>().is_ok_and(|age| age <= 0) {
                delete = true;
            }
        } else if attr_name.eq_ignore_ascii_case("Expires")
            && httpdate::parse_http_date(attr_value.trim())
                .is_ok_and(|expires| expires <= SystemTime::now())
        {
            delete = true;
        }
    }

    Some((name.to_string(), value.to_string(), delete))
}

fn cookie_header_from_map(cookies: &BTreeMap<String, String>) -> String {
    cookies
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn cookie_round_trip_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 1, "cookie_round_trip")?;
    let (request_value, set_cookie_value) = if args.len() == 1 {
        (VmValue::Nil, &args[0])
    } else {
        (args[0].clone(), &args[1])
    };

    let parsed_request = parse_cookie_builtin(&[request_value])?;
    let mut cookies = BTreeMap::<String, String>::new();
    if let VmValue::Dict(result) = parsed_request {
        if let Some(VmValue::Dict(parsed)) = result.get("cookies") {
            for (name, value) in parsed.iter() {
                cookies.insert(name.clone(), value.display());
            }
        }
    }

    for header in raw_set_cookie_headers(set_cookie_value)? {
        if let Some((name, value, delete)) = parse_set_cookie(&header) {
            if delete {
                cookies.remove(&name);
            } else {
                cookies.insert(name, value);
            }
        }
    }

    let header = cookie_header_from_map(&cookies);
    let cookie_values = cookies
        .iter()
        .map(|(name, value)| (name.clone(), string(value.clone())))
        .collect();
    Ok(dict(BTreeMap::from([
        ("cookie_header".to_string(), string(header)),
        ("cookies".to_string(), dict(cookie_values)),
    ])))
}

pub(crate) fn register_cookie_builtins(vm: &mut Vm) {
    vm.register_builtin("cookie_parse", |args, _out| parse_cookie_builtin(args));
    vm.register_builtin("cookie_serialize", |args, _out| {
        cookie_serialize_builtin(args)
    });
    vm.register_builtin("cookie_delete", |args, _out| cookie_delete_builtin(args));
    vm.register_builtin("cookie_sign", |args, _out| cookie_sign_builtin(args));
    vm.register_builtin("cookie_verify", |args, _out| cookie_verify_builtin(args));
    vm.register_builtin("session_sign", |args, _out| session_sign_builtin(args));
    vm.register_builtin("session_verify", |args, _out| session_verify_builtin(args));
    vm.register_builtin("session_cookie", |args, _out| session_cookie_builtin(args));
    vm.register_builtin("session_from_cookies", |args, _out| {
        session_from_cookies_builtin(args)
    });
    vm.register_builtin("cookie_round_trip", |args, _out| {
        cookie_round_trip_builtin(args)
    });
}
