//! In-process orchestrator harness: starts pumps, connectors, and the HTTP
//! listener in a dedicated background thread with its own Tokio runtime.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, watch};

use harn_vm::event_log::AnyEventLog;

use super::errors::OrchestratorError;
use super::listener::AdminReloadHandle;

mod audit;
mod config;
mod lifecycle;
mod pumps;
mod reload;
mod routing;
mod shutdown;

pub use config::{DrainConfig, HarnessError, OrchestratorConfig, PumpConfig, ShutdownReport};

pub(crate) use lifecycle::{absolutize_from_cwd, load_manifest};

// ── Module-wide constants ─────────────────────────────────────────────────────

const LIFECYCLE_TOPIC: &str = "orchestrator.lifecycle";
#[cfg_attr(not(unix), allow(dead_code))]
const MANIFEST_TOPIC: &str = "orchestrator.manifest";
const STATE_SNAPSHOT_FILE: &str = "orchestrator-state.json";
const PENDING_TOPIC: &str = "orchestrator.triggers.pending";
const CRON_TICK_TOPIC: &str = "connectors.cron.tick";
const TEST_INBOX_TASK_RELEASE_FILE_ENV: &str = "HARN_TEST_ORCHESTRATOR_INBOX_TASK_RELEASE_FILE";
const TEST_FAIL_PENDING_PUMP_ENV: &str = "HARN_TEST_ORCHESTRATOR_FAIL_PENDING_PUMP";

// ── Public harness ────────────────────────────────────────────────────────────

/// In-process orchestrator runtime.
///
/// Starts all pumps, connectors, and the HTTP listener in a dedicated
/// background thread with its own single-threaded Tokio runtime + LocalSet.
/// Tests hold `Arc<AnyEventLog>` directly and use `EventLog::subscribe()` for
/// event-driven waits instead of polling.
#[allow(dead_code)]
pub struct OrchestratorHarness {
    event_log: Arc<AnyEventLog>,
    listener_url: String,
    local_addr: SocketAddr,
    state_dir: PathBuf,
    admin_reload: AdminReloadHandle,
    shutdown_tx: Arc<watch::Sender<bool>>,
    pump_drain_gate: pumps::PumpDrainGate,
    join: Option<std::thread::JoinHandle<()>>,
}

pub(super) struct ReadyState {
    pub(super) event_log: Arc<AnyEventLog>,
    pub(super) listener_url: String,
    pub(super) local_addr: SocketAddr,
    pub(super) state_dir: PathBuf,
    pub(super) admin_reload: AdminReloadHandle,
}

#[allow(dead_code)]
impl OrchestratorHarness {
    /// Start the orchestrator in-process.  Resolves once the HTTP listener
    /// is ready and the startup lifecycle event has been appended.
    pub async fn start(config: OrchestratorConfig) -> Result<Self, HarnessError> {
        let (ready_tx, ready_rx) = oneshot::channel::<Result<ReadyState, OrchestratorError>>();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let shutdown_tx = Arc::new(shutdown_tx);
        let pump_drain_gate = pumps::PumpDrainGate::new();
        let task_pump_drain_gate = pump_drain_gate.clone();

        let join = std::thread::spawn(move || {
            // Use a multi-thread runtime so that blocking I/O (e.g. the OTEL
            // SimpleSpanProcessor calling futures::executor::block_on for each
            // span) can proceed on reactor threads while the LocalSet thread is
            // temporarily paused.  A current-thread runtime would deadlock:
            // block_on holds the only thread while the reactor needs that same
            // thread to drive the TCP export.
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("failed to build OrchestratorHarness tokio runtime");
            let local = tokio::task::LocalSet::new();
            rt.block_on(local.run_until(lifecycle::orchestrator_task(
                config,
                ready_tx,
                shutdown_rx,
                task_pump_drain_gate,
            )));
        });

        match ready_rx.await {
            Ok(Ok(ready)) => Ok(Self {
                event_log: ready.event_log,
                listener_url: ready.listener_url,
                local_addr: ready.local_addr,
                state_dir: ready.state_dir,
                admin_reload: ready.admin_reload,
                shutdown_tx,
                pump_drain_gate,
                join: Some(join),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(HarnessError::from(error))
            }
            Err(_) => {
                let _ = join.join();
                Err(HarnessError(
                    "harness thread exited before signaling readiness".to_string(),
                ))
            }
        }
    }

    pub fn listener_url(&self) -> &str {
        &self.listener_url
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn event_log(&self) -> Arc<AnyEventLog> {
        self.event_log.clone()
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Returns a handle that can be used to trigger admin-reload from outside
    /// the harness (e.g. on SIGHUP in the CLI wrapper).
    pub fn admin_reload(&self) -> AdminReloadHandle {
        self.admin_reload.clone()
    }

    /// Returns a sender that, when `send(true)` is called, triggers graceful
    /// shutdown.  Use this to wire OS signal handlers in the CLI wrapper.
    pub fn shutdown_trigger(&self) -> Arc<watch::Sender<bool>> {
        self.shutdown_tx.clone()
    }

    /// Pause topic pumps at the next event admission. Tests can wait for
    /// `pump_drain_waiting` before triggering shutdown.
    pub fn pause_pump_drain(&self) {
        self.pump_drain_gate.pause();
    }

    /// Release topic pumps paused by `pause_pump_drain`.
    pub fn release_pump_drain(&self) {
        self.pump_drain_gate.release();
    }

    /// Cancel-safe, idempotent.  Performs the same drain logic as SIGTERM.
    pub async fn shutdown(mut self, _deadline: Duration) -> Result<ShutdownReport, HarnessError> {
        // Signal the background runtime to start graceful shutdown.
        let _ = self.shutdown_tx.send(true);
        // Wait for the background thread to finish (graceful shutdown runs there).
        let Some(join) = self.join.take() else {
            return Err(HarnessError("harness already shut down".to_string()));
        };
        tokio::task::spawn_blocking(move || join.join())
            .await
            .map_err(|_| HarnessError("spawn_blocking join failed".to_string()))?
            .map_err(|_| HarnessError("harness background thread panicked".to_string()))?;
        Ok(ShutdownReport { timed_out: false })
    }
}

impl Drop for OrchestratorHarness {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
#[path = "../harness_tests.rs"]
mod harness_tests;
