use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use base64::Engine;
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, X509Certificate};

use super::mock::consume_http_mock;
use super::{
    get_options_arg, handle_from_value, next_transport_handle, string_option, vm_error,
    vm_get_bool_option, vm_get_int_option, vm_get_int_option_prefer, vm_get_optional_int_option,
    DEFAULT_BACKOFF_MS, DEFAULT_MAX_MESSAGE_BYTES, DEFAULT_RETRYABLE_METHODS,
    DEFAULT_RETRYABLE_STATUSES, DEFAULT_TIMEOUT_MS, MAX_HTTP_SESSIONS, MAX_HTTP_STREAMS,
    MAX_RETRY_DELAY_MS, MULTIPART_MOCK_BOUNDARY,
};

#[derive(Clone)]
struct RetryConfig {
    max: u32,
    backoff_ms: u64,
    retryable_statuses: Vec<u16>,
    retryable_methods: Vec<String>,
    respect_retry_after: bool,
}

#[derive(Clone)]
pub(super) struct HttpRequestConfig {
    pub(super) total_timeout_ms: u64,
    connect_timeout_ms: Option<u64>,
    read_timeout_ms: Option<u64>,
    retry: RetryConfig,
    follow_redirects: bool,
    max_redirects: usize,
    proxy: Option<HttpProxyConfig>,
    tls: HttpTlsConfig,
    decompress: bool,
}

#[derive(Clone, Default)]
struct HttpTlsConfig {
    ca_bundle_path: Option<String>,
    client_cert_path: Option<String>,
    client_key_path: Option<String>,
    client_identity_path: Option<String>,
    pinned_sha256: Vec<String>,
}

#[derive(Clone)]
struct HttpProxyConfig {
    url: String,
    auth: Option<(String, String)>,
    no_proxy: Option<String>,
}

#[derive(Clone)]
pub(super) struct HttpSession {
    pub(super) client: reqwest::Client,
    options: BTreeMap<String, VmValue>,
}

pub(super) struct HttpRequestParts {
    pub(super) method: reqwest::Method,
    pub(super) headers: reqwest::header::HeaderMap,
    recorded_headers: BTreeMap<String, VmValue>,
    pub(super) body: Option<String>,
    multipart: Option<MultipartRequest>,
}

#[derive(Clone)]
struct MultipartRequest {
    parts: Vec<MultipartField>,
    mock_body: String,
}

#[derive(Clone)]
struct MultipartField {
    name: String,
    value: Vec<u8>,
    filename: Option<String>,
    content_type: Option<String>,
}

struct HttpStreamHandle {
    kind: HttpStreamKind,
    status: i64,
    headers: BTreeMap<String, VmValue>,
    pending: VecDeque<u8>,
    closed: bool,
}

enum HttpStreamKind {
    Real(Rc<tokio::sync::Mutex<reqwest::Response>>),
    Fake,
}

thread_local! {
    static HTTP_CLIENTS: RefCell<HashMap<String, reqwest::Client>> = RefCell::new(HashMap::new());
    pub(super) static HTTP_SESSIONS: RefCell<HashMap<String, HttpSession>> = RefCell::new(HashMap::new());
    static HTTP_STREAMS: RefCell<HashMap<String, HttpStreamHandle>> = RefCell::new(HashMap::new());
}

pub(super) fn reset_client_state() {
    HTTP_CLIENTS.with(|clients| clients.borrow_mut().clear());
    HTTP_SESSIONS.with(|sessions| sessions.borrow_mut().clear());
    clear_http_streams();
}

pub(super) fn clear_http_streams() {
    HTTP_STREAMS.with(|streams| streams.borrow_mut().clear());
}

fn build_http_response(status: i64, headers: BTreeMap<String, VmValue>, body: String) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("status".to_string(), VmValue::Int(status));
    result.insert("headers".to_string(), VmValue::Dict(Rc::new(headers)));
    result.insert("body".to_string(), VmValue::String(Rc::from(body)));
    result.insert(
        "ok".to_string(),
        VmValue::Bool((200..300).contains(&(status as u16))),
    );
    VmValue::Dict(Rc::new(result))
}

fn build_http_download_response(
    status: i64,
    headers: BTreeMap<String, VmValue>,
    bytes_written: u64,
) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("status".to_string(), VmValue::Int(status));
    result.insert("headers".to_string(), VmValue::Dict(Rc::new(headers)));
    result.insert(
        "bytes_written".to_string(),
        VmValue::Int(bytes_written as i64),
    );
    result.insert(
        "ok".to_string(),
        VmValue::Bool((200..300).contains(&(status as u16))),
    );
    VmValue::Dict(Rc::new(result))
}

fn response_headers(headers: &reqwest::header::HeaderMap) -> BTreeMap<String, VmValue> {
    let mut resp_headers = BTreeMap::new();
    for (name, value) in headers {
        if let Ok(v) = value.to_str() {
            resp_headers.insert(name.as_str().to_string(), VmValue::String(Rc::from(v)));
        }
    }
    resp_headers
}

fn merge_options(
    base: &BTreeMap<String, VmValue>,
    overrides: &BTreeMap<String, VmValue>,
) -> BTreeMap<String, VmValue> {
    let mut merged = base.clone();
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

fn resolve_http_path(
    builtin: &str,
    path: &str,
    access: crate::stdlib::sandbox::FsAccess,
) -> Result<std::path::PathBuf, VmError> {
    let resolved = crate::stdlib::process::resolve_source_relative_path(path);
    crate::stdlib::sandbox::enforce_fs_path(builtin, &resolved, access)?;
    Ok(resolved)
}

fn value_to_bytes(value: &VmValue) -> Vec<u8> {
    match value {
        VmValue::Bytes(bytes) => bytes.as_ref().clone(),
        other => other.display().into_bytes(),
    }
}

fn parse_multipart_field(value: &VmValue) -> Result<MultipartField, VmError> {
    let dict = value
        .as_dict()
        .ok_or_else(|| vm_error("http: multipart entries must be dicts"))?;
    let name = dict
        .get("name")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| vm_error("http: multipart entry requires name"))?;
    let content_type = dict
        .get("content_type")
        .or_else(|| dict.get("mime_type"))
        .map(|value| value.display())
        .filter(|value| !value.is_empty());

    let mut filename = dict
        .get("filename")
        .map(|value| value.display())
        .filter(|value| !value.is_empty());
    let value = if let Some(path_value) = dict.get("path") {
        let path = path_value.display();
        let resolved = resolve_http_path(
            "http multipart",
            &path,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
        if filename.is_none() {
            filename = resolved
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string());
        }
        std::fs::read(&resolved).map_err(|error| {
            vm_error(format!(
                "http: failed to read multipart file {}: {error}",
                resolved.display()
            ))
        })?
    } else if let Some(base64_value) = dict.get("value_base64").or_else(|| dict.get("base64")) {
        base64::engine::general_purpose::STANDARD
            .decode(base64_value.display())
            .map_err(|error| vm_error(format!("http: invalid multipart base64 value: {error}")))?
    } else {
        dict.get("value").map(value_to_bytes).ok_or_else(|| {
            vm_error("http: multipart entry requires value, value_base64, or path")
        })?
    };

    Ok(MultipartField {
        name,
        value,
        filename,
        content_type,
    })
}

fn parse_multipart_request(
    options: &BTreeMap<String, VmValue>,
) -> Result<Option<MultipartRequest>, VmError> {
    let Some(value) = options.get("multipart") else {
        return Ok(None);
    };
    let VmValue::List(items) = value else {
        return Err(vm_error("http: multipart must be a list"));
    };
    let parts = items
        .iter()
        .map(parse_multipart_field)
        .collect::<Result<Vec<_>, _>>()?;
    let mock_body = multipart_mock_body(&parts);
    Ok(Some(MultipartRequest { parts, mock_body }))
}

fn multipart_mock_body(parts: &[MultipartField]) -> String {
    let mut out = String::new();
    for part in parts {
        out.push_str("--");
        out.push_str(MULTIPART_MOCK_BOUNDARY);
        out.push_str("\r\nContent-Disposition: form-data; name=\"");
        out.push_str(&part.name);
        out.push('"');
        if let Some(filename) = &part.filename {
            out.push_str("; filename=\"");
            out.push_str(filename);
            out.push('"');
        }
        out.push_str("\r\n");
        if let Some(content_type) = &part.content_type {
            out.push_str("Content-Type: ");
            out.push_str(content_type);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out.push_str(&String::from_utf8_lossy(&part.value));
        out.push_str("\r\n");
    }
    out.push_str("--");
    out.push_str(MULTIPART_MOCK_BOUNDARY);
    out.push_str("--\r\n");
    out
}

fn multipart_form(request: &MultipartRequest) -> Result<reqwest::multipart::Form, VmError> {
    let mut form = reqwest::multipart::Form::new();
    for field in &request.parts {
        let mut part = reqwest::multipart::Part::bytes(field.value.clone());
        if let Some(filename) = &field.filename {
            part = part.file_name(filename.clone());
        }
        if let Some(content_type) = &field.content_type {
            part = part.mime_str(content_type).map_err(|error| {
                vm_error(format!("http: invalid multipart content_type: {error}"))
            })?;
        }
        form = form.part(field.name.clone(), part);
    }
    Ok(form)
}

async fn http_verb_handler(
    method: &str,
    has_body: bool,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let url = args.first().map(|a| a.display()).unwrap_or_default();
    if url.is_empty() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "http_{}: URL is required",
            method.to_ascii_lowercase()
        )))));
    }
    let mut options = if has_body {
        match (args.get(1), args.get(2)) {
            (Some(VmValue::Dict(d)), None) => (**d).clone(),
            (_, Some(VmValue::Dict(d))) => (**d).clone(),
            _ => BTreeMap::new(),
        }
    } else {
        match args.get(1) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        }
    };
    if has_body && !(matches!(args.get(1), Some(VmValue::Dict(_))) && args.get(2).is_none()) {
        let body = args.get(1).map(|a| a.display()).unwrap_or_default();
        options.insert("body".to_string(), VmValue::String(Rc::from(body)));
    }
    vm_execute_http_request(method, &url, &options).await
}

fn parse_proxy_config(options: &BTreeMap<String, VmValue>) -> Option<HttpProxyConfig> {
    let proxy = options.get("proxy")?;
    let (url, no_proxy) = match proxy {
        VmValue::Dict(dict) => (
            dict.get("url")
                .map(|value| value.display())
                .filter(|value| !value.is_empty())?,
            dict.get("no_proxy")
                .map(|value| value.display())
                .filter(|value| !value.is_empty()),
        ),
        other => (other.display(), None),
    };
    if url.is_empty() {
        return None;
    }
    let auth = options
        .get("proxy_auth")
        .and_then(|value| value.as_dict())
        .map(|dict| {
            (
                dict.get("user")
                    .map(|value| value.display())
                    .unwrap_or_default(),
                dict.get("pass")
                    .or_else(|| dict.get("password"))
                    .map(|value| value.display())
                    .unwrap_or_default(),
            )
        })
        .filter(|(user, _)| !user.is_empty());
    Some(HttpProxyConfig {
        url,
        auth,
        no_proxy,
    })
}

fn parse_tls_config(options: &BTreeMap<String, VmValue>) -> HttpTlsConfig {
    let Some(tls) = options.get("tls").and_then(|value| value.as_dict()) else {
        return HttpTlsConfig::default();
    };
    let pinned_sha256 = match tls.get("pinned_sha256") {
        Some(VmValue::List(values)) => values
            .iter()
            .map(|value| value.display())
            .filter(|value| !value.is_empty())
            .collect(),
        Some(value) => {
            let value = value.display();
            if value.is_empty() {
                Vec::new()
            } else {
                vec![value]
            }
        }
        None => Vec::new(),
    };
    HttpTlsConfig {
        ca_bundle_path: string_option(tls, "ca_bundle_path"),
        client_cert_path: string_option(tls, "client_cert_path"),
        client_key_path: string_option(tls, "client_key_path"),
        client_identity_path: string_option(tls, "client_identity_path"),
        pinned_sha256,
    }
}

fn parse_retry_statuses(options: &BTreeMap<String, VmValue>) -> Vec<u16> {
    match options.get("retry_on") {
        Some(VmValue::List(values)) => {
            let statuses: Vec<u16> = values
                .iter()
                .filter_map(|value| value.as_int())
                .filter(|status| (0..=u16::MAX as i64).contains(status))
                .map(|status| status as u16)
                .collect();
            if statuses.is_empty() {
                DEFAULT_RETRYABLE_STATUSES.to_vec()
            } else {
                statuses
            }
        }
        _ => DEFAULT_RETRYABLE_STATUSES.to_vec(),
    }
}

fn parse_retry_methods(options: &BTreeMap<String, VmValue>) -> Vec<String> {
    match options.get("retry_methods") {
        Some(VmValue::List(values)) => {
            let methods: Vec<String> = values
                .iter()
                .map(|value| value.display().trim().to_ascii_uppercase())
                .filter(|value| !value.is_empty())
                .collect();
            if methods.is_empty() {
                DEFAULT_RETRYABLE_METHODS
                    .iter()
                    .map(|method| (*method).to_string())
                    .collect()
            } else {
                methods
            }
        }
        _ => DEFAULT_RETRYABLE_METHODS
            .iter()
            .map(|method| (*method).to_string())
            .collect(),
    }
}

pub(super) fn parse_http_options(options: &BTreeMap<String, VmValue>) -> HttpRequestConfig {
    let total_timeout_ms = vm_get_int_option(options, "total_timeout_ms", -1);
    let total_timeout_ms = if total_timeout_ms >= 0 {
        total_timeout_ms as u64
    } else {
        vm_get_int_option_prefer(options, "timeout_ms", "timeout", DEFAULT_TIMEOUT_MS as i64).max(0)
            as u64
    };
    let retry_options = options.get("retry").and_then(|value| value.as_dict());
    let retry_max = retry_options
        .and_then(|retry| retry.get("max"))
        .and_then(|value| value.as_int())
        .unwrap_or_else(|| vm_get_int_option(options, "retries", 0))
        .max(0) as u32;
    let retry_backoff_ms = retry_options
        .and_then(|retry| retry.get("backoff_ms"))
        .and_then(|value| value.as_int())
        .unwrap_or_else(|| vm_get_int_option(options, "backoff", DEFAULT_BACKOFF_MS as i64))
        .max(0) as u64;
    let respect_retry_after = vm_get_bool_option(options, "respect_retry_after", true);
    let follow_redirects = vm_get_bool_option(options, "follow_redirects", true);
    let max_redirects = vm_get_int_option(options, "max_redirects", 10).max(0) as usize;

    HttpRequestConfig {
        total_timeout_ms,
        connect_timeout_ms: vm_get_optional_int_option(options, "connect_timeout_ms"),
        read_timeout_ms: vm_get_optional_int_option(options, "read_timeout_ms"),
        retry: RetryConfig {
            max: retry_max,
            backoff_ms: retry_backoff_ms,
            retryable_statuses: parse_retry_statuses(options),
            retryable_methods: parse_retry_methods(options),
            respect_retry_after,
        },
        follow_redirects,
        max_redirects,
        proxy: parse_proxy_config(options),
        tls: parse_tls_config(options),
        decompress: vm_get_bool_option(options, "decompress", true),
    }
}

fn http_client_key(config: &HttpRequestConfig) -> String {
    format!(
        "follow_redirects={};max_redirects={};connect_timeout={:?};read_timeout={:?};proxy={};proxy_auth={};no_proxy={};ca={};client_cert={};client_key={};identity={};pins={};decompress={}",
        config.follow_redirects,
        config.max_redirects,
        config.connect_timeout_ms,
        config.read_timeout_ms,
        config
            .proxy
            .as_ref()
            .map(|proxy| proxy.url.as_str())
            .unwrap_or(""),
        config
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.auth.as_ref())
            .map(|(user, _)| user.as_str())
            .unwrap_or(""),
        config
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.no_proxy.as_deref())
            .unwrap_or(""),
        config.tls.ca_bundle_path.as_deref().unwrap_or(""),
        config.tls.client_cert_path.as_deref().unwrap_or(""),
        config.tls.client_key_path.as_deref().unwrap_or(""),
        config.tls.client_identity_path.as_deref().unwrap_or(""),
        config.tls.pinned_sha256.join(","),
        config.decompress,
    )
}

pub(super) fn build_http_client(config: &HttpRequestConfig) -> Result<reqwest::Client, VmError> {
    let redirect_policy = if config.follow_redirects {
        let max_redirects = config.max_redirects;
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= max_redirects {
                attempt.error("too many redirects")
            } else if crate::egress::redirect_url_allowed("http_redirect", attempt.url().as_str()) {
                attempt.follow()
            } else {
                attempt.error("egress policy blocked redirect target")
            }
        })
    } else {
        reqwest::redirect::Policy::none()
    };

    let mut builder = reqwest::Client::builder().redirect(redirect_policy);
    if let Some(ms) = config.connect_timeout_ms {
        builder = builder.connect_timeout(Duration::from_millis(ms));
    }
    if let Some(ms) = config.read_timeout_ms {
        builder = builder.read_timeout(Duration::from_millis(ms));
    }
    if !config.decompress {
        builder = builder.no_gzip().no_brotli().no_deflate().no_zstd();
    }
    if let Some(proxy_config) = &config.proxy {
        let mut proxy = reqwest::Proxy::all(&proxy_config.url)
            .map_err(|e| vm_error(format!("http: invalid proxy '{}': {e}", proxy_config.url)))?;
        if let Some((user, pass)) = &proxy_config.auth {
            proxy = proxy.basic_auth(user, pass);
        }
        if let Some(no_proxy) = &proxy_config.no_proxy {
            proxy = proxy.no_proxy(reqwest::NoProxy::from_string(no_proxy));
        }
        builder = builder.proxy(proxy);
    }
    builder = configure_tls(builder, &config.tls)?;
    builder
        .build()
        .map_err(|e| vm_error(format!("http: failed to build client: {e}")))
}

fn configure_tls(
    mut builder: reqwest::ClientBuilder,
    tls: &HttpTlsConfig,
) -> Result<reqwest::ClientBuilder, VmError> {
    if let Some(path) = &tls.ca_bundle_path {
        let resolved = resolve_http_path("http tls", path, crate::stdlib::sandbox::FsAccess::Read)?;
        let bytes = std::fs::read(&resolved).map_err(|error| {
            vm_error(format!(
                "http: failed to read CA bundle {}: {error}",
                resolved.display()
            ))
        })?;
        match reqwest::Certificate::from_pem_bundle(&bytes) {
            Ok(certs) => {
                for cert in certs {
                    builder = builder.add_root_certificate(cert);
                }
            }
            Err(pem_error) => {
                let cert = reqwest::Certificate::from_der(&bytes).map_err(|der_error| {
                    vm_error(format!(
                        "http: failed to parse CA bundle {} as PEM ({pem_error}) or DER ({der_error})",
                        resolved.display()
                    ))
                })?;
                builder = builder.add_root_certificate(cert);
            }
        }
    }

    if let Some(path) = &tls.client_identity_path {
        let resolved = resolve_http_path("http tls", path, crate::stdlib::sandbox::FsAccess::Read)?;
        let bytes = std::fs::read(&resolved).map_err(|error| {
            vm_error(format!(
                "http: failed to read client identity {}: {error}",
                resolved.display()
            ))
        })?;
        let identity = reqwest::Identity::from_pem(&bytes).map_err(|error| {
            vm_error(format!(
                "http: failed to parse client identity {}: {error}",
                resolved.display()
            ))
        })?;
        builder = builder.identity(identity);
    } else if let Some(cert_path) = &tls.client_cert_path {
        let cert = {
            let resolved = resolve_http_path(
                "http tls",
                cert_path,
                crate::stdlib::sandbox::FsAccess::Read,
            )?;
            std::fs::read(&resolved).map_err(|error| {
                vm_error(format!(
                    "http: failed to read client certificate {}: {error}",
                    resolved.display()
                ))
            })?
        };
        let mut identity_pem = cert;
        if let Some(key_path) = &tls.client_key_path {
            let resolved =
                resolve_http_path("http tls", key_path, crate::stdlib::sandbox::FsAccess::Read)?;
            let key = std::fs::read(&resolved).map_err(|error| {
                vm_error(format!(
                    "http: failed to read client key {}: {error}",
                    resolved.display()
                ))
            })?;
            identity_pem.extend_from_slice(b"\n");
            identity_pem.extend_from_slice(&key);
        }
        let identity = reqwest::Identity::from_pem(&identity_pem)
            .map_err(|error| vm_error(format!("http: failed to parse client identity: {error}")))?;
        builder = builder.identity(identity);
    }

    if !tls.pinned_sha256.is_empty() {
        builder = builder.tls_info(true);
    }
    Ok(builder)
}

pub(super) fn pooled_http_client(config: &HttpRequestConfig) -> Result<reqwest::Client, VmError> {
    let key = http_client_key(config);
    if let Some(client) = HTTP_CLIENTS.with(|clients| clients.borrow().get(&key).cloned()) {
        return Ok(client);
    }

    let client = build_http_client(config)?;
    HTTP_CLIENTS.with(|clients| {
        clients.borrow_mut().insert(key, client.clone());
    });
    Ok(client)
}

fn normalize_pin(value: &str) -> String {
    let trimmed = value.trim();
    let trimmed = trimmed
        .strip_prefix("sha256/")
        .or_else(|| trimmed.strip_prefix("sha256:"))
        .unwrap_or(trimmed);
    let compact = trimmed.replace(':', "");
    if !compact.is_empty() && compact.chars().all(|ch| ch.is_ascii_hexdigit()) {
        compact.to_ascii_lowercase()
    } else {
        compact
    }
}

fn verify_tls_pin(response: &reqwest::Response, pins: &[String]) -> Result<(), VmError> {
    if pins.is_empty() {
        return Ok(());
    }
    let Some(info) = response.extensions().get::<reqwest::tls::TlsInfo>() else {
        return Err(vm_error(
            "http: TLS pinning requested but TLS info is unavailable",
        ));
    };
    let Some(cert_der) = info.peer_certificate() else {
        return Err(vm_error(
            "http: TLS pinning requested but no peer certificate was presented",
        ));
    };
    let (_, cert) = X509Certificate::from_der(cert_der)
        .map_err(|error| vm_error(format!("http: failed to parse peer certificate: {error}")))?;
    let digest = Sha256::digest(cert.tbs_certificate.subject_pki.raw);
    let hex_pin = hex::encode(digest.as_slice());
    let base64_pin = base64::engine::general_purpose::STANDARD.encode(digest);
    let base64url_pin = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    let wanted = pins
        .iter()
        .map(|pin| normalize_pin(pin))
        .collect::<Vec<_>>();
    if wanted
        .iter()
        .any(|pin| pin == &hex_pin || pin == &base64_pin || pin == &base64url_pin)
    {
        Ok(())
    } else {
        Err(vm_error("http: TLS SPKI pin mismatch"))
    }
}

pub(super) fn parse_http_request_parts(
    method: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<HttpRequestParts, VmError> {
    let req_method = method
        .parse::<reqwest::Method>()
        .map_err(|e| vm_error(format!("http: invalid method '{method}': {e}")))?;

    let mut header_map = reqwest::header::HeaderMap::new();
    let mut recorded_headers = BTreeMap::new();

    if let Some(auth_val) = options.get("auth") {
        match auth_val {
            VmValue::String(s) => {
                let hv = reqwest::header::HeaderValue::from_str(s)
                    .map_err(|e| vm_error(format!("http: invalid auth header value: {e}")))?;
                header_map.insert(reqwest::header::AUTHORIZATION, hv);
                recorded_headers.insert(
                    "authorization".to_string(),
                    VmValue::String(Rc::from(s.as_ref())),
                );
            }
            VmValue::Dict(d) => {
                if let Some(bearer) = d.get("bearer") {
                    let token = bearer.display();
                    let authorization = format!("Bearer {token}");
                    let hv = reqwest::header::HeaderValue::from_str(&authorization)
                        .map_err(|e| vm_error(format!("http: invalid bearer token: {e}")))?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                    recorded_headers.insert(
                        "authorization".to_string(),
                        VmValue::String(Rc::from(authorization)),
                    );
                } else if let Some(VmValue::Dict(basic)) = d.get("basic") {
                    let user = basic.get("user").map(|v| v.display()).unwrap_or_default();
                    let password = basic
                        .get("password")
                        .map(|v| v.display())
                        .unwrap_or_default();
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(format!("{user}:{password}"));
                    let authorization = format!("Basic {encoded}");
                    let hv = reqwest::header::HeaderValue::from_str(&authorization)
                        .map_err(|e| vm_error(format!("http: invalid basic auth: {e}")))?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                    recorded_headers.insert(
                        "authorization".to_string(),
                        VmValue::String(Rc::from(authorization)),
                    );
                }
            }
            _ => {}
        }
    }

    if let Some(VmValue::Dict(hdrs)) = options.get("headers") {
        for (k, v) in hdrs.iter() {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| vm_error(format!("http: invalid header name '{k}': {e}")))?;
            let val = reqwest::header::HeaderValue::from_str(&v.display())
                .map_err(|e| vm_error(format!("http: invalid header value for '{k}': {e}")))?;
            header_map.insert(name, val);
            recorded_headers.insert(
                k.to_ascii_lowercase(),
                VmValue::String(Rc::from(v.display())),
            );
        }
    }

    let multipart = parse_multipart_request(options)?;
    if multipart.is_some() {
        if options.contains_key("body") {
            return Err(vm_error(
                "http: body and multipart options are mutually exclusive",
            ));
        }
        recorded_headers.insert(
            "content-type".to_string(),
            VmValue::String(Rc::from(format!(
                "multipart/form-data; boundary={MULTIPART_MOCK_BOUNDARY}"
            ))),
        );
    }

    Ok(HttpRequestParts {
        method: req_method,
        headers: header_map,
        recorded_headers,
        body: if multipart.is_some() {
            multipart.as_ref().map(|request| request.mock_body.clone())
        } else {
            options.get("body").map(|v| v.display())
        },
        multipart,
    })
}

fn final_http_url(
    url: &str,
    options: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<String, VmError> {
    let Some(query) = options.get("query").and_then(VmValue::as_dict) else {
        return Ok(url.to_string());
    };
    let mut parsed = url::Url::parse(url)
        .map_err(|error| vm_error(format!("{builtin}: invalid URL '{url}': {error}")))?;
    {
        let mut pairs = parsed.query_pairs_mut();
        for (key, value) in query.iter() {
            if !matches!(value, VmValue::Nil) {
                pairs.append_pair(key, &value.display());
            }
        }
    }
    Ok(parsed.to_string())
}

pub(super) fn session_from_options(options: &BTreeMap<String, VmValue>) -> Option<String> {
    options
        .get("session")
        .and_then(|value| handle_from_value(value, "http_request").ok())
}

fn method_is_retryable(retry: &RetryConfig, method: &reqwest::Method) -> bool {
    retry
        .retryable_methods
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(method.as_str()))
}

fn should_retry_response(
    config: &HttpRequestConfig,
    method: &reqwest::Method,
    status: u16,
    attempt: u32,
) -> bool {
    attempt < config.retry.max
        && method_is_retryable(&config.retry, method)
        && config.retry.retryable_statuses.contains(&status)
}

fn should_retry_transport(
    config: &HttpRequestConfig,
    method: &reqwest::Method,
    error: &reqwest::Error,
    attempt: u32,
) -> bool {
    attempt < config.retry.max
        && method_is_retryable(&config.retry, method)
        && (error.is_timeout() || error.is_connect())
}

pub(super) fn parse_retry_after_value(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(secs) = value.parse::<f64>() {
        if !secs.is_finite() || secs < 0.0 {
            return Some(Duration::from_millis(0));
        }
        let millis = (secs * 1_000.0) as u64;
        return Some(Duration::from_millis(millis.min(MAX_RETRY_DELAY_MS)));
    }

    if let Ok(target) = httpdate::parse_http_date(value) {
        let millis = target
            .duration_since(SystemTime::now())
            .map(|delta| delta.as_millis() as u64)
            .unwrap_or(0);
        return Some(Duration::from_millis(millis.min(MAX_RETRY_DELAY_MS)));
    }

    None
}

fn parse_retry_after_header(value: &reqwest::header::HeaderValue) -> Option<Duration> {
    value.to_str().ok().and_then(parse_retry_after_value)
}

fn mock_retry_after(status: u16, headers: &BTreeMap<String, VmValue>) -> Option<Duration> {
    if !(status == 429 || status == 503) {
        return None;
    }

    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| parse_retry_after_value(&value.display()))
}

fn response_retry_after(
    status: u16,
    headers: &reqwest::header::HeaderMap,
    respect_retry_after: bool,
) -> Option<Duration> {
    if !respect_retry_after || !(status == 429 || status == 503) {
        return None;
    }
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(parse_retry_after_header)
}

pub(super) fn compute_retry_delay(
    attempt: u32,
    base_ms: u64,
    retry_after: Option<Duration>,
) -> Duration {
    use rand::RngExt;

    let base_delay = base_ms.saturating_mul(1u64 << attempt.min(30));
    let jitter: f64 = rand::rng().random_range(0.75..=1.25);
    let exponential_ms = ((base_delay as f64 * jitter) as u64).min(MAX_RETRY_DELAY_MS);
    let retry_after_ms = retry_after
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
        .min(MAX_RETRY_DELAY_MS);
    Duration::from_millis(exponential_ms.max(retry_after_ms))
}

pub(super) async fn vm_execute_http_request(
    method: &str,
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    if let Some(session_id) = session_from_options(options) {
        return vm_execute_http_session_request(&session_id, method, url, options).await;
    }

    let config = parse_http_options(options);
    let client = pooled_http_client(&config)?;
    vm_execute_http_request_with_client(client, &config, method, url, options).await
}

pub(super) async fn vm_execute_http_session_request(
    session_id: &str,
    method: &str,
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let session = HTTP_SESSIONS.with(|sessions| sessions.borrow().get(session_id).cloned());
    let Some(session) = session else {
        return Err(vm_error(format!(
            "http_session_request: unknown HTTP session '{session_id}'"
        )));
    };
    let merged_options = merge_options(&session.options, options);
    let config = parse_http_options(&merged_options);
    vm_execute_http_request_with_client(session.client, &config, method, url, &merged_options).await
}

async fn vm_execute_http_request_with_client(
    client: reqwest::Client,
    config: &HttpRequestConfig,
    method: &str,
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let parts = parse_http_request_parts(method, options)?;
    let final_url = final_http_url(url, options, "http")?;

    if !final_url.starts_with("http://") && !final_url.starts_with("https://") {
        return Err(vm_error(format!(
            "http: URL must start with http:// or https://, got '{url}'"
        )));
    }
    crate::egress::enforce_url_allowed("http_request", &final_url).await?;

    for attempt in 0..=config.retry.max {
        if let Some(mock_response) = consume_http_mock(
            method,
            &final_url,
            parts.recorded_headers.clone(),
            parts.body.clone(),
        ) {
            let status = mock_response.status.clamp(0, u16::MAX as i64) as u16;
            if should_retry_response(config, &parts.method, status, attempt) {
                let retry_after = if config.retry.respect_retry_after {
                    mock_retry_after(status, &mock_response.headers)
                } else {
                    None
                };
                tokio::time::sleep(compute_retry_delay(
                    attempt,
                    config.retry.backoff_ms,
                    retry_after,
                ))
                .await;
                continue;
            }

            return Ok(build_http_response(
                mock_response.status,
                mock_response.headers,
                mock_response.body,
            ));
        }

        let mut req = client.request(parts.method.clone(), &final_url);
        req = req
            .headers(parts.headers.clone())
            .timeout(Duration::from_millis(config.total_timeout_ms));
        if let Some(multipart) = &parts.multipart {
            req = req.multipart(multipart_form(multipart)?);
        } else if let Some(ref b) = parts.body {
            req = req.body(b.clone());
        }

        match req.send().await {
            Ok(response) => {
                verify_tls_pin(&response, &config.tls.pinned_sha256)?;
                let status = response.status().as_u16();
                if should_retry_response(config, &parts.method, status, attempt) {
                    let retry_after = response_retry_after(
                        status,
                        response.headers(),
                        config.retry.respect_retry_after,
                    );
                    tokio::time::sleep(compute_retry_delay(
                        attempt,
                        config.retry.backoff_ms,
                        retry_after,
                    ))
                    .await;
                    continue;
                }

                let resp_headers = response_headers(response.headers());

                let body_text = response
                    .text()
                    .await
                    .map_err(|e| vm_error(format!("http: failed to read response body: {e}")))?;
                return Ok(build_http_response(status as i64, resp_headers, body_text));
            }
            Err(e) => {
                if should_retry_transport(config, &parts.method, &e, attempt) {
                    tokio::time::sleep(compute_retry_delay(attempt, config.retry.backoff_ms, None))
                        .await;
                    continue;
                }
                return Err(vm_error(format!("http: request failed: {e}")));
            }
        }
    }

    Err(vm_error("http: request failed"))
}

pub(super) async fn vm_http_download(
    url: &str,
    dst_path: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let method = options
        .get("method")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "GET".to_string())
        .to_uppercase();
    let parts = parse_http_request_parts(&method, options)?;
    let final_url = final_http_url(url, options, "http_download")?;
    if let Some(mock_response) = consume_http_mock(
        &method,
        &final_url,
        parts.recorded_headers.clone(),
        parts.body.clone(),
    ) {
        let resolved = resolve_http_path(
            "http_download",
            dst_path,
            crate::stdlib::sandbox::FsAccess::Write,
        )?;
        std::fs::write(&resolved, mock_response.body.as_bytes()).map_err(|error| {
            vm_error(format!(
                "http_download: failed to write {}: {error}",
                resolved.display()
            ))
        })?;
        return Ok(build_http_download_response(
            mock_response.status,
            mock_response.headers,
            mock_response.body.len() as u64,
        ));
    }

    if !final_url.starts_with("http://") && !final_url.starts_with("https://") {
        return Err(vm_error(format!(
            "http_download: URL must start with http:// or https://, got '{url}'"
        )));
    }
    crate::egress::enforce_url_allowed("http_download", &final_url).await?;
    let config = parse_http_options(options);
    let client = if let Some(session_id) = session_from_options(options) {
        HTTP_SESSIONS
            .with(|sessions| sessions.borrow().get(&session_id).cloned())
            .map(|session| session.client)
            .ok_or_else(|| {
                vm_error(format!(
                    "http_download: unknown HTTP session '{session_id}'"
                ))
            })?
    } else {
        pooled_http_client(&config)?
    };
    let mut request = client
        .request(parts.method, &final_url)
        .headers(parts.headers)
        .timeout(Duration::from_millis(config.total_timeout_ms));
    if let Some(multipart) = &parts.multipart {
        request = request.multipart(multipart_form(multipart)?);
    } else if let Some(body) = parts.body {
        request = request.body(body);
    }
    let mut response = request
        .send()
        .await
        .map_err(|error| vm_error(format!("http_download: request failed: {error}")))?;
    verify_tls_pin(&response, &config.tls.pinned_sha256)?;
    let status = response.status().as_u16() as i64;
    let headers = response_headers(response.headers());
    let resolved = resolve_http_path(
        "http_download",
        dst_path,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    let mut file = std::fs::File::create(&resolved).map_err(|error| {
        vm_error(format!(
            "http_download: failed to create {}: {error}",
            resolved.display()
        ))
    })?;
    let mut bytes_written = 0_u64;
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        vm_error(format!(
            "http_download: failed to read response body: {error}"
        ))
    })? {
        file.write_all(&chunk).map_err(|error| {
            vm_error(format!(
                "http_download: failed to write {}: {error}",
                resolved.display()
            ))
        })?;
        bytes_written += chunk.len() as u64;
    }
    Ok(build_http_download_response(status, headers, bytes_written))
}

pub(super) async fn vm_http_stream_open(
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    let method = options
        .get("method")
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "GET".to_string())
        .to_uppercase();
    let parts = parse_http_request_parts(&method, options)?;
    let final_url = final_http_url(url, options, "http_stream_open")?;
    let id = next_transport_handle("http-stream");
    if let Some(mock_response) = consume_http_mock(
        &method,
        &final_url,
        parts.recorded_headers.clone(),
        parts.body.clone(),
    ) {
        let handle = HttpStreamHandle {
            kind: HttpStreamKind::Fake,
            status: mock_response.status,
            headers: mock_response.headers,
            pending: mock_response.body.into_bytes().into(),
            closed: false,
        };
        HTTP_STREAMS.with(|streams| {
            let mut streams = streams.borrow_mut();
            if streams.len() >= MAX_HTTP_STREAMS {
                return Err(vm_error(format!(
                    "http_stream_open: maximum open streams ({MAX_HTTP_STREAMS}) reached"
                )));
            }
            streams.insert(id.clone(), handle);
            Ok(())
        })?;
        return Ok(VmValue::String(Rc::from(id)));
    }

    if !final_url.starts_with("http://") && !final_url.starts_with("https://") {
        return Err(vm_error(format!(
            "http_stream_open: URL must start with http:// or https://, got '{url}'"
        )));
    }
    crate::egress::enforce_url_allowed("http_stream_open", &final_url).await?;
    let config = parse_http_options(options);
    let client = if let Some(session_id) = session_from_options(options) {
        HTTP_SESSIONS
            .with(|sessions| sessions.borrow().get(&session_id).cloned())
            .map(|session| session.client)
            .ok_or_else(|| {
                vm_error(format!(
                    "http_stream_open: unknown HTTP session '{session_id}'"
                ))
            })?
    } else {
        pooled_http_client(&config)?
    };
    let mut request = client
        .request(parts.method, &final_url)
        .headers(parts.headers)
        .timeout(Duration::from_millis(config.total_timeout_ms));
    if let Some(multipart) = &parts.multipart {
        request = request.multipart(multipart_form(multipart)?);
    } else if let Some(body) = parts.body {
        request = request.body(body);
    }
    let response = request
        .send()
        .await
        .map_err(|error| vm_error(format!("http_stream_open: request failed: {error}")))?;
    verify_tls_pin(&response, &config.tls.pinned_sha256)?;
    let status = response.status().as_u16() as i64;
    let headers = response_headers(response.headers());
    let handle = HttpStreamHandle {
        kind: HttpStreamKind::Real(Rc::new(tokio::sync::Mutex::new(response))),
        status,
        headers,
        pending: VecDeque::new(),
        closed: false,
    };
    HTTP_STREAMS.with(|streams| {
        let mut streams = streams.borrow_mut();
        if streams.len() >= MAX_HTTP_STREAMS {
            return Err(vm_error(format!(
                "http_stream_open: maximum open streams ({MAX_HTTP_STREAMS}) reached"
            )));
        }
        streams.insert(id.clone(), handle);
        Ok(())
    })?;
    Ok(VmValue::String(Rc::from(id)))
}

pub(super) async fn vm_http_stream_read(
    stream_id: &str,
    max_bytes: usize,
) -> Result<VmValue, VmError> {
    let (kind, mut pending, closed) = HTTP_STREAMS
        .with(|streams| {
            let mut streams = streams.borrow_mut();
            let handle = streams.get_mut(stream_id)?;
            let kind = match &handle.kind {
                HttpStreamKind::Real(response) => HttpStreamKind::Real(response.clone()),
                HttpStreamKind::Fake => HttpStreamKind::Fake,
            };
            let pending = std::mem::take(&mut handle.pending);
            Some((kind, pending, handle.closed))
        })
        .ok_or_else(|| vm_error(format!("http_stream_read: unknown stream '{stream_id}'")))?;
    if closed {
        return Ok(VmValue::Nil);
    }
    if pending.is_empty() {
        match kind {
            HttpStreamKind::Fake => {}
            HttpStreamKind::Real(response) => {
                let mut response = response.lock().await;
                if let Some(chunk) = response.chunk().await.map_err(|error| {
                    vm_error(format!(
                        "http_stream_read: failed to read response body: {error}"
                    ))
                })? {
                    pending.extend(chunk);
                }
            }
        }
    }
    if pending.is_empty() {
        HTTP_STREAMS.with(|streams| {
            if let Some(handle) = streams.borrow_mut().get_mut(stream_id) {
                handle.closed = true;
            }
        });
        return Ok(VmValue::Nil);
    }
    let take = pending.len().min(max_bytes.max(1));
    let chunk = pending.drain(..take).collect::<Vec<_>>();
    HTTP_STREAMS.with(|streams| {
        if let Some(handle) = streams.borrow_mut().get_mut(stream_id) {
            handle.pending = pending;
        }
    });
    Ok(VmValue::Bytes(Rc::new(chunk)))
}

pub(super) fn vm_http_stream_info(stream_id: &str) -> Result<VmValue, VmError> {
    HTTP_STREAMS.with(|streams| {
        let streams = streams.borrow();
        let handle = streams
            .get(stream_id)
            .ok_or_else(|| vm_error(format!("http_stream_info: unknown stream '{stream_id}'")))?;
        let mut dict = BTreeMap::new();
        dict.insert("status".to_string(), VmValue::Int(handle.status));
        dict.insert(
            "headers".to_string(),
            VmValue::Dict(Rc::new(handle.headers.clone())),
        );
        dict.insert(
            "ok".to_string(),
            VmValue::Bool((200..300).contains(&(handle.status as u16))),
        );
        Ok(VmValue::Dict(Rc::new(dict)))
    })
}

pub(super) fn register_http_verb_builtins(vm: &mut Vm) {
    vm.register_async_builtin("http_get", |args| async move {
        http_verb_handler("GET", false, args).await
    });
    vm.register_async_builtin("http_post", |args| async move {
        http_verb_handler("POST", true, args).await
    });
    vm.register_async_builtin("http_put", |args| async move {
        http_verb_handler("PUT", true, args).await
    });
    vm.register_async_builtin("http_patch", |args| async move {
        http_verb_handler("PATCH", true, args).await
    });
    vm.register_async_builtin("http_delete", |args| async move {
        http_verb_handler("DELETE", false, args).await
    });
}

pub(super) fn register_http_client_builtins(vm: &mut Vm) {
    vm.register_async_builtin("http_request", |args| async move {
        let method = args
            .first()
            .map(|a| a.display())
            .unwrap_or_default()
            .to_uppercase();
        if method.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_request: method is required",
            ))));
        }
        let url = args.get(1).map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_request: URL is required",
            ))));
        }
        let options = match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        vm_execute_http_request(&method, &url, &options).await
    });

    vm.register_async_builtin("http_download", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(vm_error("http_download: URL is required"));
        }
        let dst_path = args.get(1).map(|a| a.display()).unwrap_or_default();
        if dst_path.is_empty() {
            return Err(vm_error("http_download: destination path is required"));
        }
        let options = get_options_arg(&args, 2);
        vm_http_download(&url, &dst_path, &options).await
    });

    vm.register_async_builtin("http_stream_open", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(vm_error("http_stream_open: URL is required"));
        }
        let options = get_options_arg(&args, 1);
        vm_http_stream_open(&url, &options).await
    });

    vm.register_async_builtin("http_stream_read", |args| async move {
        let Some(handle) = args.first() else {
            return Err(vm_error("http_stream_read: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "http_stream_read")?;
        let max_bytes = args
            .get(1)
            .and_then(|value| value.as_int())
            .map(|value| value.max(1) as usize)
            .unwrap_or(DEFAULT_MAX_MESSAGE_BYTES);
        vm_http_stream_read(&stream_id, max_bytes).await
    });

    vm.register_builtin("http_stream_info", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("http_stream_info: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "http_stream_info")?;
        vm_http_stream_info(&stream_id)
    });

    vm.register_builtin("http_stream_close", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("http_stream_close: requires a stream handle"));
        };
        let stream_id = handle_from_value(handle, "http_stream_close")?;
        let removed = HTTP_STREAMS.with(|streams| streams.borrow_mut().remove(&stream_id));
        Ok(VmValue::Bool(removed.is_some()))
    });

    vm.register_builtin("http_session", |args, _out| {
        let options = get_options_arg(args, 0);
        let config = parse_http_options(&options);
        let client = build_http_client(&config)?;
        let id = next_transport_handle("http-session");
        HTTP_SESSIONS.with(|sessions| {
            let mut sessions = sessions.borrow_mut();
            if sessions.len() >= MAX_HTTP_SESSIONS {
                return Err(vm_error(format!(
                    "http_session: maximum open sessions ({MAX_HTTP_SESSIONS}) reached"
                )));
            }
            sessions.insert(id.clone(), HttpSession { client, options });
            Ok(())
        })?;
        Ok(VmValue::String(Rc::from(id)))
    });

    vm.register_async_builtin("http_session_request", |args| async move {
        if args.len() < 3 {
            return Err(vm_error(
                "http_session_request: requires session, method, and URL",
            ));
        }
        let session_id = handle_from_value(&args[0], "http_session_request")?;
        let method = args[1].display().to_uppercase();
        if method.is_empty() {
            return Err(vm_error("http_session_request: method is required"));
        }
        let url = args[2].display();
        if url.is_empty() {
            return Err(vm_error("http_session_request: URL is required"));
        }
        let options = get_options_arg(&args, 3);
        vm_execute_http_session_request(&session_id, &method, &url, &options).await
    });

    vm.register_builtin("http_session_close", |args, _out| {
        let Some(handle) = args.first() else {
            return Err(vm_error("http_session_close: requires a session handle"));
        };
        let session_id = handle_from_value(handle, "http_session_close")?;
        let removed = HTTP_SESSIONS.with(|sessions| sessions.borrow_mut().remove(&session_id));
        Ok(VmValue::Bool(removed.is_some()))
    });
}
