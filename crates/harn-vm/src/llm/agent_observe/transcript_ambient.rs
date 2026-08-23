//! Ambient transcript routing and per-scope deduplication state.

use std::cell::RefCell;
use std::collections::BTreeSet;

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
    static TRANSCRIPT_DIR_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Per-agent transcript routing and deduplication state. It is swapped with
/// the rest of the ambient execution scope so cancellation or sibling tasks
/// cannot leak a pushed transcript directory across executions.
#[derive(Clone, Default)]
pub(crate) struct LlmTranscriptAmbient {
    system_prompt_hash: Option<u64>,
    context_manifest_hash: Option<u64>,
    tool_schemas_hash: Option<u64>,
    capability_snapshot_ids: BTreeSet<String>,
    transcript_dirs: Vec<String>,
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
        transcript_dirs: TRANSCRIPT_DIR_STACK
            .with(|slot| std::mem::replace(&mut *slot.borrow_mut(), replacement.transcript_dirs)),
    }
}

fn reset_deduplication() {
    LAST_SYSTEM_PROMPT_HASH.with(|hash| *hash.borrow_mut() = None);
    LAST_CONTEXT_MANIFEST_HASH.with(|hash| *hash.borrow_mut() = None);
    LAST_TOOL_SCHEMAS_HASH.with(|hash| *hash.borrow_mut() = None);
    EMITTED_CAPABILITY_SNAPSHOT_IDS.with(|ids| ids.borrow_mut().clear());
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

/// Whether this transcript scope still needs a capability definition.
///
/// Without an explicit transcript directory there is no durable scope in
/// which a prior definition is guaranteed to remain reachable (run-event and
/// event-log sinks may turn over independently), so callers conservatively
/// emit the definition beside every reference.
pub(super) fn capability_snapshot_needs_definition(snapshot_id: &str) -> bool {
    if current_transcript_dir().is_none() {
        return true;
    }
    EMITTED_CAPABILITY_SNAPSHOT_IDS.with(|ids| !ids.borrow().contains(snapshot_id))
}

/// Mark a definition only after its transcript write succeeds. Claiming before
/// the append would let one transient I/O failure poison every later reference.
pub(super) fn record_capability_snapshot_definition(snapshot_id: &str) {
    if current_transcript_dir().is_some() {
        EMITTED_CAPABILITY_SNAPSHOT_IDS.with(|ids| {
            ids.borrow_mut().insert(snapshot_id.to_string());
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

pub(crate) fn push_llm_transcript_dir(dir: &str) {
    if dir.trim().is_empty() {
        return;
    }
    TRANSCRIPT_DIR_STACK.with(|stack| stack.borrow_mut().push(dir.to_string()));
    reset_deduplication();
}

pub(crate) fn pop_llm_transcript_dir() {
    TRANSCRIPT_DIR_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
    reset_deduplication();
}

pub(crate) fn current_transcript_dir() -> Option<String> {
    let stacked = TRANSCRIPT_DIR_STACK.with(|stack| stack.borrow().last().cloned());
    stacked.or_else(|| {
        std::env::var("HARN_LLM_TRANSCRIPT_DIR")
            .ok()
            .filter(|dir| !dir.is_empty())
    })
}

pub(crate) fn current_transcript_path() -> Option<std::path::PathBuf> {
    current_transcript_dir().map(|dir| std::path::PathBuf::from(dir).join("llm_transcript.jsonl"))
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use crate::orchestration::{scope_ambient, AmbientExecutionScope};

    use super::*;

    #[test]
    fn capability_snapshot_claims_are_scoped_to_a_pushed_transcript() {
        let saved = swap_llm_transcript_ambient(LlmTranscriptAmbient::default());
        assert!(
            capability_snapshot_needs_definition("blake3:unscoped"),
            "an unscoped event sink needs a definition beside every reference"
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
