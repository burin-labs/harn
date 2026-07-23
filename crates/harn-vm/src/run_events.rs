//! Run-event sink: an execution-scoped bus the CLI attaches to capture every
//! observable side effect of a `harn run` invocation as a single
//! ordered stream.
//!
//! Concrete sinks live in `harn-cli` (`harn run --json` writes them as
//! NDJSON). The VM only knows it should call [`emit`] from a handful of
//! observability checkpoints; sinks fan-out from there.
//!
//! Variants intentionally mirror the surface area of the run command
//! rather than the on-disk event log. Stdout/stderr writes are captured
//! here because they never enter the event log; transcript / persona /
//! hook / tool events are forwarded here in addition to their existing
//! persistent topics so a single subscriber can see the whole run
//! without joining across topics.

use std::cell::RefCell;
use std::future::Future;
use std::sync::Arc;

use serde::Serialize;

/// One observable event from a running pipeline. Variants are
/// `#[serde(tag = "event_type")]` so wire consumers (notably the CLI
/// `--json` NDJSON stream) can discriminate without inspecting the
/// payload shape.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum RunEvent {
    /// Bytes written to stdout (raw, including any trailing newlines).
    Stdout { payload: String },
    /// Bytes written to stderr (raw, including any trailing newlines).
    Stderr { payload: String },
    /// One append on a transcript stream (`agent.transcript.llm` topic).
    /// `kind` mirrors the transcript entry's `type` field.
    Transcript {
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        kind: String,
        payload: serde_json::Value,
    },
    /// A model-issued tool call. `call_id` matches the transcript
    /// `call_id`; agents reconcile [`Self::ToolCall`] /
    /// [`Self::ToolResult`] pairs by it.
    ToolCall {
        call_id: String,
        name: String,
        args: serde_json::Value,
        /// RFC 3339 timestamp captured at emission.
        started_at: String,
    },
    /// Outcome of a tool call.
    ToolResult {
        call_id: String,
        ok: bool,
        result: serde_json::Value,
    },
    /// A workflow hook fired during the run.
    Hook {
        name: String,
        phase: String,
        #[serde(skip_serializing_if = "serde_json::Value::is_null")]
        payload: serde_json::Value,
    },
    /// A persona-stage transition. Mirrors the `persona.runtime.events`
    /// topic; the `transition` field captures the stage state change
    /// (`"started"`, `"completed"`, `"handoff"`, ...).
    PersonaStage {
        persona: String,
        stage: String,
        transition: String,
    },
    /// `harn run <bundle.harnpack>` resolved a pack to execute. Carries
    /// the verified bundle hash, whether the embedded Ed25519 signature
    /// verified end-to-end, the signing key fingerprint (when signed),
    /// and whether the unpacked archive came from the content-addressed
    /// cache or was extracted fresh on this run.
    PackRun {
        bundle_hash: String,
        signature_verified: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        key_id: Option<String>,
        cache_hit: bool,
        dry_run_verify: bool,
    },
}

/// Receiver of [`RunEvent`]s. Implementations must be cheap (the VM
/// calls [`emit`] on hot paths like every `println`).
pub trait RunEventSink: Send + Sync {
    fn emit(&self, event: RunEvent);
}

thread_local! {
    /// The sink for the execution currently being polled on this thread.
    ///
    /// `AmbientExecutionScope` swaps this slot at every task poll, so the
    /// thread-local is execution-owned even when unrelated futures interleave
    /// on one runtime thread.
    static RUN_EVENT_SINK_CONTEXT: RefCell<Option<Arc<dyn RunEventSink>>> =
        RefCell::new(None);
}

pub(crate) fn swap_run_event_sink(
    sink: Option<Arc<dyn RunEventSink>>,
) -> Option<Arc<dyn RunEventSink>> {
    RUN_EVENT_SINK_CONTEXT.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), sink))
}

/// Run `inner` with `sink` attached to its ambient execution scope.
///
/// The scope is installed only while `inner` is being polled. Inline and
/// spawned VM work that captures the ambient execution inherits the sink;
/// unrelated or sibling executions do not.
pub fn scope<F: Future>(sink: Arc<dyn RunEventSink>, inner: F) -> impl Future<Output = F::Output> {
    crate::orchestration::scope_run_event_sink(sink, inner)
}

/// Whether the current execution has a sink. Useful as a fast-path gate
/// for callers that would otherwise build a payload speculatively.
pub fn sink_active() -> bool {
    RUN_EVENT_SINK_CONTEXT.with(|slot| slot.borrow().is_some())
}

/// Emit `event` to the current execution's sink. No-op when no sink is active,
/// so it is safe to call from every hook point unconditionally.
pub fn emit(event: RunEvent) {
    let sink = RUN_EVENT_SINK_CONTEXT.with(|slot| slot.borrow().clone());
    if let Some(sink) = sink {
        sink.emit(redact_run_event(event));
    }
}

/// Scrub the secret-bearing JSON field of a `RunEvent` once, centrally, before
/// it reaches any sink — so emitters can't forget (the `Hook` payload did, while
/// the transcript/tool variants were only clean because `agent_observe`
/// pre-scrubbed them). A second idempotent pass over a pre-scrubbed payload is
/// cheap and makes the bus correct-by-construction for every future variant.
/// `Stdout`/`Stderr` are the program's own raw output and pass through
/// unredacted; they also take the fast path (no policy lookup) since `emit` runs
/// on every `println`.
fn redact_run_event(mut event: RunEvent) -> RunEvent {
    let payload = match &mut event {
        RunEvent::Transcript { payload, .. } => payload,
        RunEvent::ToolCall { args, .. } => args,
        RunEvent::ToolResult { result, .. } => result,
        RunEvent::Hook { payload, .. } => payload,
        RunEvent::Stdout { .. }
        | RunEvent::Stderr { .. }
        | RunEvent::PersonaStage { .. }
        | RunEvent::PackRun { .. } => return event,
    };
    crate::redact::current_policy().redact_json_in_place(payload);
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct CapturingSink {
        events: Mutex<Vec<RunEvent>>,
    }

    impl RunEventSink for CapturingSink {
        fn emit(&self, event: RunEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn nested_scopes_restore_the_outer_sink() {
        let outer = Arc::new(CapturingSink {
            events: Mutex::new(Vec::new()),
        });
        let inner = Arc::new(CapturingSink {
            events: Mutex::new(Vec::new()),
        });

        scope(outer.clone(), async {
            emit(RunEvent::Stdout {
                payload: "outer-before\n".into(),
            });
            crate::orchestration::scope_inline_subtask(async {
                emit(RunEvent::Stdout {
                    payload: "outer-inline\n".into(),
                });
            })
            .await;
            let inherited = crate::orchestration::AmbientExecutionScope::capture_inherited();
            crate::orchestration::scope_ambient(inherited, async {
                emit(RunEvent::Stdout {
                    payload: "outer-worker\n".into(),
                });
            })
            .await;
            scope(inner.clone(), async {
                emit(RunEvent::Stdout {
                    payload: "inner\n".into(),
                });
            })
            .await;
            emit(RunEvent::Stdout {
                payload: "outer-after\n".into(),
            });
        })
        .await;

        assert!(!sink_active());
        emit(RunEvent::Stdout {
            payload: "unscoped\n".into(),
        });

        let payloads = |sink: &CapturingSink| {
            sink.events
                .lock()
                .unwrap()
                .iter()
                .map(|event| match event {
                    RunEvent::Stdout { payload } => payload.clone(),
                    other => panic!("unexpected event {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            payloads(&outer),
            [
                "outer-before\n",
                "outer-inline\n",
                "outer-worker\n",
                "outer-after\n"
            ]
        );
        assert_eq!(payloads(&inner), ["inner\n"]);
    }
}
