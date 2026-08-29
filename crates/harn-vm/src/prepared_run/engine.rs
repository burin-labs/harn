use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::harness_net::NetPolicyDecision;
use crate::orchestration::{PolicyEvaluation, ProcessSandboxPreset, ToolApprovalRequest};

use super::evidence::{
    approval_batch, diagnostic, fingerprint, persist_terminally, policy_evidence,
    receipted_requirements, requirement_attenuates,
};
use super::*;

#[async_trait]
pub trait PreparedRunExecutor: Send + Sync {
    type Output;
    /// Host-owned failure evidence returned without rendering or erasure.
    type Error;

    /// Perform the run. Every material operation must call
    /// AuthorityUse::authorize immediately before its side effect.
    async fn execute(&self, authority: &AuthorityUse) -> Result<Self::Output, Self::Error>;
}

pub struct PreparedRun<E> {
    pub(super) executor: E,
    pub(super) receipts: Arc<dyn AuthorityReceiptSink>,
    pub(super) now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub(super) identity_brokers: IdentityBrokerRegistry,
    pub(super) identity_consumer: SecretConsumerBinding,
}

impl<E> PreparedRun<E> {
    pub fn new(executor: E, receipts: Arc<dyn AuthorityReceiptSink>) -> Self {
        Self {
            executor,
            receipts,
            now_ms: Arc::new(|| crate::clock_mock::now_ms().max(0) as u64),
            identity_brokers: IdentityBrokerRegistry::default(),
            identity_consumer: SecretConsumerBinding {
                kind: SecretConsumerKind::Provider,
                id: "prepared-run".to_string(),
                environment_name: None,
            },
        }
    }

    #[doc(hidden)]
    pub fn with_clock(
        executor: E,
        receipts: Arc<dyn AuthorityReceiptSink>,
        now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self {
            executor,
            receipts,
            now_ms,
            identity_brokers: IdentityBrokerRegistry::default(),
            identity_consumer: SecretConsumerBinding {
                kind: SecretConsumerKind::Provider,
                id: "prepared-run".to_string(),
                environment_name: None,
            },
        }
    }

    pub fn with_identity_brokers(
        mut self,
        brokers: IdentityBrokerRegistry,
        consumer: SecretConsumerBinding,
    ) -> Self {
        self.identity_brokers = brokers;
        self.identity_consumer = consumer;
        self
    }

    pub fn prepare(&self, intent: RunIntent, host_facts: HostFacts) -> PreparationOutcome {
        let now_ms = (self.now_ms)();
        let plan = plan_from_intent(intent);
        let plan_fingerprint = match fingerprint("harn prepared-run plan v1", &plan) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                return PreparationOutcome::Blocked {
                    diagnostics: vec![diagnostic(
                        "plan_serialization",
                        error,
                        None,
                        "Correct the typed run intent so it can be serialized canonically.",
                    )],
                    receipt: None,
                }
            }
        };
        if let Some(persistent_uri) = self.receipts.persistent_uri() {
            if persistent_uri != plan.receipt_uri {
                return PreparationOutcome::Blocked {
                    diagnostics: vec![diagnostic(
                        "receipt_location_mismatch",
                        format!(
                            "declared receipt URI '{}' does not match sink '{}'",
                            plan.receipt_uri, persistent_uri
                        ),
                        None,
                        "Bind the run intent to the exact durable receipt sink.",
                    )],
                    receipt: None,
                };
            }
        }
        let requested = receipted_requirements(&plan.requirements);
        let startup = RunAuthorityReceipt::startup(
            plan.intent_id.clone(),
            plan_fingerprint.clone(),
            requested.clone(),
            now_ms,
        );
        if let Err(error) = self.receipts.persist(&startup) {
            return PreparationOutcome::Blocked {
                diagnostics: vec![diagnostic(
                    "startup_receipt_persistence",
                    error,
                    None,
                    "Make the declared receipt location writable before retrying the run.",
                )],
                receipt: None,
            };
        }

        let mut diagnostics = validate_host_facts(&plan, &host_facts, now_ms);
        let mut policy_decisions = Vec::new();
        let mut approval_candidates = Vec::new();
        let mut deciders = BTreeMap::new();
        for authority in &requested {
            let evaluation = evaluate_requirement(
                host_facts.approval_policy.effective(),
                &host_facts.net_policy,
                &authority.requirement,
            );
            match evaluation {
                Ok(evaluation) => {
                    policy_decisions.push(policy_evidence(&authority.fingerprint, &evaluation));
                    if evaluation.is_deny() {
                        diagnostics.push(diagnostic(
                            "policy_denied",
                            evaluation.reason,
                            Some(authority.fingerprint.clone()),
                            "Change the requirement or the owning policy before retrying.",
                        ));
                    } else if evaluation.is_ask() {
                        approval_candidates.push((authority.clone(), evaluation));
                    } else {
                        deciders.insert(
                            authority.fingerprint.clone(),
                            AuthorityDecider::RuntimePolicy,
                        );
                    }
                }
                Err(error) => diagnostics.push(diagnostic(
                    "policy_evaluation",
                    error,
                    Some(authority.fingerprint.clone()),
                    "Correct the requirement so the canonical evaluator can classify it.",
                )),
            }
        }

        if !diagnostics.is_empty() {
            let mut receipt = startup;
            receipt.stage = AuthorityReceiptStage::Blocked;
            receipt.status = AuthorityReceiptStatus::Blocked;
            receipt.observed_at_ms = now_ms;
            receipt.policy_decisions = policy_decisions;
            receipt.diagnostics = diagnostics.clone();
            persist_terminally(&*self.receipts, &mut diagnostics, &receipt);
            return PreparationOutcome::Blocked {
                diagnostics,
                receipt: Some(receipt),
            };
        }

        let approval_batch = approval_batch(&plan_fingerprint, &approval_candidates);
        if let Some(batch) = approval_batch.as_ref() {
            let approved = host_facts.approved_batches.get(&batch.batch_fingerprint);
            if approved.is_none() {
                if host_facts.approval_policy.posture().approval_availability
                    == ApprovalAvailability::Unavailable
                {
                    let mut diagnostics = vec![diagnostic(
                        "approval_unavailable",
                        "the prepared run requires approval but this host cannot obtain one",
                        None,
                        "Run interactively or provide a host policy that can decide the complete batch.",
                    )];
                    let mut receipt = startup;
                    receipt.stage = AuthorityReceiptStage::NeedsApproval;
                    receipt.status = AuthorityReceiptStatus::Blocked;
                    receipt.observed_at_ms = now_ms;
                    receipt.policy_decisions = policy_decisions;
                    receipt.diagnostics = diagnostics.clone();
                    persist_terminally(&*self.receipts, &mut diagnostics, &receipt);
                    return PreparationOutcome::Blocked {
                        diagnostics,
                        receipt: Some(receipt),
                    };
                }
                let mut receipt = startup;
                receipt.stage = AuthorityReceiptStage::NeedsApproval;
                receipt.status = AuthorityReceiptStatus::NeedsApproval;
                receipt.observed_at_ms = now_ms;
                receipt.policy_decisions = policy_decisions;
                if let Err(error) = self.receipts.persist(&receipt) {
                    return PreparationOutcome::Blocked {
                        diagnostics: vec![diagnostic(
                            "approval_receipt_persistence",
                            error,
                            None,
                            "Make receipt persistence healthy before presenting the approval.",
                        )],
                        receipt: Some(receipt),
                    };
                }
                return PreparationOutcome::NeedsApproval {
                    batched_requests: batch.clone(),
                    receipt,
                };
            }
            let decider = *approved.expect("checked above");
            for (authority, _) in &approval_candidates {
                deciders.insert(authority.fingerprint.clone(), decider);
            }
        }

        let expires_at_ms = plan
            .budget
            .time_ms
            .and_then(|duration| now_ms.checked_add(duration))
            .map_or(plan.startup_deadline_at_ms, |deadline| {
                deadline.min(plan.startup_deadline_at_ms)
            });
        let requirement_fingerprints = requested
            .iter()
            .map(|authority| (authority.fingerprint.clone(), authority.requirement.clone()))
            .collect::<BTreeMap<_, _>>();
        let lease_fingerprint = fingerprint(
            "harn authority lease v1",
            &(
                &plan_fingerprint,
                requirement_fingerprints.keys().collect::<Vec<_>>(),
                &deciders,
                host_facts.approval_policy.posture(),
                now_ms,
                expires_at_ms,
            ),
        )
        .expect("authority lease inputs are serializable");
        let lease = AuthorityLease {
            lease_fingerprint: lease_fingerprint.clone(),
            plan_fingerprint,
            plan,
            requested_fingerprints: requirement_fingerprints.clone(),
            requirement_fingerprints,
            approval_policy: host_facts.approval_policy,
            net_policy: host_facts.net_policy,
            deciders: deciders.clone(),
            prior_used: BTreeSet::new(),
            prior_denied: Vec::new(),
            prior_policy_decisions: Vec::new(),
            invalidated: None,
            expires_at_ms,
        };
        let mut receipt = startup;
        receipt.stage = AuthorityReceiptStage::Ready;
        receipt.status = AuthorityReceiptStatus::Ready;
        receipt.lease_fingerprint = Some(lease_fingerprint);
        receipt.observed_at_ms = now_ms;
        receipt.granted = requested;
        receipt.deciders = deciders;
        receipt.policy_decisions = policy_decisions;
        if let Err(error) = self.receipts.persist(&receipt) {
            return PreparationOutcome::Blocked {
                diagnostics: vec![diagnostic(
                    "ready_receipt_persistence",
                    error,
                    None,
                    "Make receipt persistence healthy before executing the lease.",
                )],
                receipt: Some(receipt),
            };
        }
        PreparationOutcome::Ready {
            authority_lease: Box::new(lease),
            receipt,
        }
    }

    pub fn request_delta(
        &self,
        authority_lease: &AuthorityLease,
        requirement: AuthorityRequirement,
    ) -> LeaseDeltaOutcome {
        let requirement_fingerprint = requirement_fingerprint(&requirement);
        if authority_lease
            .requirement_fingerprints
            .contains_key(&requirement_fingerprint)
        {
            return LeaseDeltaOutcome::Covered;
        }
        if !authority_lease
            .requirement_fingerprints
            .values()
            .any(|granted| requirement_attenuates(granted, &requirement))
        {
            return LeaseDeltaOutcome::Blocked(diagnostic(
                "delta_widens_parent",
                "the requested authority is not an attenuation of the parent lease",
                Some(requirement_fingerprint),
                "Prepare a new run for widened authority; deltas may only narrow a parent lease.",
            ));
        }
        match evaluate_requirement(
            authority_lease.approval_policy.effective(),
            &authority_lease.net_policy,
            &requirement,
        ) {
            Ok(evaluation) if !evaluation.is_deny() => {
                LeaseDeltaOutcome::Attenuated(AuthorityLeaseDelta {
                    parent_lease_fingerprint: authority_lease.lease_fingerprint.clone(),
                    requirement,
                    requirement_fingerprint,
                    expires_at_ms: authority_lease.expires_at_ms,
                })
            }
            Ok(evaluation) => LeaseDeltaOutcome::Blocked(diagnostic(
                "delta_policy_denied",
                evaluation.reason,
                Some(requirement_fingerprint),
                "Narrow the delta or change the owning policy.",
            )),
            Err(error) => LeaseDeltaOutcome::Blocked(diagnostic(
                "delta_policy_evaluation",
                error,
                Some(requirement_fingerprint),
                "Correct the delta so the canonical evaluator can classify it.",
            )),
        }
    }
}

impl<E: PreparedRunExecutor> PreparedRun<E> {
    pub async fn execute(
        &self,
        authority_lease: Box<AuthorityLease>,
    ) -> ExecutionOutcome<E::Output, E::Error> {
        let now_ms = (self.now_ms)();
        let invalidated = authority_lease.invalidated.clone();
        let expired = now_ms > authority_lease.expires_at_ms;
        let authority = AuthorityUse::new(authority_lease, self.now_ms.clone());
        let result = if let Some(diagnostic) = &invalidated {
            Err(format!(
                "authority lease invalidated during discovery: {}",
                diagnostic.message
            ))
        } else if expired {
            Err("authority lease expired before execution".to_string())
        } else {
            authority.mark_executor_invoked();
            Ok(scope_prepared_identity(
                authority.clone(),
                self.identity_brokers.clone(),
                self.identity_consumer.clone(),
                self.executor.execute(&authority),
            )
            .await)
        };
        let succeeded = matches!(result, Ok(Ok(_)));
        let mut receipt = authority.terminal_receipt(succeeded, (self.now_ms)());
        let persistence_error = self.receipts.persist(&receipt).err();
        if let Some(error) = &persistence_error {
            receipt.status = AuthorityReceiptStatus::Failed;
            receipt.diagnostics.push(diagnostic(
                "terminal_receipt_persistence",
                error,
                None,
                "Repair receipt persistence and inspect the retained startup receipt.",
            ));
        }
        match result {
            Ok(Ok(output)) if persistence_error.is_none() => {
                ExecutionOutcome::Completed { output, receipt }
            }
            Ok(Ok(_)) => ExecutionOutcome::AuthorityFailed {
                error: "execution completed but terminal authority receipt was not persisted"
                    .to_string(),
                receipt,
            },
            Ok(Err(error)) => ExecutionOutcome::ExecutorFailed { error, receipt },
            Err(error) => ExecutionOutcome::AuthorityFailed { error, receipt },
        }
    }
}

/// The terminal result of a prepared run.
///
/// Executor failures preserve the executor's concrete error type. Failures in
/// lease validation or receipt persistence remain separate because Harn, not
/// the executor, owns those errors.
pub enum ExecutionOutcome<T, E> {
    Completed {
        output: T,
        receipt: RunAuthorityReceipt,
    },
    ExecutorFailed {
        error: E,
        receipt: RunAuthorityReceipt,
    },
    AuthorityFailed {
        error: String,
        receipt: RunAuthorityReceipt,
    },
}

#[derive(Clone)]
pub struct AuthorityUse {
    lease: Arc<AuthorityLease>,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    state: Arc<std::sync::Mutex<AuthorityUseState>>,
}

struct AuthorityUseState {
    dynamic_requirements: BTreeMap<String, AuthorityRequirement>,
    dynamic_deciders: BTreeMap<String, AuthorityDecider>,
    used: BTreeSet<String>,
    denied: Vec<DeniedAuthority>,
    policy_decisions: Vec<PolicyDecisionEvidence>,
    executor_invoked: bool,
}

impl AuthorityUse {
    pub(super) fn new(
        lease: Box<AuthorityLease>,
        now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        let lease: Arc<AuthorityLease> = Arc::from(lease);
        Self {
            lease: lease.clone(),
            now_ms,
            state: Arc::new(std::sync::Mutex::new(AuthorityUseState {
                dynamic_requirements: BTreeMap::new(),
                dynamic_deciders: BTreeMap::new(),
                used: lease.prior_used.clone(),
                denied: lease.prior_denied.clone(),
                policy_decisions: lease.prior_policy_decisions.clone(),
                executor_invoked: false,
            })),
        }
    }

    pub fn authorize(&self, requirement: &AuthorityRequirement) -> Result<(), String> {
        let granted_fingerprint = self.check(requirement)?;
        self.mark_used(granted_fingerprint);
        Ok(())
    }

    pub(crate) fn check(&self, requirement: &AuthorityRequirement) -> Result<String, String> {
        let requested_fingerprint = requirement_fingerprint(requirement);
        if (self.now_ms)() > self.lease.expires_at_ms {
            return self.deny(
                requirement,
                requested_fingerprint,
                "authority lease expired before use".to_string(),
            );
        }
        let dynamic_requirements = self
            .state
            .lock()
            .expect("authority use state poisoned")
            .dynamic_requirements
            .clone();
        let granted_fingerprint = self
            .lease
            .requirement_fingerprints
            .iter()
            .chain(dynamic_requirements.iter())
            .find_map(|(fingerprint, granted)| {
                (granted == requirement || requirement_attenuates(granted, requirement))
                    .then(|| fingerprint.clone())
            });
        let Some(granted_fingerprint) = granted_fingerprint else {
            return self.deny(
                requirement,
                requested_fingerprint,
                "requested use is outside the fingerprinted authority lease".to_string(),
            );
        };
        let evaluation = match evaluate_requirement(
            self.lease.approval_policy.effective(),
            &self.lease.net_policy,
            requirement,
        ) {
            Ok(evaluation) => evaluation,
            Err(error) => return self.deny(requirement, requested_fingerprint, error),
        };
        self.state
            .lock()
            .expect("authority use state poisoned")
            .policy_decisions
            .push(policy_evidence(&requested_fingerprint, &evaluation));
        if evaluation.is_deny() {
            return self.deny(requirement, requested_fingerprint, evaluation.reason);
        }
        if evaluation.is_ask()
            && !self.lease.deciders.contains_key(&granted_fingerprint)
            && !self
                .state
                .lock()
                .expect("authority use state poisoned")
                .dynamic_deciders
                .contains_key(&granted_fingerprint)
        {
            return self.deny(
                requirement,
                requested_fingerprint,
                "canonical policy still requires approval and the lease has no decider".to_string(),
            );
        }
        Ok(granted_fingerprint)
    }

    pub(crate) fn mark_used(&self, granted_fingerprint: String) {
        self.state
            .lock()
            .expect("authority use state poisoned")
            .used
            .insert(granted_fingerprint);
    }

    pub(super) fn grant_delta(&self, delta: &AuthorityLeaseDelta, decider: AuthorityDecider) {
        let mut state = self.state.lock().expect("authority use state poisoned");
        state.dynamic_requirements.insert(
            delta.requirement_fingerprint.clone(),
            delta.requirement.clone(),
        );
        state
            .dynamic_deciders
            .insert(delta.requirement_fingerprint.clone(), decider);
    }

    pub(crate) fn mark_executor_invoked(&self) {
        self.state
            .lock()
            .expect("authority use state poisoned")
            .executor_invoked = true;
    }

    pub(crate) fn identity_requirements(&self) -> Vec<IdentityBrokerRequirement> {
        let mut requirements = self
            .lease
            .requirement_fingerprints
            .values()
            .filter_map(|requirement| match requirement {
                AuthorityRequirement::IdentityBroker(identity) => Some(identity.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        requirements.extend(
            self.state
                .lock()
                .expect("authority use state poisoned")
                .dynamic_requirements
                .values()
                .filter_map(|requirement| match requirement {
                    AuthorityRequirement::IdentityBroker(identity) => Some(identity.clone()),
                    _ => None,
                }),
        );
        requirements
    }

    pub(crate) fn now_ms(&self) -> u64 {
        (self.now_ms)()
    }

    pub(super) fn lease(&self) -> &AuthorityLease {
        &self.lease
    }

    pub(crate) fn record_denial(
        &self,
        requirement: &IdentityBrokerRequirement,
        reason: impl Into<String>,
    ) {
        let authority = AuthorityRequirement::IdentityBroker(requirement.clone());
        let _ = self.deny::<()>(
            &authority,
            requirement_fingerprint(&authority),
            reason.into(),
        );
    }

    fn deny<T>(
        &self,
        requirement: &AuthorityRequirement,
        fingerprint: String,
        reason: String,
    ) -> Result<T, String> {
        self.state
            .lock()
            .expect("authority use state poisoned")
            .denied
            .push(DeniedAuthority {
                authority: ReceiptedAuthority {
                    fingerprint,
                    requirement: requirement.clone(),
                },
                reason: reason.clone(),
                decider: AuthorityDecider::RuntimePolicy,
            });
        Err(reason)
    }

    pub(super) fn terminal_receipt(
        &self,
        completed: bool,
        observed_at_ms: u64,
    ) -> RunAuthorityReceipt {
        let state = self.state.lock().expect("authority use state poisoned");
        let requested = receipted_requirements(
            &self
                .lease
                .requested_fingerprints
                .values()
                .cloned()
                .collect::<Vec<_>>(),
        );
        let mut granted_requirements = self
            .lease
            .requirement_fingerprints
            .values()
            .cloned()
            .collect::<Vec<_>>();
        granted_requirements.extend(state.dynamic_requirements.values().cloned());
        let granted = receipted_requirements(&granted_requirements);
        let used = granted
            .iter()
            .filter(|authority| state.used.contains(&authority.fingerprint))
            .cloned()
            .collect::<Vec<_>>();
        let unused = granted
            .iter()
            .filter(|authority| !state.used.contains(&authority.fingerprint))
            .cloned()
            .collect::<Vec<_>>();
        RunAuthorityReceipt {
            schema: RUN_AUTHORITY_RECEIPT_SCHEMA.to_string(),
            stage: AuthorityReceiptStage::Terminal,
            status: if completed {
                AuthorityReceiptStatus::Completed
            } else {
                AuthorityReceiptStatus::Failed
            },
            intent_id: self.lease.plan.intent_id.clone(),
            plan_fingerprint: self.lease.plan_fingerprint.clone(),
            lease_fingerprint: Some(self.lease.lease_fingerprint.clone()),
            observed_at_ms,
            requested,
            granted,
            used,
            denied: state.denied.clone(),
            unused,
            deciders: self
                .lease
                .deciders
                .iter()
                .chain(state.dynamic_deciders.iter())
                .map(|(fingerprint, decider)| (fingerprint.clone(), *decider))
                .collect(),
            policy_decisions: state.policy_decisions.clone(),
            diagnostics: Vec::new(),
            executor_invoked: state.executor_invoked,
        }
    }
}

fn plan_from_intent(mut intent: RunIntent) -> RunAuthorityPlanV1 {
    normalize_policy(&mut intent.capability_policy);
    intent.network.sort();
    intent.network.dedup();
    intent.secrets.sort();
    intent.secrets.dedup();
    intent.admitted_environment.sort();
    intent.admitted_environment.dedup();
    intent.process_sockets.sort();
    intent.process_sockets.dedup();
    intent.mcp.sort();
    intent.mcp.dedup();
    intent.toolchain_probes.sort();
    intent.toolchain_probes.dedup();
    intent.identity_brokers.sort();
    intent.identity_brokers.dedup();
    let mut requirements = policy_requirements(&intent.capability_policy);
    requirements.extend(
        intent
            .network
            .iter()
            .cloned()
            .map(AuthorityRequirement::Network),
    );
    requirements.extend(
        intent
            .secrets
            .iter()
            .cloned()
            .map(AuthorityRequirement::Secret),
    );
    requirements.extend(
        intent
            .admitted_environment
            .iter()
            .cloned()
            .map(|name| AuthorityRequirement::Environment { name }),
    );
    requirements.extend(
        intent
            .process_sockets
            .iter()
            .cloned()
            .map(AuthorityRequirement::ProcessSocket),
    );
    requirements.extend(intent.mcp.iter().cloned().map(AuthorityRequirement::Mcp));
    requirements.extend(
        intent
            .toolchain_probes
            .iter()
            .cloned()
            .map(AuthorityRequirement::ToolchainProbe),
    );
    requirements.extend(
        intent
            .identity_brokers
            .iter()
            .cloned()
            .map(AuthorityRequirement::IdentityBroker),
    );
    requirements.push(AuthorityRequirement::Budget {
        budget: intent.budget.clone(),
    });
    requirements.push(AuthorityRequirement::Provenance {
        provenance: intent.provenance.clone(),
    });
    requirements.push(AuthorityRequirement::Startup {
        deadline_at_ms: intent.startup_deadline_at_ms,
        receipt_uri: intent.receipt_uri.clone(),
    });
    requirements.sort_by_key(requirement_fingerprint);
    requirements.dedup();
    RunAuthorityPlanV1 {
        schema: RUN_AUTHORITY_PLAN_SCHEMA.to_string(),
        intent_id: intent.intent_id.trim().to_string(),
        capability_policy: intent.capability_policy,
        requirements,
        budget: intent.budget,
        provenance: intent.provenance,
        interactivity: intent.interactivity,
        startup_deadline_at_ms: intent.startup_deadline_at_ms,
        receipt_uri: intent.receipt_uri,
    }
}

fn normalize_policy(policy: &mut crate::orchestration::CapabilityPolicy) {
    sort_dedup(&mut policy.tools);
    sort_dedup(&mut policy.workspace_roots);
    sort_dedup(&mut policy.read_only_roots);
    for operations in policy.capabilities.values_mut() {
        sort_dedup(operations);
    }
    sort_dedup(&mut policy.process_sandbox.read_roots);
    sort_dedup(&mut policy.process_sandbox.write_roots);
    if let Some(presets) = policy.process_sandbox.presets.as_mut() {
        presets.sort();
        presets.dedup();
    }
}

fn sort_dedup(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn policy_requirements(
    policy: &crate::orchestration::CapabilityPolicy,
) -> Vec<AuthorityRequirement> {
    let mut requirements = Vec::new();
    requirements.extend(
        policy
            .read_only_roots
            .iter()
            .cloned()
            .map(|root| AuthorityRequirement::FilesystemRead { root }),
    );
    requirements.extend(
        policy
            .workspace_roots
            .iter()
            .cloned()
            .map(|root| AuthorityRequirement::FilesystemWrite { root }),
    );
    requirements.extend(
        policy
            .process_sandbox
            .read_roots
            .iter()
            .cloned()
            .map(|root| AuthorityRequirement::ProcessReadRoot { root }),
    );
    requirements.extend(
        policy
            .process_sandbox
            .write_roots
            .iter()
            .cloned()
            .map(|root| AuthorityRequirement::ProcessWriteRoot { root }),
    );
    let presets = policy.process_sandbox.effective_presets();
    let presets = if presets.is_empty() {
        vec!["none".to_string()]
    } else {
        presets
            .into_iter()
            .map(preset_name)
            .map(str::to_string)
            .collect()
    };
    requirements.extend(
        presets
            .into_iter()
            .map(|preset| AuthorityRequirement::ProcessSandbox {
                profile: policy.sandbox_profile.as_str().to_string(),
                preset,
            }),
    );
    requirements.extend(
        policy
            .tools
            .iter()
            .cloned()
            .map(|pattern| AuthorityRequirement::Tool { pattern }),
    );
    for (capability, operations) in &policy.capabilities {
        let operations = if operations.is_empty() {
            vec!["*".to_string()]
        } else {
            operations.clone()
        };
        requirements.extend(operations.into_iter().map(|operation| {
            AuthorityRequirement::HostCapability {
                capability: capability.clone(),
                operation,
            }
        }));
    }
    if let Some(level) = &policy.side_effect_level {
        requirements.push(AuthorityRequirement::SideEffectCeiling {
            level: level.clone(),
        });
    }
    if let Some(depth) = policy.recursion_limit {
        requirements.push(AuthorityRequirement::RecursionLimit { depth });
    }
    requirements
}

fn preset_name(preset: ProcessSandboxPreset) -> &'static str {
    match preset {
        ProcessSandboxPreset::SystemRuntime => "system_runtime",
        ProcessSandboxPreset::DeveloperToolchains => "developer_toolchains",
        ProcessSandboxPreset::PackageManagerConfig => "package_manager_config",
        ProcessSandboxPreset::UserTemp => "user_temp",
    }
}

fn validate_host_facts(
    plan: &RunAuthorityPlanV1,
    host: &HostFacts,
    now_ms: u64,
) -> Vec<AuthorityDiagnostic> {
    let mut diagnostics = Vec::new();
    if host.approval_policy.posture().interactivity != plan.interactivity {
        diagnostics.push(diagnostic(
            "approval_policy_posture_mismatch",
            "approval policy was constructed for a different run interactivity posture",
            None,
            "Construct the approval policy from the same typed posture as the run intent.",
        ));
    }
    if plan.intent_id.is_empty() {
        diagnostics.push(diagnostic(
            "empty_intent_id",
            "run intent id is empty",
            None,
            "Provide a stable non-empty intent id.",
        ));
    }
    if plan.receipt_uri.trim().is_empty() {
        diagnostics.push(diagnostic(
            "missing_receipt_uri",
            "run intent does not declare a receipt location",
            None,
            "Declare the durable startup receipt location.",
        ));
    }
    if now_ms > plan.startup_deadline_at_ms {
        diagnostics.push(diagnostic(
            "startup_deadline_expired",
            format!(
                "startup deadline {} expired before preparation at {now_ms}",
                plan.startup_deadline_at_ms
            ),
            None,
            "Create a fresh run intent with a future startup deadline.",
        ));
    }
    if plan.provenance != host.provenance {
        diagnostics.push(diagnostic(
            "runtime_provenance_mismatch",
            provenance_mismatch(&plan.provenance, &host.provenance),
            None,
            "Install the exact expected runtime and contracts, then prepare again.",
        ));
    }
    let missing_provenance = missing_provenance_fields(&plan.provenance);
    if !missing_provenance.is_empty() {
        diagnostics.push(diagnostic(
            "incomplete_runtime_provenance",
            format!(
                "runtime provenance is missing: {}",
                missing_provenance.join(", ")
            ),
            None,
            "Populate every runtime and contract provenance field from observed build facts.",
        ));
    }
    if let Err(error) = host
        .capability_ceiling
        .assert_within_ceiling(&plan.capability_policy)
    {
        diagnostics.push(diagnostic(
            "capability_ceiling",
            error,
            None,
            "Narrow the workflow capability policy or select a capable host.",
        ));
    }
    for dimension in plan.budget.exceeds(&host.budget_ceiling) {
        diagnostics.push(diagnostic(
            "budget_ceiling",
            format!("requested {dimension} exceeds the host ceiling"),
            None,
            "Reduce the requested budget or select a host with a larger ceiling.",
        ));
    }
    let missing_budget = plan.budget.missing_dimensions();
    if !missing_budget.is_empty() {
        diagnostics.push(diagnostic(
            "incomplete_run_budget",
            format!("run budget is missing: {}", missing_budget.join(", ")),
            None,
            "Declare explicit spend, time, and turn budgets before preparation.",
        ));
    }
    for requirement in &plan.requirements {
        let fingerprint = Some(requirement_fingerprint(requirement));
        match requirement {
            AuthorityRequirement::Environment { name }
                if !host.admitted_environment.contains(name) =>
            {
                diagnostics.push(diagnostic(
                    "environment_not_admitted",
                    format!("environment name '{name}' is not admitted by the host"),
                    fingerprint,
                    "Remove the environment name or admit it explicitly at the host seam.",
                ));
            }
            AuthorityRequirement::ProcessSocket(socket)
                if !host.process_sockets.contains(socket) =>
            {
                diagnostics.push(diagnostic(
                    "process_socket_unavailable",
                    format!("process socket {socket:?} is not available to this host"),
                    fingerprint,
                    "Remove the socket need or select a host that can confine it.",
                ));
            }
            AuthorityRequirement::Secret(secret) => {
                if !host.secret_bindings.contains(secret) {
                    diagnostics.push(diagnostic(
                        "secret_consumer_binding",
                        format!(
                            "secret reference '{}' is not bound to consumer {:?}:{}",
                            secret.reference, secret.consumer.kind, secret.consumer.id
                        ),
                        fingerprint.clone(),
                        "Declare the exact value-free secret-to-consumer binding.",
                    ));
                }
                match host.secret_brokers.get(&secret.source) {
                    Some(broker) if !broker.outside_sandbox => diagnostics.push(diagnostic(
                        "secret_broker_inside_sandbox",
                        "durable secret broker is not isolated from the workload sandbox",
                        fingerprint.clone(),
                        "Use a host-side broker that returns only a consumer-scoped handle.",
                    )),
                    Some(broker)
                        if plan.interactivity == RunInteractivity::NonInteractive
                            && (!broker.supports_non_interactive || broker.may_prompt_gui) =>
                    {
                        diagnostics.push(diagnostic(
                            "gui_keyring_forbidden",
                            "non-interactive runs cannot use a broker that may invoke GUI-capable keyring APIs",
                            fingerprint.clone(),
                            "Stage an env/dotenv value in MemorySecretProvider or use a non-interactive host broker.",
                        ));
                    }
                    Some(broker)
                        if secret.source == SecretSourceKind::ProcessLocal
                            && !broker.zeroizing_handles =>
                    {
                        diagnostics.push(diagnostic(
                            "process_local_secret_not_zeroizing",
                            "process-local secret broker does not provide zeroizing handles",
                            fingerprint.clone(),
                            "Stage env and dotenv values in MemorySecretProvider before execution.",
                        ));
                    }
                    None => diagnostics.push(diagnostic(
                        "secret_broker_unavailable",
                        format!(
                            "no broker is available for secret source {:?}",
                            secret.source
                        ),
                        fingerprint.clone(),
                        "Configure a process-local or durable host broker before preparation.",
                    )),
                    _ => {}
                }
            }
            AuthorityRequirement::Mcp(mcp) if !host.mcp.contains(mcp) => {
                diagnostics.push(diagnostic(
                    "mcp_capability_unavailable",
                    format!("MCP capability '{}:{}' is unavailable", mcp.server, mcp.tool),
                    fingerprint,
                    "Remove the MCP need or connect a host with that exact tool and side-effect ceiling.",
                ));
            }
            AuthorityRequirement::ToolchainProbe(probe) => {
                if !host.toolchain_probes.contains(probe) {
                    diagnostics.push(diagnostic(
                        "toolchain_probe_unavailable",
                        format!("toolchain probe '{}' is not available with the exact declared command and root ceiling", probe.probe_id),
                        fingerprint.clone(),
                        "Declare the exact host-supported probe or remove command-derived toolchain discovery.",
                    ));
                }
                if probe.probe_id.trim().is_empty()
                    || probe.executable.trim().is_empty()
                    || !Path::new(&probe.working_directory).is_absolute()
                    || !Path::new(&probe.read_root_ceiling).is_absolute()
                {
                    diagnostics.push(diagnostic(
                        "invalid_toolchain_probe",
                        "toolchain probes require a stable id, executable, absolute working directory, and absolute read-root ceiling",
                        fingerprint,
                        "Normalize the probe at the host seam before preparing the run.",
                    ));
                }
            }
            AuthorityRequirement::IdentityBroker(identity) => {
                validate_identity_broker(plan, host, identity, fingerprint, &mut diagnostics);
            }
            _ => {}
        }
    }
    diagnostics
}

fn validate_identity_broker(
    plan: &RunAuthorityPlanV1,
    host: &HostFacts,
    identity: &IdentityBrokerRequirement,
    fingerprint: Option<String>,
    diagnostics: &mut Vec<AuthorityDiagnostic>,
) {
    let Some(broker) = host.identity_brokers.get(&identity.broker_id) else {
        diagnostics.push(diagnostic(
            "identity_broker_unavailable",
            format!("identity broker '{}' is unavailable", identity.broker_id),
            fingerprint,
            "Configure the exact consumer-bound broker before preparation.",
        ));
        return;
    };
    if broker.broker_id != identity.broker_id {
        diagnostics.push(diagnostic(
            "identity_broker_mismatch",
            "host identity broker facts do not match the requested broker id",
            fingerprint.clone(),
            "Publish truthful broker identity facts at the host seam.",
        ));
    }
    if !broker.material_outside_sandbox || !broker.opaque_process_local_handles {
        diagnostics.push(diagnostic(
            "identity_broker_sandbox_boundary",
            "identity material is not host-isolated behind opaque process-local handles",
            fingerprint.clone(),
            "Use a broker that keeps durable material outside the workload sandbox.",
        ));
    }
    if plan.interactivity == RunInteractivity::NonInteractive
        && (!broker.supports_non_interactive || broker.may_prompt_gui)
    {
        diagnostics.push(diagnostic(
            "identity_broker_interactivity",
            "non-interactive runs cannot use an identity broker that may require GUI interaction",
            fingerprint.clone(),
            "Use a non-interactive workload or hosted identity broker.",
        ));
    }
    if !broker.sources.contains(&identity.source) {
        diagnostics.push(diagnostic(
            "identity_source_mismatch",
            format!(
                "broker '{}' does not advertise source {:?}",
                identity.broker_id, identity.source
            ),
            fingerprint.clone(),
            "Select a broker that advertises the exact identity source.",
        ));
    }
    if !broker.renewal_modes.contains(&identity.renewal) {
        diagnostics.push(diagnostic(
            "identity_renewal_mismatch",
            format!(
                "broker '{}' does not advertise renewal mode {:?}",
                identity.broker_id, identity.renewal
            ),
            fingerprint.clone(),
            "Select a compatible broker renewal mode.",
        ));
    }
    if !broker.bindings.contains(&identity.binding) {
        diagnostics.push(diagnostic(
            "identity_consumer_binding",
            format!(
                "identity reference '{}' is not bound by broker '{}' to provider '{}' audience '{}' tenant {:?} consumer {:?}:{}",
                identity.reference,
                identity.broker_id,
                identity.binding.provider,
                identity.binding.audience,
                identity.binding.tenant,
                identity.binding.consumer.kind,
                identity.binding.consumer.id,
            ),
            fingerprint,
            "Declare the exact broker/provider/audience/tenant/consumer binding.",
        ));
    }
}

fn missing_provenance_fields(provenance: &RuntimeContractProvenance) -> Vec<&'static str> {
    [
        ("harn_version", provenance.harn_version.as_str()),
        ("harn_revision", provenance.harn_revision.as_str()),
        ("host_name", provenance.host_name.as_str()),
        ("host_version", provenance.host_version.as_str()),
        ("host_revision", provenance.host_revision.as_str()),
        ("contracts_version", provenance.contracts_version.as_str()),
        ("runtime_digest", provenance.runtime_digest.as_str()),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.trim().is_empty().then_some(name))
    .collect()
}

fn provenance_mismatch(
    expected: &RuntimeContractProvenance,
    observed: &RuntimeContractProvenance,
) -> String {
    let fields = [
        (
            "harn_version",
            &expected.harn_version,
            &observed.harn_version,
        ),
        (
            "harn_revision",
            &expected.harn_revision,
            &observed.harn_revision,
        ),
        ("host_name", &expected.host_name, &observed.host_name),
        (
            "host_version",
            &expected.host_version,
            &observed.host_version,
        ),
        (
            "host_revision",
            &expected.host_revision,
            &observed.host_revision,
        ),
        (
            "contracts_version",
            &expected.contracts_version,
            &observed.contracts_version,
        ),
        (
            "runtime_digest",
            &expected.runtime_digest,
            &observed.runtime_digest,
        ),
    ];
    let mismatched = fields
        .into_iter()
        .filter(|(_, expected, observed)| expected != observed)
        .map(|(name, expected, observed)| {
            format!("{name}: expected '{expected}', observed '{observed}'")
        })
        .collect::<Vec<_>>();
    format!("runtime provenance mismatch ({})", mismatched.join("; "))
}

pub(super) fn evaluate_requirement(
    approval_policy: &crate::orchestration::ToolApprovalPolicy,
    net_policy: &crate::harness_net::NetPolicy,
    requirement: &AuthorityRequirement,
) -> Result<PolicyEvaluation, String> {
    if let AuthorityRequirement::Network(network) = requirement {
        match net_policy
            .evaluate("CONNECT", &network.url())
            .map_err(|error| error.to_string())?
        {
            NetPolicyDecision::Allow { .. } => {}
            NetPolicyDecision::Deny { audit, .. } => {
                return Ok(PolicyEvaluation {
                    action: "deny".to_string(),
                    reason: format!(
                        "network policy denied before endpoint health: {}",
                        audit.reason
                    ),
                    matched_rule: None,
                    required_approval: None,
                    risk_labels: vec!["network_policy".to_string()],
                    receipt: json!({
                        "type": "harn.permission_policy_decision.v1",
                        "action": "deny",
                        "reason": audit.reason,
                        "policy_source": "harn.net_policy",
                    }),
                });
            }
        }
    }
    Ok(approval_policy.evaluate_request(&policy_request(requirement)))
}

fn policy_request(requirement: &AuthorityRequirement) -> ToolApprovalRequest {
    let (tool_name, arguments) = match requirement {
        AuthorityRequirement::FilesystemRead { root } => (
            "prepared_run.filesystem",
            json!({"path": root, "access": "read"}),
        ),
        AuthorityRequirement::FilesystemWrite { root } => (
            "prepared_run.filesystem",
            json!({"path": root, "access": "write"}),
        ),
        AuthorityRequirement::ProcessReadRoot { root } => (
            "prepared_run.process",
            json!({"path": root, "access": "read", "scope": "process"}),
        ),
        AuthorityRequirement::ProcessWriteRoot { root } => (
            "prepared_run.process",
            json!({"path": root, "access": "write", "scope": "process"}),
        ),
        AuthorityRequirement::ProcessSandbox { profile, preset } => (
            "prepared_run.process",
            json!({"profile": profile, "preset": preset}),
        ),
        AuthorityRequirement::ProcessSocket(socket) => (
            "prepared_run.process",
            json!({
                "socket": format!("{:?}", socket.socket_kind).to_ascii_lowercase(),
                "endpoint": socket.endpoint
            }),
        ),
        AuthorityRequirement::Network(network) => (
            "prepared_run.network",
            json!({
                "url": network.url(),
                "domain": network.destination,
                "protocol": network.protocol,
                "port": network.port,
                "method": "CONNECT"
            }),
        ),
        AuthorityRequirement::Secret(secret) => (
            "prepared_run.secret",
            json!({
                "secret_ref": secret.reference.as_str(),
                "source": secret.source,
                "consumer_kind": secret.consumer.kind,
                "consumer": secret.consumer.id,
                "env_mode": "replace"
            }),
        ),
        AuthorityRequirement::Environment { name } => (
            "prepared_run.environment",
            json!({"name": name, "env_mode": "replace"}),
        ),
        AuthorityRequirement::Tool { pattern } => ("prepared_run.tool", json!({"tool": pattern})),
        AuthorityRequirement::HostCapability {
            capability,
            operation,
        } => (
            "prepared_run.capability",
            json!({"capability": format!("{capability}.{operation}")}),
        ),
        AuthorityRequirement::SideEffectCeiling { level } => {
            ("prepared_run.side_effect", json!({"side_effect": level}))
        }
        AuthorityRequirement::RecursionLimit { depth } => {
            ("prepared_run.budget", json!({"recursion_depth": depth}))
        }
        AuthorityRequirement::Mcp(mcp) => (
            "prepared_run.mcp",
            json!({
                "mcp_server": mcp.server,
                "mcp_tool": mcp.tool,
                "side_effect": mcp.side_effect
            }),
        ),
        AuthorityRequirement::ToolchainProbe(probe) => (
            "prepared_run.process",
            json!({
                "probe_id": probe.probe_id,
                "executable": probe.executable,
                "arguments": probe.arguments,
                "working_directory": probe.working_directory,
                "read_root_ceiling": probe.read_root_ceiling,
                "operation": "toolchain_discovery"
            }),
        ),
        AuthorityRequirement::IdentityBroker(identity) => (
            "prepared_run.identity",
            json!({
                "identity_ref": identity.reference.as_str(),
                "broker_id": identity.broker_id,
                "source": identity.source,
                "renewal": identity.renewal,
                "provider": identity.binding.provider,
                "audience": identity.binding.audience,
                "tenant": identity.binding.tenant,
                "consumer_kind": identity.binding.consumer.kind,
                "consumer": identity.binding.consumer.id,
            }),
        ),
        AuthorityRequirement::Budget { budget } => (
            "prepared_run.budget",
            serde_json::to_value(budget).expect("budget serializes"),
        ),
        AuthorityRequirement::Provenance { provenance } => (
            "prepared_run.provenance",
            serde_json::to_value(provenance).expect("provenance serializes"),
        ),
        AuthorityRequirement::Startup {
            deadline_at_ms,
            receipt_uri,
        } => (
            "prepared_run.startup",
            json!({"deadline_at_ms": deadline_at_ms, "receipt_uri": receipt_uri}),
        ),
    };
    ToolApprovalRequest {
        tool_name: tool_name.to_string(),
        arguments,
        ..Default::default()
    }
}
