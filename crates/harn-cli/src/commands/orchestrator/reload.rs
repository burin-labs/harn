use super::errors::OrchestratorError;
use std::env;
use std::fs;
use std::time::Duration;

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{cli::OrchestratorReloadArgs, net};

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

pub(crate) async fn run(args: OrchestratorReloadArgs) -> Result<(), OrchestratorError> {
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
    let client = net::http_client(
        "cli.orchestrator.reload",
        Duration::from_secs(args.timeout.max(1)),
    )?;
    let mut request = client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .body(body.clone());
    request = authorize_request(request, &url, &body)?;
    let response = request.send().await.map_err(|error| {
        format!(
            "failed to request orchestrator reload at {url}: {}",
            net::reqwest_error(&error)
        )
    })?;
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
        )
        .into());
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

fn resolve_admin_url(args: &OrchestratorReloadArgs) -> Result<String, OrchestratorError> {
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
) -> Result<reqwest::RequestBuilder, OrchestratorError> {
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

    Err(
        format!("set {API_KEYS_ENV} or {HMAC_SECRET_ENV} so the reload command can authenticate")
            .into(),
    )
}

fn canonical_authorization(
    secret: &str,
    method: &str,
    path: &str,
    timestamp: i64,
    body: &[u8],
) -> String {
    let signed = canonical_request_message(method, path, &timestamp.to_string(), body);
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(signed.as_bytes());
    let signature = mac.finalize().into_bytes();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_authorization_matches_hmac_sha256_vector() {
        let authorization = canonical_authorization(
            "shared-secret",
            "POST",
            "/admin/reload",
            1_700_000_000,
            br#"{"source":"cli"}"#,
        );

        assert_eq!(
            authorization,
            "HMAC-SHA256 timestamp=1700000000,signature=T+24RdJUvyIi81K07TCWx+E8hGNnTnBozRW+jzUacPg="
        );
    }
}
