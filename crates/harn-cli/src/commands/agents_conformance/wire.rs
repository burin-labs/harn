use std::time::{Duration, Instant};

use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use url::Url;

use crate::net;

pub(super) const PROTOCOL_VERSION: &str = "agents-protocol-2026-04-25";

pub(super) type ProbeResult<T = ()> = Result<T, String>;

#[derive(Debug, Clone)]
pub(super) struct ConformanceClient {
    http: reqwest::Client,
    base_url: Url,
    api_key: Option<String>,
    timeout: Duration,
}

#[derive(Debug)]
pub(super) struct HttpResult {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
    json: Option<Value>,
}

#[derive(Debug)]
pub(super) struct SseFrame {
    id: Option<String>,
    event: Option<String>,
    data: Option<Value>,
}

impl ConformanceClient {
    pub(super) fn new(
        base_url: &str,
        api_key: Option<String>,
        timeout: Duration,
    ) -> ProbeResult<Self> {
        let mut base_url = Url::parse(base_url).map_err(|error| {
            format!("invalid agents conformance target URL {base_url}: {error}")
        })?;
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path().trim_end_matches('/'));
            base_url.set_path(&path);
        }
        let http = net::http_client_builder("cli.agents_conformance")
            .timeout(timeout)
            .build()
            .map_err(|error| {
                format!(
                    "failed to create HTTP client: {}",
                    net::reqwest_error(&error)
                )
            })?;
        Ok(Self {
            http,
            base_url,
            api_key,
            timeout,
        })
    }

    pub(super) fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub(super) fn timeout(&self) -> Duration {
        self.timeout
    }

    pub(super) fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    pub(super) fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    fn url(&self, path: &str) -> ProbeResult<Url> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .map_err(|error| format!("failed to resolve URL path {path}: {error}"))
    }

    pub(super) async fn get_public_json(&self, path: &str) -> ProbeResult<HttpResult> {
        self.request_json(Method::GET, path, None, None, false, None)
            .await
    }

    pub(super) async fn get_json(&self, path: &str) -> ProbeResult<HttpResult> {
        self.request_json(Method::GET, path, None, None, true, None)
            .await
    }

    pub(super) async fn get_json_without_auth(&self, path: &str) -> ProbeResult<HttpResult> {
        self.request_json(Method::GET, path, None, None, false, None)
            .await
    }

    pub(super) async fn post_json(
        &self,
        path: &str,
        body: &Value,
        idempotency_key: Option<&str>,
    ) -> ProbeResult<HttpResult> {
        self.request_json(Method::POST, path, Some(body), idempotency_key, true, None)
            .await
    }

    pub(super) async fn patch_json(
        &self,
        path: &str,
        body: &Value,
        idempotency_key: Option<&str>,
    ) -> ProbeResult<HttpResult> {
        self.request_json(Method::PATCH, path, Some(body), idempotency_key, true, None)
            .await
    }

    pub(super) async fn request_json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        idempotency_key: Option<&str>,
        auth: bool,
        protocol_version: Option<&str>,
    ) -> ProbeResult<HttpResult> {
        let url = self.url(path)?;
        let mut request = self
            .http
            .request(method.clone(), url)
            .header(ACCEPT, "application/json")
            .header(
                "Harn-Agents-Protocol-Version",
                protocol_version.unwrap_or(PROTOCOL_VERSION),
            );
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        if auth {
            if let Some(api_key) = self.api_key.as_deref() {
                request = request.header(AUTHORIZATION, bearer_value(api_key)?);
            }
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("{method} {path} failed: {}", net::reqwest_error(&error)))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body_text = response
            .text()
            .await
            .map_err(|error| format!("{method} {path} body read failed: {error}"))?;
        let json = if body_text.trim().is_empty() {
            None
        } else {
            serde_json::from_str(&body_text).ok()
        };
        Ok(HttpResult {
            status,
            headers,
            body: body_text,
            json,
        })
    }
}

impl HttpResult {
    pub(super) fn status(&self) -> StatusCode {
        self.status
    }

    pub(super) fn body_contains(&self, value: &str) -> bool {
        self.body.contains(value)
    }
}

pub(super) async fn stream_sse_frames(
    client: &ConformanceClient,
    path: &str,
    min_frames: usize,
) -> ProbeResult<Vec<SseFrame>> {
    let mut request = client
        .http
        .get(client.url(path)?)
        .header("Harn-Agents-Protocol-Version", PROTOCOL_VERSION)
        .header(ACCEPT, "text/event-stream");
    if let Some(api_key) = client.api_key.as_deref() {
        request = request.header(AUTHORIZATION, format!("Bearer {api_key}"));
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("GET {path} failed: {}", net::reqwest_error(&error)))?;
    if response.status() != StatusCode::OK {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("GET {path} returned {status}: {body}"));
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type.starts_with("text/event-stream") {
        return Err(format!(
            "GET {path} returned content-type {content_type}, expected text/event-stream"
        ));
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut frames = Vec::new();
    let deadline = Instant::now() + client.timeout.min(Duration::from_secs(5));
    while frames.len() < min_frames && Instant::now() < deadline {
        let next = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
        let Some(chunk) = next.ok().flatten() else {
            continue;
        };
        let chunk = chunk.map_err(|error| {
            format!("SSE read failed for {path}: {}", net::reqwest_error(&error))
        })?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buffer.find("\n\n") {
            let raw = buffer[..idx].to_string();
            buffer = buffer[idx + 2..].to_string();
            if raw.trim().is_empty() {
                continue;
            }
            frames.push(parse_sse_frame(&raw)?);
            if frames.len() >= min_frames {
                break;
            }
        }
    }

    if frames.len() < min_frames {
        return Err(format!(
            "SSE stream {path} produced {} frame(s), expected at least {min_frames}",
            frames.len()
        ));
    }
    Ok(frames)
}

fn parse_sse_frame(raw: &str) -> ProbeResult<SseFrame> {
    let mut id = None;
    let mut event = None;
    let mut data_lines = Vec::new();
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("id:") {
            id = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }
    let data = if data_lines.is_empty() {
        None
    } else {
        Some(
            serde_json::from_str(&data_lines.join("\n"))
                .map_err(|error| format!("SSE data is not valid JSON: {error}"))?,
        )
    };
    Ok(SseFrame { id, event, data })
}

pub(super) fn validate_sse_frames(frames: &[SseFrame]) -> ProbeResult {
    for frame in frames {
        let id = frame
            .id
            .as_deref()
            .ok_or_else(|| "SSE frame is missing id".to_string())?;
        if id.is_empty() {
            return Err("SSE frame id must not be empty".to_string());
        }
        let event = frame
            .event
            .as_deref()
            .ok_or_else(|| "SSE frame is missing event".to_string())?;
        if event.is_empty() {
            return Err("SSE frame event must not be empty".to_string());
        }
        let data = frame
            .data
            .as_ref()
            .ok_or_else(|| "SSE frame is missing data".to_string())?;
        expect_field_eq(data, "object", "event")?;
        expect_string_field(data, "id")?;
        expect_string_field(data, "event")?;
        expect_u64_field(data, "sequence")?;
    }
    Ok(())
}

fn bearer_value(api_key: &str) -> ProbeResult<HeaderValue> {
    HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|error| format!("invalid API key for Authorization header: {error}"))
}

pub(super) fn expect_status(response: &HttpResult, expected: StatusCode) -> ProbeResult {
    if response.status == expected {
        Ok(())
    } else {
        Err(format!(
            "expected HTTP {}, got {}{}",
            expected.as_u16(),
            response.status.as_u16(),
            body_suffix(response)
        ))
    }
}

pub(super) fn expect_status_any(response: &HttpResult, expected: &[StatusCode]) -> ProbeResult {
    if expected.contains(&response.status) {
        Ok(())
    } else {
        let expected = expected
            .iter()
            .map(|status| status.as_u16().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "expected HTTP one of [{}], got {}{}",
            expected,
            response.status.as_u16(),
            body_suffix(response)
        ))
    }
}

fn body_suffix(response: &HttpResult) -> String {
    let mut suffix = String::new();
    if let Some(content_type) = response
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    {
        suffix.push_str(&format!(" content-type={content_type}"));
    }
    if !response.body.trim().is_empty() {
        let body = response.body.trim();
        suffix.push_str(": ");
        suffix.push_str(if body.len() > 500 { &body[..500] } else { body });
    }
    suffix
}

pub(super) fn expect_json(response: &HttpResult) -> ProbeResult<&Value> {
    response.json.as_ref().ok_or_else(|| {
        format!(
            "expected JSON response for HTTP {}, body was {}",
            response.status, response.body
        )
    })
}

pub(super) fn expect_list_response<'a>(value: &'a Value, label: &str) -> ProbeResult<&'a [Value]> {
    let data = list_data(value)?;
    value
        .get("object")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{label} list is missing object discriminator"))?;
    Ok(data)
}

pub(super) fn list_data(value: &Value) -> ProbeResult<&[Value]> {
    value
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| "list response must include data array".to_string())
}

pub(super) fn expect_field_eq<'a>(
    value: &'a Value,
    field: &str,
    expected: &str,
) -> ProbeResult<&'a str> {
    let actual = expect_string_field(value, field)?;
    if actual == expected {
        Ok(actual)
    } else {
        Err(format!("{field} must be {expected:?}, got {actual:?}"))
    }
}

pub(super) fn expect_string_field<'a>(value: &'a Value, field: &str) -> ProbeResult<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}

pub(super) fn expect_u64_field(value: &Value, field: &str) -> ProbeResult<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{field} must be an unsigned integer"))
}

pub(super) fn expect_array_field<'a>(value: &'a Value, field: &str) -> ProbeResult<&'a [Value]> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{field} must be an array"))
}

pub(super) fn expect_string_array_contains(
    value: &Value,
    field: &str,
    expected: &str,
) -> ProbeResult {
    let values = expect_array_field(value, field)?;
    if values.iter().any(|value| value.as_str() == Some(expected)) {
        Ok(())
    } else {
        Err(format!("{field} must contain {expected}"))
    }
}

pub(super) fn expect_task_status(value: &Value) -> ProbeResult<&str> {
    let status = expect_string_field(value, "status")?;
    if is_task_status(status) {
        Ok(status)
    } else {
        Err(format!("unknown task status {status:?}"))
    }
}

fn is_task_status(status: &str) -> bool {
    matches!(
        status,
        "SUBMITTED"
            | "WORKING"
            | "INPUT_REQUIRED"
            | "AUTH_REQUIRED"
            | "COMPLETED"
            | "FAILED"
            | "CANCELED"
    )
}

pub(super) fn is_terminal_status(status: &str) -> bool {
    matches!(status, "COMPLETED" | "FAILED" | "CANCELED")
}

pub(super) fn validate_status_transitions(statuses: &[String]) -> ProbeResult {
    for pair in statuses.windows(2) {
        let from = pair[0].as_str();
        let to = pair[1].as_str();
        let allowed = match from {
            "SUBMITTED" => matches!(to, "WORKING" | "CANCELED" | "FAILED"),
            "WORKING" => matches!(
                to,
                "INPUT_REQUIRED" | "AUTH_REQUIRED" | "COMPLETED" | "FAILED" | "CANCELED"
            ),
            "INPUT_REQUIRED" | "AUTH_REQUIRED" => matches!(to, "WORKING" | "FAILED" | "CANCELED"),
            "COMPLETED" | "FAILED" | "CANCELED" => false,
            _ => false,
        };
        if !allowed {
            return Err(format!("invalid task status transition {from} -> {to}"));
        }
    }
    Ok(())
}

pub(super) fn expect_error_code(response: &HttpResult, expected: &str) -> ProbeResult {
    let actual = error_code(response)?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("expected error code {expected}, got {actual}"))
    }
}

pub(super) fn error_code(response: &HttpResult) -> ProbeResult<String> {
    let body = expect_json(response)?;
    body.get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "error response must include error.code".to_string())
}

pub(super) fn first_id_from_list(value: &Value) -> Option<String> {
    value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|data| data.first())
        .and_then(|item| item.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_task_transitions() {
        validate_status_transitions(&[
            "SUBMITTED".to_string(),
            "WORKING".to_string(),
            "COMPLETED".to_string(),
        ])
        .unwrap();
        assert!(
            validate_status_transitions(&["COMPLETED".to_string(), "WORKING".to_string()]).is_err()
        );
    }

    #[test]
    fn parses_sse_frame() {
        let frame = parse_sse_frame(
            "id: evt_1\nevent: task.started\ndata: {\"object\":\"event\",\"id\":\"evt_1\",\"event\":\"task.started\",\"sequence\":1}\n",
        )
        .unwrap();
        assert_eq!(frame.id.as_deref(), Some("evt_1"));
        assert_eq!(frame.event.as_deref(), Some("task.started"));
        validate_sse_frames(&[frame]).unwrap();
    }
}
