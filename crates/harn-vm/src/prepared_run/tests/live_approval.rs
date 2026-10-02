use super::*;

struct ApprovalExecutor;

#[async_trait::async_trait]
impl PreparedRunExecutor for ApprovalExecutor {
    type Output = (
        crate::orchestration::PolicyEvaluation,
        crate::orchestration::PolicyEvaluation,
    );
    type Error = String;

    async fn execute(&self, _: &AuthorityUse) -> Result<Self::Output, String> {
        fn decision() -> crate::orchestration::PolicyEvaluation {
            crate::orchestration::current_run_approval_policy()
                .expect("HostFacts policy reaches the executor")
                .evaluate_detailed("exec", &json!({}))
        }
        let parent = decision();
        let child = crate::orchestration::scope_ambient(
            crate::orchestration::AmbientExecutionScope::capture_inherited(),
            async { decision() },
        )
        .await;
        Ok((parent, child))
    }
}

#[tokio::test]
async fn prepared_host_policy_reaches_live_dispatch_and_delegated_scope() {
    crate::orchestration::clear_execution_policy_stacks();
    let run = PreparedRun::with_clock(
        ApprovalExecutor,
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    );
    let mut facts = host_facts();
    facts.approval_policy = RunApprovalPolicy::construct(
        RunAuthorityPosture {
            interactivity: RunInteractivity::NonInteractive,
            approval_availability: ApprovalAvailability::Unavailable,
            workspace_trust: WorkspaceTrust::HostMaterialized,
        },
        |_| ToolApprovalPolicy {
            require_approval: vec!["exec".to_string()],
            allow_sensitive_paths: true,
            allow_external_paths: true,
            ..Default::default()
        },
    );
    let lease = match run.prepare(intent(), facts) {
        PreparationOutcome::Ready {
            authority_lease, ..
        } => authority_lease,
        other => panic!("unrequested tool approval must not block preparation: {other:?}"),
    };
    let result = run.execute(lease).await;
    let (parent, child) = match result {
        ExecutionOutcome::Completed { output, .. } => output,
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("executor should complete: {error}")
        }
    };
    for decision in [parent, child] {
        assert!(decision.is_deny());
        assert_eq!(
            decision.denial_gate(),
            crate::agent_events::DenialGate::ApprovalUnavailable
        );
        assert_eq!(decision.receipt["requested_rule"]["action"], "ask");
    }
    assert!(
        crate::orchestration::current_run_approval_policy().is_none(),
        "prepared authority must leave with its execution scope"
    );
}
