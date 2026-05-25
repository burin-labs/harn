use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tracing::Instrument as _;

use harn_vm::event_log::{EventLog, LogEvent, Topic};
use harn_vm::secrets::{SecretId, SecretProvider};

use super::{AuthMode, RouteContext, SignatureMode, TenantRequestScope};

pub(super) async fn authorize_request(
    context: &RouteContext,
    tenant_scope: Option<&TenantRequestScope>,
    method: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<(), HttpError> {
    match context.route.auth_mode {
        AuthMode::Public => Ok(()),
        AuthMode::BearerOrHmac
            if tenant_scope.is_some_and(|tenant| tenant.credential_authenticated) =>
        {
            Ok(())
        }
        AuthMode::BearerOrHmac => context
            .auth
            .authorize(context.event_log.as_ref(), method, path, headers, body)
            .await
            .map_err(|()| HttpError::unauthorized("auth failed")),
    }
}

pub(super) async fn normalize_request(
    context: &RouteContext,
    normalized_headers: &BTreeMap<String, String>,
    query: &BTreeMap<String, String>,
    body: &[u8],
    trace_id: harn_vm::TraceId,
    tenant_scope: Option<&harn_vm::TenantScope>,
) -> Result<NormalizedRequest, HttpError> {
    let received_at = OffsetDateTime::now_utc();
    if let Some(connector) = context.route.connector.as_ref() {
        let mut raw = harn_vm::RawInbound::new("", normalized_headers.clone(), body.to_vec());
        raw.query = query.clone();
        raw.received_at = received_at;
        raw.metadata = json!({
            "binding_id": context.route.trigger_id,
            "binding_version": context.route.binding_version,
            "path": context.route.path,
            "tenant_id": tenant_scope.map(|tenant| tenant.id.0.as_str()),
        });
        let result = connector
            .lock()
            .await
            .normalize_inbound_result(raw)
            .await
            .map_err(HttpError::from_connector)?;
        return connector_normalize_result_to_request(result, trace_id, tenant_scope);
    }

    let normalized_body = normalize_body(body, normalized_headers);
    let provider = context.route.provider.clone();

    let signature_status = match context.route.signature_mode {
        SignatureMode::Unsigned => harn_vm::SignatureStatus::Unsigned,
        SignatureMode::GitHub => {
            let secret =
                load_secret(context, tenant_scope, context.route.signing_secret.as_ref()).await?;
            harn_vm::connectors::hmac::verify_hmac_signed(
                context.event_log.as_ref(),
                &provider,
                harn_vm::connectors::HmacSignatureStyle::github(),
                body,
                normalized_headers,
                &secret,
                None,
                received_at,
            )
            .await
            .map_err(HttpError::from_connector)?;
            harn_vm::SignatureStatus::Verified
        }
        SignatureMode::Standard => {
            let secret =
                load_secret(context, tenant_scope, context.route.signing_secret.as_ref()).await?;
            harn_vm::connectors::hmac::verify_hmac_signed(
                context.event_log.as_ref(),
                &provider,
                harn_vm::connectors::HmacSignatureStyle::standard_webhooks(),
                body,
                normalized_headers,
                &secret,
                Some(time::Duration::minutes(5)),
                received_at,
            )
            .await
            .map_err(HttpError::from_connector)?;
            harn_vm::SignatureStatus::Verified
        }
        SignatureMode::Slack => {
            let secret =
                load_secret(context, tenant_scope, context.route.signing_secret.as_ref()).await?;
            harn_vm::connectors::hmac::verify_hmac_signed(
                context.event_log.as_ref(),
                &provider,
                harn_vm::connectors::HmacSignatureStyle::slack(),
                body,
                normalized_headers,
                &secret,
                Some(time::Duration::minutes(5)),
                received_at,
            )
            .await
            .map_err(HttpError::from_connector)?;
            harn_vm::SignatureStatus::Verified
        }
        SignatureMode::Notion => {
            let secret =
                load_secret(context, tenant_scope, context.route.signing_secret.as_ref()).await?;
            harn_vm::connectors::hmac::verify_hmac_signed(
                context.event_log.as_ref(),
                &provider,
                harn_vm::connectors::HmacSignatureStyle::notion(),
                body,
                normalized_headers,
                &secret,
                None,
                received_at,
            )
            .await
            .map_err(HttpError::from_connector)?;
            harn_vm::SignatureStatus::Verified
        }
    };

    let provider_kind = provider_event_kind(&provider, normalized_headers, &normalized_body);
    let trigger_kind = trigger_event_kind(&provider, normalized_headers, &normalized_body);
    let dedupe_key = dedupe_key(&provider, normalized_headers, &normalized_body, body);
    let provider_payload = harn_vm::ProviderPayload::normalize(
        &provider,
        &provider_kind,
        normalized_headers,
        normalized_body,
    )
    .map_err(|error| HttpError::unprocessable(error.to_string()))?;

    Ok(NormalizedRequest::Events(vec![harn_vm::TriggerEvent {
        id: harn_vm::TriggerEventId::new(),
        provider,
        kind: trigger_kind,
        received_at,
        occurred_at: infer_occurred_at(&provider_payload),
        dedupe_key,
        trace_id,
        tenant_id: tenant_scope.map(|tenant| tenant.id.clone()),
        headers: harn_vm::redact_headers(
            normalized_headers,
            &harn_vm::HeaderRedactionPolicy::default(),
        ),
        batch: None,
        raw_body: Some(body.to_vec()),
        provider_payload,
        signature_status,
        dedupe_claimed: false,
    }]))
}

pub(super) enum NormalizedRequest {
    Events(Vec<harn_vm::TriggerEvent>),
    Immediate {
        response: Response,
        events: Vec<harn_vm::TriggerEvent>,
    },
    Rejected(Response),
}

pub(super) struct EnqueueSummary {
    pub(super) accepted: usize,
    pub(super) duplicates: usize,
    pub(super) first_event_id: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) struct IngressLifecycleTiming {
    pub(super) accepted_at_ms: i64,
    pub(super) normalized_at_ms: i64,
    pub(super) accepted_to_normalized: Duration,
}

fn connector_normalize_result_to_request(
    result: harn_vm::ConnectorNormalizeResult,
    trace_id: harn_vm::TraceId,
    tenant_scope: Option<&harn_vm::TenantScope>,
) -> Result<NormalizedRequest, HttpError> {
    match result {
        harn_vm::ConnectorNormalizeResult::Event(event) => {
            let mut event = *event;
            if let Some(challenge) = slack_url_verification_challenge(&event) {
                return Ok(NormalizedRequest::Immediate {
                    response: (
                        StatusCode::OK,
                        [("content-type", "text/plain; charset=utf-8")],
                        challenge,
                    )
                        .into_response(),
                    events: Vec::new(),
                });
            }
            if let Some(response) = notion_subscription_verification_response(&event) {
                return Ok(NormalizedRequest::Immediate {
                    response,
                    events: Vec::new(),
                });
            }
            event.trace_id = trace_id;
            apply_tenant_scope(vec![event], tenant_scope).map(NormalizedRequest::Events)
        }
        harn_vm::ConnectorNormalizeResult::Batch(mut events) => {
            set_trace_id(&mut events, trace_id);
            apply_tenant_scope(events, tenant_scope).map(NormalizedRequest::Events)
        }
        harn_vm::ConnectorNormalizeResult::ImmediateResponse {
            response,
            mut events,
        } => {
            set_trace_id(&mut events, trace_id);
            Ok(NormalizedRequest::Immediate {
                response: connector_http_response_to_response(response)?,
                events: apply_tenant_scope(events, tenant_scope)?,
            })
        }
        harn_vm::ConnectorNormalizeResult::Reject(response) => Ok(NormalizedRequest::Rejected(
            connector_http_response_to_response(response)?,
        )),
    }
}

fn apply_tenant_scope(
    mut events: Vec<harn_vm::TriggerEvent>,
    tenant_scope: Option<&harn_vm::TenantScope>,
) -> Result<Vec<harn_vm::TriggerEvent>, HttpError> {
    let Some(tenant_scope) = tenant_scope else {
        return Ok(events);
    };
    for event in &mut events {
        match event.tenant_id.as_ref() {
            Some(existing) if existing != &tenant_scope.id => {
                return Err(HttpError::forbidden(format!(
                    "event tenant '{}' does not match request tenant '{}'",
                    existing.0, tenant_scope.id.0
                )));
            }
            Some(_) => {}
            None => event.tenant_id = Some(tenant_scope.id.clone()),
        }
    }
    Ok(events)
}

fn set_trace_id(events: &mut [harn_vm::TriggerEvent], trace_id: harn_vm::TraceId) {
    for event in events {
        event.trace_id = trace_id.clone();
    }
}

fn connector_http_response_to_response(
    response: harn_vm::ConnectorHttpResponse,
) -> Result<Response, HttpError> {
    let status = StatusCode::from_u16(response.status).map_err(|error| {
        HttpError::internal(format!(
            "connector returned invalid HTTP status {}: {error}",
            response.status
        ))
    })?;
    let mut builder = Response::builder().status(status);
    let has_content_type = response
        .headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("content-type"));
    for (name, value) in response.headers {
        let name = axum::http::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            HttpError::internal(format!(
                "connector returned invalid response header name: {error}"
            ))
        })?;
        let value = axum::http::HeaderValue::from_str(&value).map_err(|error| {
            HttpError::internal(format!(
                "connector returned invalid response header value for '{}': {error}",
                name.as_str()
            ))
        })?;
        builder = builder.header(name, value);
    }

    let body = match response.body {
        JsonValue::Null => Body::empty(),
        JsonValue::String(value) => {
            if !has_content_type {
                builder = builder.header(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                );
            }
            Body::from(value)
        }
        value => {
            if !has_content_type {
                builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
            }
            let bytes = serde_json::to_vec(&value)
                .map_err(|error| HttpError::internal(error.to_string()))?;
            Body::from(bytes)
        }
    };

    builder
        .body(body)
        .map_err(|error| HttpError::internal(error.to_string()))
}

pub(super) async fn enqueue_normalized_events(
    context: &RouteContext,
    events: Vec<harn_vm::TriggerEvent>,
    span_context_headers: &BTreeMap<String, String>,
    timing: IngressLifecycleTiming,
) -> Result<EnqueueSummary, HttpError> {
    let mut summary = EnqueueSummary {
        accepted: 0,
        duplicates: 0,
        first_event_id: None,
    };

    for event in events {
        let binding_key =
            listener_binding_key(&context.route.trigger_id, context.route.binding_version);
        context
            .metrics_registry
            .record_trigger_accepted_to_normalized(
                &context.route.trigger_id,
                &binding_key,
                event.provider.as_str(),
                event.tenant_id.as_ref().map(|tenant| tenant.0.as_str()),
                "normalized",
                timing.accepted_to_normalized,
            );
        let postprocess = harn_vm::postprocess_normalized_event(
            context.inbox.as_ref(),
            &context.route.trigger_id,
            context.route.dedupe_key_template.is_some(),
            context.route.dedupe_ttl(),
            event,
        )
        .await
        .map_err(HttpError::from_connector)?;
        match postprocess {
            harn_vm::PostNormalizeOutcome::DuplicateDropped => {
                summary.duplicates += 1;
                context
                    .metrics_registry
                    .record_trigger_deduped(&context.route.trigger_id, "inbox_duplicate");
            }
            harn_vm::PostNormalizeOutcome::Ready(event) => {
                let event = *event;
                let pending_topic = topic_for_event(&event, &context.pending_topic)
                    .map_err(|error| HttpError::internal(error.to_string()))?;
                let payload = json!({
                    "trigger_id": context.route.trigger_id,
                    "binding_version": context.route.binding_version,
                    "event": event,
                });
                let queue_span = tracing::info_span!(
                    "queue_append",
                    trigger_id = %context.route.trigger_id,
                    binding_version = context.route.binding_version,
                    event_id = %event.id.0,
                    trace_id = %event.trace_id.0
                );
                let _ = harn_vm::observability::otel::set_span_parent_from_headers(
                    &queue_span,
                    span_context_headers,
                    &event.trace_id,
                    None,
                );
                let mut pending_headers = BTreeMap::new();
                pending_headers.insert("trace_id".to_string(), event.trace_id.0.clone());
                pending_headers.insert(
                    harn_vm::triggers::dispatcher::TRIGGER_ACCEPTED_AT_MS_HEADER.to_string(),
                    timing.accepted_at_ms.to_string(),
                );
                pending_headers.insert(
                    harn_vm::triggers::dispatcher::TRIGGER_NORMALIZED_AT_MS_HEADER.to_string(),
                    timing.normalized_at_ms.to_string(),
                );
                pending_headers.insert("trigger_id".to_string(), context.route.trigger_id.clone());
                pending_headers.insert("binding_key".to_string(), binding_key.clone());
                pending_headers.insert("provider".to_string(), event.provider.as_str().to_string());
                if let Some(tenant_id) = event.tenant_id.as_ref() {
                    pending_headers.insert("tenant_id".to_string(), tenant_id.0.clone());
                }
                let _ = harn_vm::observability::otel::inject_current_context_headers(
                    &queue_span,
                    &mut pending_headers,
                );
                let append_started = Instant::now();
                let payload_size_bytes = serde_json::to_vec(&payload)
                    .map(|bytes| bytes.len())
                    .unwrap_or(0);
                let mut log_event = LogEvent::new("trigger_event", payload);
                let queue_appended_at_ms = log_event.occurred_at_ms;
                pending_headers.insert(
                    harn_vm::triggers::dispatcher::TRIGGER_QUEUE_APPENDED_AT_MS_HEADER.to_string(),
                    queue_appended_at_ms.to_string(),
                );
                log_event.headers = pending_headers;
                let event_id = context
                    .event_log
                    .append(&pending_topic, log_event)
                    .instrument(queue_span)
                    .await
                    .map_err(|error| {
                        HttpError::internal(format!(
                            "failed to append trigger event to pending log: {error}"
                        ))
                    })?;
                context.metrics_registry.record_event_log_append(
                    pending_topic.as_str(),
                    append_started.elapsed(),
                    payload_size_bytes,
                );
                context
                    .metrics_registry
                    .record_trigger_accepted_to_queue_append(
                        &context.route.trigger_id,
                        &binding_key,
                        event.provider.as_str(),
                        event.tenant_id.as_ref().map(|tenant| tenant.0.as_str()),
                        "queued",
                        duration_between_ms(queue_appended_at_ms, timing.accepted_at_ms),
                    );
                context.metrics_registry.note_trigger_pending_event(
                    event.id.0.as_str(),
                    &context.route.trigger_id,
                    &binding_key,
                    event.provider.as_str(),
                    event.tenant_id.as_ref().map(|tenant| tenant.0.as_str()),
                    timing.accepted_at_ms,
                    queue_appended_at_ms,
                );
                tracing::info!(
                    component = "listener",
                    trace_id = %event.trace_id.0,
                    trigger_id = %context.route.trigger_id,
                    event_id = %event_id,
                    "trigger event accepted"
                );
                summary.accepted += 1;
                if summary.first_event_id.is_none() {
                    summary.first_event_id = Some(event_id.to_string());
                }
            }
        }
    }

    Ok(summary)
}

pub(super) fn enqueue_summary_response(
    context: &RouteContext,
    summary: EnqueueSummary,
) -> Response {
    if summary.accepted == 0 && summary.duplicates > 0 {
        return (
            StatusCode::OK,
            axum::Json(json!({
                "status": "duplicate_dropped",
                "trigger_id": context.route.trigger_id,
            })),
        )
            .into_response();
    }

    if summary.accepted == 1 && summary.duplicates == 0 {
        return (
            StatusCode::OK,
            axum::Json(json!({
                "status": "accepted",
                "event_id": summary.first_event_id,
                "trigger_id": context.route.trigger_id,
            })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        axum::Json(json!({
            "status": "accepted",
            "events_accepted": summary.accepted,
            "duplicates_dropped": summary.duplicates,
            "trigger_id": context.route.trigger_id,
        })),
    )
        .into_response()
}

async fn load_secret(
    context: &RouteContext,
    tenant_scope: Option<&harn_vm::TenantScope>,
    secret_id: Option<&SecretId>,
) -> Result<String, HttpError> {
    let secret_id = secret_id.ok_or_else(|| {
        HttpError::internal(format!(
            "trigger '{}' requires a signing secret",
            context.route.trigger_id
        ))
    })?;
    let tenant_provider;
    let provider: &dyn SecretProvider = if let Some(scope) = tenant_scope {
        tenant_provider =
            harn_vm::TenantSecretProvider::new(context.secrets.clone(), scope.clone());
        &tenant_provider
    } else {
        context.secrets.as_ref()
    };
    let secret = provider
        .get(secret_id)
        .await
        .map_err(|error| HttpError::internal(error.to_string()))?;
    secret.with_exposed(|bytes| {
        std::str::from_utf8(bytes)
            .map(|value| value.to_string())
            .map_err(|error| {
                HttpError::internal(format!("secret '{secret_id}' is not valid UTF-8: {error}"))
            })
    })
}

pub(in crate::commands::orchestrator::listener) fn normalize_headers(
    headers: &HeaderMap,
) -> BTreeMap<String, String> {
    let mut normalized = BTreeMap::new();
    for (name, value) in headers {
        if let Ok(value) = value.to_str() {
            normalized.insert(name.as_str().to_string(), value.to_string());
        }
    }

    for (raw, canonical) in [
        ("content-type", "Content-Type"),
        ("content-length", "Content-Length"),
        ("origin", "Origin"),
        ("x-github-event", "X-GitHub-Event"),
        ("x-github-delivery", "X-GitHub-Delivery"),
        ("x-hub-signature-256", "X-Hub-Signature-256"),
        ("linear-signature", "Linear-Signature"),
        ("linear-delivery", "Linear-Delivery"),
        ("linear-event", "Linear-Event"),
        ("x-slack-signature", "X-Slack-Signature"),
        ("x-slack-request-timestamp", "X-Slack-Request-Timestamp"),
        ("x-slack-retry-num", "X-Slack-Retry-Num"),
        ("x-slack-retry-reason", "X-Slack-Retry-Reason"),
        ("x-notion-signature", "X-Notion-Signature"),
        ("request-id", "request-id"),
        ("x-request-id", "x-request-id"),
        ("webhook-id", "webhook-id"),
        ("webhook-signature", "webhook-signature"),
        ("webhook-timestamp", "webhook-timestamp"),
        ("x-a2a-delivery", "X-A2A-Delivery"),
    ] {
        if let Some(value) = header_value(&normalized, raw) {
            let value = value.to_string();
            normalized.entry(canonical.to_string()).or_insert(value);
        }
    }

    normalized
}

fn normalize_body(body: &[u8], headers: &BTreeMap<String, String>) -> JsonValue {
    let content_type = header_value(headers, "content-type").unwrap_or_default();
    if content_type.contains("json") {
        if let Ok(value) = serde_json::from_slice(body) {
            return value;
        }
    }
    use base64::Engine;

    let raw_base64 = base64::engine::general_purpose::STANDARD.encode(body);
    serde_json::from_slice(body).unwrap_or_else(|_| {
        json!({
            "raw_base64": raw_base64,
            "raw_utf8": std::str::from_utf8(body).ok(),
        })
    })
}

fn provider_event_kind(
    provider: &harn_vm::ProviderId,
    headers: &BTreeMap<String, String>,
    body: &JsonValue,
) -> String {
    match provider.as_str() {
        "github" => header_value(headers, "x-github-event")
            .map(ToString::to_string)
            .unwrap_or_else(|| "webhook".to_string()),
        "a2a-push" => "push".to_string(),
        _ => body
            .get("type")
            .and_then(JsonValue::as_str)
            .or_else(|| body.get("event").and_then(JsonValue::as_str))
            .unwrap_or("webhook")
            .to_string(),
    }
}

fn trigger_event_kind(
    provider: &harn_vm::ProviderId,
    headers: &BTreeMap<String, String>,
    body: &JsonValue,
) -> String {
    if provider.as_str() == "github" {
        let event = header_value(headers, "x-github-event").unwrap_or("webhook");
        if let Some(action) = body.get("action").and_then(JsonValue::as_str) {
            return format!("{event}.{action}");
        }
        return event.to_string();
    }
    provider_event_kind(provider, headers, body)
}

fn dedupe_key(
    provider: &harn_vm::ProviderId,
    headers: &BTreeMap<String, String>,
    body: &JsonValue,
    raw_body: &[u8],
) -> String {
    match provider.as_str() {
        "github" => header_value(headers, "x-github-delivery")
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
        "webhook" => header_value(headers, "webhook-id")
            .map(ToString::to_string)
            .or_else(|| {
                body.get("id")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
        _ => header_value(headers, "x-a2a-delivery")
            .map(ToString::to_string)
            .unwrap_or_else(|| fallback_body_digest(raw_body)),
    }
}

fn infer_occurred_at(payload: &harn_vm::ProviderPayload) -> Option<OffsetDateTime> {
    let harn_vm::ProviderPayload::Known(payload) = payload else {
        return None;
    };
    let raw = match payload {
        harn_vm::triggers::event::KnownProviderPayload::GitHub(payload) => github_raw(payload),
        harn_vm::triggers::event::KnownProviderPayload::Slack(payload) => slack_raw(payload),
        harn_vm::triggers::event::KnownProviderPayload::Webhook(payload) => &payload.raw,
        harn_vm::triggers::event::KnownProviderPayload::A2aPush(payload) => &payload.raw,
        _ => return None,
    };
    raw.get("timestamp")
        .and_then(JsonValue::as_str)
        .and_then(|value| {
            OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
        })
}

fn github_raw(payload: &harn_vm::triggers::event::GitHubEventPayload) -> &JsonValue {
    match payload {
        harn_vm::triggers::event::GitHubEventPayload::Issues(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::PullRequest(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::IssueComment(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::PullRequestReview(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::Push(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::WorkflowRun(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::DeploymentStatus(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::CheckRun(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::CheckSuite(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::Status(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::MergeGroup(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::Installation(inner) => &inner.common.raw,
        harn_vm::triggers::event::GitHubEventPayload::InstallationRepositories(inner) => {
            &inner.common.raw
        }
        harn_vm::triggers::event::GitHubEventPayload::Other(common) => &common.raw,
    }
}

fn slack_raw(payload: &harn_vm::triggers::event::SlackEventPayload) -> &JsonValue {
    match payload {
        harn_vm::triggers::event::SlackEventPayload::Message(inner) => &inner.common.raw,
        harn_vm::triggers::event::SlackEventPayload::AppMention(inner) => &inner.common.raw,
        harn_vm::triggers::event::SlackEventPayload::ReactionAdded(inner) => &inner.common.raw,
        harn_vm::triggers::event::SlackEventPayload::AppHomeOpened(inner) => &inner.common.raw,
        harn_vm::triggers::event::SlackEventPayload::AssistantThreadStarted(inner) => {
            &inner.common.raw
        }
        harn_vm::triggers::event::SlackEventPayload::Other(common) => &common.raw,
    }
}

fn slack_url_verification_challenge(event: &harn_vm::TriggerEvent) -> Option<String> {
    let harn_vm::ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Slack(
        payload,
    )) = &event.provider_payload
    else {
        return None;
    };
    if event.kind != "url_verification" {
        return None;
    }
    slack_raw(payload)
        .get("challenge")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn notion_subscription_verification_response(event: &harn_vm::TriggerEvent) -> Option<Response> {
    let harn_vm::ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Notion(
        payload,
    )) = &event.provider_payload
    else {
        return None;
    };
    if event.kind != "subscription.verification" {
        return None;
    }
    Some(
        (
            StatusCode::OK,
            axum::Json(json!({
                "status": "handshake_captured",
                "verification_token": payload.verification_token,
            })),
        )
            .into_response(),
    )
}

pub(super) fn header_value<'a>(
    headers: &'a BTreeMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn fallback_body_digest(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("sha256:{encoded}")
}

pub(in crate::commands::orchestrator::listener) struct HttpError {
    status: StatusCode,
    message: String,
}

impl HttpError {
    pub(in crate::commands::orchestrator::listener) fn unauthorized(
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    pub(in crate::commands::orchestrator::listener) fn forbidden(
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    pub(in crate::commands::orchestrator::listener) fn payment_required(
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::PAYMENT_REQUIRED,
            message: message.into(),
        }
    }

    pub(in crate::commands::orchestrator::listener) fn internal(
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    pub(in crate::commands::orchestrator::listener) fn unprocessable(
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
        }
    }

    fn from_connector(error: harn_vm::ConnectorError) -> Self {
        match error {
            harn_vm::ConnectorError::MissingHeader(_)
            | harn_vm::ConnectorError::InvalidHeader { .. }
            | harn_vm::ConnectorError::InvalidSignature(_)
            | harn_vm::ConnectorError::TimestampOutOfWindow { .. } => Self {
                status: StatusCode::BAD_REQUEST,
                message: error.to_string(),
            },
            harn_vm::ConnectorError::Unsupported(_) => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                message: error.to_string(),
            },
            _ => Self::internal(error.to_string()),
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> axum::response::Response {
        (self.status, self.message).into_response()
    }
}

fn listener_binding_key(trigger_id: &str, binding_version: u32) -> String {
    format!("{trigger_id}@v{binding_version}")
}

fn topic_for_event(
    event: &harn_vm::TriggerEvent,
    topic: &Topic,
) -> Result<Topic, harn_vm::event_log::LogError> {
    match event.tenant_id.as_ref() {
        Some(tenant_id) => harn_vm::tenant_topic(tenant_id, topic),
        None => Ok(topic.clone()),
    }
}

pub(super) fn current_unix_ms() -> i64 {
    unix_ms(OffsetDateTime::now_utc())
}

fn unix_ms(timestamp: OffsetDateTime) -> i64 {
    harn_vm::clock::offset_datetime_to_ms(timestamp)
}

fn duration_between_ms(later_ms: i64, earlier_ms: i64) -> Duration {
    Duration::from_millis(later_ms.saturating_sub(earlier_ms).max(0) as u64)
}
