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

use axum::http::{Extensions, HeaderMap, HeaderValue, StatusCode, Version};
use axum::middleware::{self, Next};
use axum::Router;
use tower_http::compression::predicate::{And, DefaultPredicate};
use tower_http::compression::{CompressionLayer, Predicate};
use tower_http::cors::{AllowOrigin, CorsLayer};

mod etag;
mod jsonrpc_stdio;

pub use etag::compute_strong_etag;
pub use jsonrpc_stdio::{
    read_jsonrpc_stdio_frame, write_jsonrpc_stdio_message, JsonRpcStdioFrame,
    JsonRpcStdioFrameStyle,
};

/// Handler-set marker that opts the response out of compression. The
/// custom [`Predicate`] honours it; an outer middleware strips the
/// header before the response leaves the server so clients never see
/// the implementation detail.
///
/// `x-compress: never` is the only value that disables compression;
/// any other value (or absence of the header) leaves the default
/// predicate logic in effect.
pub const COMPRESSION_OPT_OUT_HEADER: &str = "x-compress";
pub const COMPRESSION_OPT_OUT_VALUE: &str = "never";

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
    /// wildcard. A `"*"` entry is treated exactly like `allow_any_origin`,
    /// so per the CORS spec it also forces `allow_credentials` off.
    pub allow_origins: Vec<String>,
    /// Send `Access-Control-Allow-Origin: *`. Per the CORS spec this
    /// is mutually exclusive with credentials, so `allow_credentials`
    /// is ignored when this is true (or when `allow_origins` contains
    /// `"*"`).
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
        // The strip layer sits outside compression so the predicate sees
        // the marker on the response-path return; the strip runs after
        // and the client never observes the implementation header.
        router = router.layer(middleware::from_fn(strip_compression_marker));
    }
    if let Some(cors) = &config.cors {
        router = router.layer(build_cors_layer(cors));
    }
    router
}

fn compression_layer() -> CompressionLayer<And<HeaderOptOutPredicate, DefaultPredicate>> {
    CompressionLayer::new()
        .gzip(true)
        .br(true)
        .zstd(true)
        .compress_when(HeaderOptOutPredicate.and(DefaultPredicate::new()))
}

/// Returns `false` (i.e. don't compress) when the response carries
/// `x-compress: never`. Any other header value, or absence of the
/// header, defers to the next predicate in the chain. Implemented via
/// the closure-form [`Predicate`] blanket impl so the trait method
/// resolves through axum's re-exported `HeaderMap`/`StatusCode` types
/// without pulling in a direct `http-body` dependency.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeaderOptOutPredicate;

impl Predicate for HeaderOptOutPredicate {
    fn should_compress<B>(&self, response: &axum::http::Response<B>) -> bool {
        opt_out_predicate(
            response.status(),
            response.version(),
            response.headers(),
            response.extensions(),
        )
    }
}

fn opt_out_predicate(
    _status: StatusCode,
    _version: Version,
    headers: &HeaderMap,
    _extensions: &Extensions,
) -> bool {
    !headers
        .get_all(COMPRESSION_OPT_OUT_HEADER)
        .iter()
        .any(|value| {
            value
                .to_str()
                .map(|s| s.eq_ignore_ascii_case(COMPRESSION_OPT_OUT_VALUE))
                .unwrap_or(false)
        })
}

async fn strip_compression_marker(
    req: axum::extract::Request,
    next: Next,
) -> axum::response::Response {
    let mut response = next.run(req).await;
    response.headers_mut().remove(COMPRESSION_OPT_OUT_HEADER);
    response
}

fn build_cors_layer(config: &CorsConfig) -> CorsLayer {
    let mut layer = CorsLayer::new();

    // Both `allow_any_origin` and a literal `"*"` in `allow_origins` request the
    // wildcard. Decide once so the origin and credentials branches below cannot
    // diverge: tower-http panics at build time if credentials accompany the
    // wildcard, so the same predicate must gate both.
    let wildcard_origin =
        config.allow_any_origin || config.allow_origins.iter().any(|origin| origin == "*");

    if wildcard_origin {
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

    if config.allow_credentials && !wildcard_origin {
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
    use axum::response::IntoResponse;
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
    async fn cors_list_wildcard_with_credentials_does_not_panic_or_set_credentials() {
        // `allow_origins: ["*"]` is the documented equivalent of `allow_any_origin`,
        // so pairing it with credentials must degrade to a credential-less wildcard
        // rather than panicking in tower-http's `ensure_usable_cors_rules` at
        // router-build time.
        let config = TransportConfig {
            compression: false,
            etag: false,
            cors: Some(CorsConfig {
                allow_origins: vec!["*".into()],
                allow_credentials: true,
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
            "*"
        );
        assert!(!response
            .headers()
            .contains_key("access-control-allow-credentials"));
    }

    #[tokio::test]
    async fn handler_x_compress_never_skips_compression_and_strips_marker() {
        let app = apply_transport_layers(
            Router::new().route(
                "/raw",
                get(|| async {
                    let mut response = axum::Json(serde_json::json!({
                        "data": "x".repeat(2048),
                    }))
                    .into_response();
                    response.headers_mut().insert(
                        COMPRESSION_OPT_OUT_HEADER,
                        HeaderValue::from_static(COMPRESSION_OPT_OUT_VALUE),
                    );
                    response
                }),
            ),
            &TransportConfig::default_enabled(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/raw")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            !response.headers().contains_key(header::CONTENT_ENCODING),
            "x-compress: never must skip compression",
        );
        assert!(
            !response.headers().contains_key(COMPRESSION_OPT_OUT_HEADER),
            "marker header must be stripped before flushing to client",
        );
    }

    #[tokio::test]
    async fn handler_x_compress_other_value_still_compresses() {
        let app = apply_transport_layers(
            Router::new().route(
                "/maybe",
                get(|| async {
                    let mut response = axum::Json(serde_json::json!({
                        "data": "x".repeat(2048),
                    }))
                    .into_response();
                    // Anything other than the literal "never" leaves
                    // the default predicate in charge.
                    response
                        .headers_mut()
                        .insert(COMPRESSION_OPT_OUT_HEADER, HeaderValue::from_static("auto"));
                    response
                }),
            ),
            &TransportConfig::default_enabled(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/maybe")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_ENCODING)
                .map(HeaderValue::to_str)
                .transpose()
                .unwrap(),
            Some("gzip"),
        );
        // Strip layer is unconditional — the marker is removed even
        // when the value didn't disable compression.
        assert!(!response.headers().contains_key(COMPRESSION_OPT_OUT_HEADER));
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
