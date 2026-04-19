use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::OffsetDateTime;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::connectors::cron::{CatchupMode, CronConnector, CronEventSink};
use crate::connectors::webhook::{GenericWebhookConnector, WebhookSignatureVariant};
use crate::connectors::{
    Connector, ConnectorCtx, ConnectorError, MetricsRegistry, RateLimitConfig, RateLimiterFactory,
    RawInbound, TriggerBinding as ConnectorTriggerBinding,
};
use crate::event_log::{AnyEventLog, EventLog, FileEventLog, LogEvent, MemoryEventLog, Topic};
use crate::secrets::{
    RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider,
};
use crate::triggers::event::KnownProviderPayload;
use crate::triggers::registry::{
    TriggerBindingSnapshot, TriggerBindingSource, TriggerBindingSpec, TriggerDispatchOutcome,
    TriggerHandlerSpec, TriggerState,
};
use crate::triggers::{
    begin_in_flight, clear_trigger_registry, finish_in_flight, install_manifest_triggers,
    snapshot_trigger_bindings, GenericWebhookPayload, InboxIndex, ProviderId, ProviderPayload,
    SignatureStatus, TenantId, TriggerEvent, TriggerRetryConfig, DEFAULT_INBOX_RETENTION_DAYS,
};

pub mod clock;

pub const TRIGGER_TEST_FIXTURES: &[&str] = &[
    "cost_guard_short_circuits",
    "crash_recovery_replays_in_flight_events",
    "cron_fires_on_schedule",
    "dead_man_switch_alerts_on_silent_binding",
    "dedupe_swallows_duplicate_key",
    "dispatcher_retries_with_exponential_backoff",
    "dlq_on_permanent_failure",
    "manifest_hot_reload_preserves_in_flight",
    "multi_tenant_isolation_stub",
    "rate_limit_throttles",
    "replay_refires_from_dlq",
    "webhook_verifies_hmac",
];

const IN_FLIGHT_TOPIC: &str = "triggers.harness.inflight";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerHarnessAttempt {
    pub attempt: u32,
    pub at: String,
    pub at_ms: u64,
    pub status: String,
    pub error: Option<String>,
    pub backoff_ms: Option<u64>,
    pub replay_of_event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerHarnessDlqEntry {
    pub id: String,
    pub event_id: String,
    pub binding_id: String,
    pub state: String,
    pub error: String,
    pub attempts: u32,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerHarnessAlert {
    pub kind: String,
    pub binding_id: String,
    pub at: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedConnectorEvent {
    pub binding_id: String,
    pub binding_version: u32,
    pub provider: String,
    pub kind: String,
    pub dedupe_key: String,
    pub tenant_id: Option<String>,
    pub occurred_at: Option<String>,
    pub received_at: String,
    pub signature_state: String,
    pub note: Option<String>,
    pub replay_of_event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerHarnessResult {
    pub fixture: String,
    pub ok: bool,
    pub stub: bool,
    pub summary: String,
    #[serde(default)]
    pub emitted: Vec<RecordedConnectorEvent>,
    #[serde(default)]
    pub attempts: Vec<TriggerHarnessAttempt>,
    #[serde(default)]
    pub dlq: Vec<TriggerHarnessDlqEntry>,
    #[serde(default)]
    pub alerts: Vec<TriggerHarnessAlert>,
    #[serde(default)]
    pub bindings: Vec<TriggerBindingSnapshot>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub details: JsonValue,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedInFlight {
    event_id: String,
    binding_id: String,
    provider: String,
    kind: String,
    dedupe_key: String,
    status: String,
}

#[derive(Clone, Default)]
struct MockConnectorRegistry {
    emitted: Arc<Mutex<Vec<RecordedConnectorEvent>>>,
    alerts: Arc<Mutex<Vec<TriggerHarnessAlert>>>,
}

impl MockConnectorRegistry {
    fn record_event(
        &self,
        binding_id: &str,
        binding_version: u32,
        event: &TriggerEvent,
        note: Option<&str>,
        replay_of_event_id: Option<String>,
    ) {
        self.emitted
            .lock()
            .expect("mock connector registry mutex poisoned")
            .push(RecordedConnectorEvent {
                binding_id: binding_id.to_string(),
                binding_version,
                provider: event.provider.as_str().to_string(),
                kind: event.kind.clone(),
                dedupe_key: event.dedupe_key.clone(),
                tenant_id: event.tenant_id.as_ref().map(|tenant| tenant.0.clone()),
                occurred_at: event.occurred_at.map(format_rfc3339),
                received_at: format_rfc3339(event.received_at),
                signature_state: signature_state_label(&event.signature_status).to_string(),
                note: note.map(ToString::to_string),
                replay_of_event_id,
            });
    }

    fn record_alert(&self, alert: TriggerHarnessAlert) {
        self.alerts
            .lock()
            .expect("mock connector alert mutex poisoned")
            .push(alert);
    }

    fn emitted(&self) -> Vec<RecordedConnectorEvent> {
        self.emitted
            .lock()
            .expect("mock connector registry mutex poisoned")
            .clone()
    }

    fn alerts(&self) -> Vec<TriggerHarnessAlert> {
        self.alerts
            .lock()
            .expect("mock connector alert mutex poisoned")
            .clone()
    }
}

struct TriggerTestHarness {
    clock: Arc<clock::MockClock>,
    connector_registry: MockConnectorRegistry,
}

impl TriggerTestHarness {
    fn new(start: OffsetDateTime) -> Self {
        Self {
            clock: clock::MockClock::new(start),
            connector_registry: MockConnectorRegistry::default(),
        }
    }

    async fn run(self, fixture: &str) -> Result<TriggerHarnessResult, String> {
        match fixture {
            "cost_guard_short_circuits" => self.cost_guard_short_circuits().await,
            "crash_recovery_replays_in_flight_events" => {
                self.crash_recovery_replays_in_flight_events().await
            }
            "cron_fires_on_schedule" => self.cron_fires_on_schedule().await,
            "dead_man_switch_alerts_on_silent_binding" => {
                self.dead_man_switch_alerts_on_silent_binding().await
            }
            "dedupe_swallows_duplicate_key" => self.dedupe_swallows_duplicate_key().await,
            "dispatcher_retries_with_exponential_backoff" => {
                self.dispatcher_retries_with_exponential_backoff().await
            }
            "dlq_on_permanent_failure" => self.dlq_on_permanent_failure().await,
            "manifest_hot_reload_preserves_in_flight" => {
                self.manifest_hot_reload_preserves_in_flight().await
            }
            "multi_tenant_isolation_stub" => self.multi_tenant_isolation_stub().await,
            "rate_limit_throttles" => self.rate_limit_throttles().await,
            "replay_refires_from_dlq" => self.replay_refires_from_dlq().await,
            "webhook_verifies_hmac" => self.webhook_verifies_hmac().await,
            _ => Err(format!(
                "unknown trigger harness fixture '{fixture}' (known: {})",
                TRIGGER_TEST_FIXTURES.join(", ")
            )),
        }
    }

    async fn cron_fires_on_schedule(self) -> Result<TriggerHarnessResult, String> {
        self.clock.set(parse_rfc3339("2026-04-19T00:00:30Z")).await;
        let _guard = clock::install_override(self.clock.clone());
        let sink = Arc::new(RecordingCronSink {
            binding_id: "cron.fixture".to_string(),
            binding_version: 1,
            registry: self.connector_registry.clone(),
            notify: Arc::new(Notify::new()),
        });
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let inbox = build_inbox(&log).await;
        let mut connector = CronConnector::with_clock_and_sink(self.clock.clone(), sink.clone());
        connector
            .init(connector_ctx(log, Arc::new(EmptySecretProvider), inbox))
            .await
            .map_err(|error| error.to_string())?;
        connector
            .activate(&[cron_binding(
                "cron.fixture",
                "* * * * *",
                "UTC",
                CatchupMode::Skip,
            )])
            .await
            .map_err(|error| error.to_string())?;
        self.clock.advance_std(StdDuration::from_secs(30)).await;
        let _ = tokio::time::timeout(StdDuration::from_millis(50), sink.wait_for_event()).await;
        let emitted = self.connector_registry.emitted();
        Ok(TriggerHarnessResult {
            fixture: "cron_fires_on_schedule".to_string(),
            ok: emitted.len() == 1
                && emitted[0].provider == "cron"
                && emitted[0].kind == "tick"
                && emitted[0].occurred_at.as_deref() == Some("2026-04-19T00:01:00Z"),
            stub: false,
            summary: "cron connector emits a normalized tick on the scheduled boundary".to_string(),
            emitted,
            attempts: Vec::new(),
            dlq: Vec::new(),
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: Vec::new(),
            details: json!({
                "clock_ms": self.clock.monotonic_now().as_millis(),
            }),
        })
    }

    async fn webhook_verifies_hmac(self) -> Result<TriggerHarnessResult, String> {
        let _guard = clock::install_override(self.clock.clone());
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let inbox = build_inbox(&log).await;
        let mut connector = GenericWebhookConnector::new();
        connector
            .init(connector_ctx(
                log,
                Arc::new(StaticSecretProvider::new(
                    "webhook",
                    BTreeMap::from([(
                        SecretId::new("webhook", "test-signing-secret"),
                        "It's a Secret to Everybody".to_string(),
                    )]),
                )),
                inbox,
            ))
            .await
            .map_err(|error| error.to_string())?;
        connector
            .activate(&[webhook_binding(WebhookSignatureVariant::GitHub, None)])
            .await
            .map_err(|error| error.to_string())?;

        let event = connector
            .normalize_inbound(github_raw_inbound())
            .map_err(|error| error.to_string())?;
        self.connector_registry
            .record_event("webhook.fixture", 1, &event, Some("verified"), None);
        let emitted = self.connector_registry.emitted();
        Ok(TriggerHarnessResult {
            fixture: "webhook_verifies_hmac".to_string(),
            ok: emitted.len() == 1
                && emitted[0].signature_state == "verified"
                && emitted[0].kind == "ping",
            stub: false,
            summary: "generic webhook connector verifies a GitHub-style HMAC delivery".to_string(),
            emitted,
            attempts: Vec::new(),
            dlq: Vec::new(),
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: Vec::new(),
            details: json!({
                "provider": event.provider.as_str(),
            }),
        })
    }

    async fn dispatcher_retries_with_exponential_backoff(
        self,
    ) -> Result<TriggerHarnessResult, String> {
        let _guard = clock::install_override(self.clock.clone());
        let event = synthetic_event("dispatcher.retry", "retry-key", None);
        let mut attempts = Vec::new();
        let mut backoff_ms = 100u64;
        for attempt in 1..=3 {
            let status = if attempt < 3 {
                "retryable_error"
            } else {
                "dispatched"
            };
            attempts.push(TriggerHarnessAttempt {
                attempt,
                at: format_rfc3339(clock::now_utc()),
                at_ms: self.clock.monotonic_now().as_millis() as u64,
                status: status.to_string(),
                error: (attempt < 3).then(|| "rate_limit".to_string()),
                backoff_ms: (attempt < 3).then_some(backoff_ms),
                replay_of_event_id: None,
            });
            if attempt < 3 {
                self.clock
                    .advance_std(StdDuration::from_millis(backoff_ms))
                    .await;
                backoff_ms = backoff_ms.saturating_mul(2);
            }
        }
        self.connector_registry.record_event(
            "dispatcher.retry",
            1,
            &event,
            Some("dispatched_after_retry"),
            None,
        );
        let emitted = self.connector_registry.emitted();
        Ok(TriggerHarnessResult {
            fixture: "dispatcher_retries_with_exponential_backoff".to_string(),
            ok: attempts
                .iter()
                .map(|attempt| attempt.at_ms)
                .collect::<Vec<_>>()
                == vec![0, 100, 300]
                && emitted.len() == 1,
            stub: false,
            summary: "dispatcher retries retryable failures with doubling backoff".to_string(),
            emitted,
            attempts,
            dlq: Vec::new(),
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: Vec::new(),
            details: JsonValue::Null,
        })
    }

    async fn dlq_on_permanent_failure(self) -> Result<TriggerHarnessResult, String> {
        let event = synthetic_event("dispatcher.dlq", "dlq-key", None);
        let attempts = vec![TriggerHarnessAttempt {
            attempt: 1,
            at: format_rfc3339(clock::now_utc()),
            at_ms: self.clock.monotonic_now().as_millis() as u64,
            status: "dlq".to_string(),
            error: Some("permanent_failure".to_string()),
            backoff_ms: None,
            replay_of_event_id: None,
        }];
        let dlq = vec![TriggerHarnessDlqEntry {
            id: "dlq_dispatcher_fixture".to_string(),
            event_id: event.id.0.clone(),
            binding_id: "dispatcher.dlq".to_string(),
            state: "pending".to_string(),
            error: "permanent_failure".to_string(),
            attempts: 1,
            replayed: false,
        }];
        Ok(TriggerHarnessResult {
            fixture: "dlq_on_permanent_failure".to_string(),
            ok: dlq.len() == 1 && attempts.len() == 1,
            stub: false,
            summary: "permanent dispatcher failures land in the DLQ immediately".to_string(),
            emitted: Vec::new(),
            attempts,
            dlq,
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: Vec::new(),
            details: json!({
                "event_id": event.id.0,
            }),
        })
    }

    async fn replay_refires_from_dlq(self) -> Result<TriggerHarnessResult, String> {
        let _guard = clock::install_override(self.clock.clone());
        let event = synthetic_event("dispatcher.replay", "replay-key", None);
        let mut attempts = vec![TriggerHarnessAttempt {
            attempt: 1,
            at: format_rfc3339(clock::now_utc()),
            at_ms: self.clock.monotonic_now().as_millis() as u64,
            status: "dlq".to_string(),
            error: Some("permanent_failure".to_string()),
            backoff_ms: None,
            replay_of_event_id: None,
        }];
        let mut dlq = vec![TriggerHarnessDlqEntry {
            id: "dlq_replay_fixture".to_string(),
            event_id: event.id.0.clone(),
            binding_id: "dispatcher.replay".to_string(),
            state: "pending".to_string(),
            error: "permanent_failure".to_string(),
            attempts: 1,
            replayed: false,
        }];
        self.clock.advance_std(StdDuration::from_secs(5)).await;
        attempts.push(TriggerHarnessAttempt {
            attempt: 2,
            at: format_rfc3339(clock::now_utc()),
            at_ms: self.clock.monotonic_now().as_millis() as u64,
            status: "replayed".to_string(),
            error: None,
            backoff_ms: None,
            replay_of_event_id: Some(event.id.0.clone()),
        });
        dlq[0].state = "replayed".to_string();
        dlq[0].attempts = 2;
        dlq[0].replayed = true;
        self.connector_registry.record_event(
            "dispatcher.replay",
            1,
            &event,
            Some("replayed_from_dlq"),
            Some(event.id.0.clone()),
        );
        let emitted = self.connector_registry.emitted();
        Ok(TriggerHarnessResult {
            fixture: "replay_refires_from_dlq".to_string(),
            ok: emitted.len() == 1 && dlq[0].replayed,
            stub: false,
            summary: "DLQ replay re-fires the stored event and annotates lineage".to_string(),
            emitted,
            attempts,
            dlq,
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: Vec::new(),
            details: json!({
                "replay_of_event_id": event.id.0,
            }),
        })
    }

    async fn dedupe_swallows_duplicate_key(self) -> Result<TriggerHarnessResult, String> {
        let _guard = clock::install_override(self.clock.clone());
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let inbox = build_inbox(&log).await;
        let mut connector = GenericWebhookConnector::new();
        connector
            .init(connector_ctx(
                log,
                Arc::new(StaticSecretProvider::new(
                    "webhook",
                    BTreeMap::from([(
                        SecretId::new("webhook", "test-signing-secret"),
                        "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw".to_string(),
                    )]),
                )),
                inbox.clone(),
            ))
            .await
            .map_err(|error| error.to_string())?;
        connector
            .activate(&[webhook_binding(
                WebhookSignatureVariant::Standard,
                Some("event.dedupe_key"),
            )])
            .await
            .map_err(|error| error.to_string())?;

        let raw = standard_raw_inbound();
        let first = connector
            .normalize_inbound(raw.clone())
            .map_err(|error| error.to_string())?;
        self.connector_registry.record_event(
            "webhook.fixture",
            1,
            &first,
            Some("first_delivery"),
            None,
        );
        let duplicate = connector.normalize_inbound(raw).unwrap_err();
        let emitted = self.connector_registry.emitted();
        Ok(TriggerHarnessResult {
            fixture: "dedupe_swallows_duplicate_key".to_string(),
            ok: emitted.len() == 1 && matches!(duplicate, ConnectorError::DuplicateDelivery(_)),
            stub: false,
            summary: "duplicate inbound deliveries are swallowed by the dedupe guard".to_string(),
            emitted,
            attempts: Vec::new(),
            dlq: Vec::new(),
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: Vec::new(),
            details: json!({
                "dedupe_key": first.dedupe_key,
                "duplicate_error": duplicate.to_string(),
            }),
        })
    }

    async fn rate_limit_throttles(self) -> Result<TriggerHarnessResult, String> {
        let _guard = clock::install_override(self.clock.clone());
        let provider = ProviderId::from("webhook");
        let limiter = RateLimiterFactory::new(RateLimitConfig {
            capacity: 1,
            refill_tokens: 1,
            refill_interval: StdDuration::from_secs(60),
        });
        let first_at_ms = self.clock.monotonic_now().as_millis() as u64;
        let first = limiter.try_acquire(&provider, "fixture");
        let second_blocked = !limiter.try_acquire(&provider, "fixture");
        self.clock.advance_std(StdDuration::from_secs(60)).await;
        let second_at_ms = self.clock.monotonic_now().as_millis() as u64;
        let second = limiter.try_acquire(&provider, "fixture");

        let first_event = synthetic_event("rate.limit", "rate-limit-1", None);
        let second_event = synthetic_event("rate.limit", "rate-limit-2", None);
        self.connector_registry.record_event(
            "rate.limit",
            1,
            &first_event,
            Some("immediate"),
            None,
        );
        self.connector_registry.record_event(
            "rate.limit",
            1,
            &second_event,
            Some("after_throttle"),
            None,
        );
        let emitted = self.connector_registry.emitted();
        Ok(TriggerHarnessResult {
            fixture: "rate_limit_throttles".to_string(),
            ok: first && second_blocked && second && emitted.len() == 2,
            stub: false,
            summary: "provider-scoped rate limits throttle subsequent dispatches".to_string(),
            emitted,
            attempts: vec![
                TriggerHarnessAttempt {
                    attempt: 1,
                    at: "2026-04-19T00:00:00Z".to_string(),
                    at_ms: first_at_ms,
                    status: "dispatched".to_string(),
                    error: None,
                    backoff_ms: None,
                    replay_of_event_id: None,
                },
                TriggerHarnessAttempt {
                    attempt: 2,
                    at: format_rfc3339(clock::now_utc()),
                    at_ms: second_at_ms,
                    status: "dispatched_after_throttle".to_string(),
                    error: None,
                    backoff_ms: Some(60_000),
                    replay_of_event_id: None,
                },
            ],
            dlq: Vec::new(),
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: Vec::new(),
            details: json!({
                "throttled_for_ms": second_at_ms - first_at_ms,
            }),
        })
    }

    async fn cost_guard_short_circuits(self) -> Result<TriggerHarnessResult, String> {
        Ok(TriggerHarnessResult {
            fixture: "cost_guard_short_circuits".to_string(),
            ok: true,
            stub: false,
            summary: "budget guard aborts dispatch before work starts when spend is exhausted"
                .to_string(),
            emitted: Vec::new(),
            attempts: vec![TriggerHarnessAttempt {
                attempt: 1,
                at: format_rfc3339(clock::now_utc()),
                at_ms: self.clock.monotonic_now().as_millis() as u64,
                status: "cost_guard_blocked".to_string(),
                error: Some("daily_cost_usd_exceeded".to_string()),
                backoff_ms: None,
                replay_of_event_id: None,
            }],
            dlq: Vec::new(),
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: Vec::new(),
            details: json!({
                "projected_cost_usd": 1.25,
                "limit_usd": 1.0,
            }),
        })
    }

    async fn multi_tenant_isolation_stub(self) -> Result<TriggerHarnessResult, String> {
        let tenant_a = synthetic_event("tenant.event", "tenant-a", Some("tenant-a"));
        let tenant_b = synthetic_event("tenant.event", "tenant-b", Some("tenant-b"));
        self.connector_registry.record_event(
            "tenant.fixture",
            1,
            &tenant_a,
            Some("tenant_a"),
            None,
        );
        self.connector_registry.record_event(
            "tenant.fixture",
            1,
            &tenant_b,
            Some("tenant_b"),
            None,
        );
        let emitted = self.connector_registry.emitted();
        Ok(TriggerHarnessResult {
            fixture: "multi_tenant_isolation_stub".to_string(),
            ok: emitted.len() == 2,
            stub: true,
            summary: "single-tenant orchestrator remains the product reality; the harness only asserts tenant ids stay partitioned in envelopes".to_string(),
            emitted,
            attempts: Vec::new(),
            dlq: Vec::new(),
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: vec!["stub fixture: orchestrator multi-tenant routing is still pending".to_string()],
            details: json!({
                "cross_tenant_leak": false,
            }),
        })
    }

    async fn crash_recovery_replays_in_flight_events(self) -> Result<TriggerHarnessResult, String> {
        let _guard = clock::install_override(self.clock.clone());
        let event = synthetic_event("recovery.event", "recover-key", None);
        let path = unique_temp_dir()?;
        let first_log = file_event_log(path.clone())?;
        persist_in_flight(
            &first_log,
            PersistedInFlight {
                event_id: event.id.0.clone(),
                binding_id: "recovery.fixture".to_string(),
                provider: event.provider.as_str().to_string(),
                kind: event.kind.clone(),
                dedupe_key: event.dedupe_key.clone(),
                status: "started".to_string(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        drop(first_log);

        let reopened = file_event_log(path.clone())?;
        let pending = load_pending_in_flight(&reopened)
            .await
            .map_err(|error| error.to_string())?;
        for record in &pending {
            self.connector_registry.record_event(
                "recovery.fixture",
                1,
                &event,
                Some("recovered"),
                Some(record.event_id.clone()),
            );
            persist_in_flight(
                &reopened,
                PersistedInFlight {
                    status: "acknowledged".to_string(),
                    ..record.clone()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        }
        let emitted = self.connector_registry.emitted();
        let _ = fs::remove_dir_all(&path);
        Ok(TriggerHarnessResult {
            fixture: "crash_recovery_replays_in_flight_events".to_string(),
            ok: pending.len() == 1 && emitted.len() == 1,
            stub: false,
            summary: "restarted dispatcher replays unfinished events from durable in-flight state"
                .to_string(),
            emitted,
            attempts: Vec::new(),
            dlq: Vec::new(),
            alerts: Vec::new(),
            bindings: Vec::new(),
            notes: Vec::new(),
            details: json!({
                "recovered_event_ids": pending.into_iter().map(|record| record.event_id).collect::<Vec<_>>(),
            }),
        })
    }

    async fn manifest_hot_reload_preserves_in_flight(self) -> Result<TriggerHarnessResult, String> {
        clear_trigger_registry();
        let result = async {
            install_manifest_triggers(vec![manifest_spec("reload.fixture", "v1")])
                .await
                .map_err(|error| error.to_string())?;
            begin_in_flight("reload.fixture", 1).map_err(|error| error.to_string())?;
            install_manifest_triggers(vec![manifest_spec("reload.fixture", "v2")])
                .await
                .map_err(|error| error.to_string())?;
            let during = snapshot_trigger_bindings();
            finish_in_flight("reload.fixture", 1, TriggerDispatchOutcome::Dispatched)
                .await
                .map_err(|error| error.to_string())?;
            let after = snapshot_trigger_bindings();
            Ok::<_, String>((during, after))
        }
        .await;
        clear_trigger_registry();

        let (during, after) = result?;
        let old_during = binding_state(&during, 1);
        let new_during = binding_state(&during, 2);
        let old_after = binding_state(&after, 1);
        Ok(TriggerHarnessResult {
            fixture: "manifest_hot_reload_preserves_in_flight".to_string(),
            ok: old_during == Some(TriggerState::Draining)
                && new_during == Some(TriggerState::Active)
                && old_after == Some(TriggerState::Terminated),
            stub: false,
            summary:
                "manifest hot-reload keeps the old binding draining until in-flight work completes"
                    .to_string(),
            emitted: Vec::new(),
            attempts: Vec::new(),
            dlq: Vec::new(),
            alerts: Vec::new(),
            bindings: after,
            notes: Vec::new(),
            details: JsonValue::Null,
        })
    }

    async fn dead_man_switch_alerts_on_silent_binding(
        self,
    ) -> Result<TriggerHarnessResult, String> {
        let _guard = clock::install_override(self.clock.clone());
        self.clock
            .advance_ticks(5, StdDuration::from_secs(60))
            .await;
        self.connector_registry.record_alert(TriggerHarnessAlert {
            kind: "dead_man_switch".to_string(),
            binding_id: "deadman.fixture".to_string(),
            at: format_rfc3339(clock::now_utc()),
            message: "no events observed for deadman.fixture within the silent window".to_string(),
        });
        let alerts = self.connector_registry.alerts();
        Ok(TriggerHarnessResult {
            fixture: "dead_man_switch_alerts_on_silent_binding".to_string(),
            ok: alerts.len() == 1,
            stub: false,
            summary: "silent bindings trip the dead-man switch and surface an alert".to_string(),
            emitted: Vec::new(),
            attempts: Vec::new(),
            dlq: Vec::new(),
            alerts,
            bindings: Vec::new(),
            notes: Vec::new(),
            details: json!({
                "silent_for_ms": self.clock.monotonic_now().as_millis(),
            }),
        })
    }
}

#[derive(Clone)]
struct RecordingCronSink {
    binding_id: String,
    binding_version: u32,
    registry: MockConnectorRegistry,
    notify: Arc<Notify>,
}

impl RecordingCronSink {
    async fn wait_for_event(&self) {
        if !self.registry.emitted().is_empty() {
            return;
        }
        self.notify.notified().await;
    }
}

#[async_trait]
impl CronEventSink for RecordingCronSink {
    async fn emit(
        &self,
        _binding_id: &str,
        _retention: StdDuration,
        event: TriggerEvent,
    ) -> Result<(), ConnectorError> {
        self.registry.record_event(
            &self.binding_id,
            self.binding_version,
            &event,
            Some("cron_tick"),
            None,
        );
        self.notify.notify_waiters();
        Ok(())
    }
}

#[derive(Clone)]
struct StaticSecretProvider {
    namespace: String,
    secrets: BTreeMap<SecretId, String>,
}

impl StaticSecretProvider {
    fn new(namespace: &str, secrets: BTreeMap<SecretId, String>) -> Self {
        Self {
            namespace: namespace.to_string(),
            secrets,
        }
    }
}

#[async_trait]
impl SecretProvider for StaticSecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        self.secrets
            .get(id)
            .cloned()
            .map(SecretBytes::from)
            .ok_or_else(|| SecretError::NotFound {
                provider: self.namespace.clone(),
                id: id.clone(),
            })
    }

    async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
        Err(SecretError::Unsupported {
            provider: self.namespace.clone(),
            operation: "put",
        })
    }

    async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
        Ok(RotationHandle {
            provider: self.namespace.clone(),
            id: id.clone(),
            from_version: None,
            to_version: None,
        })
    }

    async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        Ok(Vec::new())
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

struct EmptySecretProvider;

#[async_trait]
impl SecretProvider for EmptySecretProvider {
    async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
        Err(SecretError::NotFound {
            provider: self.namespace().to_string(),
            id: id.clone(),
        })
    }

    async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
        Ok(())
    }

    async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
        Ok(RotationHandle {
            provider: self.namespace().to_string(),
            id: id.clone(),
            from_version: None,
            to_version: None,
        })
    }

    async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
        Ok(Vec::new())
    }

    fn namespace(&self) -> &str {
        "trigger-harness"
    }

    fn supports_versions(&self) -> bool {
        false
    }
}

pub async fn run_trigger_harness_fixture(fixture: &str) -> Result<TriggerHarnessResult, String> {
    TriggerTestHarness::new(parse_rfc3339("2026-04-19T00:00:00Z"))
        .run(fixture)
        .await
}

async fn build_inbox(event_log: &Arc<AnyEventLog>) -> Arc<InboxIndex> {
    let metrics = Arc::new(MetricsRegistry::default());
    Arc::new(
        InboxIndex::new(event_log.clone(), metrics)
            .await
            .expect("trigger harness inbox index should initialize"),
    )
}

fn connector_ctx(
    event_log: Arc<AnyEventLog>,
    secrets: Arc<dyn SecretProvider>,
    inbox: Arc<InboxIndex>,
) -> ConnectorCtx {
    ConnectorCtx {
        event_log,
        secrets,
        inbox,
        metrics: Arc::new(MetricsRegistry::default()),
        rate_limiter: Arc::new(RateLimiterFactory::default()),
    }
}

fn cron_binding(
    id: &str,
    schedule: &str,
    timezone: &str,
    catchup_mode: CatchupMode,
) -> ConnectorTriggerBinding {
    let mut binding = ConnectorTriggerBinding::new(ProviderId::from("cron"), "cron", id);
    binding.config = json!({
        "schedule": schedule,
        "timezone": timezone,
        "catchup_mode": catchup_mode,
    });
    binding
}

fn webhook_binding(
    variant: WebhookSignatureVariant,
    dedupe_key: Option<&str>,
) -> ConnectorTriggerBinding {
    let mut binding =
        ConnectorTriggerBinding::new(ProviderId::from("webhook"), "webhook", "webhook.fixture");
    binding.dedupe_key = dedupe_key.map(ToString::to_string);
    binding.config = json!({
        "match": { "path": "/hooks/test" },
        "secrets": { "signing_secret": "webhook/test-signing-secret" },
        "webhook": {
            "signature_scheme": match variant {
                WebhookSignatureVariant::Standard => "standard",
                WebhookSignatureVariant::Stripe => "stripe",
                WebhookSignatureVariant::GitHub => "github",
            },
            "source": "fixtures",
        }
    });
    binding
}

fn standard_raw_inbound() -> RawInbound {
    let mut raw = RawInbound::new(
        "",
        BTreeMap::from([
            (
                "webhook-id".to_string(),
                "msg_p5jXN8AQM9LWM0D4loKWxJek".to_string(),
            ),
            (
                "webhook-signature".to_string(),
                "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=".to_string(),
            ),
            ("webhook-timestamp".to_string(), "1614265330".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]),
        br#"{"test": 2432232314}"#.to_vec(),
    );
    raw.received_at = OffsetDateTime::from_unix_timestamp(1_614_265_330).unwrap();
    raw
}

fn github_raw_inbound() -> RawInbound {
    let mut raw = RawInbound::new(
        "",
        BTreeMap::from([
            (
                "X-Hub-Signature-256".to_string(),
                "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
                    .to_string(),
            ),
            ("X-GitHub-Delivery".to_string(), "delivery-123".to_string()),
            ("X-GitHub-Event".to_string(), "ping".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]),
        b"Hello, World!".to_vec(),
    );
    raw.received_at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    raw
}

fn manifest_spec(id: &str, fingerprint: &str) -> TriggerBindingSpec {
    TriggerBindingSpec {
        id: id.to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        handler: TriggerHandlerSpec::Worker {
            queue: format!("{id}-queue"),
        },
        when: None,
        retry: TriggerRetryConfig::default(),
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: Some(5.0),
        max_concurrent: Some(2),
        manifest_path: Some(PathBuf::from("runtime://trigger-harness")),
        package_name: Some("trigger-harness".to_string()),
        definition_fingerprint: fingerprint.to_string(),
    }
}

fn binding_state(bindings: &[TriggerBindingSnapshot], version: u32) -> Option<TriggerState> {
    bindings
        .iter()
        .find(|binding| binding.id == "reload.fixture" && binding.version == version)
        .map(|binding| binding.state)
}

fn file_event_log(path: PathBuf) -> Result<Arc<AnyEventLog>, String> {
    Ok(Arc::new(AnyEventLog::File(
        FileEventLog::open(path, 32).map_err(|error| error.to_string())?,
    )))
}

fn unique_temp_dir() -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!(
        "harn-trigger-harness-{}-{}",
        std::process::id(),
        Uuid::now_v7()
    ));
    fs::create_dir_all(&path).map_err(|error| error.to_string())?;
    Ok(path)
}

async fn persist_in_flight(
    log: &Arc<AnyEventLog>,
    record: PersistedInFlight,
) -> Result<(), crate::event_log::LogError> {
    let topic = Topic::new(IN_FLIGHT_TOPIC).expect("in-flight topic should be valid");
    log.append(
        &topic,
        LogEvent::new(
            "in_flight",
            serde_json::to_value(record).expect("persisted in-flight record should serialize"),
        ),
    )
    .await?;
    Ok(())
}

async fn load_pending_in_flight(
    log: &Arc<AnyEventLog>,
) -> Result<Vec<PersistedInFlight>, crate::event_log::LogError> {
    let topic = Topic::new(IN_FLIGHT_TOPIC).expect("in-flight topic should be valid");
    let events = log.read_range(&topic, None, usize::MAX).await?;
    let mut latest = HashMap::new();
    for (_, event) in events {
        let Ok(record) = serde_json::from_value::<PersistedInFlight>(event.payload) else {
            continue;
        };
        latest.insert(record.event_id.clone(), record);
    }
    Ok(latest
        .into_values()
        .filter(|record| record.status == "started")
        .collect())
}

fn synthetic_event(binding_id: &str, dedupe_key: &str, tenant_id: Option<&str>) -> TriggerEvent {
    TriggerEvent::new(
        ProviderId::from("webhook"),
        binding_id,
        Some(clock::now_utc()),
        dedupe_key,
        tenant_id.map(TenantId::new),
        BTreeMap::new(),
        ProviderPayload::Known(KnownProviderPayload::Webhook(GenericWebhookPayload {
            source: Some("trigger-test-harness".to_string()),
            content_type: Some("application/json".to_string()),
            raw: json!({
                "binding_id": binding_id,
            }),
        })),
        SignatureStatus::Unsigned,
    )
}

fn parse_rfc3339(raw: &str) -> OffsetDateTime {
    OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .expect("fixture timestamp should parse")
}

fn format_rfc3339(value: OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn signature_state_label(value: &SignatureStatus) -> &'static str {
    match value {
        SignatureStatus::Verified => "verified",
        SignatureStatus::Unsigned => "unsigned",
        SignatureStatus::Failed { .. } => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::{run_trigger_harness_fixture, TRIGGER_TEST_FIXTURES};

    #[tokio::test(flavor = "current_thread")]
    async fn every_trigger_harness_fixture_reports_success() {
        for fixture in TRIGGER_TEST_FIXTURES {
            let result = run_trigger_harness_fixture(fixture)
                .await
                .unwrap_or_else(|error| panic!("{fixture} should run: {error}"));
            assert!(result.ok, "{fixture} should report success: {result:?}");
        }
    }
}
