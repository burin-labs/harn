use super::*;

#[derive(Clone)]
struct FixtureIdentityBroker {
    requirement: IdentityBrokerRequirement,
}

#[async_trait::async_trait]
impl ConsumerBoundIdentityBroker for FixtureIdentityBroker {
    fn facts(&self) -> IdentityBrokerFacts {
        identity_facts(&self.requirement)
    }

    async fn acquire(
        &self,
        requirement: &IdentityBrokerRequirement,
    ) -> Result<OpaqueIdentityHandle, IdentityBrokerError> {
        if requirement != &self.requirement {
            return Err(IdentityBrokerError::new(
                "identity_binding_mismatch",
                "fixture broker binding mismatch",
            ));
        }
        Ok(OpaqueIdentityHandle::new(
            requirement,
            crate::secrets::SecretBytes::from(SECRET_CANARY),
            Some(DEADLINE_MS),
        ))
    }
}

struct IdentityConsumingExecutor {
    requirement: IdentityBrokerRequirement,
    model_calls: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct MaterialIdentityBroker {
    requirement: IdentityBrokerRequirement,
    material: &'static str,
}

struct SequencedIdentityBroker {
    facts: IdentityBrokerFacts,
    acquisitions: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ConsumerBoundIdentityBroker for SequencedIdentityBroker {
    fn facts(&self) -> IdentityBrokerFacts {
        self.facts.clone()
    }

    async fn acquire(
        &self,
        requirement: &IdentityBrokerRequirement,
    ) -> Result<OpaqueIdentityHandle, IdentityBrokerError> {
        let attempt = self.acquisitions.fetch_add(1, Ordering::SeqCst);
        Ok(OpaqueIdentityHandle::new(
            requirement,
            crate::secrets::SecretBytes::from(SECRET_CANARY),
            Some(if attempt == 0 {
                NOW_MS - 1
            } else {
                DEADLINE_MS
            }),
        ))
    }
}

#[async_trait::async_trait]
impl ConsumerBoundIdentityBroker for MaterialIdentityBroker {
    fn facts(&self) -> IdentityBrokerFacts {
        identity_facts(&self.requirement)
    }

    async fn acquire(
        &self,
        requirement: &IdentityBrokerRequirement,
    ) -> Result<OpaqueIdentityHandle, IdentityBrokerError> {
        Ok(OpaqueIdentityHandle::new(
            requirement,
            crate::secrets::SecretBytes::from(self.material),
            Some(DEADLINE_MS),
        ))
    }
}

enum ProviderIdentityExecutor {
    Bedrock,
    Vertex,
}

#[async_trait::async_trait]
impl PreparedRunExecutor for ProviderIdentityExecutor {
    type Output = String;
    type Error = String;

    async fn execute(&self, _authority: &AuthorityUse) -> Result<Self::Output, Self::Error> {
        match self {
            Self::Bedrock => crate::llm::providers::bedrock::resolve_bedrock_credentials(
                "us-east-1",
                "https://bedrock.local",
            )
            .await
            .map(|credentials| credentials.access_key_id)
            .map_err(|error| error.to_string()),
            Self::Vertex => crate::llm::providers::vertex::resolve_vertex_token("", "project-a")
                .await
                .map_err(|error| error.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl PreparedRunExecutor for IdentityConsumingExecutor {
    type Output = usize;
    type Error = String;

    async fn execute(&self, _authority: &AuthorityUse) -> Result<Self::Output, Self::Error> {
        let material_len = consume_provider_identity(
            &self.requirement.binding.provider,
            &self.requirement.binding.audience,
            self.requirement.binding.tenant.as_deref(),
            |material| Ok(material.as_ref().len()),
        )
        .await
        .map_err(|error| error.code)?
        .ok_or_else(|| "prepared_identity_context_missing".to_string())?;
        self.model_calls.fetch_add(1, Ordering::SeqCst);
        Ok(material_len)
    }
}

#[tokio::test]
async fn identity_broker_is_readiness_bound_and_handle_is_opaque_and_consumer_exact() {
    let identity = identity_requirement();
    let mut run_intent = intent();
    run_intent.identity_brokers = vec![identity.clone()];
    let mut host = host_facts();
    host.identity_brokers
        .insert(identity.broker_id.clone(), identity_facts(&identity));
    let model_calls = Arc::new(AtomicUsize::new(0));
    let broker = Arc::new(FixtureIdentityBroker {
        requirement: identity.clone(),
    });
    let mut brokers = IdentityBrokerRegistry::default();
    brokers.insert(identity.broker_id.clone(), broker.clone());
    let run = PreparedRun::with_clock(
        IdentityConsumingExecutor {
            requirement: identity.clone(),
            model_calls: model_calls.clone(),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    )
    .with_identity_brokers(brokers, identity.binding.consumer.clone());
    let lease = approved_lease(&run, &run_intent, &host);
    assert_eq!(broker.facts(), identity_facts(&identity));
    match run.execute(lease).await {
        ExecutionOutcome::Completed { output, receipt } => {
            assert_eq!(output, SECRET_CANARY.len());
            assert!(receipt.used.iter().any(|authority| {
                authority.requirement == AuthorityRequirement::IdentityBroker(identity.clone())
            }));
            let serialized = serde_json::to_string(&receipt).expect("receipt JSON");
            assert!(!serialized.contains(SECRET_CANARY));
            assert!(serialized.contains("burin-workload"));
        }
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("identity run failed: {error}")
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bedrock_and_vertex_resolvers_consume_the_prepared_provider_identity() {
    let cases = [
        (
            "bedrock",
            "bedrock.local",
            None,
            PlatformIdentitySourceKind::SdkProfile,
            r#"{"access_key_id":"brokered-access","secret_access_key":"brokered-secret"}"#,
            "brokered-access",
            ProviderIdentityExecutor::Bedrock,
        ),
        (
            "vertex",
            "https://www.googleapis.com/auth/cloud-platform",
            Some("project-a"),
            PlatformIdentitySourceKind::WorkloadIdentity,
            "brokered-vertex-token",
            "brokered-vertex-token",
            ProviderIdentityExecutor::Vertex,
        ),
    ];
    for (provider, audience, tenant, source, material, expected, executor) in cases {
        let identity = IdentityBrokerRequirement {
            reference: PlatformIdentityReference::parse(&format!(
                "harn-identity://burin.provider_auth/{provider}"
            ))
            .expect("identity reference"),
            broker_id: format!("{provider}-broker"),
            source,
            renewal: IdentityRenewalMode::BrokerManaged,
            binding: IdentityBrokerBinding {
                provider: provider.to_string(),
                audience: audience.to_string(),
                tenant: tenant.map(str::to_string),
                consumer: SecretConsumerBinding {
                    kind: SecretConsumerKind::Provider,
                    id: provider.to_string(),
                    environment_name: None,
                },
            },
        };
        let mut run_intent = intent();
        run_intent.identity_brokers = vec![identity.clone()];
        let mut host = host_facts();
        host.identity_brokers
            .insert(identity.broker_id.clone(), identity_facts(&identity));
        let broker = Arc::new(MaterialIdentityBroker {
            requirement: identity.clone(),
            material,
        });
        let mut brokers = IdentityBrokerRegistry::default();
        brokers.insert(identity.broker_id.clone(), broker);
        let run = PreparedRun::with_clock(
            executor,
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        )
        .with_identity_brokers(brokers, identity.binding.consumer.clone());
        let lease = approved_lease(&run, &run_intent, &host);
        match run.execute(lease).await {
            ExecutionOutcome::Completed { output, receipt } => {
                assert_eq!(output, expected);
                assert_eq!(receipt.used.len(), 1);
                assert!(!serde_json::to_string(&receipt)
                    .expect("receipt JSON")
                    .contains(material));
            }
            ExecutionOutcome::ExecutorFailed { error, .. }
            | ExecutionOutcome::AuthorityFailed { error, .. } => {
                panic!("{provider} prepared identity failed: {error}")
            }
        }
    }
}

#[tokio::test]
async fn broker_managed_identity_renews_once_and_runtime_fact_drift_fails_closed() {
    let mut identity = identity_requirement();
    identity.source = PlatformIdentitySourceKind::InstanceMetadata;
    let mut run_intent = intent();
    run_intent.identity_brokers = vec![identity.clone()];
    let mut host = host_facts();
    host.identity_brokers
        .insert(identity.broker_id.clone(), identity_facts(&identity));

    let acquisitions = Arc::new(AtomicUsize::new(0));
    let mut brokers = IdentityBrokerRegistry::default();
    brokers.insert(
        identity.broker_id.clone(),
        Arc::new(SequencedIdentityBroker {
            facts: identity_facts(&identity),
            acquisitions: acquisitions.clone(),
        }),
    );
    let run = PreparedRun::with_clock(
        IdentityConsumingExecutor {
            requirement: identity.clone(),
            model_calls: Arc::new(AtomicUsize::new(0)),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    )
    .with_identity_brokers(brokers, identity.binding.consumer.clone());
    match run.execute(approved_lease(&run, &run_intent, &host)).await {
        ExecutionOutcome::Completed { receipt, .. } => {
            assert!(receipt.used.iter().any(|authority| authority.requirement
                == AuthorityRequirement::IdentityBroker(identity.clone())));
        }
        ExecutionOutcome::ExecutorFailed { error, .. }
        | ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("broker-managed renewal failed: {error}")
        }
    }
    assert_eq!(acquisitions.load(Ordering::SeqCst), 2);

    let mut drifted_facts = identity_facts(&identity);
    drifted_facts
        .sources
        .remove(&PlatformIdentitySourceKind::InstanceMetadata);
    let model_calls = Arc::new(AtomicUsize::new(0));
    let mut drifted_brokers = IdentityBrokerRegistry::default();
    drifted_brokers.insert(
        identity.broker_id.clone(),
        Arc::new(SequencedIdentityBroker {
            facts: drifted_facts,
            acquisitions: Arc::new(AtomicUsize::new(0)),
        }),
    );
    let drifted_run = PreparedRun::with_clock(
        IdentityConsumingExecutor {
            requirement: identity.clone(),
            model_calls: model_calls.clone(),
        },
        Arc::new(MemoryAuthorityReceiptSink::default()),
        Arc::new(|| NOW_MS),
    )
    .with_identity_brokers(drifted_brokers, identity.binding.consumer.clone());
    match drifted_run
        .execute(approved_lease(&drifted_run, &run_intent, &host))
        .await
    {
        ExecutionOutcome::ExecutorFailed { receipt, .. } => {
            assert!(receipt.used.is_empty());
            assert_eq!(receipt.denied.len(), 1);
        }
        ExecutionOutcome::Completed { .. } => panic!("metadata drift reached model spend"),
        ExecutionOutcome::AuthorityFailed { error, .. } => {
            panic!("unexpected authority failure: {error}")
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn prepared_identity_missing_or_wrong_consumer_fails_before_model_spend() {
    for wrong_consumer in [false, true] {
        let identity = identity_requirement();
        let mut run_intent = intent();
        run_intent.identity_brokers = vec![identity.clone()];
        let mut host = host_facts();
        host.identity_brokers
            .insert(identity.broker_id.clone(), identity_facts(&identity));
        let model_calls = Arc::new(AtomicUsize::new(0));
        let mut brokers = IdentityBrokerRegistry::default();
        if wrong_consumer {
            brokers.insert(
                identity.broker_id.clone(),
                Arc::new(FixtureIdentityBroker {
                    requirement: identity.clone(),
                }),
            );
        }
        let consumer = if wrong_consumer {
            SecretConsumerBinding {
                id: "different-consumer".to_string(),
                ..identity.binding.consumer.clone()
            }
        } else {
            identity.binding.consumer.clone()
        };
        let run = PreparedRun::with_clock(
            IdentityConsumingExecutor {
                requirement: identity.clone(),
                model_calls: model_calls.clone(),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        )
        .with_identity_brokers(brokers, consumer);
        let lease = approved_lease(&run, &run_intent, &host);
        match run.execute(lease).await {
            ExecutionOutcome::ExecutorFailed { receipt, .. } => {
                assert!(receipt.used.is_empty());
                assert!(!receipt.denied.is_empty());
            }
            ExecutionOutcome::Completed { .. } => {
                panic!("prepared identity failure must not reach model spend")
            }
            ExecutionOutcome::AuthorityFailed { error, .. } => {
                panic!("provider identity refusal must remain an executor failure: {error}")
            }
        }
        assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn opaque_identity_handle_rejects_a_different_consumer_binding() {
    let identity = identity_requirement();
    let broker = FixtureIdentityBroker {
        requirement: identity.clone(),
    };
    let handle = broker.acquire(&identity).await.expect("opaque handle");
    let mut drifted = identity;
    drifted.binding.consumer.id = "different-consumer".to_string();
    let error = handle
        .consume(&drifted, NOW_MS, |_| ())
        .expect_err("a handle cannot cross consumer bindings");
    assert_eq!(error.code, "identity_binding_mismatch");
}

#[test]
fn identity_broker_host_properties_are_readiness_facts() {
    let identity = identity_requirement();
    let cases = [
        ("noninteractive", "identity_broker_interactivity"),
        ("gui", "identity_broker_interactivity"),
        ("sandbox", "identity_broker_sandbox_boundary"),
        ("opaque", "identity_broker_sandbox_boundary"),
        ("source", "identity_source_mismatch"),
        ("renewal", "identity_renewal_mismatch"),
    ];
    for (case, expected_code) in cases {
        let mut facts = identity_facts(&identity);
        match case {
            "noninteractive" => facts.supports_non_interactive = false,
            "gui" => facts.may_prompt_gui = true,
            "sandbox" => facts.material_outside_sandbox = false,
            "opaque" => facts.opaque_process_local_handles = false,
            "source" => facts.sources.clear(),
            "renewal" => facts.renewal_modes.clear(),
            _ => unreachable!(),
        }
        let mut run_intent = intent();
        run_intent.identity_brokers = vec![identity.clone()];
        let mut host = host_facts();
        host.identity_brokers
            .insert(identity.broker_id.clone(), facts);
        let run = PreparedRun::with_clock(
            FixtureExecutor {
                requirements: Vec::new(),
                model_calls: Arc::new(AtomicUsize::new(0)),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        );
        match run.prepare(run_intent, host) {
            PreparationOutcome::Blocked { diagnostics, .. } => assert!(
                diagnostics.iter().any(|item| item.code == expected_code),
                "{case} must emit {expected_code}: {diagnostics:#?}"
            ),
            other => panic!("{case} must block readiness, got {other:?}"),
        }
    }
}

#[test]
fn provider_audience_tenant_and_consumer_drift_each_block_before_spend() {
    let original = identity_requirement();
    for drift in ["provider", "audience", "tenant", "consumer"] {
        let mut changed = original.clone();
        match drift {
            "provider" => changed.binding.provider = "other-provider".to_string(),
            "audience" => changed.binding.audience = "https://other.invalid".to_string(),
            "tenant" => changed.binding.tenant = Some("tenant-b".to_string()),
            "consumer" => changed.binding.consumer.id = "other-consumer".to_string(),
            _ => unreachable!(),
        }
        let mut run_intent = intent();
        run_intent.identity_brokers = vec![changed];
        let mut host = host_facts();
        host.identity_brokers
            .insert(original.broker_id.clone(), identity_facts(&original));
        let model_calls = Arc::new(AtomicUsize::new(0));
        let run = PreparedRun::with_clock(
            FixtureExecutor {
                requirements: Vec::new(),
                model_calls: model_calls.clone(),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        );
        match run.prepare(run_intent, host) {
            PreparationOutcome::Blocked { diagnostics, .. } => assert!(diagnostics
                .iter()
                .any(|item| item.code == "identity_consumer_binding")),
            other => panic!("{drift} drift must block readiness, got {other:?}"),
        }
        assert_eq!(model_calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn platform_identity_reference_rejects_values_and_ambiguous_paths() {
    assert!(PlatformIdentityReference::parse(SECRET_CANARY).is_err());
    assert!(PlatformIdentityReference::parse("harn-identity://tenant/name/extra").is_err());
    assert!(serde_json::from_value::<PlatformIdentityReference>(json!(SECRET_CANARY)).is_err());
}

fn prepared_session_binding() -> PreparedSessionBindingV1 {
    PreparedSessionBindingV1 {
        session_id: "prepared-session-1".to_string(),
        workspace_fingerprint: "blake3:workspace-a".to_string(),
        runtime: provenance(),
        consumer: SecretConsumerBinding {
            kind: SecretConsumerKind::Provider,
            id: "prepared-run".to_string(),
            environment_name: None,
        },
    }
}

fn prepared_runtime_attachment() -> PreparedRuntimeAttachment {
    PreparedRuntimeAttachment {
        session_id: "prepared-session-1".to_string(),
        workspace_fingerprint: "blake3:workspace-a".to_string(),
        runtime: provenance(),
        consumer: SecretConsumerBinding {
            kind: SecretConsumerKind::Provider,
            id: "prepared-run".to_string(),
            environment_name: None,
        },
    }
}

#[tokio::test]
async fn prepared_session_persists_one_approval_reuses_the_envelope_and_rejects_replay() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let receipts = Arc::new(MemoryAuthorityReceiptSink::default());
    let claims = Arc::new(MemoryPreparedSessionLeaseStore::default());
    let host_session = PreparedSession::new(
        PreparedRun::with_clock(
            FixtureExecutor {
                requirements: executor_requirements(),
                model_calls: model_calls.clone(),
            },
            receipts.clone(),
            Arc::new(|| NOW_MS),
        ),
        claims.clone(),
    );
    let batch = match host_session.prepare(prepared_session_binding(), intent(), host_facts()) {
        PreparedSessionUpdate::NeedsApproval { batch, .. } => batch,
        other => panic!("interactive session must present one grouped batch, got {other:?}"),
    };
    let decision = PreparedSessionApprovalDecision {
        batch_fingerprint: batch.batch_fingerprint,
        approved: true,
        decider: AuthorityDecider::Person,
    };
    let lease = match host_session.decide("prepared-session-1", decision) {
        PreparedSessionUpdate::Ready { lease, .. } => *lease,
        other => panic!("approved session must become ready, got {other:?}"),
    };
    assert_eq!(lease.schema, PREPARED_SESSION_SCHEMA);
    let schema: serde_json::Value =
        serde_json::from_str(PREPARED_SESSION_V1_SCHEMA_JSON).expect("prepared-session schema");
    let validator = jsonschema::validator_for(&schema).expect("compile prepared-session schema");
    let lease_value = serde_json::to_value(&lease).expect("serialize prepared-session lease");
    let errors = validator
        .iter_errors(&lease_value)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "prepared-session lease schema violations: {errors:#?}"
    );
    assert!(receipts
        .receipts()
        .iter()
        .any(|receipt| receipt.stage == AuthorityReceiptStage::ApprovalDecision));

    // A separate state-machine instance models a long-lived external harn
    // serve process attaching from the persisted, value-free lease.
    let server_session = PreparedSession::new(
        PreparedRun::with_clock(
            FixtureExecutor {
                requirements: executor_requirements(),
                model_calls: model_calls.clone(),
            },
            receipts.clone(),
            Arc::new(|| NOW_MS),
        ),
        claims,
    );
    let active = server_session
        .attach(lease.clone(), host_facts(), prepared_runtime_attachment())
        .expect("external server accepts the exact prepared lease");
    assert_eq!(server_session.run_turn(&active).await.unwrap(), "completed");
    assert_eq!(server_session.run_turn(&active).await.unwrap(), "completed");
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);

    let replay = server_session.attach(lease, host_facts(), prepared_runtime_attachment());
    match replay {
        Err(PreparedSessionUpdate::Blocked { diagnostics, .. }) => assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "prepared_session_replay")),
        _ => panic!("a claimed prepared-session lease must reject replay"),
    }
    match server_session
        .finish(active, true)
        .expect("terminal receipt")
    {
        PreparedSessionUpdate::Terminal { receipt, .. } => {
            assert_eq!(receipt.status, AuthorityReceiptStatus::Completed);
            assert_eq!(receipt.used.len(), executor_requirements().len());
            assert!(receipt.unused.len() < receipt.granted.len());
        }
        other => panic!("finished session must emit terminal accounting, got {other:?}"),
    }
}

#[tokio::test]
async fn prepared_session_rejects_stale_runtime_and_cross_workspace_attach_before_turns() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let session = PreparedSession::new(
        PreparedRun::with_clock(
            FixtureExecutor {
                requirements: executor_requirements(),
                model_calls: model_calls.clone(),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        ),
        Arc::new(MemoryPreparedSessionLeaseStore::default()),
    );
    let mut approved_host = host_facts();
    let batch = match session.prepare(prepared_session_binding(), intent(), approved_host.clone()) {
        PreparedSessionUpdate::NeedsApproval { batch, .. } => batch,
        other => panic!("expected approval batch, got {other:?}"),
    };
    approved_host
        .approved_batches
        .insert(batch.batch_fingerprint.clone(), AuthorityDecider::Person);
    let lease = match session.decide(
        "prepared-session-1",
        PreparedSessionApprovalDecision {
            batch_fingerprint: batch.batch_fingerprint,
            approved: true,
            decider: AuthorityDecider::Person,
        },
    ) {
        PreparedSessionUpdate::Ready { lease, .. } => *lease,
        other => panic!("expected ready lease, got {other:?}"),
    };
    for drift in ["workspace", "runtime", "consumer"] {
        let mut attachment = prepared_runtime_attachment();
        if drift == "workspace" {
            attachment.workspace_fingerprint = "blake3:workspace-b".to_string();
        } else if drift == "runtime" {
            attachment.runtime.runtime_digest = "blake3:stale-runtime".to_string();
        } else {
            attachment.consumer.id = "wrong-provider".to_string();
        }
        match session.attach(lease.clone(), approved_host.clone(), attachment) {
            Err(PreparedSessionUpdate::Blocked { diagnostics, .. }) => assert!(diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "prepared_session_attachment_binding")),
            _ => panic!("{drift} drift must block before engine startup"),
        }
    }
    assert_eq!(model_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn prepared_session_widening_is_one_semantic_delta_batch() {
    let session = PreparedSession::new(
        PreparedRun::with_clock(
            FixtureExecutor {
                requirements: Vec::new(),
                model_calls: Arc::new(AtomicUsize::new(0)),
            },
            Arc::new(MemoryAuthorityReceiptSink::default()),
            Arc::new(|| NOW_MS),
        ),
        Arc::new(MemoryPreparedSessionLeaseStore::default()),
    );
    let batch = match session.prepare(prepared_session_binding(), intent(), host_facts()) {
        PreparedSessionUpdate::NeedsApproval { batch, .. } => batch,
        other => panic!("expected initial approval batch, got {other:?}"),
    };
    let lease = match session.decide(
        "prepared-session-1",
        PreparedSessionApprovalDecision {
            batch_fingerprint: batch.batch_fingerprint,
            approved: true,
            decider: AuthorityDecider::Person,
        },
    ) {
        PreparedSessionUpdate::Ready { lease, .. } => *lease,
        other => panic!("expected ready lease, got {other:?}"),
    };
    let active = session
        .attach(lease, host_facts(), prepared_runtime_attachment())
        .expect("attach prepared session");
    let widened = AuthorityRequirement::Tool {
        pattern: "deploy".to_string(),
    };
    let delta_batch = match session.request_delta(&active, widened.clone()) {
        PreparedSessionUpdate::Delta {
            outcome: PreparedSessionDelta::NeedsApproval { batch },
            ..
        } => batch,
        other => panic!("widening must yield one approval batch, got {other:?}"),
    };
    assert_eq!(delta_batch.groups.len(), 1);
    assert_eq!(delta_batch.groups[0].semantic_group, "host_capabilities");
    assert!(active.authorize(&widened).is_err());
    match session.decide_delta(
        &active,
        PreparedSessionApprovalDecision {
            batch_fingerprint: delta_batch.batch_fingerprint,
            approved: true,
            decider: AuthorityDecider::Person,
        },
    ) {
        PreparedSessionUpdate::Delta {
            outcome: PreparedSessionDelta::Granted { .. },
            ..
        } => {}
        other => panic!("approved delta must join the active envelope, got {other:?}"),
    }
    active
        .authorize(&widened)
        .expect("approved widening is live without re-preparation");
}
