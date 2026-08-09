use super::*;

const HOST_CAPABILITY: &str = "hypothesis";
const HOST_OPERATION: &str = "attest_event";
const HOST_WORKFLOW_OPERATION: &str = "operation";
const HOST_RESULT_SCHEMA: &str = "harn.host-result.v1";
const HOST_RESULT_KIND: &str = "hypothesis_native_attestation";

#[harn_builtin(
    exposure = "harness.obs.hypothesis_operation_request",
    effects = ["authority.write@const=hypothesis.operation"],
    sig = "event_log.hypothesis_operation_request(request: dict) -> dict?",
    kind = "async",
    category = "event_log"
)]
pub(super) async fn hypothesis_operation_request_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let builtin = "event_log.hypothesis_operation_request";
    if args.len() != 1 {
        return Err(VmError::TypeError(format!(
            "{builtin}: expected exactly 1 argument, got {}",
            args.len()
        )));
    }
    let request = args
        .first()
        .and_then(VmValue::as_dict)
        .ok_or_else(|| VmError::TypeError(format!("{builtin}: request must be a dict")))?;
    match crate::stdlib::host::dispatch_host_call_bridge(
        HOST_CAPABILITY,
        HOST_WORKFLOW_OPERATION,
        request,
    )
    .await
    {
        Some(result) => result,
        None => Ok(VmValue::Nil),
    }
}

#[harn_builtin(
    exposure = "harness.obs.hypothesis_event_authority_request",
    effects = ["authority.write@arg0"],
    sig = "event_log.hypothesis_authority_request(authority_kind: string, event_fingerprint: string, plan_fingerprint: string, hypothesis_id: string, operation_receipt_id: string, run_id?: string, event?: any) -> resource",
    kind = "async",
    category = "event_log"
)]
pub(super) async fn hypothesis_event_authority_request_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let builtin = "event_log.hypothesis_authority_request";
    let authority_kind = required_non_empty_string(args.first(), builtin, "authority_kind")?;
    HypothesisAuthorityKind::parse(&authority_kind, builtin)?;
    let event_fingerprint = required_sha256_fingerprint(args.get(1), builtin, "event_fingerprint")?;
    let plan_fingerprint = required_sha256_fingerprint(args.get(2), builtin, "plan_fingerprint")?;
    let hypothesis_id = required_non_empty_string(args.get(3), builtin, "hypothesis_id")?;
    let operation_receipt_id =
        required_non_empty_string(args.get(4), builtin, "operation_receipt_id")?;
    let run_id = optional_non_empty_string(args.get(5), builtin, "run_id")?;
    let event = args.get(6).cloned().unwrap_or(VmValue::Nil);
    if args.len() > 7 {
        return Err(VmError::TypeError(format!(
            "{builtin}: expected at most 7 arguments, got {}",
            args.len()
        )));
    }

    let params = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("authority_kind"),
            string_value(&authority_kind),
        ),
        (
            crate::value::intern_key("event_fingerprint"),
            string_value(&event_fingerprint),
        ),
        (
            crate::value::intern_key("plan_fingerprint"),
            string_value(&plan_fingerprint),
        ),
        (
            crate::value::intern_key("hypothesis_id"),
            string_value(&hypothesis_id),
        ),
        (
            crate::value::intern_key("operation_receipt_id"),
            string_value(&operation_receipt_id),
        ),
        (
            crate::value::intern_key("run_id"),
            run_id.as_deref().map(string_value).unwrap_or(VmValue::Nil),
        ),
        (crate::value::intern_key("event"), event),
    ]);
    let response = crate::stdlib::host::dispatch_host_call_bridge(
        HOST_CAPABILITY,
        HOST_OPERATION,
        &params,
    )
    .await
    .ok_or_else(|| {
        VmError::Runtime(format!(
            "{builtin}: no registered native hypothesis adapter handled {HOST_CAPABILITY}.{HOST_OPERATION}"
        ))
    })??;
    validate_host_result(&response, builtin)?;

    let attestation = mint_hypothesis_native_attestation(
        &authority_kind,
        &event_fingerprint,
        &plan_fingerprint,
        &hypothesis_id,
        run_id.as_deref(),
    )?;
    hypothesis_event_authority_mint_impl(
        ctx,
        vec![
            attestation,
            string_value(&authority_kind),
            string_value(&event_fingerprint),
            string_value(&plan_fingerprint),
            string_value(&hypothesis_id),
            run_id.as_deref().map(string_value).unwrap_or(VmValue::Nil),
        ],
    )
    .await
}

fn string_value(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn validate_host_result(value: &VmValue, builtin: &str) -> Result<(), VmError> {
    let expected_error = || {
        VmError::Runtime(format!(
            "{builtin}: native adapter returned an invalid {HOST_CAPABILITY}.{HOST_OPERATION} result"
        ))
    };
    let outer = value.as_dict().ok_or_else(&expected_error)?;
    if outer.len() != 1 {
        return Err(expected_error());
    }
    let meta = outer
        .get("_meta")
        .and_then(VmValue::as_dict)
        .ok_or_else(&expected_error)?;
    if meta.len() != 1 {
        return Err(expected_error());
    }
    let harn = meta
        .get("harn")
        .and_then(VmValue::as_dict)
        .ok_or_else(&expected_error)?;
    if harn.len() != 1 {
        return Err(expected_error());
    }
    let result = harn
        .get("hostResult")
        .and_then(VmValue::as_dict)
        .ok_or_else(&expected_error)?;
    if result.len() != 2
        || !matches!(result.get("schema"), Some(VmValue::String(value)) if value.as_str() == HOST_RESULT_SCHEMA)
        || !matches!(result.get("kind"), Some(VmValue::String(value)) if value.as_str() == HOST_RESULT_KIND)
    {
        return Err(expected_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;
    use crate::observability::execution_scope::{enter_execution_scope, mint_execution_scope};
    use crate::stdlib::host::{clear_host_call_bridge, set_host_call_bridge, HostCallBridge};

    enum BridgeResponse {
        Value(VmValue),
        Decline,
        Error(String),
    }

    struct RecordingBridge {
        calls: AtomicUsize,
        response: Mutex<BridgeResponse>,
    }

    struct WorkflowBridge {
        calls: AtomicUsize,
        response: Mutex<BridgeResponse>,
    }

    struct LifecycleBridge {
        operation_calls: AtomicUsize,
        attestation_calls: AtomicUsize,
        fail_decision_attestation_once: std::sync::atomic::AtomicBool,
    }

    impl RecordingBridge {
        fn new(response: BridgeResponse) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                response: Mutex::new(response),
            })
        }
    }

    impl HostCallBridge for RecordingBridge {
        fn dispatch<'a>(
            &'a self,
            capability: &'a str,
            operation: &'a str,
            _params: &'a crate::value::DictMap,
        ) -> crate::stdlib::host::HostCallDispatchFuture<'a> {
            assert_eq!(capability, HOST_CAPABILITY);
            assert_eq!(operation, HOST_OPERATION);
            self.calls.fetch_add(1, Ordering::SeqCst);
            let result = match &*self.response.lock().expect("bridge response lock") {
                BridgeResponse::Value(value) => Ok(Some(value.clone())),
                BridgeResponse::Decline => Ok(None),
                BridgeResponse::Error(message) => Err(VmError::Runtime(message.clone())),
            };
            Box::pin(async move { result })
        }
    }

    impl WorkflowBridge {
        fn new(response: BridgeResponse) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                response: Mutex::new(response),
            })
        }
    }

    impl HostCallBridge for WorkflowBridge {
        fn dispatch<'a>(
            &'a self,
            capability: &'a str,
            operation: &'a str,
            _params: &'a crate::value::DictMap,
        ) -> crate::stdlib::host::HostCallDispatchFuture<'a> {
            assert_eq!(capability, HOST_CAPABILITY);
            assert_eq!(operation, HOST_WORKFLOW_OPERATION);
            self.calls.fetch_add(1, Ordering::SeqCst);
            let result = match &*self.response.lock().expect("bridge response lock") {
                BridgeResponse::Value(value) => Ok(Some(value.clone())),
                BridgeResponse::Decline => Ok(None),
                BridgeResponse::Error(message) => Err(VmError::Runtime(message.clone())),
            };
            Box::pin(async move { result })
        }
    }

    impl LifecycleBridge {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                operation_calls: AtomicUsize::new(0),
                attestation_calls: AtomicUsize::new(0),
                fail_decision_attestation_once: std::sync::atomic::AtomicBool::new(false),
            })
        }

        fn with_decision_attestation_failure() -> Arc<Self> {
            Arc::new(Self {
                operation_calls: AtomicUsize::new(0),
                attestation_calls: AtomicUsize::new(0),
                fail_decision_attestation_once: std::sync::atomic::AtomicBool::new(true),
            })
        }
    }

    impl HostCallBridge for LifecycleBridge {
        fn dispatch<'a>(
            &'a self,
            capability: &'a str,
            operation: &'a str,
            params: &'a crate::value::DictMap,
        ) -> crate::stdlib::host::HostCallDispatchFuture<'a> {
            assert_eq!(capability, HOST_CAPABILITY);
            let result = match operation {
                HOST_WORKFLOW_OPERATION => {
                    self.operation_calls.fetch_add(1, Ordering::SeqCst);
                    let action = params
                        .get("action")
                        .cloned()
                        .expect("workflow request has an action");
                    let operation_receipt_id = params
                        .get("operation_receipt_id")
                        .cloned()
                        .expect("workflow request has a receipt id");
                    let mut entries = vec![
                        (
                            "schema",
                            string_value("harn.hypothesis.operation_result.v1"),
                        ),
                        ("kind", string_value("accepted")),
                        ("action", action.clone()),
                        ("operation_receipt_id", operation_receipt_id),
                        ("occurred_at", string_value("2026-08-09T00:00:00Z")),
                        ("actor", string_value("native-test-adapter")),
                        ("source", string_value("harn-vm-test")),
                    ];
                    if matches!(&action, VmValue::String(value) if value.as_str() == "advance") {
                        let assignment_plan = params
                            .get("assignment_plan")
                            .and_then(VmValue::as_dict)
                            .expect("advance request has an assignment plan");
                        let plan = params
                            .get("plan")
                            .and_then(VmValue::as_dict)
                            .expect("advance request has the registered plan");
                        let registration = plan
                            .get("registration")
                            .and_then(VmValue::as_dict)
                            .expect("registered plan has its experiment registration");
                        let baseline_id = registration
                            .get("baseline")
                            .and_then(VmValue::as_dict)
                            .and_then(|baseline| baseline.get("id"))
                            .and_then(|value| match value {
                                VmValue::String(value) => Some(value.as_str()),
                                _ => None,
                            })
                            .expect("registration has a baseline id");
                        let primary_metric = registration
                            .get("metrics")
                            .and_then(VmValue::as_dict)
                            .and_then(|metrics| metrics.get("primary"))
                            .and_then(VmValue::as_dict)
                            .and_then(|metric| metric.get("id"))
                            .and_then(|value| match value {
                                VmValue::String(value) => Some(value.as_str()),
                                _ => None,
                            })
                            .expect("registration has a primary metric");
                        let assignments = match assignment_plan.get("assignments") {
                            Some(VmValue::List(assignments)) => assignments,
                            _ => panic!("assignment plan has its randomized arm order"),
                        };
                        let arm_observations = assignments
                            .iter()
                            .map(|assignment| {
                                let assignment = assignment
                                    .as_dict()
                                    .expect("planned assignment is a dict");
                                let arm_id = assignment
                                    .get("arm_id")
                                    .cloned()
                                    .expect("planned assignment has an arm id");
                                let is_baseline = matches!(&arm_id, VmValue::String(value) if value.as_str() == baseline_id);
                                dict([
                                    ("arm_id", arm_id),
                                    (
                                        "metrics",
                                        VmValue::dict(crate::value::DictMap::from_iter([(
                                            crate::value::intern_key(primary_metric),
                                            VmValue::Float(if is_baseline { 0.0 } else { 1.0 }),
                                        )])),
                                    ),
                                    ("spend_delta_usd", VmValue::Float(0.0)),
                                    ("compute_delta_ms", VmValue::Int(1)),
                                    ("token_delta", VmValue::Int(0)),
                                    ("api_call_delta", VmValue::Int(0)),
                                    ("telemetry_status", string_value("observed")),
                                    (
                                        "capability_degradations",
                                        VmValue::List(Arc::new(vec![])),
                                    ),
                                ])
                            })
                            .collect();
                        entries.extend([
                            (
                                "assignment_plan_id",
                                assignment_plan
                                    .get("plan_id")
                                    .cloned()
                                    .expect("assignment plan has an id"),
                            ),
                            (
                                "observed_blocking_values",
                                assignment_plan
                                    .get("blocking_values")
                                    .cloned()
                                    .expect("assignment plan has frozen blocking values"),
                            ),
                            (
                                "arm_observations",
                                VmValue::List(Arc::new(arm_observations)),
                            ),
                            ("elapsed_ms", VmValue::Int(1_000)),
                        ]);
                    }
                    dict(entries)
                }
                HOST_OPERATION => {
                    self.attestation_calls.fetch_add(1, Ordering::SeqCst);
                    let Some(VmValue::String(event_fingerprint)) = params.get("event_fingerprint")
                    else {
                        panic!("attestation request has an event fingerprint");
                    };
                    let event = params
                        .get("event")
                        .and_then(VmValue::as_dict)
                        .expect("attestation request carries the canonical event");
                    assert!(
                        matches!(event.get("fingerprint"), Some(VmValue::String(value)) if value == event_fingerprint),
                        "the adapter receives the exact event that will be appended"
                    );
                    let is_decision = event
                        .get("content")
                        .and_then(VmValue::as_dict)
                        .and_then(|content| content.get("payload"))
                        .and_then(VmValue::as_dict)
                        .and_then(|payload| payload.get("kind"))
                        .is_some_and(
                            |kind| matches!(kind, VmValue::String(value) if value.as_str() == "decision_recorded"),
                        );
                    if is_decision
                        && self
                            .fail_decision_attestation_once
                            .swap(false, Ordering::SeqCst)
                    {
                        return Box::pin(async {
                            Err(VmError::Runtime(
                                "injected decision attestation boundary failure".to_string(),
                            ))
                        });
                    }
                    host_result()
                }
                other => panic!("unexpected hypothesis host operation: {other}"),
            };
            Box::pin(async move { Ok(Some(result)) })
        }
    }

    fn dict(entries: impl IntoIterator<Item = (&'static str, VmValue)>) -> VmValue {
        VmValue::dict(
            entries
                .into_iter()
                .map(|(key, value)| (crate::value::intern_key(key), value))
                .collect::<crate::value::DictMap>(),
        )
    }

    fn host_result() -> VmValue {
        dict([(
            "_meta",
            dict([(
                "harn",
                dict([(
                    "hostResult",
                    dict([
                        ("schema", string_value(HOST_RESULT_SCHEMA)),
                        ("kind", string_value(HOST_RESULT_KIND)),
                    ]),
                )]),
            )]),
        )])
    }

    fn request_args() -> Vec<VmValue> {
        vec![
            string_value("plan_admission"),
            string_value(&format!("sha256:{}", "a".repeat(64))),
            string_value(&format!("sha256:{}", "b".repeat(64))),
            string_value("hyp-1"),
            string_value("receipt-1"),
            VmValue::Nil,
        ]
    }

    fn ctx() -> crate::vm::AsyncBuiltinCtx {
        crate::vm::AsyncBuiltinCtx::for_test(Vm::new())
    }

    #[test]
    fn request_contract_is_scoped_to_the_requested_authority_kind() {
        use crate::stdlib::macros::{EffectAccess, EffectKind, ResourceSelector};

        let effects = HYPOTHESIS_EVENT_AUTHORITY_REQUEST_IMPL_DEF.contract.effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].kind, EffectKind::Authority);
        assert_eq!(effects[0].access, EffectAccess::Write);
        assert_eq!(effects[0].resources, &[ResourceSelector::Argument(0)]);
    }

    #[test]
    fn operation_contract_has_one_scoped_native_authority_effect() {
        use crate::stdlib::macros::{EffectAccess, EffectKind, ResourceSelector};

        let effects = HYPOTHESIS_OPERATION_REQUEST_IMPL_DEF.contract.effects;
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].kind, EffectKind::Authority);
        assert_eq!(effects[0].access, EffectAccess::Write);
        assert_eq!(
            effects[0].resources,
            &[ResourceSelector::Constant("hypothesis.operation")]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn operation_request_is_unavailable_without_a_native_adapter() {
        clear_host_call_bridge();
        let request = dict([(
            "schema",
            string_value("harn.hypothesis.operation_request.v1"),
        )]);
        let absent = hypothesis_operation_request_impl(ctx(), vec![request.clone()])
            .await
            .expect("an absent adapter is a typed unavailable result");
        assert!(matches!(absent, VmValue::Nil));

        let bridge = WorkflowBridge::new(BridgeResponse::Decline);
        set_host_call_bridge(bridge.clone());
        let declined = hypothesis_operation_request_impl(ctx(), vec![request])
            .await
            .expect("a declining adapter is a typed unavailable result");
        assert!(matches!(declined, VmValue::Nil));
        assert_eq!(bridge.calls.load(Ordering::SeqCst), 1);
        clear_host_call_bridge();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn every_operation_request_reaches_the_native_adapter() {
        clear_host_call_bridge();
        let response = dict([
            (
                "schema",
                string_value("harn.hypothesis.operation_result.v1"),
            ),
            ("kind", string_value("denied")),
        ]);
        let bridge = WorkflowBridge::new(BridgeResponse::Value(response.clone()));
        set_host_call_bridge(bridge.clone());
        let request = dict([(
            "schema",
            string_value("harn.hypothesis.operation_request.v1"),
        )]);

        for _ in 0..2 {
            let actual = hypothesis_operation_request_impl(ctx(), vec![request.clone()])
                .await
                .expect("native workflow response");
            assert_eq!(actual.display(), response.display());
        }
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            2,
            "native operations must never use the turn-stable read memo"
        );
        clear_host_call_bridge();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_adapter_drives_the_portable_lifecycle_without_host_owned_state_logic() {
        crate::event_log::reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        clear_host_call_bridge();
        let bridge = LifecycleBridge::new();
        set_host_call_bridge(bridge.clone());
        let _scope = enter_execution_scope(mint_execution_scope());

        let temp = tempfile::tempdir().expect("temporary workflow module directory");
        let helper = temp.path().join("hypothesis_fixture_lib.harn");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../conformance/tests/stdlib/hypothesis_fixture_lib.harn"
            ),
            &helper,
        )
        .expect("copy canonical hypothesis fixture helpers");
        let entry = temp.path().join("workflow_native.harn");
        std::fs::write(
            &entry,
            r#"
import {
  compile_experiment_intent,
  hypothesis_workflow,
} from "std/eval/hypothesis"
import {
  hypothesis_fixture_context,
  hypothesis_fixture_intent,
} from "./hypothesis_fixture_lib.harn"

pub fn run(harness: Harness) {
  const compiled = compile_experiment_intent(
    hypothesis_fixture_intent(),
    hypothesis_fixture_context(),
  )
  if compiled.plan == nil || compiled.plan.kind != "registered_experiment" {
    throw "native workflow fixture did not compile"
  }
  const plan = compiled.plan
  const started = hypothesis_workflow(
    harness.obs,
    {kind: "start", plan: plan, run_id: "native-workflow-run"},
  )
  require started.kind == "applied"
    && started.state == "running"
    && len(started.appends) == 3,
    "native start registers, schedules, and starts the run"
  const paused = hypothesis_workflow(
    harness.obs,
    {kind: "pause", hypothesis_id: plan.hypothesis_id, reason: "operator hold"},
  )
  require paused.kind == "applied" && paused.state == "paused",
    "native pause records the portable paused state"
  const resumed = hypothesis_workflow(
    harness.obs,
    {kind: "resume", hypothesis_id: plan.hypothesis_id},
  )
  require resumed.kind == "applied" && resumed.state == "running",
    "native resume records the portable running state"
  const stopped = hypothesis_workflow(
    harness.obs,
    {kind: "stand_down", hypothesis_id: plan.hypothesis_id, reason: "bounded stop"},
  )
  require stopped.kind == "applied"
    && stopped.state == "cancelled"
    && stopped.ledger.snapshot.event_count == 6,
    "native stand-down records the terminal state on the canonical ledger"
  return stopped
}
"#,
        )
        .expect("write native workflow test module");

        let mut vm = Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let (harness, _clock) = crate::Harness::test();
        vm.set_harness(harness.clone());
        let exports = vm
            .load_module_exports(&entry)
            .await
            .expect("native workflow module loads");
        let run = exports.get("run").expect("workflow exports run");
        let stopped = vm
            .call_closure_pub(run, &[harness.into_vm_value()])
            .await
            .expect("native lifecycle completes through the public Harn workflow");
        let stopped: serde_json::Value =
            serde_json::from_str(&crate::stdlib::json::vm_value_to_json(&stopped))
                .expect("workflow result is JSON-shaped");
        assert_eq!(stopped["kind"], "applied");
        assert_eq!(stopped["state"], "cancelled");
        assert_eq!(bridge.operation_calls.load(Ordering::SeqCst), 4);
        assert_eq!(
            bridge.attestation_calls.load(Ordering::SeqCst),
            6,
            "every canonical lifecycle event requires a fresh native attestation"
        );

        clear_host_call_bridge();
        crate::event_log::reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_adapter_recovers_a_decision_after_its_first_attestation_fails() {
        crate::event_log::reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        clear_host_call_bridge();
        let bridge = LifecycleBridge::with_decision_attestation_failure();
        set_host_call_bridge(bridge.clone());
        let _scope = enter_execution_scope(mint_execution_scope());

        let temp = tempfile::tempdir().expect("temporary experiment workflow directory");
        let helper = temp.path().join("hypothesis_fixture_lib.harn");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../conformance/tests/stdlib/hypothesis_fixture_lib.harn"
            ),
            &helper,
        )
        .expect("copy canonical hypothesis fixture helpers");
        let entry = temp.path().join("workflow_experiment.harn");
        std::fs::write(
            &entry,
            r#"
import {
  compile_experiment_intent,
  hypothesis_workflow,
} from "std/eval/hypothesis"
import {
  hypothesis_fixture_context,
  hypothesis_fixture_intent,
} from "./hypothesis_fixture_lib.harn"

pub fn run(harness: Harness) {
  const compiled = compile_experiment_intent(
    hypothesis_fixture_intent(),
    hypothesis_fixture_context(),
  )
  if compiled.plan == nil || compiled.plan.kind != "registered_experiment" {
    throw "native experiment fixture did not compile"
  }
  const plan = compiled.plan
  let result = hypothesis_workflow(
    harness.obs,
    {kind: "start", plan: plan, run_id: "native-experiment-run"},
  )
  let advances = 0
  while result.state == "running" && advances < 64 {
    result = hypothesis_workflow(
      harness.obs,
      {
        kind: "advance",
        hypothesis_id: plan.hypothesis_id,
        blocking_values: {host: "native-test-host", time_slot: "frozen-slot"},
      },
    )
    advances = advances + 1
  }
  require result.kind == "applied"
    && result.state == "completed"
    && result.ledger.snapshot.decision
    != nil,
    "randomized native blocks terminate only through the canonical Harn decision"
  require result.ledger.snapshot.decision.decision.verdict == "ITERATE_WINNER",
    "the deterministic all-better candidate wins the iterate phase"
  return {result: result, advances: advances}
}

pub fn recover(harness: Harness) {
  const compiled = compile_experiment_intent(
    hypothesis_fixture_intent(),
    hypothesis_fixture_context(),
  )
  if compiled.plan == nil || compiled.plan.kind != "registered_experiment" {
    throw "native recovery fixture did not compile"
  }
  return hypothesis_workflow(
    harness.obs,
    {
      kind: "advance",
      hypothesis_id: compiled.plan.hypothesis_id,
      blocking_values: {host: "ignored-after-freeze", time_slot: "ignored-after-freeze"},
    },
  )
}
"#,
        )
        .expect("write native experiment test module");

        let mut vm = Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let (harness, _clock) = crate::Harness::test();
        vm.set_harness(harness.clone());
        let exports = vm
            .load_module_exports(&entry)
            .await
            .expect("native experiment module loads");
        let run = exports.get("run").expect("experiment exports run");
        let error = vm
            .call_closure_pub(run, &[harness.clone().into_vm_value()])
            .await
            .expect_err("the injected decision attestation fails after completion");
        assert!(
            error
                .to_string()
                .contains("injected decision attestation boundary failure"),
            "{error}"
        );
        let recover = exports.get("recover").expect("experiment exports recovery");
        let outcome = vm
            .call_closure_pub(recover, &[harness.into_vm_value()])
            .await
            .expect("retry appends the pending canonical decision");
        let outcome: serde_json::Value =
            serde_json::from_str(&crate::stdlib::json::vm_value_to_json(&outcome))
                .expect("experiment result is JSON-shaped");
        assert_eq!(outcome["state"], "completed");
        assert_eq!(
            outcome["ledger"]["snapshot"]["decision"]["decision"]["verdict"],
            "ITERATE_WINNER"
        );
        assert!(
            bridge.attestation_calls.load(Ordering::SeqCst)
                > bridge.operation_calls.load(Ordering::SeqCst),
            "registration, lifecycle, observations, completion, and decision are all attested"
        );

        clear_host_call_bridge();
        crate::event_log::reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_requires_a_registered_native_adapter() {
        clear_host_call_bridge();
        let error = hypothesis_event_authority_request_impl(ctx(), request_args())
            .await
            .expect_err("an absent native adapter must fail closed");
        assert!(error
            .to_string()
            .contains("no registered native hypothesis adapter"));

        let bridge = RecordingBridge::new(BridgeResponse::Decline);
        set_host_call_bridge(bridge);
        let error = hypothesis_event_authority_request_impl(ctx(), request_args())
            .await
            .expect_err("an adapter that declines the operation must fail closed");
        assert!(error
            .to_string()
            .contains("no registered native hypothesis adapter"));
        clear_host_call_bridge();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exact_host_result_mints_a_scoped_proof_and_each_request_reaches_the_adapter() {
        crate::event_log::reset_active_event_log();
        install_memory_for_current_thread(EVENT_LOG_QUEUE_DEPTH);
        clear_host_call_bridge();
        let bridge = RecordingBridge::new(BridgeResponse::Value(host_result()));
        set_host_call_bridge(bridge.clone());
        let _scope = enter_execution_scope(mint_execution_scope());

        for _ in 0..2 {
            let proof = hypothesis_event_authority_request_impl(ctx(), request_args())
                .await
                .expect("exact native response should mint an authority proof");
            let proof = hypothesis_authority_proof(Some(&proof), "test")
                .expect("result must be an opaque hypothesis authority resource");
            assert_eq!(proof.authority_kind, HypothesisAuthorityKind::PlanAdmission);
            assert_eq!(proof.hypothesis_id.as_ref(), "hyp-1");
            assert_eq!(
                proof.execution_scope,
                crate::observability::execution_scope::current_execution_scope()
            );
        }

        let content = dict([
            ("schema", string_value("harn.hypothesis.event.v1")),
            ("event_id", string_value("event-1")),
            ("hypothesis_id", string_value("hyp-1")),
            ("plan_id", VmValue::Nil),
            ("run_id", VmValue::Nil),
            ("payload", dict([("kind", string_value("plan_registered"))])),
        ]);
        let canonical = crate::stdlib::json::vm_value_to_json(&content);
        let event_fingerprint = format!(
            "sha256:{}",
            harn_kernel::pure::sha256_hex(canonical.as_bytes())
        );
        let payload = dict([
            ("content", content),
            ("fingerprint", string_value(&event_fingerprint)),
        ]);
        let mut authority_args = request_args();
        authority_args[1] = string_value(&event_fingerprint);
        let proof = hypothesis_event_authority_request_impl(ctx(), authority_args)
            .await
            .expect("ACP-issued authority should be usable by the reserved append owner");
        let outcome = hypothesis_event_append_impl(
            ctx(),
            vec![
                proof,
                string_value("hypothesis.plan_registered"),
                string_value(&event_fingerprint),
                VmValue::Nil,
                string_value(&event_fingerprint),
                string_value(&format!("sha256:{}", "b".repeat(64))),
                string_value("hyp-1"),
                VmValue::Nil,
                payload,
                dict([]),
            ],
        )
        .await
        .expect("scoped proof should append its exact event");
        assert_eq!(vm_value_to_json(&outcome)["inserted"], true);
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            3,
            "attestations are operation-completion boundaries and must never be turn-memoized"
        );
        clear_host_call_bridge();
        crate::event_log::reset_active_event_log();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn script_host_mock_cannot_satisfy_the_native_attestation_request() {
        crate::stdlib::host::reset_host_state();
        crate::stdlib::host::host_mock_builtin(
            &[
                string_value(HOST_CAPABILITY),
                string_value(HOST_OPERATION),
                dict([
                    ("result", host_result()),
                    ("unregistered_ok", VmValue::Bool(true)),
                ]),
            ],
            &mut String::new(),
        )
        .expect("install exact script host mock");
        clear_host_call_bridge();
        let bridge = RecordingBridge::new(BridgeResponse::Value(host_result()));
        set_host_call_bridge(bridge.clone());
        let _scope = enter_execution_scope(mint_execution_scope());
        let result = hypothesis_event_authority_request_impl(ctx(), request_args())
            .await
            .expect("the real native adapter response should mint authority");
        assert!(matches!(result, VmValue::Resource(_)));
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            1,
            "a script host_mock must not satisfy the specialized native request"
        );
        clear_host_call_bridge();
        crate::stdlib::host::reset_host_state();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn legacy_namespace_request_enforces_and_records_the_harness_contract() {
        use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

        let source = format!(
            r#"
pipeline main(_harness: Harness) {{
  return event_log.hypothesis_authority_request(
    "plan_admission",
    "sha256:{event_digest}",
    "sha256:{plan_digest}",
    "hyp-1",
    "receipt-1",
    nil,
  )
}}
"#,
            event_digest = "a".repeat(64),
            plan_digest = "b".repeat(64),
        );
        let chunk = crate::compile_source(&source).expect("compile legacy namespace request");
        let bridge = RecordingBridge::new(BridgeResponse::Value(host_result()));
        clear_host_call_bridge();
        set_host_call_bridge(bridge.clone());

        push_execution_policy(CapabilityPolicy {
            capabilities: BTreeMap::from([("connector".to_string(), vec!["call".to_string()])]),
            ..CapabilityPolicy::default()
        });
        let mut denied_vm = Vm::new();
        crate::register_vm_stdlib(&mut denied_vm);
        let (harness, _clock) = crate::Harness::test();
        denied_vm.set_harness(harness);
        let error = denied_vm
            .execute(&chunk)
            .await
            .expect_err("the legacy namespace must not bypass the authority ceiling");
        pop_execution_policy();
        assert!(error
            .to_string()
            .contains("authority:write (plan_admission)"));
        assert_eq!(
            bridge.calls.load(Ordering::SeqCst),
            0,
            "policy denial must happen before the native adapter call"
        );

        push_execution_policy(CapabilityPolicy {
            capabilities: BTreeMap::from([(
                "authority".to_string(),
                vec!["write@plan_admission".to_string()],
            )]),
            ..CapabilityPolicy::default()
        });
        let mut allowed_vm = Vm::new();
        crate::register_vm_stdlib(&mut allowed_vm);
        let (harness, _clock) = crate::Harness::test();
        allowed_vm.set_harness(harness);
        let proof = allowed_vm
            .execute(&chunk)
            .await
            .expect("the exact authority ceiling should allow the namespace alias");
        pop_execution_policy();
        assert!(matches!(proof, VmValue::Resource(_)));
        assert_eq!(bridge.calls.load(Ordering::SeqCst), 1);
        let effects = allowed_vm.executed_effects();
        assert!(effects.iter().any(|effect| {
            effect.kind == crate::orchestration::EffectKind::Authority
                && effect.scope == crate::orchestration::EffectScope::Write
                && effect.resource.as_deref() == Some("plan_admission")
        }));

        let append_source = format!(
            r#"
pipeline main(_harness: Harness) {{
  return event_log.hypothesis_event_append(
    nil,
    "hypothesis.plan_registered",
    "sha256:{event_digest}",
    nil,
    "sha256:{event_digest}",
    "sha256:{plan_digest}",
    "hyp-1",
    nil,
    {{}},
    {{}},
  )
}}
"#,
            event_digest = "a".repeat(64),
            plan_digest = "b".repeat(64),
        );
        let append_chunk =
            crate::compile_source(&append_source).expect("compile legacy namespace append");
        push_execution_policy(CapabilityPolicy {
            capabilities: BTreeMap::from([(
                "authority".to_string(),
                vec!["write@plan_admission".to_string()],
            )]),
            ..CapabilityPolicy::default()
        });
        let mut append_vm = Vm::new();
        crate::register_vm_stdlib(&mut append_vm);
        let (harness, _clock) = crate::Harness::test();
        append_vm.set_harness(harness);
        let append_error = append_vm
            .execute(&append_chunk)
            .await
            .expect_err("the legacy append alias must preserve its observability ceiling");
        pop_execution_policy();
        assert!(append_error
            .to_string()
            .contains("observability:write (hypotheses.events.v1)"));
        clear_host_call_bridge();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generic_host_call_cannot_turn_the_wire_marker_into_a_resource() {
        clear_host_call_bridge();
        let bridge = RecordingBridge::new(BridgeResponse::Value(host_result()));
        set_host_call_bridge(bridge);
        let value = crate::stdlib::host::dispatch_host_operation(
            HOST_CAPABILITY,
            HOST_OPERATION,
            &crate::value::DictMap::new(),
        )
        .await
        .expect("generic bridge call");
        assert!(value.as_dict().is_some());
        assert!(!matches!(value, VmValue::Resource(_)));
        assert!(proof_from_native_attestation(Some(&value), "test").is_err());
        clear_host_call_bridge();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_or_host_denied_results_fail_closed() {
        let invalid_results = [
            VmValue::Nil,
            dict([]),
            dict([("_meta", dict([]))]),
            dict([("_meta", dict([("harn", dict([]))]))]),
            dict([(
                "_meta",
                dict([(
                    "harn",
                    dict([(
                        "hostResult",
                        dict([
                            ("schema", string_value("harn.host-result.v0")),
                            ("kind", string_value(HOST_RESULT_KIND)),
                        ]),
                    )]),
                )]),
            )]),
            dict([(
                "_meta",
                dict([(
                    "harn",
                    dict([(
                        "hostResult",
                        dict([
                            ("schema", string_value(HOST_RESULT_SCHEMA)),
                            ("kind", string_value("ordinary_data")),
                            ("extra", VmValue::Bool(true)),
                        ]),
                    )]),
                )]),
            )]),
        ];
        for invalid in invalid_results {
            clear_host_call_bridge();
            set_host_call_bridge(RecordingBridge::new(BridgeResponse::Value(invalid)));
            let error = hypothesis_event_authority_request_impl(ctx(), request_args())
                .await
                .expect_err("malformed host response must fail closed");
            assert!(error
                .to_string()
                .contains("invalid hypothesis.attest_event result"));
        }

        clear_host_call_bridge();
        set_host_call_bridge(RecordingBridge::new(BridgeResponse::Error(
            "native receipt was stale".to_string(),
        )));
        let error = hypothesis_event_authority_request_impl(ctx(), request_args())
            .await
            .expect_err("native denial must remain an error");
        assert!(error.to_string().contains("native receipt was stale"));
        clear_host_call_bridge();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_request_never_reaches_the_native_adapter() {
        clear_host_call_bridge();
        let bridge = RecordingBridge::new(BridgeResponse::Value(host_result()));
        set_host_call_bridge(bridge.clone());
        let mut args = request_args();
        args[1] = string_value("sha256:not-a-digest");
        hypothesis_event_authority_request_impl(ctx(), args)
            .await
            .expect_err("invalid fingerprints must fail at the owning boundary");
        assert_eq!(bridge.calls.load(Ordering::SeqCst), 0);
        clear_host_call_bridge();
    }
}
