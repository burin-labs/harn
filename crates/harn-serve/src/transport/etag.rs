//! Conditional-GET middleware. Buffers the response body of safe
//! requests (`GET`, `HEAD`) into memory, derives a strong ETag, and
//! short-circuits to `304 Not Modified` when the request's
//! `If-None-Match` matches.
//!
//! Per RFC 9110 §13.1.2 `If-None-Match` accepts either `*` (matches
//! any current representation) or a list of entity tags. Both are
//! honoured. The strong/weak distinction follows §8.8.3: our derived
//! tag is strong (no `W/` prefix); a request that quotes our exact
//! strong tag with the `W/` prefix is still treated as a deliberate
//! cache hit per the conventional client behaviour, since the prefix
//! only weakens the *request's* assertion.
//!
//! Only `application/json` 2xx responses are eligible. Streamed or SSE
//! bodies pass through untouched — buffering them would defeat their
//! point. Responses that already carry `Content-Encoding` are also
//! skipped because the standard layer stack puts ETag inside
//! compression, so a pre-encoded body would have escaped the codec.

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

/// Compute a strong ETag for the given body bytes — `"<hex sha256>"`.
/// The quotes are part of the value per RFC 9110 §8.8.3.
pub fn compute_strong_etag(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    let digest = hasher.finalize();
    format!("\"{}\"", hex::encode(digest))
}

/// Apply the ETag middleware to a router. Inlined into
/// [`crate::transport::apply_transport_layers`] rather than returned as
/// a standalone `Layer` because `from_fn` generates a generic layer
/// type whose `Clone + Send + Sync` bounds don't survive the
/// `Router::layer` call without naming concrete inner types.
pub fn install_on(router: axum::Router) -> axum::Router {
    router.layer(middleware::from_fn(etag_middleware))
}

async fn etag_middleware(request: Request, next: Next) -> Response {
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
            // Body errored mid-collection; the handler's response is
            // already gone, so the cleanest fallback is a bare 500.
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let etag_value = compute_strong_etag(&bytes);
    let Ok(etag_header) = HeaderValue::from_str(&etag_value) else {
        return rebuild_response(parts, bytes);
    };

    if matches_if_none_match(if_none_match.as_deref(), &etag_value) {
        parts.status = StatusCode::NOT_MODIFIED;
        parts.headers.insert(header::ETAG, etag_header);
        parts.headers.remove(header::CONTENT_LENGTH);
        parts.headers.remove(header::CONTENT_TYPE);
        return Response::from_parts(parts, Body::empty());
    }

    parts.headers.insert(header::ETAG, etag_header);
    rebuild_response(parts, bytes)
}

fn rebuild_response(parts: axum::http::response::Parts, bytes: Bytes) -> Response {
    let len = HeaderValue::from_str(&bytes.len().to_string()).ok();
    let mut response = Response::from_parts(parts, Body::from(bytes));
    if let Some(value) = len {
        response.headers_mut().insert(header::CONTENT_LENGTH, value);
    }
    response
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

fn matches_if_none_match(if_none_match: Option<&str>, etag: &str) -> bool {
    let Some(header) = if_none_match else {
        return false;
    };
    for candidate in header.split(',') {
        let trimmed = candidate.trim();
        if trimmed == "*" || trimmed == etag {
            return true;
        }
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
        assert_eq!(
            tag,
            "\"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\""
        );
    }

    #[test]
    fn empty_if_none_match_does_not_match() {
        assert!(!matches_if_none_match(None, "\"abc\""));
        assert!(!matches_if_none_match(Some(""), "\"abc\""));
        assert!(!matches_if_none_match(Some("\"def\""), "\"abc\""));
    }

    #[test]
    fn comma_separated_if_none_match_matches_any() {
        let etag = "\"abc\"";
        assert!(matches_if_none_match(
            Some("\"def\", \"abc\", \"ghi\""),
            etag
        ));
        assert!(matches_if_none_match(Some("\"def\", *"), etag));
    }
}
