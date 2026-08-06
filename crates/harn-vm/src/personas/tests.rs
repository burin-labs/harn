use super::*;
use crate::event_log::{AnyEventLog, MemoryEventLog};
use std::sync::Mutex;

struct CapturingValueSink {
    events: Arc<Mutex<Vec<PersonaValueEvent>>>,
}

impl PersonaValueSink for CapturingValueSink {
    fn handle_value_event(&self, event: &PersonaValueEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn binding() -> PersonaRuntimeBinding {
    PersonaRuntimeBinding {
        name: "merge_captain".to_string(),
        template_ref: Some("software_factory@v0".to_string()),
        entry_workflow: "workflows/merge.harn#run".to_string(),
        schedules: vec!["*/30 * * * *".to_string()],
        triggers: vec!["github.pr_opened".to_string()],
        budget: PersonaBudgetPolicy {
            daily_usd: Some(0.02),
            hourly_usd: None,
            run_usd: Some(0.02),
            max_tokens: Some(100),
        },
        stages: Vec::new(),
    }
}

fn log() -> Arc<AnyEventLog> {
    Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64)))
}

#[test]
fn parse_rfc3339_ms_round_trips_post_2262_dates() {
    // Year 2300 sits beyond `i64` nanoseconds (~April 2262). Casting
    // `unix_timestamp_nanos()` to `i64` *before* dividing by 1_000_000
    // would truncate and yield a negative value; dividing first keeps
    // the conversion exact.
    let raw = "2300-01-01T00:00:00Z";
    let ms = parse_rfc3339_ms(raw).expect("parse 2300-01-01");
    assert!(ms > 0, "expected positive ms for {raw}, got {ms}");
    let round_tripped = parse_rfc3339_ms(&format_ms(ms)).expect("re-parse formatted ms");
    assert_eq!(ms, round_tripped, "ms should round-trip through format_ms");
}

#[tokio::test]
async fn schedule_tick_records_lifecycle_status_and_receipt() {
    let log = log();
    let binding = binding();
    let now = parse_rfc3339_ms("2026-04-24T12:30:00Z").unwrap();
    let receipt = fire_schedule(
        &log,
        &binding,
        PersonaRunCost {
            cost_usd: 0.01,
            tokens: 10,
            ..Default::default()
        },
        now,
    )
    .await
    .unwrap();
    assert_eq!(receipt.status, "completed");
    assert!(receipt.lease.is_some());
    let status = persona_status(&log, &binding, now).await.unwrap();
    assert_eq!(status.state, PersonaLifecycleState::Idle);
    assert_eq!(status.last_run.as_deref(), Some("2026-04-24T12:30:00Z"));
    assert!(status.next_scheduled_run.is_some());
    assert_eq!(status.budget.spent_today_usd, 0.01);
}

#[tokio::test]
async fn paused_personas_queue_and_resume_drains_once() {
    let log = log();
    let binding = binding();
    let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();
    pause_persona(&log, &binding, now).await.unwrap();
    let receipt = fire_trigger(
        &log,
        &binding,
        "github",
        "pull_request",
        BTreeMap::from([
            ("repository".to_string(), "burin-labs/harn".to_string()),
            ("number".to_string(), "462".to_string()),
        ]),
        PersonaRunCost::default(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(receipt.status, "queued");
    assert_eq!(
        persona_status(&log, &binding, now)
            .await
            .unwrap()
            .queued_events,
        1
    );
    let status = resume_persona(&log, &binding, now + 1000).await.unwrap();
    assert_eq!(status.state, PersonaLifecycleState::Idle);
    assert_eq!(status.queued_events, 0);
}

#[tokio::test]
async fn resumed_queued_work_reuses_original_budget_cost() {
    let log = log();
    let mut binding = binding();
    binding.budget.run_usd = Some(0.01);
    let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();
    pause_persona(&log, &binding, now).await.unwrap();
    let queued = fire_trigger(
        &log,
        &binding,
        "github",
        "pull_request",
        BTreeMap::from([
            ("repository".to_string(), "burin-labs/harn".to_string()),
            ("number".to_string(), "1379".to_string()),
        ]),
        PersonaRunCost {
            cost_usd: 0.02,
            tokens: 1,
            ..Default::default()
        },
        now + 1,
    )
    .await
    .unwrap();
    assert_eq!(queued.status, "queued");

    let status = resume_persona(&log, &binding, now + 2).await.unwrap();

    assert_eq!(status.budget.reason.as_deref(), Some("run_usd"));
    assert!(status
        .last_error
        .as_deref()
        .is_some_and(|error| error.contains("run_usd")));
    assert_eq!(status.queued_events, 1);
}

#[tokio::test]
async fn duplicate_trigger_envelope_is_not_processed_twice() {
    let log = log();
    let binding = binding();
    let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();
    let metadata = BTreeMap::from([
        ("repository".to_string(), "burin-labs/harn".to_string()),
        ("number".to_string(), "462".to_string()),
    ]);
    let first = fire_trigger(
        &log,
        &binding,
        "github",
        "pull_request",
        metadata.clone(),
        PersonaRunCost::default(),
        now,
    )
    .await
    .unwrap();
    let second = fire_trigger(
        &log,
        &binding,
        "github",
        "pull_request",
        metadata,
        PersonaRunCost::default(),
        now + 1000,
    )
    .await
    .unwrap();
    assert_eq!(first.status, "completed");
    assert_eq!(second.status, "duplicate");
    assert!(second.lease.is_none());
}

#[tokio::test]
async fn disabled_personas_dead_letter_events() {
    let log = log();
    let binding = binding();
    let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();
    disable_persona(&log, &binding, now).await.unwrap();
    let receipt = fire_trigger(
        &log,
        &binding,
        "slack",
        "message",
        BTreeMap::from([
            ("channel".to_string(), "C123".to_string()),
            ("ts".to_string(), "1713988800.000100".to_string()),
        ]),
        PersonaRunCost::default(),
        now,
    )
    .await
    .unwrap();
    assert_eq!(receipt.status, "dead_lettered");
    let status = persona_status(&log, &binding, now).await.unwrap();
    assert_eq!(status.state, PersonaLifecycleState::Disabled);
    assert_eq!(status.disabled_events, 1);
}

#[tokio::test]
async fn budget_exhaustion_blocks_expensive_work() {
    let log = log();
    let mut binding = binding();
    binding.budget.daily_usd = Some(0.01);
    let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();
    let receipt = fire_trigger(
        &log,
        &binding,
        "linear",
        "issue",
        BTreeMap::from([("issue_key".to_string(), "HAR-462".to_string())]),
        PersonaRunCost {
            cost_usd: 0.02,
            tokens: 1,
            ..Default::default()
        },
        now,
    )
    .await
    .unwrap();
    assert_eq!(receipt.status, "budget_exhausted");
    let status = persona_status(&log, &binding, now).await.unwrap();
    assert_eq!(status.budget.reason.as_deref(), Some("daily_usd"));
    assert!(status.budget.exhausted);
    assert!(status.last_error.as_deref().unwrap().contains("daily_usd"));
}

#[tokio::test]
async fn deterministic_predicate_hit_emits_value_event_with_avoided_cost() {
    let log = log();
    let binding = binding();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let _registration = register_persona_value_sink(Arc::new(CapturingValueSink {
        events: captured.clone(),
    }));
    let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();

    let receipt = fire_trigger(
        &log,
        &binding,
        "github",
        "pull_request",
        BTreeMap::from([
            ("repository".to_string(), "burin-labs/harn".to_string()),
            ("number".to_string(), "715".to_string()),
        ]),
        PersonaRunCost {
            avoided_cost_usd: 0.0042,
            deterministic_steps: 1,
            metadata: json!({
                "predicate": "pr_already_green",
                "would_have_called_model": "gpt-5.4-mini",
            }),
            ..Default::default()
        },
        now,
    )
    .await
    .unwrap();

    let run_id = receipt.run_id.expect("completed run has run_id");
    let events = captured.lock().unwrap().clone();
    let deterministic = events
        .iter()
        .find(|event| {
            event.kind == PersonaValueEventKind::DeterministicExecution
                && event.run_id == Some(run_id)
        })
        .expect("deterministic execution value event");
    assert_eq!(deterministic.persona_id, "merge_captain");
    assert_eq!(
        deterministic.template_ref.as_deref(),
        Some("software_factory@v0")
    );
    assert_eq!(deterministic.run_id, Some(run_id));
    assert_eq!(deterministic.paid_cost_usd, 0.0);
    assert_eq!(deterministic.avoided_cost_usd, 0.0042);
    assert_eq!(deterministic.deterministic_steps, 1);
    assert_eq!(
        deterministic.metadata["predicate"].as_str(),
        Some("pr_already_green")
    );

    let persisted = read_persona_events(&log, &binding.name).await.unwrap();
    assert!(persisted.iter().any(|(_, event)| {
        event.kind == "persona.value.deterministic_execution"
            && event.payload["avoided_cost_usd"] == json!(0.0042)
    }));
}

#[tokio::test]
async fn frontier_escalation_run_emits_value_event_with_paid_cost() {
    let log = log();
    let binding = binding();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let _registration = register_persona_value_sink(Arc::new(CapturingValueSink {
        events: captured.clone(),
    }));
    let now = parse_rfc3339_ms("2026-04-24T12:00:00Z").unwrap();

    let receipt = fire_trigger(
        &log,
        &binding,
        "linear",
        "issue",
        BTreeMap::from([("issue_key".to_string(), "HAR-715".to_string())]),
        PersonaRunCost {
            cost_usd: 0.011,
            tokens: 20,
            llm_steps: 1,
            frontier_escalations: 1,
            metadata: json!({
                "frontier_model": "gpt-5.4",
                "escalation_reason": "high_risk_merge",
            }),
            ..Default::default()
        },
        now,
    )
    .await
    .unwrap();

    let run_id = receipt.run_id.expect("completed run has run_id");
    let events = captured.lock().unwrap().clone();
    let escalation = events
        .iter()
        .find(|event| {
            event.kind == PersonaValueEventKind::FrontierEscalation && event.run_id == Some(run_id)
        })
        .expect("frontier escalation value event");
    assert_eq!(escalation.run_id, Some(run_id));
    assert_eq!(escalation.paid_cost_usd, 0.011);
    assert_eq!(escalation.avoided_cost_usd, 0.0);
    assert_eq!(escalation.llm_steps, 1);
    assert_eq!(
        escalation.metadata["frontier_model"].as_str(),
        Some("gpt-5.4")
    );

    let completion = events
        .iter()
        .find(|event| {
            event.kind == PersonaValueEventKind::RunCompleted && event.run_id == Some(run_id)
        })
        .expect("run completed value event");
    assert_eq!(completion.paid_cost_usd, 0.0);
}

struct CapturingSupervisionSink {
    events: Arc<Mutex<Vec<PersonaSupervisionEvent>>>,
}

impl PersonaSupervisionSink for CapturingSupervisionSink {
    fn handle_supervision_event(&self, event: &PersonaSupervisionEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn pr_metadata(repository: &str, number: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("repository".to_string(), repository.to_string()),
        ("number".to_string(), number.to_string()),
    ])
}

fn binding_named(name: &str) -> PersonaRuntimeBinding {
    PersonaRuntimeBinding {
        name: name.to_string(),
        ..binding()
    }
}

fn supervision_events_for(
    captured: &Arc<Mutex<Vec<PersonaSupervisionEvent>>>,
    persona: &str,
) -> Vec<PersonaSupervisionEvent> {
    captured
        .lock()
        .unwrap()
        .iter()
        .filter(|event| match event {
            PersonaSupervisionEvent::QueuePosition(update) => update.persona_id == persona,
            PersonaSupervisionEvent::RepairWorkerStatus(update) => update.persona_id == persona,
            PersonaSupervisionEvent::Receipt(update) => update.persona_id == persona,
            PersonaSupervisionEvent::Checkpoint(update) => update.persona_id == persona,
        })
        .cloned()
        .collect()
}

async fn drive_pause_then_resume(binding: &PersonaRuntimeBinding, now: i64) {
    let log = log();
    pause_persona(&log, binding, now).await.unwrap();
    let _ = fire_trigger(
        &log,
        binding,
        "github",
        "pull_request",
        pr_metadata("burin-labs/harn", "1480"),
        PersonaRunCost::default(),
        now,
    )
    .await
    .unwrap();
    let _ = resume_persona(&log, binding, now + 1).await.unwrap();
    let _ = restore_persona_checkpoint(
        &log,
        binding,
        PersonaCheckpointRestoreRequest {
            checkpoint_id: "cp_42".to_string(),
            work_key: Some("github:burin-labs/harn:pr:1480".to_string()),
            resumed_from: Some(PersonaCheckpointResume {
                note: Some("resumed from cp 42".to_string()),
                ..Default::default()
            }),
        },
        now + 2,
    )
    .await
    .unwrap();
    let _ = report_repair_worker_status(
        &log,
        binding,
        PersonaRepairWorkerStatusUpdate {
            persona_id: String::new(),
            template_ref: None,
            repair_worker_id: "rw_42".to_string(),
            lifecycle: PersonaRepairWorkerLifecycle::Running,
            work_key: Some("github:burin-labs/harn:pr:1480".to_string()),
            lease_id: Some("persona_lease_xyz".to_string()),
            scratchpad_url: Some("https://factory.local/rw_42".to_string()),
            last_heartbeat_ms: 0,
            occurred_at_ms: 0,
        },
        now + 3,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn supervision_sink_emits_queue_position_and_receipt() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let _registration = register_persona_supervision_sink(Arc::new(CapturingSupervisionSink {
        events: captured.clone(),
    }));
    let log = log();
    let binding = binding_named("supervision_sink_emits_queue_position_and_receipt");
    let now = parse_rfc3339_ms("2026-05-01T00:00:00Z").unwrap();

    pause_persona(&log, &binding, now).await.unwrap();
    fire_trigger(
        &log,
        &binding,
        "github",
        "pull_request",
        pr_metadata("burin-labs/harn", "1480"),
        PersonaRunCost::default(),
        now + 100,
    )
    .await
    .unwrap();
    resume_persona(&log, &binding, now + 200).await.unwrap();

    let events = supervision_events_for(&captured, &binding.name);
    let queue_events: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PersonaSupervisionEvent::QueuePosition(update) => Some(update.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(queue_events.len(), 2, "enqueue + drain emitted");
    assert_eq!(queue_events[0].position, 1);
    assert_eq!(queue_events[0].queue_depth, 1);
    assert_eq!(queue_events[1].position, 0);
    assert_eq!(queue_events[1].queue_depth, 0);
    let receipt_events: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            PersonaSupervisionEvent::Receipt(update) => Some(update.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(receipt_events.len(), 2, "queued + drained receipt");
    assert_eq!(receipt_events[0].receipt.status, "queued");
    assert_eq!(receipt_events[1].receipt.status, "completed");
    for event in &receipt_events {
        assert_eq!(event.receipt.persona, binding.name);
        assert_eq!(event.persona_id, binding.name);
        assert_eq!(event.template_ref.as_deref(), Some("software_factory@v0"));
    }
    let persisted_kinds: Vec<_> = read_persona_events(&log, &binding.name)
        .await
        .unwrap()
        .into_iter()
        .map(|(_, event)| event.kind)
        .collect();
    assert!(
        persisted_kinds
            .iter()
            .any(|kind| kind == "persona.supervision.queue_position"),
        "queue_position supervision events should be durable: {persisted_kinds:?}"
    );
    assert!(
        persisted_kinds
            .iter()
            .any(|kind| kind == "persona.supervision.receipt"),
        "receipt supervision events should be durable: {persisted_kinds:?}"
    );
}

#[tokio::test]
async fn supervision_sink_emits_repair_worker_status_idempotently() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let _registration = register_persona_supervision_sink(Arc::new(CapturingSupervisionSink {
        events: captured.clone(),
    }));
    let log = log();
    let binding = binding_named("supervision_sink_emits_repair_worker_status_idempotently");
    let now = parse_rfc3339_ms("2026-05-01T01:00:00Z").unwrap();
    let update = PersonaRepairWorkerStatusUpdate {
        persona_id: String::new(),
        template_ref: None,
        repair_worker_id: "rw_test".to_string(),
        lifecycle: PersonaRepairWorkerLifecycle::Running,
        work_key: Some("github:burin-labs/harn:pr:1480".to_string()),
        lease_id: Some("persona_lease_abc".to_string()),
        scratchpad_url: Some("https://factory.local/rw_test".to_string()),
        last_heartbeat_ms: 0,
        occurred_at_ms: 0,
    };
    let first = report_repair_worker_status(&log, &binding, update.clone(), now)
        .await
        .unwrap();
    let second = report_repair_worker_status(&log, &binding, update.clone(), now + 5)
        .await
        .unwrap();
    assert!(first);
    assert!(!second, "second identical lifecycle is idempotent");

    let mut next = update.clone();
    next.lifecycle = PersonaRepairWorkerLifecycle::Succeeded;
    let third = report_repair_worker_status(&log, &binding, next, now + 10)
        .await
        .unwrap();
    assert!(third);

    let kinds: Vec<_> = supervision_events_for(&captured, &binding.name)
        .into_iter()
        .filter_map(|event| match event {
            PersonaSupervisionEvent::RepairWorkerStatus(update) => Some(update.lifecycle),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            PersonaRepairWorkerLifecycle::Running,
            PersonaRepairWorkerLifecycle::Succeeded
        ]
    );
}

#[tokio::test]
async fn supervision_sink_emits_checkpoint_restore_ack() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let _registration = register_persona_supervision_sink(Arc::new(CapturingSupervisionSink {
        events: captured.clone(),
    }));
    let log = log();
    let binding = binding_named("supervision_sink_emits_checkpoint_restore_ack");
    let now = parse_rfc3339_ms("2026-05-01T02:00:00Z").unwrap();
    fire_trigger(
        &log,
        &binding,
        "github",
        "pull_request",
        pr_metadata("burin-labs/harn", "1480"),
        PersonaRunCost::default(),
        now,
    )
    .await
    .unwrap();

    let outcome = restore_persona_checkpoint(
        &log,
        &binding,
        PersonaCheckpointRestoreRequest {
            checkpoint_id: "cp_1".to_string(),
            work_key: Some("github:burin-labs/harn:pr:1480".to_string()),
            resumed_from: None,
        },
        now + 100,
    )
    .await
    .unwrap();
    assert!(outcome.acked);
    assert_eq!(outcome.update.checkpoint_id, "cp_1");
    let resume = outcome
        .update
        .resumed_from
        .as_ref()
        .expect("resume coordinates default-derived from status");
    assert_eq!(resume.last_run_ms, Some(now));

    let replay = restore_persona_checkpoint(
        &log,
        &binding,
        PersonaCheckpointRestoreRequest {
            checkpoint_id: "cp_1".to_string(),
            work_key: None,
            resumed_from: None,
        },
        now + 200,
    )
    .await
    .unwrap();
    assert!(!replay.acked, "duplicate restore is a no-op ack");
    assert_eq!(replay.update.occurred_at_ms, now + 100);

    let ack_events: Vec<_> = supervision_events_for(&captured, &binding.name)
        .into_iter()
        .filter_map(|event| match event {
            PersonaSupervisionEvent::Checkpoint(update) => Some(update),
            _ => None,
        })
        .collect();
    assert_eq!(ack_events.len(), 1, "ack emitted once, replay suppressed");
    assert_eq!(ack_events[0].action, PersonaCheckpointAction::RestoreAcked);
}

#[tokio::test]
async fn supervision_sink_replay_is_deterministic_under_recorded_clock() {
    use harn_clock::{ClockEventLog, PausedClock, RecordedClock};
    use time::OffsetDateTime;

    let now_ms = parse_rfc3339_ms("2026-05-01T03:00:00Z").unwrap();
    async fn drive(now_ms: i64) -> (Vec<PersonaSupervisionEvent>, Vec<harn_clock::ClockEvent>) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let _registration = register_persona_supervision_sink(Arc::new(CapturingSupervisionSink {
            events: captured.clone(),
        }));
        let paused = PausedClock::new(
            OffsetDateTime::from_unix_timestamp_nanos((now_ms as i128) * 1_000_000).unwrap(),
        );
        let recorded = Arc::new(RecordedClock::new(paused, Arc::new(ClockEventLog::new())));
        let binding = binding_named("supervision_replay_persona");
        let ts = harn_clock::now_wall_ms(&*recorded);
        drive_pause_then_resume(&binding, ts).await;
        let clock_log = recorded.log().snapshot();
        let events = supervision_events_for(&captured, &binding.name);
        (events, clock_log)
    }

    let (events_a, clock_a) = drive(now_ms).await;
    let (events_b, clock_b) = drive(now_ms).await;
    // Run receipts carry per-run `run_id`s (UUIDv7) and lease ids that
    // differ across runs. Normalize them so the deterministic envelope
    // remains comparable.
    fn normalize(event: &PersonaSupervisionEvent) -> PersonaSupervisionEvent {
        match event.clone() {
            PersonaSupervisionEvent::Receipt(mut update) => {
                update.receipt.run_id = None;
                if let Some(lease) = update.receipt.lease.as_mut() {
                    lease.id = "lease".to_string();
                }
                update.receipt.budget_receipt_id = update
                    .receipt
                    .budget_receipt_id
                    .map(|_| "budget".to_string());
                PersonaSupervisionEvent::Receipt(update)
            }
            PersonaSupervisionEvent::Checkpoint(mut update) => {
                if let Some(resume) = update.resumed_from.as_mut() {
                    resume.run_id = None;
                    resume.lease_id = None;
                }
                PersonaSupervisionEvent::Checkpoint(update)
            }
            other => other,
        }
    }
    let a: Vec<_> = events_a.iter().map(normalize).collect();
    let b: Vec<_> = events_b.iter().map(normalize).collect();
    assert_eq!(a, b, "supervision sink emits identical event envelopes");
    assert_eq!(
        clock_a, clock_b,
        "recorded clock observation log is identical across replays"
    );
}
