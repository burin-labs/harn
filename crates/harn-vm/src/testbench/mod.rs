//! Testbench: hermetic-execution composition primitive.
//!
//! Wires the four pluggable axes Harn already had — virtual time, mocked
//! LLM, filesystem overlay, recorded subprocess — behind a single
//! [`Testbench`] handle. Production wires real impls; tests/demos pick a
//! config and get an audit trail of everything that crossed the host
//! boundary.
//!
//! # Axes
//!
//! - **Clock** ([`crate::clock_mock`]). Pinned wall-clock + monotonic time
//!   honored by stdlib `now_ms`/`sleep`/`monotonic_ms`, the trigger
//!   dispatcher, and the cron scheduler. Tests advance with
//!   [`crate::clock_mock::advance`] or the script-side `advance_time(...)`.
//!
//! - **LLM** ([`crate::llm`]). The CLI replay/record path
//!   (`install_cli_llm_mocks` / `enable_cli_llm_mock_recording`) is the
//!   workhorse; [`crate::llm::FakeLlmProvider`] adds streaming/error
//!   fidelity for tests that care about per-token order.
//!
//! - **Filesystem** ([`overlay_fs`]). Copy-on-write overlay rooted at a
//!   real worktree: reads pass through, writes land in an in-memory
//!   layer, and [`overlay_fs::OverlayFs::diff`] surfaces a unified-style
//!   diff that can be applied back or discarded.
//!
//! - **Subprocess** ([`process_tape`]). Records `(program, args, cwd) →
//!   (stdout, stderr, exit, virtual Δt)` tuples in record mode and
//!   replays them deterministically in replay mode. Env-var matching
//!   is documented as future work — the JSON tape carries an `env`
//!   field reserved for it.
//!
//! # Network
//!
//! Network egress is deny-by-default in testbench mode — outbound HTTP
//! and connector requests fail fast unless an explicit allowlist names
//! the destination. The deny pass routes through [`crate::egress`], the
//! same policy engine production uses.

pub mod overlay_fs;
pub mod process_tape;

use std::path::PathBuf;
use std::sync::Arc;

use crate::clock_mock::{install_override, ClockOverrideGuard, MockClock};
use crate::egress::reset_egress_policy_for_host;

use overlay_fs::{install_overlay, OverlayFs, OverlayFsGuard};
use process_tape::{install_process_tape, ProcessTape, ProcessTapeGuard, ProcessTapeMode};

/// Declarative configuration for [`Testbench::activate`]. Every axis is
/// optional so callers can compose only the surfaces they need.
#[derive(Debug, Default, Clone)]
pub struct Testbench {
    pub clock: ClockConfig,
    pub llm: LlmConfig,
    pub filesystem: FilesystemConfig,
    pub subprocess: SubprocessConfig,
    pub network: NetworkConfig,
}

/// Configures the unified mock clock. Defaults to the runtime's real
/// clock so the testbench stays opt-in.
#[derive(Debug, Default, Clone)]
pub enum ClockConfig {
    /// Leave the clock alone. Real wall-clock + monotonic time.
    #[default]
    Real,
    /// Pin time to the given UNIX-epoch milliseconds. Honored by stdlib
    /// `now_ms`/`sleep`, the trigger dispatcher, and cron.
    Paused { starting_at_ms: i64 },
}

/// LLM provider configuration. Mirrors `harn run --llm-mock` /
/// `--llm-mock-record` so the testbench is a strict superset of that
/// flag pair. The testbench *does not* install LLM mocks itself — it
/// stays declarative so [`crate::llm::install_cli_llm_mocks`] (or its
/// `harn-cli` wrapper) remains the single mutator of LLM state.
#[derive(Debug, Default, Clone)]
pub enum LlmConfig {
    /// No LLM substitution. Calls go through the configured provider.
    #[default]
    Real,
    /// Replay scripted responses from a JSONL fixture.
    Replay { fixture: PathBuf },
    /// Capture executed responses into a JSONL fixture.
    Record { fixture: PathBuf },
}

/// Filesystem overlay configuration.
#[derive(Debug, Default, Clone)]
pub enum FilesystemConfig {
    /// No overlay. Reads and writes hit the real filesystem.
    #[default]
    Real,
    /// Read-through, copy-on-write overlay rooted at `worktree`. Writes
    /// stay in memory until the run ends, at which point the configured
    /// emitter (CLI flag, in-process API) can read the diff.
    Overlay { worktree: PathBuf },
}

/// Subprocess record/replay configuration.
#[derive(Debug, Default, Clone)]
pub enum SubprocessConfig {
    /// No interception. Subprocesses spawn against the host OS.
    #[default]
    Real,
    /// Record `(program, args, cwd)` tuples and their outputs into
    /// `tape` so a follow-up run can replay them.
    Record { tape: PathBuf },
    /// Look every spawn up in `tape` and emit the recorded result. Errors
    /// loudly when a tuple is not in the tape.
    Replay { tape: PathBuf },
}

/// Network policy. Defaults to the production egress policy (no
/// override). Testbench callers usually pick `DenyByDefault`.
#[derive(Debug, Default, Clone)]
pub enum NetworkConfig {
    /// Use whatever egress policy the host has already installed.
    #[default]
    Real,
    /// Deny outbound requests unless `allow` matches. Routes through
    /// [`crate::egress`] using the same env-var format that
    /// `HARN_EGRESS_*` accepts.
    DenyByDefault {
        /// Comma-separated allow rules (e.g. `"github.com,*.openai.com"`).
        /// Empty means deny everything.
        allow: Vec<String>,
    },
}

impl Testbench {
    /// Convenience: construct a builder.
    pub fn builder() -> TestbenchBuilder {
        TestbenchBuilder::default()
    }

    /// Activate every configured axis and return an RAII handle. Drop
    /// the handle to restore the prior state.
    pub fn activate(self) -> Result<TestbenchSession, TestbenchError> {
        TestbenchSession::install(self)
    }
}

/// Fluent constructor for [`Testbench`].
#[derive(Debug, Default, Clone)]
pub struct TestbenchBuilder {
    bench: Testbench,
}

impl TestbenchBuilder {
    pub fn paused_clock_at_ms(mut self, starting_at_ms: i64) -> Self {
        self.bench.clock = ClockConfig::Paused { starting_at_ms };
        self
    }

    pub fn replay_llm(mut self, fixture: impl Into<PathBuf>) -> Self {
        self.bench.llm = LlmConfig::Replay {
            fixture: fixture.into(),
        };
        self
    }

    pub fn record_llm(mut self, fixture: impl Into<PathBuf>) -> Self {
        self.bench.llm = LlmConfig::Record {
            fixture: fixture.into(),
        };
        self
    }

    pub fn fs_overlay(mut self, worktree: impl Into<PathBuf>) -> Self {
        self.bench.filesystem = FilesystemConfig::Overlay {
            worktree: worktree.into(),
        };
        self
    }

    pub fn record_subprocesses(mut self, tape: impl Into<PathBuf>) -> Self {
        self.bench.subprocess = SubprocessConfig::Record { tape: tape.into() };
        self
    }

    pub fn replay_subprocesses(mut self, tape: impl Into<PathBuf>) -> Self {
        self.bench.subprocess = SubprocessConfig::Replay { tape: tape.into() };
        self
    }

    pub fn deny_network(mut self) -> Self {
        self.bench.network = NetworkConfig::DenyByDefault { allow: Vec::new() };
        self
    }

    pub fn allow_network(mut self, allow: impl IntoIterator<Item = String>) -> Self {
        self.bench.network = NetworkConfig::DenyByDefault {
            allow: allow.into_iter().collect(),
        };
        self
    }

    pub fn build(self) -> Testbench {
        self.bench
    }
}

/// RAII handle returned by [`Testbench::activate`]. Holds every guard
/// for the active axes; dropping it tears them all down in order.
#[must_use = "the testbench tears down on drop; bind the handle to a `_session` local"]
pub struct TestbenchSession {
    _clock: Option<ClockOverrideGuard>,
    _process: Option<ProcessTapeGuard>,
    _overlay: Option<OverlayFsGuard>,
    process_tape: Option<Arc<ProcessTape>>,
    overlay: Option<Arc<OverlayFs>>,
    subprocess_mode: ProcessTapeMode,
    subprocess_tape_path: Option<PathBuf>,
    /// Saved env state (`HARN_EGRESS_DEFAULT`, `_ALLOW`, `_DENY`) for
    /// restoration on drop. `None` means the testbench did not override
    /// network policy this run.
    saved_egress_env: Option<SavedEgressEnv>,
}

#[derive(Debug, Clone)]
struct SavedEgressEnv {
    default: Option<String>,
    allow: Option<String>,
    deny: Option<String>,
}

impl TestbenchSession {
    fn install(bench: Testbench) -> Result<Self, TestbenchError> {
        let clock_guard = match bench.clock {
            ClockConfig::Real => None,
            ClockConfig::Paused { starting_at_ms } => {
                Some(install_override(MockClock::at_wall_ms(starting_at_ms)))
            }
        };

        // LLM state is *not* installed here — the caller owns the
        // CliLlmMockMode channel. Reading bench.llm just keeps the
        // declarative config visible to test inspection.
        let _llm_config = bench.llm;

        let (process_tape, process_guard, subprocess_mode, subprocess_tape_path) =
            match bench.subprocess {
                SubprocessConfig::Real => (None, None, ProcessTapeMode::Replay, None),
                SubprocessConfig::Record { tape } => {
                    let active = Arc::new(ProcessTape::recording());
                    let guard = install_process_tape(Arc::clone(&active));
                    (
                        Some(Arc::clone(&active)),
                        Some(guard),
                        ProcessTapeMode::Record,
                        Some(tape),
                    )
                }
                SubprocessConfig::Replay { tape } => {
                    let loaded = ProcessTape::load(&tape).map_err(TestbenchError::Subprocess)?;
                    let active = Arc::new(loaded);
                    let guard = install_process_tape(Arc::clone(&active));
                    (
                        Some(Arc::clone(&active)),
                        Some(guard),
                        ProcessTapeMode::Replay,
                        Some(tape),
                    )
                }
            };

        let (overlay, overlay_guard) = match bench.filesystem {
            FilesystemConfig::Real => (None, None),
            FilesystemConfig::Overlay { worktree } => {
                let overlay = Arc::new(OverlayFs::rooted_at(worktree));
                let guard = install_overlay(Arc::clone(&overlay));
                (Some(overlay), Some(guard))
            }
        };

        let saved_egress_env = match bench.network {
            NetworkConfig::Real => None,
            NetworkConfig::DenyByDefault { allow } => {
                let saved = SavedEgressEnv {
                    default: std::env::var("HARN_EGRESS_DEFAULT").ok(),
                    allow: std::env::var("HARN_EGRESS_ALLOW").ok(),
                    deny: std::env::var("HARN_EGRESS_DENY").ok(),
                };
                // Reset any prior policy so install_policy doesn't trip the
                // "policy already configured" guard, then install via env-var
                // so the host_policy and stdlib paths see the same view.
                reset_egress_policy_for_host();
                std::env::set_var("HARN_EGRESS_DEFAULT", "deny");
                if allow.is_empty() {
                    std::env::remove_var("HARN_EGRESS_ALLOW");
                } else {
                    std::env::set_var("HARN_EGRESS_ALLOW", allow.join(","));
                }
                std::env::remove_var("HARN_EGRESS_DENY");
                Some(saved)
            }
        };

        Ok(Self {
            _clock: clock_guard,
            _process: process_guard,
            _overlay: overlay_guard,
            process_tape,
            overlay,
            subprocess_mode,
            subprocess_tape_path,
            saved_egress_env,
        })
    }

    /// Whether subprocess interception is recording new entries.
    pub fn subprocess_mode(&self) -> ProcessTapeMode {
        self.subprocess_mode
    }

    /// Path that recorded subprocess tape entries should land in, or
    /// where replay loaded them from.
    pub fn subprocess_tape_path(&self) -> Option<&std::path::Path> {
        self.subprocess_tape_path.as_deref()
    }

    /// Reference to the active filesystem overlay (if any).
    pub fn overlay(&self) -> Option<&Arc<OverlayFs>> {
        self.overlay.as_ref()
    }

    /// Reference to the active process tape (if any).
    pub fn process_tape(&self) -> Option<&Arc<ProcessTape>> {
        self.process_tape.as_ref()
    }

    /// Persist the recorded subprocess tape (if recording) and return
    /// the filesystem diff (if an overlay is active). Tearing down the
    /// session via [`Drop`] will not persist; call this explicitly to
    /// flush.
    pub fn finalize(self) -> Result<TestbenchFinalize, TestbenchError> {
        let diff = self
            .overlay
            .as_ref()
            .map(|overlay| overlay.diff())
            .unwrap_or_default();
        let recorded = if matches!(self.subprocess_mode, ProcessTapeMode::Record) {
            if let (Some(tape), Some(path)) = (
                self.process_tape.as_ref(),
                self.subprocess_tape_path.as_ref(),
            ) {
                tape.persist(path).map_err(TestbenchError::Subprocess)?;
            }
            self.process_tape
                .as_ref()
                .map(|tape| tape.recorded())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        // The Drop impl undoes mocks regardless of finalize success.
        Ok(TestbenchFinalize {
            fs_diff: diff,
            recorded_subprocesses: recorded,
        })
    }
}

impl Drop for TestbenchSession {
    fn drop(&mut self) {
        if let Some(saved) = self.saved_egress_env.take() {
            restore_env("HARN_EGRESS_DEFAULT", saved.default);
            restore_env("HARN_EGRESS_ALLOW", saved.allow);
            restore_env("HARN_EGRESS_DENY", saved.deny);
            reset_egress_policy_for_host();
        }
        // The remaining `_clock`/`_overlay`/`_process` guards drop in
        // field-declared order, restoring the prior thread-local state.
    }
}

fn restore_env(key: &str, prior: Option<String>) {
    match prior {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

/// Outcome of a finalized testbench session — the artifacts the operator
/// inspects after a hermetic run.
#[derive(Debug, Default, Clone)]
pub struct TestbenchFinalize {
    pub fs_diff: Vec<overlay_fs::DiffEntry>,
    pub recorded_subprocesses: Vec<process_tape::TapeEntry>,
}

/// Errors surfaced when activating or finalizing a testbench session.
#[derive(Debug)]
pub enum TestbenchError {
    Subprocess(String),
}

impl std::fmt::Display for TestbenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Subprocess(msg) => write!(f, "testbench subprocess: {msg}"),
        }
    }
}

impl std::error::Error for TestbenchError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_clock_pins_now_ms_for_session_lifetime() {
        let bench = Testbench::builder()
            .paused_clock_at_ms(1_700_000_000_000)
            .build();
        let session = bench.activate().expect("activate");
        assert_eq!(crate::clock_mock::now_ms(), 1_700_000_000_000);
        crate::clock_mock::advance(std::time::Duration::from_secs(60));
        assert_eq!(crate::clock_mock::now_ms(), 1_700_000_060_000);
        drop(session);
        // After drop the override is gone; no assertion on real time.
        assert!(!crate::clock_mock::is_mocked());
    }

    #[test]
    fn deny_by_default_blocks_egress_until_drop() {
        // Lock state to avoid leaking env into other tests.
        let bench = Testbench::builder().deny_network().build();
        let session = bench.activate().expect("activate");
        assert_eq!(std::env::var("HARN_EGRESS_DEFAULT").as_deref(), Ok("deny"));
        drop(session);
        assert!(std::env::var("HARN_EGRESS_DEFAULT").is_err());
    }
}
