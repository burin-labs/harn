use std::env;
use std::fs;
use std::time::Duration;

use base64::Engine;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::cli::OrchestratorReloadArgs;

const STATE_SNAPSHOT_FILE: &str = "orchestrator-state.json";
const ADMIN_RELOAD_PATH: &str = "/admin/reload";
const API_KEYS_ENV: &str = "HARN_ORCHESTRATOR_API_KEYS";
const HMAC_SECRET_ENV: &str = "HARN_ORCHESTRATOR_HMAC_SECRET";

#[derive(Debug, Deserialize)]
struct StateSnapshot {
    bind: String,
    #[serde(default)]
    listener_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReloadResponse {
    status: String,
    source: String,
    #[serde(default)]
    summary: serde_json::Value,
}

pub(crate) async fn run(args: OrchestratorReloadArgs) -> Result<(), String> {
    let base_url = resolve_admin_url(&args)?;
    let url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        ADMIN_RELOAD_PATH.trim_start_matches('/')
    );
    let body = serde_json::to_vec(&json!({
        "source": "cli",
    }))
    .map_err(|error| format!("failed to encode reload request: {error}"))?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(args.timeout.max(1)))
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))?;
    let mut request = client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .body(body.clone());
    request = authorize_request(request, &url, &body)?;
    let response = request
        .send()
        .await
        .map_err(|error| format!("failed to request orchestrator reload at {url}: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("failed to read orchestrator reload response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "orchestrator reload failed with HTTP {}: {}",
            status.as_u16(),
            text.trim()
        ));
    }
    if args.json {
        println!("{text}");
        return Ok(());
    }
    let parsed: ReloadResponse = serde_json::from_str(&text)
        .map_err(|error| format!("failed to decode orchestrator reload response: {error}"))?;
    let summary = &parsed.summary;
    let added = summary
        .get("added")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let modified = summary
        .get("modified")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let removed = summary
        .get("removed")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    println!(
        "reload {} via {} (+{} ~{} -{})",
        parsed.status, parsed.source, added, modified, removed
    );
    Ok(())
}

fn resolve_admin_url(args: &OrchestratorReloadArgs) -> Result<String, String> {
    if let Some(url) = &args.admin_url {
        return Ok(url.trim_end_matches('/').to_string());
    }
    let path = args.local.state_dir.join(STATE_SNAPSHOT_FILE);
    let body = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let snapshot: StateSnapshot = serde_json::from_str(&body)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    if let Some(url) = snapshot.listener_url {
        return Ok(url.trim_end_matches('/').to_string());
    }
    Ok(format!("http://{}", snapshot.bind))
}

fn authorize_request(
    request: reqwest::RequestBuilder,
    url: &str,
    body: &[u8],
) -> Result<reqwest::RequestBuilder, String> {
    if let Some(api_key) = env::var(API_KEYS_ENV).ok().and_then(|value| {
        value
            .split(',')
            .map(str::trim)
            .find(|segment| !segment.is_empty())
            .map(ToString::to_string)
    }) {
        return Ok(request.header(AUTHORIZATION, format!("Bearer {api_key}")));
    }

    if let Some(secret) = env::var(HMAC_SECRET_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        let parsed = reqwest::Url::parse(url)
            .map_err(|error| format!("invalid admin URL '{url}': {error}"))?;
        let timestamp = OffsetDateTime::now_utc().unix_timestamp();
        let authorization =
            canonical_authorization(&secret, "POST", parsed.path(), timestamp, body);
        return Ok(request.header(AUTHORIZATION, authorization));
    }

    Err(format!(
        "set {API_KEYS_ENV} or {HMAC_SECRET_ENV} so the reload command can authenticate"
    ))
}

fn canonical_authorization(
    secret: &str,
    method: &str,
    path: &str,
    timestamp: i64,
    body: &[u8],
) -> String {
    let signed = canonical_request_message(method, path, &timestamp.to_string(), body);
    let signature = hmac_sha256(secret.as_bytes(), signed.as_bytes());
    format!(
        "{} timestamp={},signature={}",
        harn_vm::connectors::DEFAULT_CANONICAL_HMAC_SCHEME,
        timestamp,
        base64::engine::general_purpose::STANDARD.encode(signature)
    )
}

fn canonical_request_message(method: &str, path: &str, timestamp: &str, body: &[u8]) -> String {
    let body_hash = Sha256::digest(body);
    let body_hex = body_hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "{}\n{}\n{}\n{}",
        method.to_uppercase(),
        path,
        timestamp,
        body_hex
    )
}

fn hmac_sha256(secret: &[u8], data: &[u8]) -> Vec<u8> {
    const BLOCK_SIZE: usize = 64;

    let mut key = if secret.len() > BLOCK_SIZE {
        Sha256::digest(secret).to_vec()
    } else {
        secret.to_vec()
    };
    key.resize(BLOCK_SIZE, 0);

    let mut inner_pad = vec![0x36; BLOCK_SIZE];
    let mut outer_pad = vec![0x5c; BLOCK_SIZE];
    for (pad, key_byte) in inner_pad.iter_mut().zip(key.iter()) {
        *pad ^= *key_byte;
    }
    for (pad, key_byte) in outer_pad.iter_mut().zip(key.iter()) {
        *pad ^= *key_byte;
    }

    let mut inner = Sha256::new();
    inner.update(&inner_pad);
    inner.update(data);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(&outer_pad);
    outer.update(inner_digest);
    outer.finalize().to_vec()
}
