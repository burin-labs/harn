//! Conditional-GET middleware: hashes the buffered response body of
//! safe (`GET`, `HEAD`) requests, attaches a strong `ETag` header, and
//! short-circuits to `304 Not Modified` when the request's
//! `If-None-Match` matches.
//!
//! Per RFC 9110 §13.1.2, the `If-None-Match` precondition takes the
//! value `*` (matches any current representation) or a list of entity
//! tags. We honour both: `*` always matches when the resource exists
//! (status 2xx/3xx); a list matches when any tag equals the freshly
//! computed strong ETag.
//!
//! The middleware ignores already-compressed bodies (responses that
//! carry a `Content-Encoding` header). The `apply_transport_layers`
//! caller stacks ETag inside compression, so under the standard stack
//! the hash is always computed over the uncompressed JSON — and the
//! ETag is therefore stable across `Accept-Encoding` negotiations.
//!
//! Only `application/json` bodies are eligible. Streamed and SSE
//! responses (which the [`crate::http_codec`] decoder labels with
//! `Content-Type: text/event-stream` or `application/octet-stream`)
//! are passed through untouched: hashing a multi-chunk stream would
//! force the middleware to buffer it, defeating the point.

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

/// Compute a strong ETag for the given body bytes. The format is
/// `"<hex-encoded sha256 digest>"`. The quotes are part of the value
/// per RFC 9110 §8.8.3.
pub fn compute_strong_etag(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let digest = hasher.finalize();
    format!("\"{}\"", hex::encode(digest))
}

pub(super) async fn etag_middleware(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let if_none_match = request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let response = next.run(request).await;

    if !is_eligible(&method, &response) {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => {
            // The body errored out mid-collection. Re-emit a 500 with
            // no body — the original handler's response is already
            // gone, so we cannot reconstitute it.
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let etag_value = compute_strong_etag(&bytes);
    let etag_header = match HeaderValue::from_str(&etag_value) {
        Ok(value) => value,
        Err(_) => return rebuild_response(parts, bytes),
    };

    if matches(if_none_match.as_deref(), &etag_value) {
        parts.status = StatusCode::NOT_MODIFIED;
        parts.headers.insert(header::ETAG, etag_header);
        parts.headers.remove(header::CONTENT_LENGTH);
        parts.headers.remove(header::CONTENT_TYPE);
        return Response::from_parts(parts, Body::empty());
    }

    parts.headers.insert(header::ETAG, etag_header);
    rebuild_response(parts, bytes)
}

fn rebuild_response(mut parts: axum::http::response::Parts, bytes: Bytes) -> Response {
    // Buffering may have removed any pre-set `Content-Length` header.
    // Re-attach one matching the materialised body so downstream
    // proxies and clients can size the response without reading to EOF.
    if let Ok(value) = HeaderValue::from_str(&bytes.len().to_string()) {
        parts.headers.insert(header::CONTENT_LENGTH, value);
    }
    Response::from_parts(parts, Body::from(bytes))
}

fn is_eligible(method: &Method, response: &Response) -> bool {
    if !matches!(method, &Method::GET | &Method::HEAD) {
        return false;
    }
    if !response.status().is_success() {
        return false;
    }
    if response.headers().contains_key(header::CONTENT_ENCODING) {
        return false;
    }
    match response.headers().get(header::CONTENT_TYPE) {
        Some(value) => value
            .to_str()
            .map(|v| v.starts_with("application/json"))
            .unwrap_or(false),
        None => false,
    }
}

fn matches(if_none_match: Option<&str>, etag: &str) -> bool {
    let Some(header) = if_none_match else {
        return false;
    };
    for candidate in header.split(',') {
        let trimmed = candidate.trim();
        if trimmed == "*" || trimmed == etag {
            return true;
        }
        // Tolerate weak validators in the request header (`W/"..."`)
        // by stripping the weak prefix before comparing. Our generated
        // tags are strong, so a weak match downgrades to no match per
        // §13.1.2 — but a request that quotes our exact strong tag
        // with a `W/` prefix is still a deliberate cache hit and we
        // honour it.
        if let Some(stripped) = trimmed.strip_prefix("W/") {
            if stripped == etag {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::middleware::from_fn;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn json_router() -> Router {
        Router::new()
            .route(
                "/v1/resource",
                get(|| async { axum::Json(serde_json::json!({"id": "abc", "n": 1})) }),
            )
            .route(
                "/v1/text",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "text/plain")],
                        "not json".to_string(),
                    )
                }),
            )
            .layer(from_fn(etag_middleware))
    }

    #[tokio::test]
    async fn json_get_gets_etag() {
        let response = json_router()
            .oneshot(
                Request::builder()
                    .uri("/v1/resource")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::ETAG).is_some());
    }

    #[tokio::test]
    async fn matching_if_none_match_yields_304() {
        let app = json_router();

        // First request: capture the ETag.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/resource")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = response
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // Second request: present the ETag and expect 304.
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/resource")
                    .header(header::IF_NONE_MATCH, &etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn star_in_if_none_match_yields_304() {
        let response = json_router()
            .oneshot(
                Request::builder()
                    .uri("/v1/resource")
                    .header(header::IF_NONE_MATCH, "*")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn weak_validator_in_request_matches_strong_tag() {
        let app = json_router();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/resource")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = response
            .headers()
            .get(header::ETAG)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/resource")
                    .header(header::IF_NONE_MATCH, format!("W/{etag}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn non_matching_if_none_match_serves_body() {
        let response = json_router()
            .oneshot(
                Request::builder()
                    .uri("/v1/resource")
                    .header(header::IF_NONE_MATCH, "\"deadbeef\"")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(!bytes.is_empty());
    }

    #[tokio::test]
    async fn non_json_responses_are_passthrough() {
        let response = json_router()
            .oneshot(
                Request::builder()
                    .uri("/v1/text")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::ETAG).is_none());
    }

    #[tokio::test]
    async fn post_is_passthrough() {
        let app = Router::new()
            .route(
                "/v1/resource",
                axum::routing::post(|| async { axum::Json(serde_json::json!({"created": true})) }),
            )
            .layer(from_fn(etag_middleware));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/resource")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.headers().get(header::ETAG).is_none());
    }

    #[test]
    fn strong_etag_is_quoted_hex_sha256() {
        let tag = compute_strong_etag(b"hello");
        assert!(tag.starts_with('"'));
        assert!(tag.ends_with('"'));
        // sha256("hello") in hex
        assert_eq!(
            tag,
            "\"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\""
        );
    }
}
