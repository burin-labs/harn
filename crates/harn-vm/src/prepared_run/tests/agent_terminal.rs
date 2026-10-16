use crate::agent_events::{AgentTerminalKind, AgentTerminalOutcome};

use super::*;

struct TerminalExecutor {
    terminal: Option<AgentTerminalOutcome>,
    fails: bool,
    calls: Arc<AtomicUsize>,
    producer_policy: Option<crate::redact::RedactionPolicy>,
}

#[tokio::test]
async fn persisted_terminal_omits_provider_prose_and_redacts_custom_reasons() {
    let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789ABCD";
    let mut terminal = AgentTerminalOutcome::new(
        AgentTerminalKind::UserCancelled,
        format!("custom stop {secret}"),
    );
    terminal.message = Some("provider diagnostic must stay out of authority evidence".into());
    terminal.detail = Some(format!("provider credential {secret}"));
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let run = PreparedRun::with_clock(
        TerminalExecutor {
            terminal: Some(terminal),
            fails: false,
            calls: Arc::new(AtomicUsize::new(0)),
            producer_policy: Some(crate::redact::RedactionPolicy::default()),
        },
        receipts.clone(),
        Arc::new(|| NOW_MS),
    );
    let lease = approved_lease(&run, &intent(), &host_facts());
    // The producer scope differs from the scope active when Harn persists.
    let _outer_policy =
        crate::redact::PolicyGuard::new(crate::redact::RedactionPolicy::passthrough());
    let receipt = match run.execute(lease).await {
        ExecutionOutcome::Completed { receipt, .. } => receipt,
        _ => panic!("executor must deliver its stopped payload"),
    };
    assert_eq!(receipt.status, AuthorityReceiptStatus::Stopped);
    let persisted = receipts
        .receipts()
        .pop()
        .expect("persisted terminal receipt");
    assert_eq!(persisted, receipt);
    let observed = persisted.agent_terminal.as_ref().expect("typed terminal");
    assert_eq!(observed.kind, AgentTerminalKind::UserCancelled);
    assert_eq!(observed.owner, "user");
    assert!(observed.reason.contains("<redacted:openai_key:"));
    assert!(observed.message.is_none());
    assert!(observed.detail.is_none());
    let wire = serde_json::to_string(&persisted).expect("encode persisted receipt");
    assert!(!wire.contains(secret));
    assert!(!wire.contains("provider diagnostic"));
    assert!(!wire.contains("provider credential"));
}

#[async_trait::async_trait]
impl PreparedRunExecutor for TerminalExecutor {
    type Output = ();
    type Error = &'static str;

    async fn execute(&self, authority: &AuthorityUse) -> Result<(), Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(terminal) = self.terminal.clone() {
            let _policy = self
                .producer_policy
                .clone()
                .map(crate::redact::PolicyGuard::new);
            authority.record_agent_terminal(terminal);
        }
        if self.fails {
            Err("executor failed after its terminal observation")
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn generic_execution_persists_the_producer_terminal_once() {
    for (kind, fails, expected) in [
        (None, false, AuthorityReceiptStatus::Completed),
        (None, true, AuthorityReceiptStatus::Failed),
        (
            Some(AgentTerminalKind::Natural),
            false,
            AuthorityReceiptStatus::Completed,
        ),
        (
            Some(AgentTerminalKind::Natural),
            true,
            AuthorityReceiptStatus::Failed,
        ),
        (
            Some(AgentTerminalKind::UserCancelled),
            false,
            AuthorityReceiptStatus::Stopped,
        ),
        (
            Some(AgentTerminalKind::PolicyThrash),
            false,
            AuthorityReceiptStatus::Stopped,
        ),
        (
            Some(AgentTerminalKind::ProviderError),
            false,
            AuthorityReceiptStatus::Failed,
        ),
        (
            Some(AgentTerminalKind::Suspended),
            false,
            AuthorityReceiptStatus::Failed,
        ),
    ] {
        let terminal = kind.map(|kind| AgentTerminalOutcome::new(kind, "actual producer decision"));
        let calls = Arc::new(AtomicUsize::new(0));
        let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
        let run = PreparedRun::with_clock(
            TerminalExecutor {
                terminal: terminal.clone(),
                fails,
                calls: calls.clone(),
                producer_policy: None,
            },
            receipts.clone(),
            Arc::new(|| NOW_MS),
        );
        let lease = approved_lease(&run, &intent(), &host_facts());
        let receipt = match run.execute(lease).await {
            ExecutionOutcome::Completed { receipt, .. } if !fails => receipt,
            ExecutionOutcome::ExecutorFailed { receipt, error } if fails => {
                assert_eq!(error, "executor failed after its terminal observation");
                receipt
            }
            _ => panic!("execution result must preserve the executor outcome"),
        };
        assert_eq!(receipt.status, expected, "{kind:?}, fails={fails}");
        assert_eq!(receipt.agent_terminal, terminal);
        assert_eq!(
            receipt.stage,
            if expected == AuthorityReceiptStatus::Stopped {
                AuthorityReceiptStage::Stopped
            } else {
                AuthorityReceiptStage::Terminal
            },
        );
        assert!(receipt.executor_invoked);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let all_receipts = receipts.receipts();
        let terminal_receipts: Vec<_> = all_receipts
            .iter()
            .filter(|receipt| {
                matches!(
                    receipt.stage,
                    AuthorityReceiptStage::Terminal | AuthorityReceiptStage::Stopped
                )
            })
            .collect();
        assert_eq!(
            terminal_receipts.len(),
            1,
            "exactly one terminal persistence"
        );
        assert_eq!(terminal_receipts[0], &receipt);
        let wire = serde_json::to_value(&receipt).expect("encode receipt");
        if terminal.is_none() {
            assert!(
                wire.get("agent_terminal").is_none(),
                "absence is not a stop"
            );
        } else {
            assert_eq!(wire["agent_terminal"]["reason"], "actual producer decision");
            assert_eq!(
                wire["agent_terminal"]["owner"],
                kind.expect("terminal kind").owner()
            );
        }
    }
}
