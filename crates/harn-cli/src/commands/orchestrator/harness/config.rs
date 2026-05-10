//! Public configuration and error types for the in-process orchestrator harness.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use harn_vm::clock::{Clock, RealClock};

use super::super::errors::OrchestratorError;
use super::super::role::OrchestratorRole;
use super::super::tls::TlsFiles;

// ── Public config ─────────────────────────────────────────────────────────────

/// Drain limits applied during graceful shutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrainConfig {
    pub max_items: usize,
    pub deadline: Duration,
}

impl Default for DrainConfig {
    fn default() -> Self {
        Self {
            max_items: crate::package::default_orchestrator_drain_max_items(),
            deadline: Duration::from_secs(
                crate::package::default_orchestrator_drain_deadline_seconds(),
            ),
        }
    }
}

/// Topic-pump concurrency limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PumpConfig {
    pub max_outstanding: usize,
}

impl Default for PumpConfig {
    fn default() -> Self {
        Self {
            max_outstanding: crate::package::default_orchestrator_pump_max_outstanding(),
        }
    }
}

/// Configuration for `OrchestratorHarness::start`.
#[derive(Clone, Debug)]
pub struct OrchestratorConfig {
    pub manifest_path: PathBuf,
    pub state_dir: PathBuf,
    pub bind: SocketAddr,
    pub role: OrchestratorRole,
    pub watch_manifest: bool,
    pub mcp: bool,
    pub mcp_path: String,
    pub mcp_sse_path: String,
    pub mcp_messages_path: String,
    pub tls: Option<TlsFiles>,
    pub shutdown_timeout: Duration,
    pub drain: DrainConfig,
    pub pump: PumpConfig,
    /// When `Some`, installs an observability tracing subscriber.  Tests
    /// should leave this `None` to avoid conflicts with the test runtime.
    pub log_format: Option<harn_vm::observability::otel::LogFormat>,
    /// Time + sleep substrate. Defaults to [`RealClock`]; tests inject
    /// [`harn_vm::clock::PausedClock`] via [`OrchestratorConfig::with_clock`].
    pub clock: Arc<dyn Clock>,
}

impl OrchestratorConfig {
    /// Minimal defaults suitable for integration tests.
    pub fn for_test(manifest_path: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            manifest_path,
            state_dir,
            bind: "127.0.0.1:0".parse().unwrap(),
            role: OrchestratorRole::SingleTenant,
            watch_manifest: false,
            mcp: false,
            mcp_path: "/mcp".to_string(),
            mcp_sse_path: "/sse".to_string(),
            mcp_messages_path: "/messages".to_string(),
            tls: None,
            shutdown_timeout: Duration::from_secs(5),
            drain: DrainConfig::default(),
            pump: PumpConfig::default(),
            log_format: None,
            clock: RealClock::arc(),
        }
    }

    /// Inject a custom [`Clock`]. Production callers leave the default
    /// [`RealClock`]; harness-driven tests pass a
    /// [`harn_vm::clock::PausedClock`] so cron and the dispatcher run on
    /// virtual time.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

// ── Public harness types ──────────────────────────────────────────────────────

/// Summary returned by `OrchestratorHarness::shutdown`.
#[derive(Debug)]
pub struct ShutdownReport {
    #[allow(dead_code)]
    pub timed_out: bool,
}

/// Error type for harness operations.
#[derive(Debug)]
pub struct HarnessError(pub String);

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<OrchestratorError> for HarnessError {
    fn from(error: OrchestratorError) -> Self {
        HarnessError(error.to_string())
    }
}

impl From<String> for HarnessError {
    fn from(s: String) -> Self {
        HarnessError(s)
    }
}
