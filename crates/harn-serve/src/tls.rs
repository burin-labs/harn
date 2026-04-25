use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Once;

use axum::http::header::HeaderName;
use axum::http::HeaderValue;
use axum::response::Response;
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;

const DEFAULT_HSTS_MAX_AGE_SECONDS: u64 = 31_536_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HstsConfig {
    pub enabled: bool,
    pub max_age_seconds: u64,
    pub include_subdomains: bool,
    pub preload: bool,
}

impl HstsConfig {
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            max_age_seconds: DEFAULT_HSTS_MAX_AGE_SECONDS,
            include_subdomains: false,
            preload: false,
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            max_age_seconds: 0,
            include_subdomains: false,
            preload: false,
        }
    }

    fn header_value(&self) -> Option<HeaderValue> {
        if !self.enabled {
            return None;
        }
        let mut value = format!("max-age={}", self.max_age_seconds);
        if self.include_subdomains {
            value.push_str("; includeSubDomains");
        }
        if self.preload {
            value.push_str("; preload");
        }
        HeaderValue::from_str(&value).ok()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HttpTlsConfig {
    #[default]
    Plain,
    EdgeTerminated {
        hsts: HstsConfig,
    },
    PemFiles {
        cert: PathBuf,
        key: PathBuf,
        hsts: HstsConfig,
    },
    SelfSignedDev {
        hosts: Vec<String>,
    },
}

impl HttpTlsConfig {
    pub fn plain() -> Self {
        Self::Plain
    }

    pub fn edge_terminated() -> Self {
        Self::EdgeTerminated {
            hsts: HstsConfig::enabled(),
        }
    }

    pub fn pem_files(cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        Self::PemFiles {
            cert: cert.into(),
            key: key.into(),
            hsts: HstsConfig::enabled(),
        }
    }

    pub fn self_signed_dev() -> Self {
        Self::SelfSignedDev {
            hosts: vec!["localhost".to_string(), "127.0.0.1".to_string()],
        }
    }

    pub fn listener_scheme(&self) -> &'static str {
        match self {
            Self::Plain | Self::EdgeTerminated { .. } => "http",
            Self::PemFiles { .. } | Self::SelfSignedDev { .. } => "https",
        }
    }

    pub fn advertised_scheme(&self) -> &'static str {
        match self {
            Self::Plain => "http",
            Self::EdgeTerminated { .. } | Self::PemFiles { .. } | Self::SelfSignedDev { .. } => {
                "https"
            }
        }
    }

    pub fn is_edge_terminated(&self) -> bool {
        matches!(self, Self::EdgeTerminated { .. })
    }

    pub fn security_headers(&self) -> Vec<(HeaderName, HeaderValue)> {
        let hsts = match self {
            Self::EdgeTerminated { hsts } | Self::PemFiles { hsts, .. } => hsts,
            Self::Plain | Self::SelfSignedDev { .. } => return Vec::new(),
        };
        hsts.header_value()
            .map(|value| (HeaderName::from_static("strict-transport-security"), value))
            .into_iter()
            .collect()
    }

    async fn rustls_config(&self) -> Result<Option<RustlsConfig>, String> {
        match self {
            Self::Plain | Self::EdgeTerminated { .. } => Ok(None),
            Self::PemFiles { cert, key, .. } => load_rustls_config(cert, key).await.map(Some),
            Self::SelfSignedDev { hosts } => self_signed_rustls_config(hosts).await.map(Some),
        }
    }
}

pub async fn serve_router_from_tcp(
    listener: TcpListener,
    router: Router,
    tls: &HttpTlsConfig,
) -> Result<(), String> {
    match tls.rustls_config().await? {
        Some(config) => axum_server::from_tcp_rustls(listener, config)
            .map_err(|error| format!("HTTPS listener setup failed: {error}"))?
            .serve(router.into_make_service())
            .await
            .map_err(|error| format!("HTTPS listener failed: {error}")),
        None => axum_server::from_tcp(listener)
            .map_err(|error| format!("HTTP listener setup failed: {error}"))?
            .serve(router.into_make_service())
            .await
            .map_err(|error| format!("HTTP listener failed: {error}")),
    }
}

pub fn bind_listener(bind: SocketAddr) -> Result<TcpListener, String> {
    let listener =
        TcpListener::bind(bind).map_err(|error| format!("failed to bind {bind}: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("failed to enable nonblocking listener mode: {error}"))?;
    Ok(listener)
}

pub fn apply_security_headers(router: Router, tls: &HttpTlsConfig) -> Router {
    let headers = tls.security_headers();
    if headers.is_empty() {
        return router;
    }
    router.layer(axum::middleware::map_response(
        move |mut response: Response| {
            let headers = headers.clone();
            async move {
                for (name, value) in headers {
                    response.headers_mut().insert(name, value);
                }
                response
            }
        },
    ))
}

async fn load_rustls_config(cert: &Path, key: &Path) -> Result<RustlsConfig, String> {
    install_crypto_provider();
    if !cert.is_file() {
        return Err(format!("TLS certificate not found: {}", cert.display()));
    }
    if !key.is_file() {
        return Err(format!("TLS private key not found: {}", key.display()));
    }
    RustlsConfig::from_pem_file(cert, key)
        .await
        .map_err(|error| {
            format!(
                "failed to load TLS certificate {} and key {}: {error}",
                cert.display(),
                key.display()
            )
        })
}

async fn self_signed_rustls_config(hosts: &[String]) -> Result<RustlsConfig, String> {
    install_crypto_provider();
    let hosts = if hosts.is_empty() {
        vec!["localhost".to_string(), "127.0.0.1".to_string()]
    } else {
        hosts.to_vec()
    };
    let cert = rcgen::generate_simple_self_signed(hosts)
        .map_err(|error| format!("failed to generate self-signed dev certificate: {error}"))?;
    RustlsConfig::from_pem(
        cert.cert.pem().into_bytes(),
        cert.key_pair.serialize_pem().into_bytes(),
    )
    .await
    .map_err(|error| format!("failed to load self-signed dev certificate: {error}"))
}

fn install_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use tempfile::TempDir;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn pem_config_reports_missing_files_before_serving() {
        let temp = TempDir::new().expect("tempdir");
        let tls =
            HttpTlsConfig::pem_files(temp.path().join("missing.pem"), temp.path().join("key.pem"));

        let error = tls
            .rustls_config()
            .await
            .expect_err("missing files should fail");

        assert!(error.contains("TLS certificate not found"), "error={error}");
    }

    #[tokio::test]
    async fn pem_config_reports_invalid_pem_before_serving() {
        let temp = TempDir::new().expect("tempdir");
        let cert = temp.path().join("cert.pem");
        let key = temp.path().join("key.pem");
        std::fs::write(&cert, "not a cert").expect("write cert");
        std::fs::write(&key, "not a key").expect("write key");
        let tls = HttpTlsConfig::pem_files(&cert, &key);

        let error = tls
            .rustls_config()
            .await
            .expect_err("invalid files should fail");

        assert!(
            error.contains("failed to load TLS certificate"),
            "error={error}"
        );
    }

    #[tokio::test]
    async fn self_signed_dev_starts_https_listener() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).expect("listener");
        let addr = listener.local_addr().expect("addr");
        let (ready_tx, ready_rx) = oneshot::channel();
        let app = Router::new().route("/health", get(|| async { "ok" }));
        let tls = HttpTlsConfig::self_signed_dev();

        let task = tokio::spawn(async move {
            let _ = ready_tx.send(());
            serve_router_from_tcp(listener, app, &tls).await
        });
        ready_rx.await.expect("ready");

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("client");
        let response = client
            .get(format!("https://{addr}/health"))
            .send()
            .await
            .expect("request");
        assert!(response.status().is_success());

        task.abort();
    }

    #[tokio::test]
    async fn plain_and_edge_modes_start_plain_listener() {
        for tls in [HttpTlsConfig::plain(), HttpTlsConfig::edge_terminated()] {
            let listener = bind_listener("127.0.0.1:0".parse().unwrap()).expect("listener");
            let addr = listener.local_addr().expect("addr");
            let (ready_tx, ready_rx) = oneshot::channel();
            let app = apply_security_headers(
                Router::new().route("/health", get(|| async { "ok" })),
                &tls,
            );
            let is_edge = tls.is_edge_terminated();

            let task = tokio::spawn(async move {
                let _ = ready_tx.send(());
                serve_router_from_tcp(listener, app, &tls).await
            });
            ready_rx.await.expect("ready");

            let response = reqwest::get(format!("http://{addr}/health"))
                .await
                .expect("request");
            assert!(response.status().is_success());
            if is_edge {
                assert!(response.headers().contains_key("strict-transport-security"));
            } else {
                assert!(!response.headers().contains_key("strict-transport-security"));
            }

            task.abort();
        }
    }
}
