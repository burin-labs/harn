use super::*;
use crate::event_log::{
    install_active_event_log, install_memory_for_current_thread, reset_active_event_log,
    AnyEventLog, SqliteEventLog,
};
use crate::observability::execution_scope::{enter_execution_scope, mint_execution_scope};

fn string(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn dict(entries: impl IntoIterator<Item = (&'static str, VmValue)>) -> VmValue {
    VmValue::dict(
        entries
            .into_iter()
            .map(|(key, value)| (crate::value::intern_key(key), value))
            .collect::<crate::value::DictMap>(),
    )
}

fn plan_fingerprint() -> String {
    format!("sha256:{}", "b".repeat(64))
}

fn hypothesis_event_payload_from_content(content: VmValue) -> (VmValue, String) {
    let canonical = crate::stdlib::json::vm_value_to_json(&content);
    let fingerprint = format!(
        "sha256:{}",
        harn_kernel::pure::sha256_hex(canonical.as_bytes())
    );
    let payload = dict([("content", content), ("fingerprint", string(&fingerprint))]);
    (payload, fingerprint)
}

fn hypothesis_event_payload() -> (VmValue, String) {
    hypothesis_event_payload_from_content(dict([
        ("schema", string("harn.hypothesis.event.v1")),
        ("event_id", string("event-1")),
        ("hypothesis_id", string("hyp-1")),
        ("plan_id", VmValue::Nil),
        ("run_id", VmValue::Nil),
        ("payload", dict([("kind", string("plan_registered"))])),
    ]))
}

fn ctx() -> crate::vm::AsyncBuiltinCtx {
    crate::vm::AsyncBuiltinCtx::for_test(Vm::new())
}

async fn mint_proof(
    authority_kind: &str,
    event_fingerprint: &str,
    plan_fingerprint: &str,
    run_id: Option<&str>,
) -> VmValue {
    let attestation = mint_hypothesis_native_attestation(
        authority_kind,
        event_fingerprint,
        plan_fingerprint,
        "hyp-1",
        run_id,
    )
    .expect("registered native adapter should issue an attestation");
    hypothesis_event_authority_mint_impl(
        ctx(),
        vec![
            attestation,
            string(authority_kind),
            string(event_fingerprint),
            string(plan_fingerprint),
            string("hyp-1"),
            run_id.map(string).unwrap_or(VmValue::Nil),
        ],
    )
    .await
    .expect("native authority mint should succeed")
}

fn append_args(
    proof: VmValue,
    payload: VmValue,
    event_fingerprint: &str,
    plan_fingerprint: &str,
    run_id: Option<&str>,
    expected_head: Option<&str>,
) -> Vec<VmValue> {
    vec![
        proof,
        string("hypothesis.plan_registered"),
        string(event_fingerprint),
        expected_head.map(string).unwrap_or(VmValue::Nil),
        string(event_fingerprint),
        string(plan_fingerprint),
        string("hyp-1"),
        run_id.map(string).unwrap_or(VmValue::Nil),
        payload,
        dict([]),
    ]
}

#[tokio::test(flavor = "current_thread")]
async fn native_append_rejects_provenance_headers_before_commit() {
    reset_active_event_log();
    install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
    let _scope = enter_execution_scope(mint_execution_scope());
    let (payload, event_fingerprint) = hypothesis_event_payload();
    let plan_fingerprint = plan_fingerprint();
    let proof = mint_proof(
        "plan_admission",
        &event_fingerprint,
        &plan_fingerprint,
        None,
    )
    .await;
    let mut args = append_args(
        proof,
        payload,
        &event_fingerprint,
        &plan_fingerprint,
        None,
        None,
    );
    args[9] = dict([("harn.provenance.record_hash", string("caller-controlled"))]);

    let error = hypothesis_event_append_impl(ctx(), args)
        .await
        .expect_err("provenance headers must be rejected before persistence");
    assert!(error
        .to_string()
        .contains("header 'harn.provenance.record_hash' is reserved"));

    let topic = Topic::new(HYPOTHESIS_LEDGER_TOPIC).unwrap();
    let events = ensure_event_log()
        .read_range(&topic, None, usize::MAX)
        .await
        .unwrap();
    assert!(
        events.is_empty(),
        "a rejected append must not commit an event"
    );
    reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn native_sqlite_reopen_replays_then_appends_a_follow_up_event() {
    reset_active_event_log();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hypothesis-events.sqlite");
    install_active_event_log(Arc::new(AnyEventLog::Sqlite(
        SqliteEventLog::open(path.clone(), EVENT_LOG_QUEUE_DEPTH).unwrap(),
    )));
    let (plan_payload, plan_event_fingerprint) = hypothesis_event_payload();
    let plan_fingerprint = plan_fingerprint();
    {
        let _scope = enter_execution_scope(mint_execution_scope());
        let proof = mint_proof(
            "plan_admission",
            &plan_event_fingerprint,
            &plan_fingerprint,
            None,
        )
        .await;
        let inserted = hypothesis_event_append_impl(
            ctx(),
            append_args(
                proof,
                plan_payload.clone(),
                &plan_event_fingerprint,
                &plan_fingerprint,
                None,
                None,
            ),
        )
        .await
        .expect("native plan admission should persist to SQLite");
        assert_eq!(vm_value_to_json(&inserted)["inserted"], true);
    }

    reset_active_event_log();
    install_active_event_log(Arc::new(AnyEventLog::Sqlite(
        SqliteEventLog::open(path, EVENT_LOG_QUEUE_DEPTH).unwrap(),
    )));
    let _scope = enter_execution_scope(mint_execution_scope());
    let replay_proof = mint_proof(
        "plan_admission",
        &plan_event_fingerprint,
        &plan_fingerprint,
        None,
    )
    .await;
    let replay = hypothesis_event_append_impl(
        ctx(),
        append_args(
            replay_proof,
            plan_payload,
            &plan_event_fingerprint,
            &plan_fingerprint,
            None,
            Some("sha256:stale-after-reopen"),
        ),
    )
    .await
    .expect("a native adapter may remint exact authority and replay after reopen");
    assert_eq!(vm_value_to_json(&replay)["inserted"], false);

    let topic = Topic::new(HYPOTHESIS_LEDGER_TOPIC).unwrap();
    let persisted = ensure_event_log()
        .read_range(&topic, None, usize::MAX)
        .await
        .unwrap();
    let head = persisted[0].1.headers[crate::provenance::HEADER_RECORD_HASH].clone();
    let (follow_up_payload, follow_up_fingerprint) = hypothesis_event_payload_from_content(dict([
        ("schema", string("harn.hypothesis.event.v1")),
        ("event_id", string("event-2")),
        ("hypothesis_id", string("hyp-1")),
        ("plan_id", string("plan-1")),
        ("run_id", string("run-1")),
        (
            "payload",
            dict([
                ("kind", string("run_transition")),
                ("state", string("scheduled")),
            ]),
        ),
    ]));
    let follow_up_proof = mint_proof(
        "lifecycle_audit",
        &follow_up_fingerprint,
        &plan_fingerprint,
        Some("run-1"),
    )
    .await;
    let mut follow_up_args = append_args(
        follow_up_proof,
        follow_up_payload,
        &follow_up_fingerprint,
        &plan_fingerprint,
        Some("run-1"),
        Some(&head),
    );
    follow_up_args[1] = string("hypothesis.run_transition");
    let follow_up = hypothesis_event_append_impl(ctx(), follow_up_args)
        .await
        .expect("a reminted native lifecycle event should append after SQLite reopen");
    assert_eq!(vm_value_to_json(&follow_up)["inserted"], true);
    let events = ensure_event_log()
        .read_range(&topic, None, usize::MAX)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    reset_active_event_log();
}
