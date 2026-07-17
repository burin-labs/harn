//! `harn run --json`: NDJSON event-stream emitter.
//!
//! Each line is a [`JsonEnvelope`] wrapping a [`RunEventWire`]. Wire
//! events tag themselves with `event_type` for cheap discrimination
//! by `jq`-style consumers and carry a strictly monotonic `seq`
//! starting at `1`. See issue #1755 / epic #1753.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use harn_vm::run_events::{RunEvent, RunEventSink};
use serde::Serialize;

use crate::json_envelope::{JsonEnvelope, JsonError};

/// Schema version for the `harn run --json` event stream. Bump on any
/// breaking change to the wire shape; agents key off this to negotiate
/// compatibility.
pub const RUN_JSON_SCHEMA_VERSION: u32 = 1;

/// Wire form of a single event emitted by `harn run --json`. The
/// `event_type` tag is flat so consumers can `jq '.data.event_type'`.
/// `seq` is monotonic and process-local — the first event in a run is
/// `seq=1`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum RunEventWire {
    Stdout {
        seq: u64,
        payload: String,
    },
    Stderr {
        seq: u64,
        payload: String,
    },
    Transcript {
        seq: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        kind: String,
        payload: serde_json::Value,
    },
    ToolCall {
        seq: u64,
        call_id: String,
        name: String,
        args: serde_json::Value,
        started_at: String,
    },
    ToolResult {
        seq: u64,
        call_id: String,
        ok: bool,
        result: serde_json::Value,
    },
    Hook {
        seq: u64,
        name: String,
        phase: String,
        #[serde(skip_serializing_if = "serde_json::Value::is_null")]
        payload: serde_json::Value,
    },
    PersonaStage {
        seq: u64,
        persona: String,
        stage: String,
        transition: String,
    },
    PackRun {
        seq: u64,
        bundle_hash: String,
        signature_verified: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        key_id: Option<String>,
        cache_hit: bool,
        dry_run_verify: bool,
    },
    Result {
        seq: u64,
        value: serde_json::Value,
        exit_code: i32,
    },
    Error {
        seq: u64,
        error: JsonError,
    },
}

impl RunEventWire {
    /// The monotonic sequence number assigned at emission.
    pub fn seq(&self) -> u64 {
        match self {
            Self::Stdout { seq, .. }
            | Self::Stderr { seq, .. }
            | Self::Transcript { seq, .. }
            | Self::ToolCall { seq, .. }
            | Self::ToolResult { seq, .. }
            | Self::Hook { seq, .. }
            | Self::PersonaStage { seq, .. }
            | Self::PackRun { seq, .. }
            | Self::Result { seq, .. }
            | Self::Error { seq, .. } => *seq,
        }
    }
}

/// Writer that drains [`RunEvent`]s, assigns monotonic seq numbers,
/// wraps them in [`JsonEnvelope`]s, and emits one NDJSON line per
/// event. Lines are flushed per event so streaming consumers see them
/// as the run progresses.
pub struct NdjsonEmitter {
    inner: Arc<NdjsonEmitterInner>,
}

struct NdjsonEmitterInner {
    seq: AtomicU64,
    quiet: bool,
    /// Output sink. Behind a Mutex so concurrent emits stay
    /// line-atomic; serde line writes are tiny so contention is
    /// negligible.
    out: Mutex<Box<dyn Write + Send>>,
}

impl NdjsonEmitter {
    /// Build an emitter that writes to `out`. `quiet` suppresses
    /// `Stdout` and `Stderr` events (transcript/tool/hook/persona/
    /// result events still flow).
    pub fn new(out: Box<dyn Write + Send>, quiet: bool) -> Self {
        Self {
            inner: Arc::new(NdjsonEmitterInner {
                seq: AtomicU64::new(0),
                quiet,
                out: Mutex::new(out),
            }),
        }
    }

    /// Build a thread-safe sink that forwards [`RunEvent`]s into this
    /// emitter, applying seq numbering and `quiet` filtering.
    pub fn sink(&self) -> Arc<dyn RunEventSink> {
        Arc::new(NdjsonSink {
            inner: self.inner.clone(),
        })
    }

    /// Next monotonic seq value. The first event in a run is `seq=1`.
    fn next_seq(&self) -> u64 {
        self.inner.seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn write_envelope(inner: &NdjsonEmitterInner, event: RunEventWire) {
        let envelope = JsonEnvelope::ok(RUN_JSON_SCHEMA_VERSION, event);
        let line = serde_json::to_string(&envelope)
            .unwrap_or_else(|_| r#"{"schemaVersion":1,"ok":false}"#.to_string());
        if let Ok(mut out) = inner.out.lock() {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }

    /// Emit the terminal `Result` event for a run.
    pub fn emit_result(&self, value: serde_json::Value, exit_code: i32) {
        let event = RunEventWire::Result {
            seq: self.next_seq(),
            value,
            exit_code,
        };
        Self::write_envelope(&self.inner, event);
    }

    /// Emit the terminal `Error` event for a fatal run failure (e.g.
    /// compile error before the VM started).
    pub fn emit_error(&self, code: impl Into<String>, message: impl Into<String>) {
        let event = RunEventWire::Error {
            seq: self.next_seq(),
            error: JsonError {
                code: code.into(),
                message: message.into(),
                details: serde_json::Value::Null,
            },
        };
        Self::write_envelope(&self.inner, event);
    }
}

struct NdjsonSink {
    inner: Arc<NdjsonEmitterInner>,
}

impl RunEventSink for NdjsonSink {
    fn emit(&self, event: RunEvent) {
        let seq = self.inner.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let wire = match event {
            RunEvent::Stdout { payload } => {
                if self.inner.quiet {
                    // Undo the seq bump so monotonicity stays tight.
                    self.inner.seq.fetch_sub(1, Ordering::SeqCst);
                    return;
                }
                RunEventWire::Stdout { seq, payload }
            }
            RunEvent::Stderr { payload } => {
                if self.inner.quiet {
                    self.inner.seq.fetch_sub(1, Ordering::SeqCst);
                    return;
                }
                RunEventWire::Stderr { seq, payload }
            }
            RunEvent::Transcript {
                agent_id,
                kind,
                payload,
            } => RunEventWire::Transcript {
                seq,
                agent_id,
                kind,
                payload,
            },
            RunEvent::ToolCall {
                call_id,
                name,
                args,
                started_at,
            } => RunEventWire::ToolCall {
                seq,
                call_id,
                name,
                args,
                started_at,
            },
            RunEvent::ToolResult {
                call_id,
                ok,
                result,
            } => RunEventWire::ToolResult {
                seq,
                call_id,
                ok,
                result,
            },
            RunEvent::Hook {
                name,
                phase,
                payload,
            } => RunEventWire::Hook {
                seq,
                name,
                phase,
                payload,
            },
            RunEvent::PersonaStage {
                persona,
                stage,
                transition,
            } => RunEventWire::PersonaStage {
                seq,
                persona,
                stage,
                transition,
            },
            RunEvent::PackRun {
                bundle_hash,
                signature_verified,
                key_id,
                cache_hit,
                dry_run_verify,
            } => RunEventWire::PackRun {
                seq,
                bundle_hash,
                signature_verified,
                key_id,
                cache_hit,
                dry_run_verify,
            },
        };
        NdjsonEmitter::write_envelope(&self.inner, wire);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `harn_vm::run_events::install_sink` writes a process-global
    /// slot, so any test that swaps the sink must serialize against
    /// peers.
    static SINK_LOCK: Mutex<()> = Mutex::new(());

    struct BufWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn emits_monotonic_seq_across_events() {
        let _guard = SINK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let emitter = NdjsonEmitter::new(Box::new(BufWriter(buf.clone())), false);
        let sink = emitter.sink();
        let prior = harn_vm::run_events::install_sink(sink);
        harn_vm::run_events::emit(RunEvent::Stdout {
            payload: "hello\n".into(),
        });
        harn_vm::run_events::emit(RunEvent::Stderr {
            payload: "warn\n".into(),
        });
        harn_vm::run_events::clear_sink();
        emitter.emit_result(serde_json::Value::Null, 0);
        if let Some(prior) = prior {
            harn_vm::run_events::install_sink(prior);
        }

        let raw = String::from_utf8(buf.lock().unwrap().clone()).expect("utf8");
        let lines: Vec<&str> = raw.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(lines.len(), 3, "expected 3 NDJSON lines, got:\n{raw}");
        let seqs: Vec<u64> = lines
            .iter()
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).expect("valid json");
                v["data"]["seq"].as_u64().expect("seq present")
            })
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        let types: Vec<String> = lines
            .iter()
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).expect("valid json");
                v["data"]["event_type"].as_str().expect("type").to_string()
            })
            .collect();
        assert_eq!(types, vec!["stdout", "stderr", "result"]);
    }

    #[test]
    fn quiet_drops_stdout_and_stderr_without_gaps() {
        let _guard = SINK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let emitter = NdjsonEmitter::new(Box::new(BufWriter(buf.clone())), true);
        let sink = emitter.sink();
        let prior = harn_vm::run_events::install_sink(sink);
        harn_vm::run_events::emit(RunEvent::Stdout {
            payload: "ignored\n".into(),
        });
        harn_vm::run_events::emit(RunEvent::Hook {
            name: "PreRun".into(),
            phase: "allow".into(),
            payload: serde_json::Value::Null,
        });
        harn_vm::run_events::clear_sink();
        emitter.emit_result(serde_json::Value::Null, 0);
        if let Some(prior) = prior {
            harn_vm::run_events::install_sink(prior);
        }

        let raw = String::from_utf8(buf.lock().unwrap().clone()).expect("utf8");
        let lines: Vec<&str> = raw.lines().filter(|line| !line.is_empty()).collect();
        // stdout suppressed; hook + result remain.
        assert_eq!(lines.len(), 2, "raw:\n{raw}");
        let seqs: Vec<u64> = lines
            .iter()
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).expect("valid json");
                v["data"]["seq"].as_u64().expect("seq")
            })
            .collect();
        assert_eq!(
            seqs,
            vec![1, 2],
            "seq must stay contiguous after quiet filtering"
        );
    }

    // The sink is process-global, so this test must retain its synchronous
    // serialization guard for the whole async run. Keep it on one thread and
    // document the intentionally scoped exception instead of dropping the
    // guard and permitting a concurrent test to corrupt the event stream.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn explicit_exit_emits_stdio_and_one_result_event() {
        let _guard = SINK_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        harn_vm::reset_thread_local_state();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let script = temp.path().join("main.harn");
        std::fs::write(
            &script,
            r#"
fn main(harness: Harness) {
  harness.stdio.print("before ")
  harness.stdio.println("exit")
  harness.stdio.eprintln("diagnostic")
  exit(2)
}
"#,
        )
        .expect("write script");
        let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));

        let outcome = super::super::execute_run_json(
            &script.to_string_lossy(),
            false,
            std::collections::HashSet::new(),
            Vec::new(),
            Vec::new(),
            super::super::CliLlmMockMode::Off,
            None,
            super::super::RunProfileOptions::default(),
            Box::new(BufWriter(buffer.clone())),
            super::super::RunJsonOptions::default(),
        )
        .await;

        assert_eq!(outcome.exit_code, 2, "stderr:\n{}", outcome.stderr);
        let events: Vec<serde_json::Value> = String::from_utf8(buffer.lock().unwrap().clone())
            .expect("utf8")
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).expect("valid NDJSON event"))
            .collect();
        let stdout = events
            .iter()
            .filter(|event| event["data"]["event_type"] == "stdout")
            .map(|event| event["data"]["payload"].as_str().expect("stdout payload"))
            .collect::<String>();
        let stderr = events
            .iter()
            .filter(|event| event["data"]["event_type"] == "stderr")
            .map(|event| event["data"]["payload"].as_str().expect("stderr payload"))
            .collect::<String>();
        let terminal: Vec<&serde_json::Value> = events
            .iter()
            .filter(|event| event["data"]["event_type"] == "result")
            .collect();

        assert_eq!(stdout, "before exit\n");
        assert_eq!(stderr, "diagnostic\n");
        assert_eq!(terminal.len(), 1, "events: {events:#?}");
        assert_eq!(terminal[0]["data"]["exit_code"], 2);
        assert!(terminal[0]["data"]["value"].is_null());
        assert!(events
            .iter()
            .all(|event| event["data"]["event_type"] != "error"));
        harn_vm::reset_thread_local_state();
    }
}
