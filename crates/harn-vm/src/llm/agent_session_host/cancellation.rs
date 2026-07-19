use crate::orchestration::NestedExecutionGuard;
use crate::value::VmError;

/// Owns a nested-policy push across the async agent lifecycle. Normal
/// finalization explicitly pops it while the session's ambient scope is
/// installed; cancellation merely disarms it because dropping the future
/// happens after that scope has been swapped out.
pub(super) struct CancelSafeNestedExecutionGuard(Option<NestedExecutionGuard>);

impl CancelSafeNestedExecutionGuard {
    pub(super) fn new(guard: NestedExecutionGuard) -> Self {
        Self(Some(guard))
    }

    pub(super) fn finish(mut self) {
        drop(self.0.take());
    }
}

impl Drop for CancelSafeNestedExecutionGuard {
    fn drop(&mut self) {
        if let Some(guard) = self.0.take() {
            guard.disarm();
        }
    }
}

/// Complete the cancellation-sensitive cleanup after finalization's last
/// await, while the owning ambient policy scope is still installed.
pub(super) fn finish_agent_session(
    session: &mut super::AgentHostSession,
    session_id: &str,
    abandon_in_flight: bool,
) {
    crate::llm::agent_runtime::fire_session_end_hooks(session_id, abandon_in_flight);
    if let Some(guard) = session.nested_policy_guard.take() {
        guard.finish();
    }
}

/// Release host-owned state after an embedding surface has cancelled and
/// awaited an agent future instead of letting it reach `finalize`.
pub(crate) async fn abandon_agent_session(session_id: &str) -> Result<(), VmError> {
    let has_host_session =
        super::AGENT_HOST_SESSIONS.with(|sessions| sessions.borrow().contains_key(session_id));
    if !has_host_session && !crate::agent_sessions::has_journal(session_id) {
        return Ok(());
    }

    // Do not discard pending transcript mutations. On failure, retain both
    // owners so a later daemon_stop call can retry the durable flush.
    crate::agent_session_journal::flush(session_id).await?;
    super::AGENT_HOST_SESSIONS.with(|sessions| sessions.borrow_mut().remove(session_id));
    crate::agent_sessions::clear_journal(session_id);
    crate::llm::permissions::clear_session_grants(session_id);
    crate::orchestration::clear_approval_policy_repeat_counts(session_id);
    crate::llm::agent_runtime::fire_session_end_hooks(session_id, false);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use serde_json::json;

    use crate::value::VmDictExt;

    #[tokio::test(flavor = "current_thread")]
    async fn abandoned_finalize_flushes_once_and_fires_native_cleanup_once() {
        crate::agent_sessions::reset_session_store();
        let root = tempfile::tempdir().expect("temp root");
        let session_id = "abandoned-finalize";
        let mut options = crate::value::DictMap::new();
        options.put_str("root", root.path().to_string_lossy().as_ref());
        let prepared = crate::agent_session_journal::prepare(
            session_id,
            &options,
            "run-abandoned".to_string(),
            "turn-abandoned".to_string(),
        )
        .await
        .expect("prepare journal");
        crate::agent_sessions::open_or_create(Some(session_id.to_string()));
        crate::agent_sessions::install_journal(session_id, prepared.state)
            .expect("install journal");
        crate::agent_sessions::inject_message(
            session_id,
            crate::stdlib::json_to_vm_value(&json!({
                "role": "user",
                "content": "persist before cancellation",
            })),
        )
        .expect("enqueue transcript mutation");

        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let observed_count = cleanup_count.clone();
        let _registration = crate::llm::agent_runtime::register_session_end_hook(Arc::new(
            move |ended_session_id| {
                if ended_session_id == session_id {
                    observed_count.fetch_add(1, Ordering::SeqCst);
                }
            },
        ));

        super::abandon_agent_session(session_id)
            .await
            .expect("first abandonment");
        super::abandon_agent_session(session_id)
            .await
            .expect("idempotent second abandonment");

        assert!(!crate::agent_sessions::has_journal(session_id));
        assert_eq!(cleanup_count.load(Ordering::SeqCst), 1);
        let store =
            crate::stdlib::session_store::open_canonical_agent_session(root.path(), session_id)
                .await
                .expect("open canonical session");
        let events = crate::stdlib::session_store::read_all_events(&store, session_id)
            .await
            .expect("read canonical events");
        assert_eq!(events.len(), 1);
        assert!(events[0]
            .payload
            .to_string()
            .contains("persist before cancellation"));
        crate::agent_sessions::reset_session_store();
    }

    #[test]
    fn cancelled_nested_guard_does_not_pop_callers_policy() {
        use crate::orchestration::{
            clear_execution_policy_stacks, current_execution_policy, enter_nested_execution_policy,
            pop_execution_policy, push_execution_policy, swap_execution_policy_stack,
            CapabilityPolicy, NestedExecutionKind,
        };

        clear_execution_policy_stacks();
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(4),
            ..Default::default()
        });
        let nested = enter_nested_execution_policy(
            None,
            NestedExecutionKind::AgentLoop,
            "cancelled-session",
        )
        .expect("enter nested policy");
        let abandoned_stack = swap_execution_policy_stack(Vec::new());
        push_execution_policy(CapabilityPolicy {
            recursion_limit: Some(99),
            ..Default::default()
        });

        drop(super::CancelSafeNestedExecutionGuard::new(nested));
        assert_eq!(
            current_execution_policy().and_then(|policy| policy.recursion_limit),
            Some(99),
            "dropping a cancelled session must not pop the caller's unrelated policy"
        );

        pop_execution_policy();
        drop(abandoned_stack);
        clear_execution_policy_stacks();
    }
}
