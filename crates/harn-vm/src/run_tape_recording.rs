//! Recording complete tool dispatches and their observed state changes.

use super::{
    overlap, process_tape, record_payload_redaction, transcript, ProcessTape, RecordedToolResult,
    RecordingLevel, RECORDING_FULL,
};
use crate::testbench::tape::{content_hash, FileEffect, TapeRecordKind, TranscriptChange};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// An in-flight tool call on the recording side.
pub(crate) struct ToolCallRecording {
    tape: Arc<ProcessTape>,
    dispatch: overlap::Dispatch,
    call_id: String,
    tool: String,
    args_digest: String,
    after_calls: u64,
    store_mutation_start: u64,
    watch: EffectWatch,
    pub(crate) transcript_changes: Vec<TranscriptChange>,
    pub(crate) store_changes: Vec<crate::store::StoreChange>,
    unservable: Option<String>,
}

enum EffectWatch {
    Scanned(String),
    ReadOnly,
    Unobserved,
}

/// Begin recording a tool call. `None` when no run tape is recording. A tool
/// declared read-only skips the two workspace scans; its effect set is empty.
pub(crate) fn begin_tool_call(
    call_id: &str,
    tool: &str,
    args: &serde_json::Value,
    read_only: bool,
) -> Option<ToolCallRecording> {
    let tape = process_tape()?;
    let dispatch = tape.dispatches.begin();
    let store_mutation_start = tape.store_mutations.load(Ordering::SeqCst);
    let watch = match (tape.level, &tape.workspace) {
        (RecordingLevel::Full, _) if read_only => EffectWatch::ReadOnly,
        (RecordingLevel::Full, Some(workspace)) => {
            static NEXT_SCAN: AtomicU64 = AtomicU64::new(0);
            let key = if call_id.is_empty() {
                format!("script-run#{}", NEXT_SCAN.fetch_add(1, Ordering::Relaxed))
            } else {
                call_id.to_string()
            };
            workspace.observer.begin(&workspace.root, &key);
            EffectWatch::Scanned(key)
        }
        _ => EffectWatch::Unobserved,
    };
    Some(ToolCallRecording {
        dispatch,
        after_calls: tape.next_llm_call.load(Ordering::SeqCst),
        store_mutation_start,
        tape,
        call_id: call_id.to_string(),
        tool: tool.to_string(),
        args_digest: args_digest(args),
        watch,
        transcript_changes: Vec::new(),
        store_changes: Vec::new(),
        unservable: None,
    })
}

/// Run a live tool dispatch, keeping each transcript change it makes on
/// `recording`.
pub(crate) async fn capture_transcript_changes<F: std::future::Future>(
    recording: &mut Option<ToolCallRecording>,
    live: F,
) -> F::Output {
    let Some(recording) = recording.as_mut() else {
        return live.await;
    };
    let ((output, changes, unservable), store_changes) =
        crate::store::capture_dispatch(transcript::capture(live)).await;
    recording.transcript_changes = changes;
    recording.store_changes = store_changes;
    recording.unservable = unservable;
    output
}

pub(crate) fn note_store_mutation() {
    if RECORDING_FULL.load(Ordering::Acquire) {
        if let Some(tape) = process_tape() {
            tape.store_mutations.fetch_add(1, Ordering::SeqCst);
        }
    }
}

pub(crate) fn args_digest(args: &serde_json::Value) -> String {
    content_hash(&crate::canonical_json::to_vec(args))
}

/// Finish recording a tool call. `Err` names why the dispatch result has no
/// faithful recorded shape; a branch then refuses to serve the call.
pub(crate) fn finish_tool_call(
    recording: ToolCallRecording,
    outcome: Result<RecordedToolResult, String>,
) {
    let ToolCallRecording {
        tape,
        dispatch,
        call_id,
        tool,
        args_digest,
        after_calls,
        store_mutation_start,
        watch,
        transcript_changes,
        store_changes,
        unservable,
    } = recording;
    let recorder = &tape.recorder;
    let full = tape.level == RecordingLevel::Full;
    let mut unservable = if full {
        unservable.or_else(|| transcript::validate_lifetimes(&transcript_changes).err())
    } else {
        None
    };
    if full
        && tape
            .store_mutations
            .load(Ordering::SeqCst)
            .saturating_sub(store_mutation_start)
            != store_changes.len() as u64
    {
        unservable.get_or_insert_with(|| {
            "runtime store mutations occurred outside the recorded dispatch context".to_string()
        });
    }
    let mut effects = Vec::new();
    match watch {
        EffectWatch::Scanned(key) => {
            let workspace = tape.workspace.as_ref().expect("a scan implies a workspace");
            let scan = workspace.observer.end(&workspace.root, &key);
            if scan.incomplete {
                unservable = Some("the file-effect scan was incomplete".to_string());
            }
            for (path, content) in scan.changes {
                if content.as_ref().is_some_and(|bytes| {
                    let text = String::from_utf8_lossy(bytes);
                    crate::redact::current_policy().redact_string(&text) != text
                }) {
                    record_payload_redaction();
                    unservable = Some("a file effect contains redacted data".to_string());
                    continue;
                }
                effects.push(FileEffect {
                    path,
                    content: content.map(|bytes| recorder.payload_cas(bytes)),
                });
            }
        }
        EffectWatch::ReadOnly => {}
        EffectWatch::Unobserved if full => {
            unservable = Some("no workspace effect observer was installed".to_string());
        }
        EffectWatch::Unobserved => {}
    }
    if dispatch.overlapped() {
        unservable.get_or_insert_with(|| {
            "overlapping tool dispatches have ambiguous effects and cannot be replayed".to_string()
        });
    }
    if tape.next_llm_call.load(Ordering::SeqCst) != after_calls {
        unservable.get_or_insert_with(|| {
            "model calls occurred during the tool dispatch; nested inference cannot be replayed"
                .to_string()
        });
    }
    let outcome_bytes = match &outcome {
        Ok(outcome) if full => serde_json::to_vec(outcome).unwrap_or_default(),
        Ok(_) => Vec::new(),
        // A metadata tape keeps no tool text, error text included.
        Err(reason) if full => {
            unservable.get_or_insert_with(|| reason.clone());
            Vec::new()
        }
        Err(_) => {
            unservable.get_or_insert_with(|| "unrecordable result".to_string());
            Vec::new()
        }
    };
    let outcome = recorder.payload_from_bytes(outcome_bytes);
    let store_changes = full.then(|| {
        recorder.payload_from_bytes(
            serde_json::to_vec(&store_changes).expect("runtime store changes serialize"),
        )
    });
    recorder.record(TapeRecordKind::ToolCall {
        call_id,
        tool,
        args_digest,
        after_calls,
        outcome,
        effects,
        unservable,
        transcript_changes: if full { transcript_changes } else { Vec::new() },
        store_changes,
    });
}
