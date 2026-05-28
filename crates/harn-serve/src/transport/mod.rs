//! Transport completeness primitives for axum routers served by
//! `harn-serve`: response compression, declarative CORS, and
//! ETag-based conditional GETs.
//!
//! These layers wrap the router built by an adapter (API, A2A, MCP)
//! before it is handed to the TLS-aware listener in [`crate::tls`].
//! They are intentionally adapter-agnostic — they see only the
//! `axum::Response` after a handler has rendered, so the same wins
//! apply to every existing Rust handler today and to any `.harn`
//! handler installed in the future via [`crate::http_codec`].

use axum::http::HeaderValue;
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

mod etag;

pub use etag::compute_strong_etag;

/// Declarative CORS policy applied at mount time. `None` disables CORS
/// entirely (no preflight handling, no `Access-Control-*` headers
/// emitted). The default is `None` — adapters opt in.
#[derive(Clone, Debug, Default)]
pub struct CorsConfig {
    /// Allowed origins. `["*"]` (or an empty list with
    /// `allow_any_origin` set) permits any origin. Origins are matched
    /// as literal strings against the request's `Origin` header.
    pub allow_origins: Vec<String>,
    /// When true, sends `Access-Control-Allow-Origin: *` and rejects
    /// requests with credentials per the CORS spec (the two are
    /// mutually exclusive).
    pub allow_any_origin: bool,
    /// Methods echoed in preflight responses. Defaults to the common
    /// safe set when empty.
    pub allow_methods: Vec<String>,
    /// Request headers permitted on cross-origin requests. Defaults to
    /// the common safe set (`Authorization`, `Content-Type`,
    /// `X-Request-Id`) when empty.
    pub allow_headers: Vec<String>,
    /// Response headers exposed to the browser beyond the CORS-safelist
    /// defaults. Honoured verbatim.
    pub expose_headers: Vec<String>,
    /// Whether cookies / Authorization may be sent on cross-origin
    /// requests. Forces `allow_any_origin = false` when true.
    pub allow_credentials: bool,
    /// Max-age (seconds) the browser may cache the preflight response.
    /// Defaults to 1 hour when unset.
    pub max_age_seconds: Option<u32>,
}

impl CorsConfig {
    pub fn allow_any() -> Self {
        Self {
            allow_any_origin: true,
            ..Self::default()
        }
    }
}

/// Configuration for the transport middleware stack. Callers build the
/// adapter's router as usual, then call [`apply_transport_layers`] to
/// wrap it.
#[derive(Clone, Debug, Default)]
pub struct TransportConfig {
    /// When true, enable gzip + brotli + zstd response compression.
    /// Defaults to true via [`TransportConfig::default_enabled`].
    pub compression: bool,
    /// When true, attach a strong ETag to JSON GET/HEAD responses and
    /// short-circuit to 304 when `If-None-Match` matches.
    pub etag: bool,
    /// Optional CORS policy. `None` disables CORS.
    pub cors: Option<CorsConfig>,
}

impl TransportConfig {
    /// The standard transport stack: compression + ETag enabled, CORS
    /// off. Mirrors what every `harn-serve` adapter wants by default;
    /// callers explicitly opt into CORS or out of compression/ETag.
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
/// Layer order matters in tower: layers added later run first on the
/// way in and last on the way out. We want, from outermost in:
///
/// 1. CORS (responds to preflight without invoking inner handlers)
/// 2. Compression (sees the final body)
/// 3. ETag (sees the uncompressed body so the digest is stable across
///    compression algorithms)
/// 4. Adapter routes
///
/// Tower's `.layer()` adds layers in outer-to-inner order, so we add
/// in reverse: ETag first (innermost), then compression, then CORS.
pub fn apply_transport_layers(mut router: Router, config: &TransportConfig) -> Router {
    if config.etag {
        router = router.layer(axum::middleware::from_fn(etag::etag_middleware));
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
    // Enable the modern compressors only; deflate is intentionally
    // omitted by Cargo feature, so it's never advertised in
    // `Accept-Encoding` negotiation. `DefaultPredicate` skips bodies
    // below the threshold and content types known to be already
    // compressed (image/*, video/*, …).
    CompressionLayer::new()
        .gzip(true)
        .br(true)
        .zstd(true)
        .compress_when(tower_http::compression::DefaultPredicate::new())
}

fn build_cors_layer(config: &CorsConfig) -> CorsLayer {
    let mut layer = CorsLayer::new();

    // Per CORS spec, `Access-Control-Allow-Origin: *` is incompatible
    // with `Access-Control-Allow-Credentials: true` (browsers reject
    // the response). If the caller asked for both, credentials win:
    // pin the origin list literally instead of advertising any-origin.
    let wants_any = (config.allow_any_origin
        || config.allow_origins.iter().any(|origin| origin == "*"))
        && !config.allow_credentials;
    if wants_any {
        layer = layer.allow_origin(AllowOrigin::any());
    } else if !config.allow_origins.is_empty() {
        let origins: Vec<HeaderValue> = config
            .allow_origins
            .iter()
            .filter(|origin| origin.as_str() != "*")
            .filter_map(|origin| HeaderValue::from_str(origin).ok())
            .collect();
        if !origins.is_empty() {
            layer = layer.allow_origin(AllowOrigin::list(origins));
        }
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

    if config.allow_credentials {
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
    async fn cors_disabled_does_not_emit_headers() {
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
        // Without CORS the OPTIONS verb is not a registered route, so
        // axum's Method-not-allowed kicks in. Either way, no
        // Allow-Origin should be emitted.
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
