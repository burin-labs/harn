//! What the parent of a sub-agent run sees when the child's `agent_loop`
//! throws.
//!
//! Every such error used to become a successful builtin return carrying an
//! error envelope, so a parent that branched on the call succeeding took the
//! success branch and continued as though the child had run. The message
//! survived inside the envelope; the only part that stops anything, the `Err`,
//! did not.
//!
//! The conversion is right for most failures. A child that initialized and then
//! failed has a per-child outcome to report, and a fan-out of many children
//! needs those outcomes rather than the first exception. It is wrong for a
//! child whose loop never initialized: there is no outcome to report, and an
//! "empty completed run" envelope is indistinguishable from a child that
//! genuinely produced nothing.

use super::{
    append_parent_sub_agent_event, finish_sub_agent, stop_details_for_error, sub_agent_error_dict,
    sub_agent_result_event, transcript_tokens_used, wrap_sub_agent_error, SubAgentExecutionResult,
    SubAgentRunSpec,
};
use crate::value::{VmError, VmValue};

fn is_agent_loop_initialization_failure(error: &VmError) -> bool {
    matches!(
        error,
        VmError::Thrown(VmValue::Dict(fields))
            if fields
                .get("kind")
                .is_some_and(|kind| kind.display() == "agent_loop_initialization_failed")
    )
}

/// Which child-loop errors the parent must not be allowed to miss.
///
/// Two classes propagate as `Err`:
///
/// * `ErrorCategory::Internal` — an engine or wiring bug. The agent loop
///   already re-raises this one layer down instead of folding it into a tool
///   observation, and folding it back in at the worker boundary would undo
///   that.
/// * `agent_loop_initialization_failed` — the loop boundary records this closed
///   kind only when option validation or other setup failed before a session
///   existed. There is no per-child run outcome to carry.
///
/// Every other child failure stays a per-child envelope.
pub(super) fn child_error_propagates(error: &VmError) -> bool {
    crate::value::error_to_category(error) == crate::value::ErrorCategory::Internal
        || is_agent_loop_initialization_failure(error)
}

/// Record the child's failure and decide whether the parent sees it as one.
///
/// The envelope, the parent's `sub_agent_result` event, and the typed subagent
/// stop are written either way, so the receipt is identical in both branches.
/// What differs is whether the parent's control flow can miss the failure.
pub(super) fn child_error_outcome(
    spec: &SubAgentRunSpec,
    error: VmError,
) -> Result<SubAgentExecutionResult, VmError> {
    let stop_details = stop_details_for_error(&error);
    let error_value = match &error {
        VmError::CategorizedError { message, category } => {
            sub_agent_error_dict(category.as_str(), message.clone(), None)
        }
        VmError::Thrown(VmValue::String(message)) => {
            sub_agent_error_dict("runtime", message.to_string(), None)
        }
        _ => sub_agent_error_dict(
            crate::value::error_to_category(&error).as_str(),
            error.to_string(),
            None,
        ),
    };
    let transcript = crate::agent_sessions::transcript(&spec.session_id)
        .unwrap_or_else(|| crate::stdlib::json_to_vm_value(&serde_json::json!({})));
    let tokens_used = transcript_tokens_used(&transcript);
    let envelope = wrap_sub_agent_error(
        String::new(),
        VmValue::List(std::sync::Arc::new(Vec::new())),
        0,
        tokens_used,
        false,
        &spec.session_id,
        error_value.clone(),
        Some(transcript.clone()),
    );
    append_parent_sub_agent_event(
        spec.parent_session_id.as_deref(),
        sub_agent_result_event(
            spec,
            false,
            "",
            0,
            false,
            Some(crate::llm::vm_value_to_json(&error_value)),
        ),
    );
    let propagates = child_error_propagates(&error);
    let finished = finish_sub_agent(
        spec,
        crate::llm::vm_value_to_json(&envelope),
        transcript,
        stop_details,
    );
    if propagates {
        return Err(error);
    }
    Ok(finished)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thrown(message: &str) -> VmError {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message)))
    }

    fn initialization_failure(message: &str) -> VmError {
        VmError::Thrown(VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("kind"),
                VmValue::String(arcstr::ArcStr::from("agent_loop_initialization_failed")),
            ),
            (
                crate::value::intern_key("message"),
                VmValue::String(arcstr::ArcStr::from(message)),
            ),
        ])))
    }

    #[test]
    fn a_child_refused_before_its_first_call_propagates() {
        assert!(child_error_propagates(&initialization_failure(
            "agent_loop: unknown option key 'loop_detect_warn'"
        )));
    }

    #[test]
    fn a_child_that_ran_and_then_failed_stays_an_envelope() {
        assert!(!child_error_propagates(&thrown(
            "the tool blew up on turn three"
        )));
    }

    #[test]
    fn an_engine_bug_propagates_even_after_the_child_ran() {
        assert!(child_error_propagates(&VmError::UndefinedBuiltin(
            "agent_emit_event".to_string()
        )));
    }

    #[test]
    fn an_untyped_pre_call_failure_stays_an_envelope() {
        assert!(!child_error_propagates(&thrown(
            "provider failed before output"
        )));
    }

    struct CliLlmMockGuard;

    impl Drop for CliLlmMockGuard {
        fn drop(&mut self) {
            crate::llm::clear_cli_llm_mock_mode();
        }
    }

    fn install_text_mock(text: &str) -> CliLlmMockGuard {
        let mock = crate::llm::parse_llm_mock_value(&serde_json::json!({"text": text}))
            .expect("valid llm mock");
        crate::llm::install_cli_llm_mocks(vec![mock]);
        CliLlmMockGuard
    }

    fn child_spec(session_id: &str, parent: &str, extra: Vec<(&str, VmValue)>) -> SubAgentRunSpec {
        let mut options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("mock")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("mock")),
            ),
            (crate::value::intern_key("max_iterations"), VmValue::Int(1)),
        ]);
        for (key, value) in extra {
            options.insert(crate::value::intern_key(key), value);
        }
        SubAgentRunSpec {
            name: "child".to_string(),
            task: "inspect the repo".to_string(),
            system: None,
            options,
            returns_schema: None,
            session_id: session_id.to_string(),
            run_id: format!("agent_run_{session_id}"),
            parent_session_id: Some(parent.to_string()),
            parent_run_id: Some(format!("agent_run_{parent}")),
            reminder_propagation: Vec::new(),
            workspace_anchor: None,
            stop_emitted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    async fn run_child(spec: SubAgentRunSpec) -> Result<SubAgentExecutionResult, VmError> {
        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
        super::super::execute_sub_agent(&ctx, spec).await
    }

    /// The falsifier from #8252: a parent that only branches on the call
    /// succeeding must take the failure branch. Reading the message out of the
    /// envelope would have passed in both worlds, so this asserts the `Err`
    /// itself.
    #[tokio::test(flavor = "current_thread")]
    async fn a_misconfigured_child_fails_its_parent() {
        crate::agent_sessions::reset_session_store();
        crate::llm::mock::reset_llm_mock_state();
        let parent = crate::agent_sessions::open_or_create_for_test(Some("parent-refusal".into()));
        let spec = child_spec(
            "child-refusal",
            &parent,
            vec![(
                "iteration_budget",
                crate::stdlib::json_to_vm_value(&serde_json::json!({
                    "mode": "unbounded",
                    "max": 4,
                })),
            )],
        );

        let error = match run_child(spec).await {
            Ok(_) => panic!("expected a failed child to fail the parent"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("iteration_budget"), "{error:?}");
        assert!(is_agent_loop_initialization_failure(&error), "{error:?}");
    }

    /// Positive control: the propagation is not "every child now fails".
    #[tokio::test(flavor = "current_thread")]
    async fn a_finishing_child_still_returns_a_successful_envelope() {
        crate::agent_sessions::reset_session_store();
        crate::llm::mock::reset_llm_mock_state();
        let _mock = install_text_mock("surveyed the repo");
        let parent = crate::agent_sessions::open_or_create_for_test(Some("parent-done".into()));
        let spec = child_spec("child-done", &parent, Vec::new());

        let result = run_child(spec).await.expect("child finished");

        assert_eq!(result.payload["ok"].as_bool(), Some(true));
    }
}
