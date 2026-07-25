use std::collections::BTreeMap;

use base64::Engine;
use cookie::time::{Duration, OffsetDateTime};
use cookie::{Cookie, CookieJar, Key, SameSite};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn cookie_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("cookie: {}", message.into()))
}

fn dict(fields: crate::value::DictMap) -> VmValue {
    VmValue::dict(fields)
}

fn list(items: Vec<VmValue>) -> VmValue {
    VmValue::List(std::sync::Arc::new(items))
}

fn string(value: impl Into<arcstr::ArcStr>) -> VmValue {
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
) -> Result<Option<&'a crate::value::DictMap>, VmError> {
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
    options: Option<&'a crate::value::DictMap>,
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

fn option_bool(options: Option<&crate::value::DictMap>, names: &[&str], default: bool) -> bool {
    option_value(options, names).map_or(default, VmValue::is_truthy)
}

fn option_string(options: Option<&crate::value::DictMap>, names: &[&str]) -> Option<String> {
    option_value(options, names).and_then(|value| match value {
        VmValue::Nil => None,
        other => Some(other.display()),
    })
}

fn option_i64(
    options: Option<&crate::value::DictMap>,
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

fn parse_same_site(raw: &str) -> Result<SameSite, VmError> {
    match raw.to_ascii_lowercase().as_str() {
        "lax" => Ok(SameSite::Lax),
        "strict" => Ok(SameSite::Strict),
        "none" => Ok(SameSite::None),
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

fn valid_attribute_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| !byte.is_ascii_control() && byte != b';')
}

fn invalid_segment(segment: &str, reason: &str) -> VmValue {
    dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("segment"),
            string(segment.to_string()),
        ),
        (
            crate::value::intern_key("reason"),
            string(reason.to_string()),
        ),
    ]))
}

struct ParsedCookieHeader {
    cookies: crate::value::DictMap,
    pairs: Vec<VmValue>,
    values: BTreeMap<String, Vec<String>>,
    invalid: Vec<VmValue>,
}

fn parse_cookie_header_value(raw: &str) -> ParsedCookieHeader {
    let mut cookies = crate::value::DictMap::new();
    let mut pairs = Vec::new();
    let mut all_values: BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    let mut invalid = Vec::new();

    for segment in raw.split(';') {
        let trimmed = segment.trim_matches(|ch| ch == ' ' || ch == '\t');
        if trimmed.is_empty() {
            continue;
        }
        let cookie = match Cookie::parse(trimmed.to_string()) {
            Ok(cookie) => cookie,
            Err(error) => {
                invalid.push(invalid_segment(trimmed, &error.to_string()));
                continue;
            }
        };
        let name = cookie.name();
        let value = cookie.value().to_string();
        if !valid_cookie_name(name) || !valid_cookie_value(&value) {
            invalid.push(invalid_segment(trimmed, "invalid name or value"));
            continue;
        }
        cookies
            .entry(crate::value::intern_key(name))
            .or_insert_with(|| string(value.clone()));
        all_values
            .entry(name.to_string())
            .or_default()
            .push(value.clone());
        pairs.push(dict(crate::value::DictMap::from_iter([
            (crate::value::intern_key("name"), string(name.to_string())),
            (crate::value::intern_key("value"), string(value)),
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
    let mut cookies = crate::value::DictMap::new();
    let mut pairs = Vec::new();
    let mut all_values: BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
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
                    crate::value::intern_key(&name),
                    list(values.into_iter().map(string).collect::<Vec<_>>()),
                ))
            } else {
                None
            }
        })
        .collect();

    Ok(dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("cookies"), dict(cookies)),
        (crate::value::intern_key("pairs"), list(pairs)),
        (crate::value::intern_key("duplicates"), dict(duplicates)),
        (crate::value::intern_key("invalid"), list(invalid)),
    ])))
}

fn serialize_cookie_with_defaults(
    name: &str,
    value: &str,
    options: Option<&crate::value::DictMap>,
    defaults: CookieDefaults,
) -> Result<String, VmError> {
    if !valid_cookie_name(name) || !valid_cookie_value(value) {
        return Err(cookie_error(
            "cookie name or value contains characters forbidden by RFC 6265",
        ));
    }
    let base = format!("{name}={value}");
    let mut cookie = Cookie::parse(base)
        .map_err(|error| cookie_error(format!("invalid name or value: {error}")))?
        .into_owned();
    if cookie.name() != name || cookie.value() != value {
        return Err(cookie_error(
            "cookie name or value contains attribute separators",
        ));
    }

    let path =
        option_string(options, &["path", "Path"]).or_else(|| defaults.path.map(str::to_string));
    if let Some(path) = path {
        if !valid_attribute_value(&path) {
            return Err(cookie_error("Path contains forbidden characters"));
        }
        cookie.set_path(path);
    }

    if let Some(domain) = option_string(options, &["domain", "Domain"]) {
        if !valid_attribute_value(&domain) {
            return Err(cookie_error("Domain contains forbidden characters"));
        }
        cookie.set_domain(domain);
    }

    if let Some(max_age) = option_i64(options, &["max_age", "Max-Age", "maxAge"])? {
        cookie.set_max_age(Duration::seconds(max_age));
    } else if let Some(max_age) = defaults.max_age {
        cookie.set_max_age(Duration::seconds(max_age));
    }

    let expires = option_string(options, &["expires", "Expires"])
        .or_else(|| defaults.expires.map(str::to_string));
    if let Some(expires) = expires {
        let parsed = Cookie::parse(format!("harn={}; Expires={expires}", cookie.value()))
            .map_err(|error| cookie_error(format!("invalid Expires value: {error}")))?;
        let expires = parsed
            .expires_datetime()
            .ok_or_else(|| cookie_error("invalid Expires value"))?;
        cookie.set_expires(expires);
    }

    let http_only = option_bool(
        options,
        &["http_only", "HttpOnly", "httponly"],
        defaults.http_only,
    );
    let mut secure = option_bool(options, &["secure", "Secure"], defaults.secure);
    let same_site = option_string(options, &["same_site", "SameSite", "sameSite"])
        .or_else(|| defaults.same_site.map(str::to_string));
    let same_site = same_site.as_deref().map(parse_same_site).transpose()?;
    if same_site == Some(SameSite::None) {
        secure = true;
    }
    cookie.set_http_only(http_only);
    cookie.set_secure(secure);
    cookie.set_same_site(same_site);

    Ok(cookie.to_string())
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

fn signing_key(secret: &str) -> Result<Key, VmError> {
    if secret.len() < 32 {
        return Err(cookie_error(
            "signing secret must contain at least 32 bytes of cryptographically random data",
        ));
    }
    Ok(Key::derive_from(secret.as_bytes()))
}

fn sign_value(value: String, secret: &str) -> Result<String, VmError> {
    let key = signing_key(secret)?;
    if !valid_cookie_value(&value) {
        return Err(cookie_error(
            "cookie value contains characters forbidden by RFC 6265",
        ));
    }
    let cookie = Cookie::parse(format!("harn={value}"))
        .map_err(|error| cookie_error(format!("invalid cookie value: {error}")))?;
    if cookie.value() != value {
        return Err(cookie_error(
            "cookie value contains attribute separators or control characters",
        ));
    }
    let mut jar = CookieJar::new();
    jar.signed_mut(&key).add(cookie.into_owned());
    Ok(jar
        .get("harn")
        .expect("signed jar retains inserted cookie")
        .value()
        .to_string())
}

fn verify_value(value: String, secret: &str) -> Result<Option<String>, VmError> {
    let key = signing_key(secret)?;
    let jar = CookieJar::new();
    Ok(jar
        .signed(&key)
        .verify(Cookie::new("harn", value))
        .map(|cookie| cookie.value().to_string()))
}

fn cookie_sign_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 2, "cookie_sign")?;
    let value = args[0].display();
    let secret = args[1].display();
    Ok(string(sign_value(value, &secret)?))
}

fn cookie_verify_result(ok: bool, value: Option<String>, error: Option<&str>) -> VmValue {
    let mut result = crate::value::DictMap::new();
    result.insert(crate::value::intern_key("ok"), bool_value(ok));
    result.insert(
        crate::value::intern_key("value"),
        value.map(string).unwrap_or_else(nil),
    );
    result.insert(
        crate::value::intern_key("error"),
        error.map(string).unwrap_or_else(nil),
    );
    dict(result)
}

fn cookie_verify_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 2, "cookie_verify")?;
    let signed = args[0].display();
    let secret = args[1].display();
    if signed.len() < 44 {
        return Ok(cookie_verify_result(false, None, Some("malformed")));
    }
    Ok(match verify_value(signed, &secret)? {
        Some(value) => cookie_verify_result(true, Some(value), None),
        None => cookie_verify_result(false, None, Some("invalid_signature")),
    })
}

fn session_sign_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 2, "session_sign")?;
    let payload_json = super::json::vm_value_to_json(&args[0]);
    let secret = args[1].display();
    let encoded_payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_json);
    Ok(string(sign_value(encoded_payload, &secret)?))
}

fn session_verify_result(ok: bool, payload: VmValue, error: Option<&str>) -> VmValue {
    dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("ok"), bool_value(ok)),
        (crate::value::intern_key("payload"), payload),
        (
            crate::value::intern_key("error"),
            error.map(string).unwrap_or_else(nil),
        ),
    ]))
}

fn session_verify_token(token: &str, secret: &str) -> Result<VmValue, VmError> {
    let encoded_payload = match verify_value(token.to_string(), secret)? {
        Some(payload) => payload,
        None if token.len() < 44 => {
            return Ok(session_verify_result(false, nil(), Some("malformed")));
        }
        None => {
            return Ok(session_verify_result(
                false,
                nil(),
                Some("invalid_signature"),
            ));
        }
    };
    let payload_bytes =
        match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded_payload) {
            Ok(bytes) => bytes,
            Err(_) => {
                return Ok(session_verify_result(
                    false,
                    nil(),
                    Some("malformed_payload"),
                ))
            }
        };
    let payload_json = match String::from_utf8(payload_bytes) {
        Ok(text) => text,
        Err(_) => {
            return Ok(session_verify_result(
                false,
                nil(),
                Some("malformed_payload"),
            ))
        }
    };
    let payload = match serde_json::from_str::<serde_json::Value>(&payload_json) {
        Ok(value) => crate::schema::json_to_vm_value(&value),
        Err(_) => {
            return Ok(session_verify_result(
                false,
                nil(),
                Some("malformed_payload"),
            ))
        }
    };
    Ok(session_verify_result(true, payload, None))
}

fn session_verify_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    require_args(args, 2, "session_verify")?;
    let token = args[0].display();
    let secret = args[1].display();
    session_verify_token(&token, &secret)
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
            Some(VmValue::Dict(cookies)) => cookies.get(name.as_str()).map(VmValue::display),
            _ => None,
        },
        _ => None,
    };
    match token {
        Some(token) => session_verify_token(&token, &secret),
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
    let cookie = Cookie::parse(header.to_string()).ok()?;
    let delete = cookie.max_age().is_some_and(|age| age <= Duration::ZERO)
        || cookie
            .expires_datetime()
            .is_some_and(|expires| expires <= OffsetDateTime::now_utc());
    Some((
        cookie.name().to_string(),
        cookie.value().to_string(),
        delete,
    ))
}

fn cookie_header_from_map(cookies: &BTreeMap<String, String>) -> String {
    cookies
        .iter()
        .map(|(name, value)| Cookie::new(name.clone(), value.clone()).to_string())
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
    let mut cookies = std::collections::BTreeMap::new();
    if let VmValue::Dict(result) = parsed_request {
        if let Some(VmValue::Dict(parsed)) = result.get("cookies") {
            for (name, value) in parsed.iter() {
                cookies.insert(name.to_string(), value.display());
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
        .map(|(name, value)| (crate::value::intern_key(name), string(value.clone())))
        .collect::<crate::value::DictMap>();
    Ok(dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("cookie_header"), string(header)),
        (crate::value::intern_key("cookies"), dict(cookie_values)),
    ])))
}

pub(crate) fn register_cookie_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "cookie_parse(header: string | dict | list) -> dict",
    category = "cookies"
)]
fn cookie_parse_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    parse_cookie_builtin(args)
}

#[harn_builtin(
    sig = "cookie_serialize(name: string, value: string, options?: dict?) -> string",
    category = "cookies"
)]
fn cookie_serialize_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    cookie_serialize_builtin(args)
}

#[harn_builtin(
    sig = "cookie_delete(name: string, options?: dict?) -> string",
    category = "cookies"
)]
fn cookie_delete_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    cookie_delete_builtin(args)
}

#[harn_builtin(
    sig = "cookie_sign(value: string, secret: string) -> string",
    category = "cookies"
)]
fn cookie_sign_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    cookie_sign_builtin(args)
}

#[harn_builtin(
    sig = "cookie_verify(signed: string, secret: string) -> dict",
    category = "cookies"
)]
fn cookie_verify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    cookie_verify_builtin(args)
}

#[harn_builtin(
    sig = "session_sign(payload: any, secret: string) -> string",
    category = "cookies"
)]
fn session_sign_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    session_sign_builtin(args)
}

#[harn_builtin(
    sig = "session_verify(token: string, secret: string) -> dict",
    category = "cookies"
)]
fn session_verify_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    session_verify_builtin(args)
}

#[harn_builtin(
    sig = "session_cookie(name: string, payload: any, secret: string, options?: dict?) -> string",
    category = "cookies"
)]
fn session_cookie_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    session_cookie_builtin(args)
}

#[harn_builtin(
    sig = "session_from_cookies(header: string | dict | list, name: string, secret: string) -> dict",
    category = "cookies"
)]
fn session_from_cookies_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    session_from_cookies_builtin(args)
}

#[harn_builtin(
    sig = "cookie_round_trip(request_or_set_cookie: string | dict | list, set_cookie?: string | dict | list) -> dict",
    category = "cookies"
)]
fn cookie_round_trip_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    cookie_round_trip_builtin(args)
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &COOKIE_PARSE_IMPL_DEF,
    &COOKIE_SERIALIZE_IMPL_DEF,
    &COOKIE_DELETE_IMPL_DEF,
    &COOKIE_SIGN_IMPL_DEF,
    &COOKIE_VERIFY_IMPL_DEF,
    &SESSION_SIGN_IMPL_DEF,
    &SESSION_VERIFY_IMPL_DEF,
    &SESSION_COOKIE_IMPL_DEF,
    &SESSION_FROM_COOKIES_IMPL_DEF,
    &COOKIE_ROUND_TRIP_IMPL_DEF,
];

#[cfg(test)]
mod tests {
    use super::{
        parse_set_cookie, serialize_cookie_with_defaults, sign_value, verify_value, Cookie,
        CookieDefaults, SameSite,
    };
    use crate::value::{intern_key, DictMap, VmValue};

    #[test]
    fn parse_set_cookie_trims_name_before_validation() {
        // Whitespace around the name (`name = value`) must not cause the whole
        // Set-Cookie to be dropped, matching `parse_cookie_header_value`.
        let (name, value, delete) = parse_set_cookie("sid = abc; Path=/")
            .expect("whitespace around name should still parse");
        assert_eq!(name, "sid");
        assert_eq!(value, "abc");
        assert!(!delete);
    }

    #[test]
    fn parse_set_cookie_flags_expired_max_age_as_delete() {
        let (name, _value, delete) = parse_set_cookie("sid=abc; Max-Age=0").expect("should parse");
        assert_eq!(name, "sid");
        assert!(delete);
    }

    #[test]
    fn serialization_rejects_invalid_names_and_secures_same_site_none() {
        assert!(
            serialize_cookie_with_defaults("bad name", "value", None, CookieDefaults::NONE)
                .is_err()
        );
        let mut injected_path = DictMap::new();
        injected_path.insert(
            intern_key("path"),
            VmValue::string("/; Secure; Domain=attacker.example"),
        );
        assert!(serialize_cookie_with_defaults(
            "sid",
            "value",
            Some(&injected_path),
            CookieDefaults::NONE,
        )
        .is_err());
        let mut options = DictMap::new();
        options.insert(intern_key("same_site"), VmValue::string("None"));
        let serialized =
            serialize_cookie_with_defaults("sid", "value", Some(&options), CookieDefaults::NONE)
                .unwrap();
        let parsed = Cookie::parse(serialized).unwrap();
        assert_eq!(parsed.same_site(), Some(SameSite::None));
        assert_eq!(parsed.secure(), Some(true));
    }

    #[test]
    fn signed_values_use_the_cookie_crate_wire_format() {
        let secret = "0123456789abcdef0123456789abcdef";
        let signed = sign_value("plain-value".to_string(), secret).unwrap();
        assert_ne!(signed, "plain-value");
        assert_eq!(
            verify_value(signed.clone(), secret).unwrap().as_deref(),
            Some("plain-value")
        );
        assert_eq!(verify_value(format!("{signed}x"), secret).unwrap(), None);
    }
}
