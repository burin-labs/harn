//! Transport completeness primitives for `harn-serve` HTTP routers.
//!
//! `harn-serve` adapters (API / A2A / MCP) build an `axum::Router` and
//! then call [`apply_transport_layers`] to wrap it with a stack of
//! tower middlewares that round out the HTTP surface beyond the
//! response-codec MVP (A.4 / #2501):
//!
//! * **Compression** — gzip / brotli / zstd negotiation via the
//!   request's `Accept-Encoding`, with the codec's response body
//!   transparently encoded when worthwhile.
//! * **ETag + conditional GET** — strong ETag attached to JSON `GET` /
//!   `HEAD` responses; matching `If-None-Match` short-circuits to
//!   `304 Not Modified`.
//! * **CORS** — declarative `cors { origin, methods, headers,
//!   credentials, max_age }` policy enforced at the layer boundary, so
//!   preflight requests answer without dispatching into a `.harn`
//!   handler.
//!
//! WebSocket upgrade lives in [`crate::ws`] because it needs to mount
//! per-route (axum can't middleware-upgrade); content negotiation,
//! ETag derivation, and multipart streaming are exposed to `.harn`
//! handlers via builtins in `harn_vm::stdlib::http_response` and
//! `harn_vm::stdlib::multipart`.

use axum::http::HeaderValue;
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

mod etag;

pub use etag::compute_strong_etag;

/// Bodies under this threshold skip compression entirely. Below ~512
/// bytes the encoder overhead (header framing + minimum CRC) usually
/// exceeds any savings. Tower-http's `DefaultPredicate` also filters
/// `Content-Type: text/event-stream` and a few other already-tiny or
/// pre-compressed types — see its source for the full list.
pub const COMPRESSION_MIN_SIZE_BYTES: u16 = 512;

/// Declarative CORS policy. `None` disables CORS entirely (no
/// `Access-Control-*` headers emitted, no preflight short-circuit).
#[derive(Clone, Debug, Default)]
pub struct CorsConfig {
    /// Explicit allowed origins. Matched as literal strings against
    /// `Origin`. Use `["*"]` (or set `allow_any_origin`) for the
    /// wildcard.
    pub allow_origins: Vec<String>,
    /// Send `Access-Control-Allow-Origin: *`. Per the CORS spec this
    /// is mutually exclusive with credentials, so `allow_credentials`
    /// is ignored when this is true.
    pub allow_any_origin: bool,
    /// Methods echoed in preflight responses. Defaults to the common
    /// safe verbs when empty.
    pub allow_methods: Vec<String>,
    /// Request headers allowed on cross-origin requests. Defaults to
    /// `Authorization`, `Content-Type`, `X-Request-Id` when empty.
    pub allow_headers: Vec<String>,
    /// Response headers exposed to JS beyond the CORS-safelist.
    pub expose_headers: Vec<String>,
    /// Permit cookies / `Authorization` on cross-origin requests.
    pub allow_credentials: bool,
    /// Max-age (seconds) for the preflight cache; defaults to 1 hour.
    pub max_age_seconds: Option<u32>,
}

impl CorsConfig {
    /// Permit any origin without credentials — the broad-open profile
    /// most public APIs use.
    pub fn allow_any() -> Self {
        Self {
            allow_any_origin: true,
            ..Self::default()
        }
    }
}

/// Configuration for the transport middleware stack. Adapters build
/// their router as usual, then wrap it via [`apply_transport_layers`].
#[derive(Clone, Debug, Default)]
pub struct TransportConfig {
    /// Enable `Accept-Encoding`-driven gzip / brotli / zstd
    /// compression.
    pub compression: bool,
    /// Attach a strong `ETag` to JSON `GET` / `HEAD` responses; honour
    /// `If-None-Match` with `304 Not Modified`.
    pub etag: bool,
    /// CORS policy. `None` disables CORS entirely.
    pub cors: Option<CorsConfig>,
}

impl TransportConfig {
    /// The standard transport stack: compression + ETag enabled, CORS
    /// off (adapters opt-in per-mount).
    pub fn default_enabled() -> Self {
        Self {
            compression: true,
            etag: true,
            cors: None,
        }
    }
}

/// Wrap an axum router with the configured transport layers.
///
/// Tower layers run outer-to-inner on the request and inner-to-outer
/// on the response. We want, on the response path: routes → ETag (sees
/// the uncompressed body, so the digest is stable across encodings) →
/// compression → CORS. `.layer()` registers each as a new outermost
/// layer, so the order below adds ETag innermost, then compression,
/// then CORS.
pub fn apply_transport_layers(mut router: Router, config: &TransportConfig) -> Router {
    if config.etag {
        router = etag::install_on(router);
    }
    if config.compression {
        router = router.layer(compression_layer());
    }
    if let Some(cors) = &config.cors {
        router = router.layer(build_cors_layer(cors));
    }
    router
}

fn compression_layer() -> CompressionLayer {
    CompressionLayer::new()
        .gzip(true)
        .br(true)
        .zstd(true)
        .compress_when(tower_http::compression::DefaultPredicate::new())
}

fn build_cors_layer(config: &CorsConfig) -> CorsLayer {
    let mut layer = CorsLayer::new();

    if config.allow_any_origin || config.allow_origins.iter().any(|origin| origin == "*") {
        layer = layer.allow_origin(AllowOrigin::any());
    } else if !config.allow_origins.is_empty() {
        let origins: Vec<HeaderValue> = config
            .allow_origins
            .iter()
            .filter_map(|origin| HeaderValue::from_str(origin).ok())
            .collect();
        layer = layer.allow_origin(AllowOrigin::list(origins));
    }

    let methods: Vec<axum::http::Method> = if config.allow_methods.is_empty() {
        ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"]
            .into_iter()
            .filter_map(|m| m.parse().ok())
            .collect()
    } else {
        config
            .allow_methods
            .iter()
            .filter_map(|m| m.parse().ok())
            .collect()
    };
    layer = layer.allow_methods(methods);

    let header_names: Vec<axum::http::HeaderName> = if config.allow_headers.is_empty() {
        ["authorization", "content-type", "x-request-id"]
            .into_iter()
            .filter_map(|h| h.parse().ok())
            .collect()
    } else {
        config
            .allow_headers
            .iter()
            .filter_map(|h| h.parse().ok())
            .collect()
    };
    layer = layer.allow_headers(header_names);

    if !config.expose_headers.is_empty() {
        let exposed: Vec<axum::http::HeaderName> = config
            .expose_headers
            .iter()
            .filter_map(|h| h.parse().ok())
            .collect();
        layer = layer.expose_headers(exposed);
    }

    if config.allow_credentials && !config.allow_any_origin {
        layer = layer.allow_credentials(true);
    }

    let max_age = config.max_age_seconds.unwrap_or(3600);
    layer.max_age(std::time::Duration::from_secs(max_age as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Method, Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    fn sample_router() -> Router {
        Router::new().route(
            "/json",
            get(|| async {
                axum::Json(serde_json::json!({
                    "data": "x".repeat(2048),
                }))
            }),
        )
    }

    #[tokio::test]
    async fn compression_layer_gzips_when_accepted() {
        let app = apply_transport_layers(sample_router(), &TransportConfig::default_enabled());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/json")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .map(HeaderValue::to_str)
                .transpose()
                .unwrap(),
            Some("gzip"),
        );
    }

    #[tokio::test]
    async fn compression_layer_skipped_without_accept_encoding() {
        let app = apply_transport_layers(sample_router(), &TransportConfig::default_enabled());
        let response = app
            .oneshot(Request::builder().uri("/json").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
    }

    #[tokio::test]
    async fn cors_preflight_returns_allow_headers() {
        let config = TransportConfig {
            compression: false,
            etag: false,
            cors: Some(CorsConfig {
                allow_origins: vec!["https://app.example.com".into()],
                ..Default::default()
            }),
        };
        let app = apply_transport_layers(sample_router(), &config);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/json")
                    .header(header::ORIGIN, "https://app.example.com")
                    .header("access-control-request-method", "GET")
                    .header("access-control-request-headers", "authorization")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap(),
            "https://app.example.com"
        );
        assert!(response
            .headers()
            .get("access-control-allow-methods")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("GET"));
    }

    #[tokio::test]
    async fn cors_disabled_emits_no_headers() {
        let app = apply_transport_layers(sample_router(), &TransportConfig::default_enabled());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/json")
                    .header(header::ORIGIN, "https://app.example.com")
                    .header("access-control-request-method", "GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(!response
            .headers()
            .contains_key("access-control-allow-origin"));
    }

    #[tokio::test]
    async fn pipeline_yields_uncompressed_body_when_no_accept_encoding() {
        let app = apply_transport_layers(sample_router(), &TransportConfig::default_enabled());
        let response = app
            .oneshot(Request::builder().uri("/json").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(parsed["data"].as_str().unwrap().starts_with("xxxx"));
    }
}
