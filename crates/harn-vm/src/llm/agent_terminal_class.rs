//! Native compatibility projection of the shared terminal-error classifier.

pub(crate) use harn_llm::agent_terminal_class::{
    agent_loop_made_no_llm_call, session_status_indicates_error,
};
#[doc(inline)]
pub use harn_llm::agent_terminal_class::{agent_terminal_class, AgentTerminalClass};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_unverified_is_a_policy_failure_without_a_runtime_error() {
        let final_status = "completion_unverified";
        let stop_reason = "turn_end_judge_cap_reached";

        assert!(
            !session_status_indicates_error(final_status),
            "a failed lifecycle state must not fire the SessionError hook without an error"
        );
        assert_eq!(agent_terminal_class(final_status, stop_reason, None), None);

        let outcome = crate::agent_events::terminal_outcome_for_finalize(
            final_status,
            stop_reason,
            None,
            false,
        );
        assert_eq!(
            outcome.kind,
            crate::agent_events::AgentTerminalKind::CompletionUnverified
        );
        assert_eq!(
            outcome.kind.lifecycle_state(),
            crate::agent_events::AgentLifecycleState::Failed
        );
        assert_eq!(outcome.reason, stop_reason);
        assert_eq!(outcome.owner, "policy");
        assert_eq!(outcome.terminal_class, None);
    }
}
