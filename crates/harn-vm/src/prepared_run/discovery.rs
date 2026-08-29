use serde_json::json;

use crate::orchestration::PolicyEvaluation;

use super::engine::evaluate_requirement;
use super::evidence::{approval_batch, diagnostic, policy_evidence, receipted_requirements};
use super::*;

#[derive(Clone, Copy)]
enum DiscoveryFailure {
    Refusal,
    Integrity,
}

pub trait ToolchainProbeRunner {
    fn run(&self, probe: &ToolchainProbeRequirement) -> Result<ToolchainProbeResult, String>;
}

impl<E> PreparedRun<E> {
    /// Run one exact toolchain probe after readiness and atomically apply only
    /// process-read roots that attenuate its reviewed ceiling. The probe is a
    /// used authority operation even when its output is rejected.
    pub fn discover_toolchain(
        &self,
        lease: &mut AuthorityLease,
        probe: &ToolchainProbeRequirement,
        runner: &dyn ToolchainProbeRunner,
    ) -> ToolchainDiscoveryOutcome {
        let now_ms = (self.now_ms)();
        let probe_requirement = AuthorityRequirement::ToolchainProbe(probe.clone());
        let probe_fingerprint = requirement_fingerprint(&probe_requirement);
        let integrity_diagnostics =
            validate_probe(lease, &probe_requirement, &probe_fingerprint, now_ms);
        if !integrity_diagnostics.is_empty() {
            return self.block_discovery(
                lease,
                integrity_diagnostics,
                Vec::new(),
                now_ms,
                DiscoveryFailure::Integrity,
            );
        }
        let evaluation = evaluate_requirement(
            lease.approval_policy.effective(),
            &lease.net_policy,
            &probe_requirement,
        );
        let mut diagnostics = Vec::new();
        validate_probe_policy(lease, &probe_fingerprint, &evaluation, &mut diagnostics);
        if !diagnostics.is_empty() {
            return self.block_discovery(
                lease,
                diagnostics,
                Vec::new(),
                now_ms,
                DiscoveryFailure::Refusal,
            );
        }

        lease.prior_used.insert(probe_fingerprint.clone());
        if let Ok(evaluation) = evaluation {
            lease
                .prior_policy_decisions
                .push(policy_evidence(&probe_fingerprint, &evaluation));
        }
        let result = match runner.run(probe) {
            Ok(result) => result,
            Err(_error) => {
                return self.block_discovery(
                    lease,
                    vec![diagnostic(
                        "toolchain_probe_failed",
                        "toolchain probe failed before producing discovery data",
                        Some(probe_fingerprint),
                        "Repair the exact probe command or choose a host with that toolchain.",
                    )],
                    Vec::new(),
                    now_ms,
                    DiscoveryFailure::Refusal,
                )
            }
        };

        let mut observed = result
            .discovered_read_roots
            .into_iter()
            .map(|root| AuthorityRequirement::ProcessReadRoot { root })
            .chain(result.attempted_authority)
            .collect::<Vec<_>>();
        observed.sort_by_key(requirement_fingerprint);
        observed.dedup();

        let mut deltas = Vec::new();
        let mut denied = Vec::new();
        let mut approval_candidates = Vec::new();
        for requirement in observed {
            let fingerprint = requirement_fingerprint(&requirement);
            lease
                .requested_fingerprints
                .insert(fingerprint.clone(), requirement.clone());
            match self.request_delta(lease, requirement.clone()) {
                LeaseDeltaOutcome::Covered => {}
                LeaseDeltaOutcome::Attenuated(delta) => deltas.push(delta),
                LeaseDeltaOutcome::Blocked(blocker) => {
                    denied.push(DeniedAuthority {
                        authority: ReceiptedAuthority {
                            fingerprint: fingerprint.clone(),
                            requirement: requirement.clone(),
                        },
                        reason: blocker.message.clone(),
                        decider: AuthorityDecider::RuntimePolicy,
                    });
                    diagnostics.push(blocker);
                    approval_candidates.push((
                        ReceiptedAuthority {
                            fingerprint,
                            requirement,
                        },
                        widening_evaluation(),
                    ));
                }
            }
        }

        if !diagnostics.is_empty() {
            if lease.approval_policy.posture().approval_availability
                == ApprovalAvailability::Available
                && lease.plan.interactivity == RunInteractivity::Interactive
            {
                lease.prior_denied.extend(denied);
                return self.needs_discovery_approval(
                    lease,
                    diagnostics,
                    approval_candidates,
                    now_ms,
                );
            }
            diagnostics.push(diagnostic(
                "discovery_approval_unavailable",
                "toolchain discovery attempted to widen authority but no interactive approval channel is available",
                None,
                "Run interactively or add the exact requirement to a newly prepared non-interactive run.",
            ));
            return self.block_discovery(
                lease,
                diagnostics,
                denied,
                now_ms,
                DiscoveryFailure::Refusal,
            );
        }

        let decider = lease
            .deciders
            .get(&probe_fingerprint)
            .copied()
            .unwrap_or(AuthorityDecider::RuntimePolicy);
        for delta in &deltas {
            lease.requirement_fingerprints.insert(
                delta.requirement_fingerprint.clone(),
                delta.requirement.clone(),
            );
            lease
                .deciders
                .insert(delta.requirement_fingerprint.clone(), decider);
        }
        let mut receipt = discovery_receipt(lease, now_ms);
        if let Err(error) = self.receipts.persist(&receipt) {
            let blocker = diagnostic(
                "discovery_receipt_persistence",
                error,
                None,
                "Repair receipt persistence before executing with discovered roots.",
            );
            lease.invalidated = Some(blocker.clone());
            receipt.status = AuthorityReceiptStatus::Blocked;
            receipt.diagnostics.push(blocker.clone());
            return ToolchainDiscoveryOutcome::Blocked {
                diagnostics: vec![blocker],
                receipt,
            };
        }
        ToolchainDiscoveryOutcome::Discovered { deltas, receipt }
    }

    fn needs_discovery_approval(
        &self,
        lease: &mut AuthorityLease,
        diagnostics: Vec<AuthorityDiagnostic>,
        candidates: Vec<(ReceiptedAuthority, PolicyEvaluation)>,
        now_ms: u64,
    ) -> ToolchainDiscoveryOutcome {
        let batch = approval_batch(&lease.plan_fingerprint, &candidates)
            .expect("widening discovery has approval candidates");
        let mut receipt = discovery_receipt(lease, now_ms);
        receipt.stage = AuthorityReceiptStage::NeedsApproval;
        receipt.status = AuthorityReceiptStatus::NeedsApproval;
        receipt.diagnostics = diagnostics;
        if let Err(error) = self.receipts.persist(&receipt) {
            return self.block_discovery(
                lease,
                vec![diagnostic(
                    "discovery_receipt_persistence",
                    error,
                    None,
                    "Repair receipt persistence before presenting discovered authority.",
                )],
                Vec::new(),
                now_ms,
                DiscoveryFailure::Integrity,
            );
        }
        ToolchainDiscoveryOutcome::NeedsApproval {
            batched_requests: batch,
            receipt,
        }
    }

    fn block_discovery(
        &self,
        lease: &mut AuthorityLease,
        mut diagnostics: Vec<AuthorityDiagnostic>,
        denied: Vec<DeniedAuthority>,
        now_ms: u64,
        failure: DiscoveryFailure,
    ) -> ToolchainDiscoveryOutcome {
        lease.prior_denied.extend(denied);
        let primary = diagnostics.first().cloned().unwrap_or_else(|| {
            diagnostic(
                "toolchain_discovery_blocked",
                "toolchain discovery was blocked",
                None,
                "Inspect the authority receipt before retrying.",
            )
        });
        if matches!(failure, DiscoveryFailure::Integrity) {
            lease.invalidated = Some(primary);
        }
        let mut receipt = discovery_receipt(lease, now_ms);
        receipt.stage = AuthorityReceiptStage::Blocked;
        receipt.status = AuthorityReceiptStatus::Blocked;
        receipt.diagnostics = diagnostics.clone();
        if let Err(error) = self.receipts.persist(&receipt) {
            let persistence = diagnostic(
                "discovery_receipt_persistence",
                error,
                None,
                "Repair receipt persistence before retrying discovery or executing the parent lease.",
            );
            lease.invalidated = Some(persistence.clone());
            receipt.diagnostics.push(persistence.clone());
            diagnostics.push(persistence);
        }
        ToolchainDiscoveryOutcome::Blocked {
            diagnostics,
            receipt,
        }
    }
}

fn validate_probe(
    lease: &AuthorityLease,
    requirement: &AuthorityRequirement,
    fingerprint: &str,
    now_ms: u64,
) -> Vec<AuthorityDiagnostic> {
    if now_ms > lease.expires_at_ms {
        return vec![diagnostic(
            "toolchain_discovery_lease_expired",
            "authority lease expired before toolchain discovery",
            Some(fingerprint.to_string()),
            "Prepare a fresh run before probing the toolchain.",
        )];
    }
    if lease.requirement_fingerprints.get(fingerprint) != Some(requirement) {
        return vec![diagnostic(
            "toolchain_probe_outside_lease",
            "toolchain probe is outside the fingerprinted authority lease",
            Some(fingerprint.to_string()),
            "Prepare the exact probe command and root ceiling before discovery.",
        )];
    }
    Vec::new()
}

fn validate_probe_policy(
    lease: &AuthorityLease,
    fingerprint: &str,
    evaluation: &Result<PolicyEvaluation, String>,
    diagnostics: &mut Vec<AuthorityDiagnostic>,
) {
    match evaluation {
        Ok(evaluation) if evaluation.is_deny() => diagnostics.push(diagnostic(
            "toolchain_probe_policy_denied",
            evaluation.reason.clone(),
            Some(fingerprint.to_string()),
            "Change the probe or the owning process policy before retrying.",
        )),
        Ok(evaluation) if evaluation.is_ask() && !lease.deciders.contains_key(fingerprint) => {
            diagnostics.push(diagnostic(
                "toolchain_probe_missing_decider",
                "the probe still requires approval and the lease has no decider",
                Some(fingerprint.to_string()),
                "Prepare and approve the complete authority batch again.",
            ));
        }
        Err(error) => diagnostics.push(diagnostic(
            "toolchain_probe_policy_evaluation",
            error.clone(),
            Some(fingerprint.to_string()),
            "Correct the probe so the canonical evaluator can classify it.",
        )),
        _ => {}
    }
}

fn widening_evaluation() -> PolicyEvaluation {
    PolicyEvaluation {
        action: "ask".to_string(),
        reason: "discovered authority widens the prepared lease".to_string(),
        matched_rule: None,
        required_approval: None,
        risk_labels: vec!["dynamic_authority".to_string()],
        receipt: json!({
            "type": "harn.permission_policy_decision.v1",
            "action": "ask",
            "reason": "discovered authority widens the prepared lease",
            "policy_source": "harn.prepared_run.discovery",
        }),
    }
}

fn discovery_receipt(lease: &AuthorityLease, observed_at_ms: u64) -> RunAuthorityReceipt {
    let requested = receipted_requirements(
        &lease
            .requested_fingerprints
            .values()
            .cloned()
            .collect::<Vec<_>>(),
    );
    let granted = receipted_requirements(
        &lease
            .requirement_fingerprints
            .values()
            .cloned()
            .collect::<Vec<_>>(),
    );
    let used = granted
        .iter()
        .filter(|authority| lease.prior_used.contains(&authority.fingerprint))
        .cloned()
        .collect::<Vec<_>>();
    let unused = granted
        .iter()
        .filter(|authority| !lease.prior_used.contains(&authority.fingerprint))
        .cloned()
        .collect::<Vec<_>>();
    RunAuthorityReceipt {
        schema: RUN_AUTHORITY_RECEIPT_SCHEMA.to_string(),
        stage: AuthorityReceiptStage::Discovery,
        status: AuthorityReceiptStatus::Ready,
        intent_id: lease.plan.intent_id.clone(),
        plan_fingerprint: lease.plan_fingerprint.clone(),
        lease_fingerprint: Some(lease.lease_fingerprint.clone()),
        observed_at_ms,
        requested,
        granted,
        used,
        denied: lease.prior_denied.clone(),
        unused,
        deciders: lease.deciders.clone(),
        policy_decisions: lease.prior_policy_decisions.clone(),
        diagnostics: Vec::new(),
        executor_invoked: false,
    }
}
