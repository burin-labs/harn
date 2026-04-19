use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TlsFiles {
    pub(crate) cert: PathBuf,
    pub(crate) key: PathBuf,
}

impl TlsFiles {
    pub(crate) fn from_args(
        cert: Option<PathBuf>,
        key: Option<PathBuf>,
    ) -> Result<Option<Self>, String> {
        match (cert, key) {
            (None, None) => Ok(None),
            (Some(cert), Some(key)) => Ok(Some(Self { cert, key })),
            (Some(_), None) => Err("`--cert` requires `--key`".to_string()),
            (None, Some(_)) => Err("`--key` requires `--cert`".to_string()),
        }
    }
}

pub(crate) struct ServerRuntime {
    local_addr: SocketAddr,
    handle: Handle,
    task: tokio::task::JoinHandle<Result<(), String>>,
    tls_enabled: bool,
}

impl ServerRuntime {
    pub(crate) async fn start(
        bind: SocketAddr,
        app: Router,
        tls: Option<&TlsFiles>,
    ) -> Result<Self, String> {
        let listener = bind_listener(bind)?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("failed to inspect listener address: {error}"))?;
        let handle = Handle::new();
        let handle_for_task = handle.clone();

        let task = if let Some(tls) = tls {
            let rustls = load_rustls_config(&tls.cert, &tls.key).await?;
            tokio::spawn(async move {
                axum_server::from_tcp_rustls(listener, rustls)
                    .handle(handle_for_task)
                    .serve(app.into_make_service())
                    .await
                    .map_err(|error| format!("HTTPS listener failed: {error}"))
            })
        } else {
            tokio::spawn(async move {
                axum_server::from_tcp(listener)
                    .handle(handle_for_task)
                    .serve(app.into_make_service())
                    .await
                    .map_err(|error| format!("HTTP listener failed: {error}"))
            })
        };

        Ok(Self {
            local_addr,
            handle,
            task,
            tls_enabled: tls.is_some(),
        })
    }

    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub(crate) fn tls_enabled(&self) -> bool {
        self.tls_enabled
    }

    pub(crate) async fn shutdown(self) -> Result<(), String> {
        self.handle.graceful_shutdown(Some(Duration::from_secs(30)));
        match self.task.await {
            Ok(result) => result,
            Err(error) => Err(format!("listener task join failed: {error}")),
        }
    }
}

async fn load_rustls_config(cert: &Path, key: &Path) -> Result<RustlsConfig, String> {
    install_crypto_provider();
    if !cert.is_file() {
        return Err(format!("TLS certificate not found: {}", cert.display()));
    }
    if !key.is_file() {
        return Err(format!("TLS private key not found: {}", key.display()));
    }

    RustlsConfig::from_pem_file(cert.to_path_buf(), key.to_path_buf())
        .await
        .map_err(|error| {
            format!(
                "failed to load TLS certificate {} and key {}: {error}",
                cert.display(),
                key.display()
            )
        })
}

fn install_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn bind_listener(bind: SocketAddr) -> Result<TcpListener, String> {
    let listener = TcpListener::bind(bind)
        .map_err(|error| format!("failed to bind listener on {bind}: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("failed to enable nonblocking listener mode: {error}"))?;
    Ok(listener)
}
