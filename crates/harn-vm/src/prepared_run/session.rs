use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::*;

pub const PREPARED_SESSION_SCHEMA: &str = "harn.prepared_session.v1";
pub const PREPARED_SESSION_SCHEMA_ARTIFACT: &str = "schemas/prepared-session-v1.schema.json";
pub const PREPARED_SESSION_V1_SCHEMA_JSON: &str =
    include_str!("../../schemas/prepared-session-v1.schema.json");

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedSessionState {
    NeedsApproval,
    Ready,
    Blocked,
    Active,
    Delta,
    Stopped,
    Pivoted,
    Terminal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreparedSessionBindingV1 {
    pub session_id: String,
    pub workspace_fingerprint: String,
    pub runtime: RuntimeContractProvenance,
    pub consumer: SecretConsumerBinding,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreparedRuntimeAttachment {
    pub session_id: String,
    pub workspace_fingerprint: String,
    pub runtime: RuntimeContractProvenance,
    pub consumer: SecretConsumerBinding,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreparedSessionApprovalDecision {
    pub batch_fingerprint: String,
    pub approved: bool,
    pub decider: AuthorityDecider,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreparedSessionLeaseV1 {
    pub schema: String,
    pub session_id: String,
    pub session_fingerprint: String,
    pub plan_fingerprint: String,
    pub binding: PreparedSessionBindingV1,
    pub intent: RunIntent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<PreparedSessionApprovalDecision>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PreparedSessionUpdate {
    NeedsApproval {
        session_id: String,
        batch: ApprovalBatch,
        receipt: RunAuthorityReceipt,
    },
    Ready {
        session_id: String,
        lease: Box<PreparedSessionLeaseV1>,
        receipt: RunAuthorityReceipt,
    },
    Blocked {
        session_id: String,
        diagnostics: Vec<AuthorityDiagnostic>,
        #[serde(skip_serializing_if = "Option::is_none")]
        receipt: Option<RunAuthorityReceipt>,
    },
    Delta {
        session_id: String,
        outcome: PreparedSessionDelta,
    },
    Stopped {
        session_id: String,
        receipt: RunAuthorityReceipt,
    },
    Pivoted {
        session_id: String,
        receipt: RunAuthorityReceipt,
    },
    Terminal {
        session_id: String,
        receipt: RunAuthorityReceipt,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PreparedSessionDelta {
    Covered,
    Attenuated { delta: AuthorityLeaseDelta },
    NeedsApproval { batch: ApprovalBatch },
    Granted { delta: AuthorityLeaseDelta },
    Blocked { diagnostic: AuthorityDiagnostic },
}

struct PendingPreparedSessionDelta {
    delta: AuthorityLeaseDelta,
    batch: ApprovalBatch,
}

pub trait PreparedSessionLeaseStore: Send + Sync {
    /// Atomically claims a ready session lease. A second claim is a replay.
    fn claim(&self, session_fingerprint: &str) -> Result<(), String>;
    fn terminal(&self, session_fingerprint: &str) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct MemoryPreparedSessionLeaseStore {
    claimed: Mutex<BTreeSet<String>>,
    terminal: Mutex<BTreeSet<String>>,
}

impl PreparedSessionLeaseStore for MemoryPreparedSessionLeaseStore {
    fn claim(&self, session_fingerprint: &str) -> Result<(), String> {
        if !self
            .claimed
            .lock()
            .map_err(|_| "prepared-session claim store poisoned".to_string())?
            .insert(session_fingerprint.to_string())
        {
            return Err("prepared-session lease replayed".to_string());
        }
        Ok(())
    }

    fn terminal(&self, session_fingerprint: &str) -> Result<(), String> {
        self.terminal
            .lock()
            .map_err(|_| "prepared-session terminal store poisoned".to_string())?
            .insert(session_fingerprint.to_string());
        Ok(())
    }
}

#[derive(Debug)]
pub struct FilePreparedSessionLeaseStore {
    root: PathBuf,
}

impl FilePreparedSessionLeaseStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn marker(&self, fingerprint: &str, suffix: &str) -> PathBuf {
        self.root
            .join(format!("{}.{suffix}", fingerprint.replace(':', "_")))
    }

    fn create_marker(&self, path: &Path, fingerprint: &str) -> Result<(), String> {
        fs::create_dir_all(&self.root)
            .map_err(|error| format!("create prepared-session store: {error}"))?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    "prepared-session lease replayed".to_string()
                } else {
                    format!("create prepared-session marker: {error}")
                }
            })?;
        file.write_all(fingerprint.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("persist prepared-session marker: {error}"))
    }
}

impl PreparedSessionLeaseStore for FilePreparedSessionLeaseStore {
    fn claim(&self, session_fingerprint: &str) -> Result<(), String> {
        self.create_marker(
            &self.marker(session_fingerprint, "claimed"),
            session_fingerprint,
        )
    }

    fn terminal(&self, session_fingerprint: &str) -> Result<(), String> {
        self.create_marker(
            &self.marker(session_fingerprint, "terminal"),
            session_fingerprint,
        )
    }
}

struct PendingPreparedSession {
    intent: RunIntent,
    host_facts: HostFacts,
    binding: PreparedSessionBindingV1,
    batch: ApprovalBatch,
    receipt: RunAuthorityReceipt,
}

pub struct ActivePreparedSession {
    lease: PreparedSessionLeaseV1,
    authority: AuthorityUse,
}

impl ActivePreparedSession {
    pub fn lease(&self) -> &PreparedSessionLeaseV1 {
        &self.lease
    }

    /// Authorize one host-side operation against the reusable session
    /// envelope, including any approved semantic deltas.
    pub fn authorize(&self, requirement: &AuthorityRequirement) -> Result<(), String> {
        self.authority.authorize(requirement)
    }
}

/// Versioned host/session state machine. It owns preparation, one grouped
/// approval, replay-safe start/attach, routine turns, deltas, and terminal
/// accounting while reusing PreparedRun's canonical evaluator and receipts.
pub struct PreparedSession<E> {
    run: PreparedRun<E>,
    leases: Arc<dyn PreparedSessionLeaseStore>,
    pending: Mutex<BTreeMap<String, PendingPreparedSession>>,
    ready: Mutex<BTreeMap<String, Box<AuthorityLease>>>,
    pending_deltas: Mutex<BTreeMap<String, PendingPreparedSessionDelta>>,
}

impl<E> PreparedSession<E> {
    pub fn new(run: PreparedRun<E>, leases: Arc<dyn PreparedSessionLeaseStore>) -> Self {
        Self {
            run,
            leases,
            pending: Mutex::new(BTreeMap::new()),
            ready: Mutex::new(BTreeMap::new()),
            pending_deltas: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn prepare(
        &self,
        binding: PreparedSessionBindingV1,
        intent: RunIntent,
        host_facts: HostFacts,
    ) -> PreparedSessionUpdate {
        if binding.session_id.trim().is_empty() || binding.workspace_fingerprint.trim().is_empty() {
            return blocked(
                binding.session_id,
                "prepared_session_binding",
                "prepared sessions require a session id and workspace fingerprint",
                None,
            );
        }
        if binding.runtime != intent.provenance {
            return blocked(
                binding.session_id,
                "prepared_session_runtime_binding",
                "prepared-session runtime binding does not match the run intent",
                None,
            );
        }
        if binding.consumer != self.run.identity_consumer {
            return blocked(
                binding.session_id,
                "prepared_session_consumer_binding",
                "prepared-session consumer does not match the runtime identity consumer",
                None,
            );
        }
        match self.run.prepare(intent.clone(), host_facts.clone()) {
            PreparationOutcome::NeedsApproval {
                batched_requests,
                receipt,
            } => {
                self.pending
                    .lock()
                    .expect("prepared-session pending state poisoned")
                    .insert(
                        binding.session_id.clone(),
                        PendingPreparedSession {
                            intent,
                            host_facts,
                            binding: binding.clone(),
                            batch: batched_requests.clone(),
                            receipt: receipt.clone(),
                        },
                    );
                PreparedSessionUpdate::NeedsApproval {
                    session_id: binding.session_id,
                    batch: batched_requests,
                    receipt,
                }
            }
            PreparationOutcome::Ready {
                authority_lease,
                receipt,
            } => self.ready_update(binding, intent, None, authority_lease, receipt),
            PreparationOutcome::Blocked {
                diagnostics,
                receipt,
            } => PreparedSessionUpdate::Blocked {
                session_id: binding.session_id,
                diagnostics,
                receipt,
            },
        }
    }

    pub fn decide(
        &self,
        session_id: &str,
        decision: PreparedSessionApprovalDecision,
    ) -> PreparedSessionUpdate {
        let Some(mut pending) = self
            .pending
            .lock()
            .expect("prepared-session pending state poisoned")
            .remove(session_id)
        else {
            return blocked(
                session_id.to_string(),
                "prepared_session_not_waiting",
                "prepared session is not waiting for an approval decision",
                None,
            );
        };
        if decision.batch_fingerprint != pending.batch.batch_fingerprint {
            return blocked(
                session_id.to_string(),
                "prepared_session_approval_binding",
                "approval decision does not match the grouped request",
                Some(pending.receipt),
            );
        }
        let mut decision_receipt = pending.receipt.clone();
        decision_receipt.stage = AuthorityReceiptStage::ApprovalDecision;
        decision_receipt.observed_at_ms = (self.run.now_ms)();
        for fingerprint in pending
            .batch
            .groups
            .iter()
            .flat_map(|group| group.requirement_fingerprints.iter())
        {
            decision_receipt
                .deciders
                .insert(fingerprint.clone(), decision.decider);
        }
        if !decision.approved {
            decision_receipt.status = AuthorityReceiptStatus::Blocked;
            let diagnostic = AuthorityDiagnostic {
                code: "prepared_session_approval_denied".to_string(),
                message: "the grouped prepared-session authority request was denied".to_string(),
                requirement_fingerprint: None,
                actionable: "Narrow the run intent before preparing another session.".to_string(),
            };
            decision_receipt.diagnostics.push(diagnostic.clone());
            let _ = self.run.receipts.persist(&decision_receipt);
            return PreparedSessionUpdate::Blocked {
                session_id: session_id.to_string(),
                diagnostics: vec![diagnostic],
                receipt: Some(decision_receipt),
            };
        }
        decision_receipt.status = AuthorityReceiptStatus::Preparing;
        if let Err(error) = self.run.receipts.persist(&decision_receipt) {
            return blocked(
                session_id.to_string(),
                "prepared_session_decision_persistence",
                &error,
                Some(decision_receipt),
            );
        }
        pending
            .host_facts
            .approved_batches
            .insert(decision.batch_fingerprint.clone(), decision.decider);
        match self.run.prepare(pending.intent.clone(), pending.host_facts) {
            PreparationOutcome::Ready {
                authority_lease,
                receipt,
            } => self.ready_update(
                pending.binding,
                pending.intent,
                Some(decision),
                authority_lease,
                receipt,
            ),
            PreparationOutcome::NeedsApproval { receipt, .. } => blocked(
                session_id.to_string(),
                "prepared_session_reprompt",
                "one persisted grouped approval did not settle the prepared session",
                Some(receipt),
            ),
            PreparationOutcome::Blocked {
                diagnostics,
                receipt,
            } => PreparedSessionUpdate::Blocked {
                session_id: session_id.to_string(),
                diagnostics,
                receipt,
            },
        }
    }

    fn ready_update(
        &self,
        binding: PreparedSessionBindingV1,
        intent: RunIntent,
        approval: Option<PreparedSessionApprovalDecision>,
        authority_lease: Box<AuthorityLease>,
        receipt: RunAuthorityReceipt,
    ) -> PreparedSessionUpdate {
        let issued_at_ms = (self.run.now_ms)();
        let plan_fingerprint = authority_lease.plan_fingerprint().to_string();
        let expires_at_ms = authority_lease.expires_at_ms();
        let fingerprint_input = (
            PREPARED_SESSION_SCHEMA,
            &binding,
            &plan_fingerprint,
            &approval,
            issued_at_ms,
            expires_at_ms,
        );
        let session_fingerprint = fingerprint("harn prepared-session lease v1", &fingerprint_input)
            .expect("prepared-session lease inputs are serializable");
        let lease = PreparedSessionLeaseV1 {
            schema: PREPARED_SESSION_SCHEMA.to_string(),
            session_id: binding.session_id.clone(),
            session_fingerprint: session_fingerprint.clone(),
            plan_fingerprint,
            binding,
            intent,
            approval,
            issued_at_ms,
            expires_at_ms,
        };
        self.ready
            .lock()
            .expect("prepared-session ready state poisoned")
            .insert(session_fingerprint, authority_lease);
        PreparedSessionUpdate::Ready {
            session_id: lease.session_id.clone(),
            lease: Box::new(lease),
            receipt,
        }
    }

    pub fn attach(
        &self,
        lease: PreparedSessionLeaseV1,
        mut host_facts: HostFacts,
        attachment: PreparedRuntimeAttachment,
    ) -> Result<ActivePreparedSession, PreparedSessionUpdate> {
        let expected = fingerprint(
            "harn prepared-session lease v1",
            &(
                PREPARED_SESSION_SCHEMA,
                &lease.binding,
                &lease.plan_fingerprint,
                &lease.approval,
                lease.issued_at_ms,
                lease.expires_at_ms,
            ),
        )
        .map_err(|error| blocked_update(&lease.session_id, "prepared_session_lease", &error))?;
        if lease.schema != PREPARED_SESSION_SCHEMA || expected != lease.session_fingerprint {
            return Err(blocked_update(
                &lease.session_id,
                "prepared_session_lease",
                "prepared-session lease fingerprint is invalid",
            ));
        }
        if attachment.session_id != lease.binding.session_id
            || attachment.workspace_fingerprint != lease.binding.workspace_fingerprint
            || attachment.runtime != lease.binding.runtime
            || attachment.consumer != lease.binding.consumer
        {
            return Err(blocked_update(
                &lease.session_id,
                "prepared_session_attachment_binding",
                "runtime, server, session, or workspace drifted before attach",
            ));
        }
        if (self.run.now_ms)() > lease.expires_at_ms {
            return Err(blocked_update(
                &lease.session_id,
                "prepared_session_expired",
                "prepared-session lease expired before attach",
            ));
        }
        self.leases
            .claim(&lease.session_fingerprint)
            .map_err(|error| {
                blocked_update(&lease.session_id, "prepared_session_replay", &error)
            })?;
        let local = self
            .ready
            .lock()
            .expect("prepared-session ready state poisoned")
            .remove(&lease.session_fingerprint);
        let authority_lease = if let Some(authority_lease) = local {
            authority_lease
        } else {
            if let Some(approval) = &lease.approval {
                if approval.approved {
                    host_facts
                        .approved_batches
                        .insert(approval.batch_fingerprint.clone(), approval.decider);
                }
            }
            match self.run.prepare(lease.intent.clone(), host_facts) {
                PreparationOutcome::Ready {
                    authority_lease, ..
                } if authority_lease.plan_fingerprint() == lease.plan_fingerprint => {
                    authority_lease
                }
                PreparationOutcome::Ready { .. } => {
                    return Err(blocked_update(
                        &lease.session_id,
                        "prepared_session_plan_drift",
                        "attached runtime produced a different authority plan",
                    ))
                }
                PreparationOutcome::NeedsApproval { .. } => {
                    return Err(blocked_update(
                        &lease.session_id,
                        "prepared_session_approval_stale",
                        "attached runtime no longer accepts the persisted approval",
                    ))
                }
                PreparationOutcome::Blocked {
                    diagnostics,
                    receipt,
                } => {
                    return Err(PreparedSessionUpdate::Blocked {
                        session_id: lease.session_id,
                        diagnostics,
                        receipt,
                    })
                }
            }
        };
        Ok(ActivePreparedSession {
            lease,
            authority: AuthorityUse::new(authority_lease, self.run.now_ms.clone()),
        })
    }

    pub fn request_delta(
        &self,
        active: &ActivePreparedSession,
        requirement: AuthorityRequirement,
    ) -> PreparedSessionUpdate {
        let outcome = match self
            .run
            .request_delta(active.authority.lease(), requirement.clone())
        {
            LeaseDeltaOutcome::Covered => PreparedSessionDelta::Covered,
            LeaseDeltaOutcome::Attenuated(delta) => PreparedSessionDelta::Attenuated { delta },
            LeaseDeltaOutcome::Blocked(diagnostic) if diagnostic.code == "delta_widens_parent" => {
                let requirement_fingerprint = requirement_fingerprint(&requirement);
                match evaluate_requirement(
                    active.authority.lease().approval_policy.effective(),
                    &active.authority.lease().net_policy,
                    &requirement,
                ) {
                    Ok(evaluation) if evaluation.is_deny() => PreparedSessionDelta::Blocked {
                        diagnostic: AuthorityDiagnostic {
                            code: "delta_policy_denied".to_string(),
                            message: evaluation.reason,
                            requirement_fingerprint: Some(requirement_fingerprint),
                            actionable: "Narrow the delta or change the owning policy.".to_string(),
                        },
                    },
                    Ok(evaluation) => {
                        let delta = AuthorityLeaseDelta {
                            parent_lease_fingerprint: active
                                .authority
                                .lease()
                                .lease_fingerprint
                                .clone(),
                            requirement: requirement.clone(),
                            requirement_fingerprint: requirement_fingerprint.clone(),
                            expires_at_ms: active.authority.lease().expires_at_ms,
                        };
                        if evaluation.is_ask() {
                            let receipted = ReceiptedAuthority {
                                fingerprint: requirement_fingerprint,
                                requirement,
                            };
                            let batch = approval_batch(
                                active.authority.lease().plan_fingerprint(),
                                &[(receipted, evaluation)],
                            )
                            .expect("one widening requirement produces one approval batch");
                            self.pending_deltas
                                .lock()
                                .expect("prepared-session delta state poisoned")
                                .insert(
                                    active.lease.session_id.clone(),
                                    PendingPreparedSessionDelta {
                                        delta,
                                        batch: batch.clone(),
                                    },
                                );
                            PreparedSessionDelta::NeedsApproval { batch }
                        } else {
                            active
                                .authority
                                .grant_delta(&delta, AuthorityDecider::RuntimePolicy);
                            PreparedSessionDelta::Granted { delta }
                        }
                    }
                    Err(error) => PreparedSessionDelta::Blocked {
                        diagnostic: AuthorityDiagnostic {
                            code: "delta_policy_evaluation".to_string(),
                            message: error,
                            requirement_fingerprint: Some(requirement_fingerprint),
                            actionable: "Correct the delta so policy can classify it.".to_string(),
                        },
                    },
                }
            }
            LeaseDeltaOutcome::Blocked(diagnostic) => PreparedSessionDelta::Blocked { diagnostic },
        };
        PreparedSessionUpdate::Delta {
            session_id: active.lease.session_id.clone(),
            outcome,
        }
    }

    pub fn decide_delta(
        &self,
        active: &ActivePreparedSession,
        decision: PreparedSessionApprovalDecision,
    ) -> PreparedSessionUpdate {
        let Some(pending) = self
            .pending_deltas
            .lock()
            .expect("prepared-session delta state poisoned")
            .remove(&active.lease.session_id)
        else {
            return blocked_update(
                &active.lease.session_id,
                "prepared_session_delta_not_waiting",
                "prepared session is not waiting for a delta approval",
            );
        };
        let outcome = if decision.batch_fingerprint != pending.batch.batch_fingerprint {
            PreparedSessionDelta::Blocked {
                diagnostic: AuthorityDiagnostic {
                    code: "prepared_session_delta_binding".to_string(),
                    message: "delta approval does not match the semantic batch".to_string(),
                    requirement_fingerprint: None,
                    actionable: "Approve the exact pending delta batch.".to_string(),
                },
            }
        } else if !decision.approved {
            PreparedSessionDelta::Blocked {
                diagnostic: AuthorityDiagnostic {
                    code: "prepared_session_delta_denied".to_string(),
                    message: "the prepared-session delta was denied".to_string(),
                    requirement_fingerprint: Some(pending.delta.requirement_fingerprint),
                    actionable: "Continue within the active authority envelope.".to_string(),
                },
            }
        } else {
            active
                .authority
                .grant_delta(&pending.delta, decision.decider);
            PreparedSessionDelta::Granted {
                delta: pending.delta,
            }
        };
        PreparedSessionUpdate::Delta {
            session_id: active.lease.session_id.clone(),
            outcome,
        }
    }
}

impl<E: PreparedRunExecutor> PreparedSession<E> {
    pub async fn run_turn(&self, active: &ActivePreparedSession) -> Result<E::Output, E::Error> {
        active.authority.mark_executor_invoked();
        scope_prepared_identity(
            active.authority.clone(),
            self.run.identity_brokers.clone(),
            self.run.identity_consumer.clone(),
            self.run.executor.execute(&active.authority),
        )
        .await
    }

    pub fn finish(
        &self,
        active: ActivePreparedSession,
        succeeded: bool,
    ) -> Result<PreparedSessionUpdate, String> {
        let receipt = active
            .authority
            .terminal_receipt(succeeded, (self.run.now_ms)());
        self.run.receipts.persist(&receipt)?;
        self.leases.terminal(&active.lease.session_fingerprint)?;
        Ok(PreparedSessionUpdate::Terminal {
            session_id: active.lease.session_id,
            receipt,
        })
    }

    pub fn stop(
        &self,
        active: ActivePreparedSession,
        pivot: bool,
    ) -> Result<PreparedSessionUpdate, String> {
        let mut receipt = active
            .authority
            .terminal_receipt(false, (self.run.now_ms)());
        receipt.diagnostics.push(AuthorityDiagnostic {
            code: if pivot {
                "prepared_session_pivot"
            } else {
                "prepared_session_stop"
            }
            .to_string(),
            message: if pivot {
                "prepared session pivoted before terminal completion"
            } else {
                "prepared session stopped before terminal completion"
            }
            .to_string(),
            requirement_fingerprint: None,
            actionable: "Prepare a new session before resuming with changed intent.".to_string(),
        });
        self.run.receipts.persist(&receipt)?;
        self.leases.terminal(&active.lease.session_fingerprint)?;
        Ok(if pivot {
            PreparedSessionUpdate::Pivoted {
                session_id: active.lease.session_id,
                receipt,
            }
        } else {
            PreparedSessionUpdate::Stopped {
                session_id: active.lease.session_id,
                receipt,
            }
        })
    }
}

fn blocked(
    session_id: String,
    code: &str,
    message: &str,
    receipt: Option<RunAuthorityReceipt>,
) -> PreparedSessionUpdate {
    PreparedSessionUpdate::Blocked {
        session_id,
        diagnostics: vec![AuthorityDiagnostic {
            code: code.to_string(),
            message: message.to_string(),
            requirement_fingerprint: None,
            actionable: "Prepare a new session with matching typed inputs.".to_string(),
        }],
        receipt,
    }
}

fn blocked_update(session_id: &str, code: &str, message: &str) -> PreparedSessionUpdate {
    blocked(session_id.to_string(), code, message, None)
}
