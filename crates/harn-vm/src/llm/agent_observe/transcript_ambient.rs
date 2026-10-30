//! Ambient transcript routing and per-scope deduplication state.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::llm::served_lineage::LineageHead;

thread_local! {
    /// Last-emitted hashes for the current transcript. These avoid writing
    /// identical prompt and schema payloads once per request.
    static LAST_SYSTEM_PROMPT_HASH: RefCell<Option<u64>> = const { RefCell::new(None) };
    static LAST_CONTEXT_MANIFEST_HASH: RefCell<Option<u64>> = const { RefCell::new(None) };
    static LAST_TOOL_SCHEMAS_HASH: RefCell<Option<u64>> = const { RefCell::new(None) };
    /// Content-addressed LLM capability snapshots already written to the
    /// active transcript. Unlike prompt/schema state, template renders carry
    /// an explicit snapshot reference, so A/B/A may safely emit A only once.
    static EMITTED_CAPABILITY_SNAPSHOT_IDS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    /// Content-addressed provider-visible messages already defined in the
    /// active transcript. Request receipts carry these ids in served order.
    static EMITTED_SERVED_MESSAGE_IDS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    /// Content-addressed output schemas already defined in the active
    /// transcript. Request receipts carry these ids as schema hashes.
    static EMITTED_OUTPUT_SCHEMA_IDS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    /// Last persisted message-lineage receipt per session in the active
    /// sinks. The next receipt for that session continues it when its served
    /// messages extend it.
    static SERVED_LINEAGE_HEADS: RefCell<BTreeMap<Option<String>, LineageHead>> = const { RefCell::new(BTreeMap::new()) };
    static TRANSCRIPT_DIR_STACK: RefCell<Vec<TranscriptDirFrame>> = const { RefCell::new(Vec::new()) };
    /// Durable sinks the emitted definition ids above were persisted to.
    static DEFINITION_SCOPE: RefCell<Option<DefinitionScope>> = const { RefCell::new(None) };
}

/// The durable sinks a definition reaches: the pushed transcript file and the
/// active event log. A later reference may omit a definition only while it
/// reaches the same sinks, because nothing compacts the transcript topic
/// during a run. The event log is held weakly so a replaced log never compares
/// equal to the one that received the definition.
#[derive(Clone)]
struct DefinitionScope {
    transcript_dir: Option<String>,
    event_log: Option<std::sync::Weak<crate::event_log::AnyEventLog>>,
}

impl DefinitionScope {
    fn current() -> Option<Self> {
        let scope = Self {
            transcript_dir: current_transcript_dir(),
            event_log: crate::event_log::active_event_log().map(|log| Arc::downgrade(&log)),
        };
        (scope.transcript_dir.is_some() || scope.event_log.is_some()).then_some(scope)
    }

    fn same_sinks(&self, other: &Self) -> bool {
        self.transcript_dir == other.transcript_dir
            && match (&self.event_log, &other.event_log) {
                (None, None) => true,
                (Some(mine), Some(theirs)) => mine.ptr_eq(theirs),
                _ => false,
            }
    }
}

/// True when emitted definitions remain reachable from the current sinks.
/// Moving to different sinks forgets every claimed definition.
fn definitions_are_retained() -> bool {
    let Some(current) = DefinitionScope::current() else {
        return false;
    };
    let unchanged = DEFINITION_SCOPE.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|scope| scope.same_sinks(&current))
    });
    if !unchanged {
        clear_definition_ids();
        DEFINITION_SCOPE.with(|slot| *slot.borrow_mut() = Some(current));
    }
    true
}

fn clear_definition_ids() {
    EMITTED_CAPABILITY_SNAPSHOT_IDS.with(|ids| ids.borrow_mut().clear());
    EMITTED_SERVED_MESSAGE_IDS.with(|ids| ids.borrow_mut().clear());
    EMITTED_OUTPUT_SCHEMA_IDS.with(|ids| ids.borrow_mut().clear());
    SERVED_LINEAGE_HEADS.with(|heads| heads.borrow_mut().clear());
}

static NEXT_TRANSCRIPT_DIR_FRAME_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

/// Opaque ownership receipt for one ambient transcript-directory push.
#[derive(Clone, Debug)]
pub(crate) struct TranscriptDirFrame {
    frame_id: u64,
    dir: String,
    active: Arc<std::sync::atomic::AtomicBool>,
}

impl TranscriptDirFrame {
    fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Acquire)
    }

    fn revoke(&self) {
        self.active
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

impl PartialEq for TranscriptDirFrame {
    fn eq(&self, other: &Self) -> bool {
        self.frame_id == other.frame_id
    }
}

impl Eq for TranscriptDirFrame {}

/// Per-agent transcript routing and deduplication state. It is swapped with
/// the rest of the ambient execution scope so cancellation or sibling tasks
/// cannot leak a pushed transcript directory across executions.
#[derive(Clone, Default)]
pub(crate) struct LlmTranscriptAmbient {
    system_prompt_hash: Option<u64>,
    context_manifest_hash: Option<u64>,
    tool_schemas_hash: Option<u64>,
    capability_snapshot_ids: BTreeSet<String>,
    served_message_ids: BTreeSet<String>,
    output_schema_ids: BTreeSet<String>,
    served_lineage_heads: BTreeMap<Option<String>, LineageHead>,
    transcript_dirs: Vec<TranscriptDirFrame>,
    definition_scope: Option<DefinitionScope>,
}

pub(crate) fn swap_llm_transcript_ambient(
    replacement: LlmTranscriptAmbient,
) -> LlmTranscriptAmbient {
    LlmTranscriptAmbient {
        system_prompt_hash: LAST_SYSTEM_PROMPT_HASH.with(|slot| {
            std::mem::replace(&mut *slot.borrow_mut(), replacement.system_prompt_hash)
        }),
        context_manifest_hash: LAST_CONTEXT_MANIFEST_HASH.with(|slot| {
            std::mem::replace(&mut *slot.borrow_mut(), replacement.context_manifest_hash)
        }),
        tool_schemas_hash: LAST_TOOL_SCHEMAS_HASH
            .with(|slot| std::mem::replace(&mut *slot.borrow_mut(), replacement.tool_schemas_hash)),
        capability_snapshot_ids: EMITTED_CAPABILITY_SNAPSHOT_IDS.with(|slot| {
            std::mem::replace(&mut *slot.borrow_mut(), replacement.capability_snapshot_ids)
        }),
        served_message_ids: EMITTED_SERVED_MESSAGE_IDS.with(|slot| {
            std::mem::replace(&mut *slot.borrow_mut(), replacement.served_message_ids)
        }),
        output_schema_ids: EMITTED_OUTPUT_SCHEMA_IDS
            .with(|slot| std::mem::replace(&mut *slot.borrow_mut(), replacement.output_schema_ids)),
        served_lineage_heads: SERVED_LINEAGE_HEADS.with(|slot| {
            std::mem::replace(&mut *slot.borrow_mut(), replacement.served_lineage_heads)
        }),
        transcript_dirs: TRANSCRIPT_DIR_STACK
            .with(|slot| std::mem::replace(&mut *slot.borrow_mut(), replacement.transcript_dirs)),
        definition_scope: DEFINITION_SCOPE
            .with(|slot| std::mem::replace(&mut *slot.borrow_mut(), replacement.definition_scope)),
    }
}

fn reset_deduplication() {
    LAST_SYSTEM_PROMPT_HASH.with(|hash| *hash.borrow_mut() = None);
    LAST_CONTEXT_MANIFEST_HASH.with(|hash| *hash.borrow_mut() = None);
    LAST_TOOL_SCHEMAS_HASH.with(|hash| *hash.borrow_mut() = None);
    clear_definition_ids();
    DEFINITION_SCOPE.with(|slot| *slot.borrow_mut() = None);
}

pub(super) fn system_prompt_changed(current: u64) -> bool {
    hash_changed(&LAST_SYSTEM_PROMPT_HASH, current)
}

pub(super) fn context_manifest_changed(current: u64) -> bool {
    hash_changed(&LAST_CONTEXT_MANIFEST_HASH, current)
}

pub(super) fn tool_schemas_changed(current: u64) -> bool {
    hash_changed(&LAST_TOOL_SCHEMAS_HASH, current)
}

/// Whether the current sinks still need a capability definition.
///
/// Without a transcript file or an active event log nothing retains a prior
/// definition, so callers emit it beside every reference.
pub(super) fn capability_snapshot_needs_definition(snapshot_id: &str) -> bool {
    !definitions_are_retained()
        || EMITTED_CAPABILITY_SNAPSHOT_IDS.with(|ids| !ids.borrow().contains(snapshot_id))
}

/// Mark a definition only after its transcript write succeeds. Claiming before
/// the append would let one transient I/O failure poison every later reference.
pub(super) fn record_capability_snapshot_definition(snapshot_id: &str) {
    if definitions_are_retained() {
        EMITTED_CAPABILITY_SNAPSHOT_IDS.with(|ids| {
            ids.borrow_mut().insert(snapshot_id.to_string());
        });
    }
}

/// Whether a provider-visible message needs a definition in the current
/// sinks. Without durable sinks it is emitted beside every reference.
pub(super) fn served_message_needs_definition(message_id: &str) -> bool {
    !definitions_are_retained()
        || EMITTED_SERVED_MESSAGE_IDS.with(|ids| !ids.borrow().contains(message_id))
}

/// Claim a provider-visible message definition only after persistence.
pub(super) fn record_served_message_definition(message_id: &str) {
    if definitions_are_retained() {
        EMITTED_SERVED_MESSAGE_IDS.with(|ids| {
            ids.borrow_mut().insert(message_id.to_string());
        });
    }
}

/// Whether an output schema needs a definition in the current sinks; without
/// durable sinks it is emitted beside every reference.
pub(super) fn output_schema_needs_definition(schema_id: &str) -> bool {
    !definitions_are_retained()
        || EMITTED_OUTPUT_SCHEMA_IDS.with(|ids| !ids.borrow().contains(schema_id))
}

/// Claim an output schema definition only after persistence.
pub(super) fn record_output_schema_definition(schema_id: &str) {
    if definitions_are_retained() {
        EMITTED_OUTPUT_SCHEMA_IDS.with(|ids| {
            ids.borrow_mut().insert(schema_id.to_string());
        });
    }
}

/// The session's last persisted lineage receipt in the current sinks. Without
/// durable sinks nothing retains a predecessor, so every receipt is full.
pub(super) fn served_lineage_head(session_id: Option<&str>) -> Option<LineageHead> {
    if !definitions_are_retained() {
        return None;
    }
    SERVED_LINEAGE_HEADS.with(|heads| heads.borrow().get(&session_id.map(str::to_string)).cloned())
}

/// Record a lineage receipt as its session's predecessor only after the
/// receipt is persisted, so a continuation never names an unwritten receipt.
pub(super) fn record_served_lineage_head(session_id: Option<&str>, head: LineageHead) {
    if definitions_are_retained() {
        SERVED_LINEAGE_HEADS.with(|heads| {
            heads
                .borrow_mut()
                .insert(session_id.map(str::to_string), head);
        });
    }
}

fn hash_changed(slot: &'static std::thread::LocalKey<RefCell<Option<u64>>>, current: u64) -> bool {
    slot.with(|cell| {
        let mut value = cell.borrow_mut();
        if value.as_ref() == Some(&current) {
            false
        } else {
            *value = Some(current);
            true
        }
    })
}

pub(crate) fn push_llm_transcript_dir(dir: &str) -> Option<TranscriptDirFrame> {
    if dir.trim().is_empty() {
        return None;
    }
    let frame = TranscriptDirFrame {
        frame_id: NEXT_TRANSCRIPT_DIR_FRAME_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        dir: dir.to_string(),
        active: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    };
    TRANSCRIPT_DIR_STACK.with(|stack| stack.borrow_mut().push(frame.clone()));
    reset_deduplication();
    Some(frame)
}

#[cfg(test)]
pub(crate) fn pop_llm_transcript_dir() {
    TRANSCRIPT_DIR_STACK.with(|stack| {
        if let Some(frame) = stack.borrow_mut().pop() {
            frame.revoke();
        }
    });
    reset_deduplication();
}

pub(crate) fn remove_llm_transcript_dir(frame: TranscriptDirFrame) -> bool {
    frame.revoke();
    let removed = TRANSCRIPT_DIR_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        let Some(index) = stack
            .iter()
            .position(|candidate| candidate.frame_id == frame.frame_id)
        else {
            return false;
        };
        stack.remove(index);
        true
    });
    if removed {
        reset_deduplication();
    }
    removed
}

pub(crate) fn current_transcript_dir() -> Option<String> {
    let stacked = TRANSCRIPT_DIR_STACK.with(|stack| {
        stack
            .borrow()
            .iter()
            .rev()
            .find(|frame| frame.is_active())
            .map(|frame| frame.dir.clone())
    });
    stacked.or_else(|| {
        std::env::var("HARN_LLM_TRANSCRIPT_DIR")
            .ok()
            .filter(|dir| !dir.is_empty())
    })
}

pub(crate) fn current_transcript_path() -> Option<std::path::PathBuf> {
    current_transcript_dir().map(|dir| {
        crate::run_tape::branch_path(
            std::path::PathBuf::from(dir).join("llm_transcript.jsonl"),
            "LLM transcript",
        )
    })
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use crate::orchestration::{scope_ambient, AmbientExecutionScope};

    use super::*;

    #[test]
    fn capability_snapshot_claims_are_scoped_to_a_pushed_transcript() {
        let saved = swap_llm_transcript_ambient(LlmTranscriptAmbient::default());
        let saved_log = crate::event_log::swap_active_event_log(None);
        assert!(
            capability_snapshot_needs_definition("blake3:unscoped"),
            "without a durable sink every reference carries its definition"
        );
        assert!(capability_snapshot_needs_definition("blake3:unscoped"));

        push_llm_transcript_dir("/tmp/harn-capability-snapshot-scope");
        assert!(capability_snapshot_needs_definition("blake3:a"));
        assert!(
            capability_snapshot_needs_definition("blake3:a"),
            "checking cannot claim a definition before persistence"
        );
        record_capability_snapshot_definition("blake3:a");
        assert!(!capability_snapshot_needs_definition("blake3:a"));
        assert!(capability_snapshot_needs_definition("blake3:b"));
        pop_llm_transcript_dir();

        push_llm_transcript_dir("/tmp/harn-capability-snapshot-scope");
        assert!(
            capability_snapshot_needs_definition("blake3:a"),
            "a new pushed scope cannot inherit a prior file's definitions"
        );
        pop_llm_transcript_dir();
        crate::event_log::swap_active_event_log(saved_log);
        let _ = swap_llm_transcript_ambient(saved);
    }

    #[test]
    fn active_event_log_retains_definitions_until_it_is_replaced() {
        let saved = swap_llm_transcript_ambient(LlmTranscriptAmbient::default());
        let saved_log = crate::event_log::swap_active_event_log(None);

        assert!(served_message_needs_definition("blake3:m"));
        record_served_message_definition("blake3:m");
        assert!(
            served_message_needs_definition("blake3:m"),
            "without a durable sink every reference carries its definition"
        );

        let first = crate::event_log::install_memory_for_current_thread(8);
        assert!(served_message_needs_definition("blake3:m"));
        record_served_message_definition("blake3:m");
        assert!(
            !served_message_needs_definition("blake3:m"),
            "the active event log retains a persisted definition"
        );

        let _second = crate::event_log::install_memory_for_current_thread(8);
        assert!(
            served_message_needs_definition("blake3:m"),
            "a replacement event log cannot inherit the first log's definitions"
        );
        drop(first);

        crate::event_log::swap_active_event_log(saved_log);
        let _ = swap_llm_transcript_ambient(saved);
    }

    #[test]
    fn exact_transcript_removal_preserves_newer_ambient_owner() {
        let saved = swap_llm_transcript_ambient(LlmTranscriptAmbient::default());
        let outer = push_llm_transcript_dir("/tmp/harn-transcript-shared").expect("outer frame");
        let inner = push_llm_transcript_dir("/tmp/harn-transcript-shared").expect("inner frame");

        assert!(remove_llm_transcript_dir(outer.clone()));
        assert_eq!(
            current_transcript_dir().as_deref(),
            Some("/tmp/harn-transcript-shared")
        );
        assert!(!remove_llm_transcript_dir(outer));
        assert!(remove_llm_transcript_dir(inner));
        assert_eq!(current_transcript_dir(), None);

        let _ = swap_llm_transcript_ambient(saved);
    }

    #[test]
    fn removed_transcript_frame_is_revoked_in_cloned_ambient_scope() {
        let saved = swap_llm_transcript_ambient(LlmTranscriptAmbient::default());
        let frame =
            push_llm_transcript_dir("/tmp/harn-transcript-revoked").expect("transcript frame");
        let inherited = LlmTranscriptAmbient {
            transcript_dirs: TRANSCRIPT_DIR_STACK.with(|stack| stack.borrow().clone()),
            ..LlmTranscriptAmbient::default()
        };

        assert!(remove_llm_transcript_dir(frame));
        let previous = swap_llm_transcript_ambient(inherited);
        assert_ne!(
            current_transcript_dir().as_deref(),
            Some("/tmp/harn-transcript-revoked"),
            "a cloned async scope must not resurrect a terminal transcript owner"
        );
        let _ = swap_llm_transcript_ambient(previous);
        let _ = swap_llm_transcript_ambient(saved);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn transcript_dir_is_isolated_across_interleaving_and_cancelled_tasks() {
        let saved = swap_llm_transcript_ambient(LlmTranscriptAmbient::default());
        push_llm_transcript_dir("/tmp/harn-transcript-parent");

        tokio::task::LocalSet::new()
            .run_until(async {
                let run_child = |dir: &'static str| {
                    tokio::task::spawn_local(scope_ambient(
                        AmbientExecutionScope::default(),
                        async move {
                            push_llm_transcript_dir(dir);
                            tokio::task::yield_now().await;
                            tokio::task::yield_now().await;
                            let observed = current_transcript_dir();
                            pop_llm_transcript_dir();
                            observed
                        },
                    ))
                };

                let alpha = run_child("/tmp/harn-transcript-alpha");
                let beta = run_child("/tmp/harn-transcript-beta");
                assert_eq!(
                    alpha.await.expect("alpha task"),
                    Some("/tmp/harn-transcript-alpha".to_string())
                );
                assert_eq!(
                    beta.await.expect("beta task"),
                    Some("/tmp/harn-transcript-beta".to_string())
                );
                assert_eq!(
                    current_transcript_dir().as_deref(),
                    Some("/tmp/harn-transcript-parent"),
                    "interleaved child polls must restore the parent transcript directory"
                );

                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let cancelled = tokio::task::spawn_local(scope_ambient(
                    AmbientExecutionScope::default(),
                    async move {
                        push_llm_transcript_dir("/tmp/harn-transcript-cancelled");
                        let _ = entered_tx.send(());
                        pending::<()>().await;
                    },
                ));
                entered_rx.await.expect("cancelled task entered its scope");
                assert_eq!(
                    current_transcript_dir().as_deref(),
                    Some("/tmp/harn-transcript-parent"),
                    "a suspended child poll must restore the parent transcript directory"
                );
                cancelled.abort();
                let error = cancelled
                    .await
                    .expect_err("aborted transcript task should report cancellation");
                assert!(error.is_cancelled(), "unexpected join error: {error}");
                assert_eq!(
                    current_transcript_dir().as_deref(),
                    Some("/tmp/harn-transcript-parent"),
                    "cancelling a task with an unpopped directory must preserve the parent"
                );
            })
            .await;

        pop_llm_transcript_dir();
        let _ = swap_llm_transcript_ambient(saved);
    }
}
