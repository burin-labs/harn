//! Subprocess record/replay tape for the testbench.
//!
//! `ProcessTape` is a thread-local override consulted by
//! [`crate::stdlib::sandbox::command_output`] before it spawns. In record
//! mode the subprocess actually runs, output is captured, and a tape
//! entry is appended. In replay mode the (program, args, cwd) tuple is
//! looked up in the loaded tape and the recorded output is returned —
//! no real OS spawn, and the unified mock clock is advanced by the
//! recorded duration so script-observed time stays consistent.
//!
//! ## What the tape does NOT cover
//!
//! Subprocesses cannot observe the testbench's mock clock — the kernel
//! always reads real wall time. The tape captures the *parent-observed*
//! duration and replays it into the parent's clock, but a script that
//! depends on a subprocess' internal timing will see real wall-clock
//! time. Full virtualization is the WASI-mediated subprocess child
//! issue — see `docs/src/dev/testbench.md`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{ExitStatus, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::clock_mock;
use crate::testbench::tape::{self, TapeRecordKind};

/// Whether the active tape is recording new entries or replaying an
/// existing tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTapeMode {
    Record,
    Replay,
}

/// One captured (or to-be-replayed) subprocess invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TapeEntry {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Reserved for future env-var matching. Currently unpopulated;
    /// invocations match on `(program, args, cwd)` only.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub exit_code: i32,
    /// Duration the parent observed via the *unified mock clock*. In
    /// record mode this is the wall-clock duration the subprocess took.
    /// Replay advances the testbench clock by this delta.
    #[serde(default)]
    pub duration_ms: u64,
}

#[derive(Debug)]
pub struct ProcessTape {
    mode: ProcessTapeMode,
    entries: Vec<TapeEntry>,
    cursor: AtomicUsize,
    captured: Mutex<Vec<TapeEntry>>,
}

impl ProcessTape {
    pub fn recording() -> Self {
        Self {
            mode: ProcessTapeMode::Record,
            entries: Vec::new(),
            cursor: AtomicUsize::new(0),
            captured: Mutex::new(Vec::new()),
        }
    }

    pub fn replay_from(entries: Vec<TapeEntry>) -> Self {
        Self {
            mode: ProcessTapeMode::Replay,
            entries,
            cursor: AtomicUsize::new(0),
            captured: Mutex::new(Vec::new()),
        }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let body = std::fs::read_to_string(path)
            .map_err(|err| format!("read {}: {err}", path.display()))?;
        let entries = if body.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str::<Vec<TapeEntry>>(&body)
                .map_err(|err| format!("parse {}: {err}", path.display()))?
        };
        Ok(Self::replay_from(entries))
    }

    pub fn persist(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| format!("mkdir {}: {err}", parent.display()))?;
            }
        }
        let recorded = self.recorded();
        let body = serde_json::to_string_pretty(&recorded)
            .map_err(|err| format!("serialize tape: {err}"))?;
        std::fs::write(path, body).map_err(|err| format!("write {}: {err}", path.display()))
    }

    pub fn mode(&self) -> ProcessTapeMode {
        self.mode
    }

    pub fn recorded(&self) -> Vec<TapeEntry> {
        self.captured
            .lock()
            .expect("process tape captured mutex poisoned")
            .clone()
    }

    /// Whether every replay entry has been consumed.
    pub fn fully_consumed(&self) -> bool {
        self.cursor.load(Ordering::SeqCst) >= self.entries.len()
    }

    fn record_entry(&self, entry: TapeEntry) {
        self.captured
            .lock()
            .expect("process tape captured mutex poisoned")
            .push(entry);
    }

    fn next_replay(&self, expected: &TapeEntry) -> Result<TapeEntry, String> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        let candidate = self.entries.get(idx).cloned().ok_or_else(|| {
            format!(
                "process tape exhausted at call #{idx}: program={:?} args={:?}",
                expected.program, expected.args
            )
        })?;
        if !invocation_matches(expected, &candidate) {
            return Err(format!(
                "process tape diverged at call #{idx}: expected {:?}({:?}) cwd={:?}, tape has {:?}({:?}) cwd={:?}",
                expected.program,
                expected.args,
                expected.cwd,
                candidate.program,
                candidate.args,
                candidate.cwd
            ));
        }
        Ok(candidate)
    }
}

fn invocation_matches(expected: &TapeEntry, recorded: &TapeEntry) -> bool {
    expected.program == recorded.program
        && expected.args == recorded.args
        && expected.cwd == recorded.cwd
}

thread_local! {
    static ACTIVE_TAPE: RefCell<Option<Arc<ProcessTape>>> = const { RefCell::new(None) };
}

pub struct ProcessTapeGuard {
    previous: Option<Arc<ProcessTape>>,
}

impl Drop for ProcessTapeGuard {
    fn drop(&mut self) {
        let prev = self.previous.take();
        ACTIVE_TAPE.with(|slot| {
            *slot.borrow_mut() = prev;
        });
    }
}

pub fn install_process_tape(tape: Arc<ProcessTape>) -> ProcessTapeGuard {
    let previous = ACTIVE_TAPE.with(|slot| slot.replace(Some(tape)));
    ProcessTapeGuard { previous }
}

pub fn active_tape() -> Option<Arc<ProcessTape>> {
    ACTIVE_TAPE.with(|slot| slot.borrow().clone())
}

/// If a tape is active, intercept the spawn. Returns `Some(output)` when
/// the call was satisfied by the tape; `None` means the caller should
/// spawn a real subprocess. In record mode this returns `None` (the real
/// subprocess will be spawned by the caller, and a follow-up call to
/// [`start_recording`] / [`RecordingSpan::finish`] should append the
/// entry to the tape).
///
/// When a WASI toolchain is active and `<toolchain>/<program>.wasm`
/// exists, the WASM module is run under wasmtime (with the testbench
/// clock virtualized into `clock_time_get` and `poll_oneoff`) and its
/// output is returned without consulting the process tape. WASI hits
/// take precedence over native record/replay so a partial `.wasm`
/// toolchain can coexist with a recorded native tape — wasm-backed
/// programs route through wasmtime, the rest hit the tape or host.
pub fn intercept_spawn(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> Option<Result<Output, String>> {
    #[cfg(feature = "testbench-wasi")]
    if let Some(intercepted) = wasi_intercept(program, args, cwd) {
        return Some(intercepted);
    }

    let tape = active_tape()?;
    if matches!(tape.mode(), ProcessTapeMode::Record) {
        return None;
    }
    let expected = TapeEntry {
        program: program.to_string(),
        args: args.to_vec(),
        cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
        env: BTreeMap::new(),
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
        duration_ms: 0,
    };
    Some(match tape.next_replay(&expected) {
        Ok(entry) => {
            if entry.duration_ms > 0 {
                clock_mock::advance(Duration::from_millis(entry.duration_ms));
            }
            record_unified_spawn(&entry);
            Ok(synthesize_output(&entry))
        }
        Err(err) => Err(err),
    })
}

#[cfg(feature = "testbench-wasi")]
fn wasi_intercept(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> Option<Result<Output, String>> {
    use crate::testbench::wasi_process;
    let module = wasi_process::wasi_module_for(program)?;
    let env: Vec<(String, String)> = Vec::new();
    let result = wasi_process::run_wasm_module(&module, args, &env);
    Some(result.map(|out| {
        let entry = TapeEntry {
            program: program.to_string(),
            args: args.to_vec(),
            cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
            env: BTreeMap::new(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            exit_code: out.exit_code,
            duration_ms: out.virtual_duration_ms,
        };
        record_unified_spawn(&entry);
        Output {
            status: synthesize_status(out.exit_code),
            stdout: out.stdout,
            stderr: out.stderr,
        }
    }))
}

fn record_unified_spawn(entry: &TapeEntry) {
    tape::with_active_recorder(|recorder| {
        let stdout_payload = recorder.payload_from_bytes(entry.stdout.as_bytes().to_vec());
        let stderr_payload = recorder.payload_from_bytes(entry.stderr.as_bytes().to_vec());
        Some(TapeRecordKind::ProcessSpawn {
            program: entry.program.clone(),
            args: entry.args.clone(),
            cwd: entry.cwd.clone(),
            exit_code: entry.exit_code,
            duration_ms: entry.duration_ms,
            stdout_payload,
            stderr_payload,
        })
    });
}

/// Begin recording a subprocess invocation. Returns `Some(span)` when a
/// tape is in record mode; `None` otherwise. The span captures the
/// invocation's start time on the injected clock so [`RecordingSpan::finish`]
/// can stamp the elapsed delta deterministically — under a paused mock
/// clock the recording is virtual time, matching what replay will
/// advance.
pub fn start_recording(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> Option<RecordingSpan> {
    let tape = active_tape()?;
    if !matches!(tape.mode(), ProcessTapeMode::Record) {
        return None;
    }
    Some(RecordingSpan {
        tape,
        program: program.to_string(),
        args: args.to_vec(),
        cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
        started_at: clock_mock::instant_now(),
    })
}

/// Pending tape entry for a subprocess invocation that is currently
/// running. Stamping the elapsed time happens at [`RecordingSpan::finish`]
/// using the unified mock clock, so testbench callers see virtual time
/// in their tapes.
pub struct RecordingSpan {
    tape: Arc<ProcessTape>,
    program: String,
    args: Vec<String>,
    cwd: Option<String>,
    started_at: clock_mock::ClockInstant,
}

impl RecordingSpan {
    pub fn finish(self, output: &Output) {
        let duration = clock_mock::instant_now().duration_since(self.started_at);
        let entry = TapeEntry {
            program: self.program,
            args: self.args,
            cwd: self.cwd,
            env: BTreeMap::new(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
            duration_ms: duration.as_millis().min(u64::MAX as u128) as u64,
        };
        record_unified_spawn(&entry);
        self.tape.record_entry(entry);
    }
}

#[cfg(unix)]
fn synthesize_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw((code & 0xff) << 8)
}

#[cfg(windows)]
fn synthesize_status(code: i32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(code as u32)
}

fn synthesize_output(entry: &TapeEntry) -> Output {
    Output {
        status: synthesize_status(entry.exit_code),
        stdout: entry.stdout.as_bytes().to_vec(),
        stderr: entry.stderr.as_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Output;

    #[test]
    fn replay_emits_recorded_output_and_advances_clock() {
        let tape = ProcessTape::replay_from(vec![TapeEntry {
            program: "git".to_string(),
            args: vec!["status".to_string()],
            cwd: Some("/tmp/repo".to_string()),
            env: BTreeMap::new(),
            stdout: "On branch main\n".to_string(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 250,
        }]);
        let _guard = install_process_tape(Arc::new(tape));
        let _clock = clock_mock::install_override(clock_mock::MockClock::at_wall_ms(1_000_000));
        let before = clock_mock::now_ms();
        let intercepted =
            intercept_spawn("git", &["status".to_string()], Some(Path::new("/tmp/repo")))
                .expect("tape produced output");
        let output = intercepted.expect("replay succeeded");
        let after = clock_mock::now_ms();
        assert_eq!(output.stdout, b"On branch main\n");
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(after - before, 250);
    }

    #[test]
    fn replay_diverges_when_program_does_not_match() {
        let tape = ProcessTape::replay_from(vec![TapeEntry {
            program: "git".to_string(),
            args: vec!["status".to_string()],
            cwd: None,
            env: BTreeMap::new(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 0,
        }]);
        let _guard = install_process_tape(Arc::new(tape));
        let result = intercept_spawn("gh", &["pr".to_string()], None)
            .expect("tape active")
            .expect_err("divergence reported");
        assert!(result.contains("diverged"), "{result}");
    }

    #[test]
    fn record_mode_appends_completed_entries() {
        let tape = Arc::new(ProcessTape::recording());
        let _guard = install_process_tape(Arc::clone(&tape));
        // Drive the recording span under a paused mock clock so the
        // captured duration is deterministic — proves the duration
        // capture honors the injected clock, not wall time.
        let _clock = clock_mock::install_override(clock_mock::MockClock::at_wall_ms(0));
        let span = start_recording("echo", &["hi".to_string()], None).expect("recording active");
        clock_mock::advance(Duration::from_millis(7));
        span.finish(&Output {
            status: synthesize_status(0),
            stdout: b"hi\n".to_vec(),
            stderr: Vec::new(),
        });
        let recorded = tape.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].stdout, "hi\n");
        assert_eq!(recorded[0].duration_ms, 7);
    }

    #[test]
    fn replay_exhausted_surfaces_clear_error() {
        let tape = ProcessTape::replay_from(Vec::new());
        let _guard = install_process_tape(Arc::new(tape));
        let result = intercept_spawn("git", &[], None)
            .expect("tape active")
            .expect_err("exhausted");
        assert!(result.contains("exhausted"), "{result}");
    }
}
