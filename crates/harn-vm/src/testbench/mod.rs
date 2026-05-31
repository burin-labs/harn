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

pub mod annotations;
pub mod fidelity;
pub mod mcp_mock;
pub mod overlay_fs;
pub mod process_tape;
pub mod tape;
#[cfg(feature = "testbench-wasi")]
pub mod wasi_process;

use std::path::PathBuf;
use std::sync::Arc;

use crate::clock_mock::leak_audit::{self, ClockLeak};
use crate::clock_mock::{install_override, ClockOverrideGuard, MockClock};
use crate::egress::reset_egress_policy_for_host;

use overlay_fs::{install_overlay, OverlayFs, OverlayFsGuard};
use process_tape::{install_process_tape, ProcessTape, ProcessTapeGuard, ProcessTapeMode};
use tape::{install_recorder, TapeHeader, TapeRecorder, TapeRecorderGuard};

/// Declarative configuration for [`Testbench::activate`]. Every axis is
/// optional so callers can compose only the surfaces they need.
#[derive(Debug, Default, Clone)]
pub struct Testbench {
    pub clock: ClockConfig,
    pub llm: LlmConfig,
    pub filesystem: FilesystemConfig,
    pub subprocess: SubprocessConfig,
    pub network: NetworkConfig,
    pub tape: TapeConfig,
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
    /// Resolve subprocess invocations against a directory of WASI
    /// (`wasm32-wasi`) modules. Each `program` resolves to
    /// `<dir>/<program>.wasm`; the module runs under wasmtime with the
    /// testbench's mock clock virtualized into `clock_time_get` and
    /// `poll_oneoff`. Calls whose program has no matching `.wasm` fall
    /// through to the native spawn path. Requires the `testbench-wasi`
    /// Cargo feature.
    WasiToolchain { dir: PathBuf },
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

/// Unified-tape configuration. Recording is opt-in: `Off` (the default)
/// installs nothing and pays nothing in production; `Emit { path }`
/// installs a [`tape::TapeRecorder`] consulted by every host-capability
/// axis, then persists the result to `path` (plus `path.cas/` for large
/// payloads) when [`TestbenchSession::finalize`] runs.
#[derive(Debug, Default, Clone)]
pub enum TapeConfig {
    #[default]
    Off,
    Emit {
        path: PathBuf,
        /// Argv forwarded to the script after `--`. Captured in the tape
        /// header so two tapes that differ only in argv are
        /// distinguishable.
        argv: Vec<String>,
        /// Path to the `.harn` script. Informational only; used to
        /// populate the tape header so consumers can attribute records.
        script_path: Option<String>,
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

    /// Use a directory of WASI modules as the subprocess source. See
    /// [`SubprocessConfig::WasiToolchain`].
    pub fn wasi_toolchain(mut self, dir: impl Into<PathBuf>) -> Self {
        self.bench.subprocess = SubprocessConfig::WasiToolchain { dir: dir.into() };
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

    pub fn emit_tape(mut self, path: impl Into<PathBuf>) -> Self {
        self.bench.tape = TapeConfig::Emit {
            path: path.into(),
            argv: Vec::new(),
            script_path: None,
        };
        self
    }

    pub fn emit_tape_for(
        mut self,
        path: impl Into<PathBuf>,
        script_path: Option<String>,
        argv: Vec<String>,
    ) -> Self {
        self.bench.tape = TapeConfig::Emit {
            path: path.into(),
            argv,
            script_path,
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
    _recorder: Option<TapeRecorderGuard>,
    process_tape: Option<Arc<ProcessTape>>,
    overlay: Option<Arc<OverlayFs>>,
    recorder: Option<Arc<TapeRecorder>>,
    tape_path: Option<PathBuf>,
    tape_started_at_unix_ms: Option<i64>,
    tape_script_path: Option<String>,
    tape_argv: Vec<String>,
    subprocess_mode: ProcessTapeMode,
    subprocess_tape_path: Option<PathBuf>,
    #[cfg(feature = "testbench-wasi")]
    _wasi_toolchain: Option<wasi_process::WasiToolchainGuard>,
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
        // Clear the leak audit so the session reports leaks it observed
        // rather than entries left behind by an earlier session that
        // never called `finalize` (e.g. a panicking test).
        leak_audit::reset();

        let (clock_guard, started_at_unix_ms) = match bench.clock {
            ClockConfig::Real => (None, None),
            ClockConfig::Paused { starting_at_ms } => (
                Some(install_override(MockClock::at_wall_ms(starting_at_ms))),
                Some(starting_at_ms),
            ),
        };

        // LLM state is *not* installed here — the caller owns the
        // CliLlmMockMode channel. Reading bench.llm just keeps the
        // declarative config visible to test inspection.
        #[allow(clippy::no_effect_underscore_binding)]
        let _llm_config = bench.llm;

        #[cfg(feature = "testbench-wasi")]
        let mut wasi_guard: Option<wasi_process::WasiToolchainGuard> = None;

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
                #[cfg(feature = "testbench-wasi")]
                SubprocessConfig::WasiToolchain { dir } => {
                    if !dir.exists() {
                        return Err(TestbenchError::Subprocess(format!(
                            "wasi toolchain directory does not exist: {}",
                            dir.display()
                        )));
                    }
                    wasi_guard = Some(wasi_process::install_wasi_toolchain(dir));
                    (None, None, ProcessTapeMode::Replay, None)
                }
                #[cfg(not(feature = "testbench-wasi"))]
                SubprocessConfig::WasiToolchain { .. } => {
                    return Err(TestbenchError::Subprocess(
                        "WasiToolchain requires the `testbench-wasi` Cargo feature".to_string(),
                    ));
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

        let (recorder, recorder_guard, tape_path, tape_argv, tape_script_path) = match bench.tape {
            TapeConfig::Off => (None, None, None, Vec::new(), None),
            TapeConfig::Emit {
                path,
                argv,
                script_path,
            } => {
                let recorder = Arc::new(TapeRecorder::new());
                let guard = install_recorder(Arc::clone(&recorder));
                (
                    Some(Arc::clone(&recorder)),
                    Some(guard),
                    Some(path),
                    argv,
                    script_path,
                )
            }
        };

        Ok(Self {
            _clock: clock_guard,
            _process: process_guard,
            _overlay: overlay_guard,
            _recorder: recorder_guard,
            process_tape,
            overlay,
            recorder,
            tape_path,
            tape_started_at_unix_ms: started_at_unix_ms,
            tape_script_path,
            tape_argv,
            subprocess_mode,
            subprocess_tape_path,
            #[cfg(feature = "testbench-wasi")]
            _wasi_toolchain: wasi_guard,
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

    /// Reference to the active tape recorder (if any).
    pub fn tape_recorder(&self) -> Option<&Arc<TapeRecorder>> {
        self.recorder.as_ref()
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
        let mut emitted_tape = None;
        if let (Some(recorder), Some(path)) = (self.recorder.as_ref(), self.tape_path.as_ref()) {
            let header = TapeHeader::current(
                self.tape_started_at_unix_ms,
                self.tape_script_path.clone(),
                self.tape_argv.clone(),
            );
            let tape = recorder.snapshot(header);
            tape.persist(path).map_err(TestbenchError::Tape)?;
            emitted_tape = Some(EmittedTape {
                path: path.clone(),
                records: tape.records.len(),
            });
        }
        // Drain the leak audit last so anything emitted while we
        // serialized other artifacts (e.g. tape persistence reading the
        // wall clock for timestamps it shouldn't be reading) is still
        // captured in this session's report.
        let clock_leaks = leak_audit::drain();
        // The Drop impl undoes mocks regardless of finalize success.
        Ok(TestbenchFinalize {
            fs_diff: diff,
            recorded_subprocesses: recorded,
            tape: emitted_tape,
            clock_leaks,
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
    pub tape: Option<EmittedTape>,
    /// Capabilities that observed real wall-clock or monotonic time
    /// during the session. Empty under a hermetic run; non-empty entries
    /// are fidelity hazards the operator should investigate or migrate
    /// off of direct host-clock reads.
    pub clock_leaks: Vec<ClockLeak>,
}

/// Summary metadata for a unified tape that was emitted at finalize-time.
#[derive(Debug, Clone)]
pub struct EmittedTape {
    pub path: PathBuf,
    pub records: usize,
}

/// Errors surfaced when activating or finalizing a testbench session.
#[derive(Debug)]
pub enum TestbenchError {
    Subprocess(String),
    Tape(String),
}

impl std::fmt::Display for TestbenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Subprocess(msg) => write!(f, "testbench subprocess: {msg}"),
            Self::Tape(msg) => write!(f, "testbench tape: {msg}"),
        }
    }
}

impl std::error::Error for TestbenchError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests in this module mutate process-global state (env vars, the
    /// leak audit registry) and must run one at a time even though
    /// `cargo test` defaults to parallel execution. We share
    /// [`leak_audit::TEST_LOCK`] so the audit module's own tests
    /// serialize with the testbench's tests against the same registry.
    fn serial<F: FnOnce()>(body: F) {
        let _guard = leak_audit::TEST_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        body();
    }

    #[test]
    fn paused_clock_pins_now_ms_for_session_lifetime() {
        serial(|| {
            let bench = Testbench::builder()
                .paused_clock_at_ms(1_700_000_000_000)
                .build();
            let session = bench.activate().expect("activate");
            assert_eq!(crate::clock_mock::now_ms(), 1_700_000_000_000);
            crate::clock_mock::advance(std::time::Duration::from_mins(1));
            assert_eq!(crate::clock_mock::now_ms(), 1_700_000_060_000);
            drop(session);
            // After drop the override is gone; no assertion on real time.
            assert!(!crate::clock_mock::is_mocked());
        });
    }

    #[test]
    fn deny_by_default_blocks_egress_until_drop() {
        serial(|| {
            let bench = Testbench::builder().deny_network().build();
            let session = bench.activate().expect("activate");
            assert_eq!(std::env::var("HARN_EGRESS_DEFAULT").as_deref(), Ok("deny"));
            drop(session);
            assert!(std::env::var("HARN_EGRESS_DEFAULT").is_err());
        });
    }

    #[test]
    fn finalize_surfaces_clock_leaks_for_contrived_capability() {
        serial(|| {
            let bench = Testbench::builder()
                .paused_clock_at_ms(1_700_000_000_000)
                .build();
            let session = bench.activate().expect("activate");

            // Contrived "leaky" capability: routes through the audit shim
            // while a paused mock is installed. Production callers (e.g.
            // `stdlib/date_iso`) follow the exact same pattern.
            let _ = leak_audit::wall_now("test/contrived_leak");
            let _ = leak_audit::instant_now("test/contrived_instant");
            let _ = leak_audit::wall_now("test/contrived_leak");

            let finalize = session.finalize().expect("finalize");
            let by_id: std::collections::BTreeMap<&str, &ClockLeak> = finalize
                .clock_leaks
                .iter()
                .map(|leak| (leak.capability_id.as_str(), leak))
                .collect();
            let wall = by_id
                .get("test/contrived_leak")
                .expect("wall leak surfaced");
            assert_eq!(wall.count, 2);
            let inst = by_id
                .get("test/contrived_instant")
                .expect("instant leak surfaced");
            assert_eq!(inst.count, 1);

            // Drain semantics: a fresh session sees no carry-over.
            let next_session = Testbench::builder()
                .paused_clock_at_ms(1_700_000_000_000)
                .build()
                .activate()
                .expect("activate next");
            let next = next_session.finalize().expect("finalize next");
            assert!(next.clock_leaks.is_empty());
        });
    }

    #[test]
    fn audit_quiet_when_no_mock_is_active() {
        serial(|| {
            leak_audit::reset();
            // No `Testbench` activated → no mock clock → no leak entries
            // even when the helpers are called.
            let _ = leak_audit::wall_now("test/no_mock");
            let _ = leak_audit::instant_now("test/no_mock");
            assert!(leak_audit::snapshot().is_empty());
        });
    }
}
