use super::*;

const EVALUATE: &str = r#"
import { EvaluationOutcome } from "std/predicate"
fn evaluate(llm: HarnessLlm, model: string) -> EvaluationOutcome {
  return llm.evaluate_request("receipt.public", {text: "observed"},
    {safe: {kind: "boolean", instructions: "Safe?"}},
    {backend: "structured_llm", provider: "mock", model: model, threshold: 0.5,
      evaluation_cost_limit: 1.0})
}
"#;
const LOOKUP: &str = r#"
pipeline main(harness: Harness) {
  const outcome = evaluate(harness.llm, "fixture")
  return {outcome: outcome, lookup: harness.llm.evaluation_receipt(outcome.receipt)}
}
"#;

#[test]
fn public_receipt_access_preserves_accounting_transport_and_execution_isolation() {
    for tokens in [Some(100), Some(0), None] {
        let mut response = answering(0.95);
        response.input_tokens = tokens;
        response.native_transport = Some(super::super::receipt::NativeTransportReceipt {
            provider_attempts_reported: tokens.filter(|tokens| *tokens > 0).map(|_| 1),
            final_provider_reported: tokens
                .filter(|tokens| *tokens > 0)
                .map(|_| "reported-provider".into()),
            data_controls: serde_json::from_value(serde_json::json!({
                "requested_posture":"strictest_available", "outcome":"provider_unresearched",
                "provider":"mock", "model":"fixture"
            }))
            .unwrap(),
        });
        let backend = Arc::new(MockDecisionBackend::scripted(vec![Ok(response)]));
        let _backend = install_backend(backend.clone());
        let _route = install_route("mock", "fixture", contract(4096, None));
        crate::test_support::current_thread_runtime().block_on(harn_clock::test_support::with_local_tasks(async {
            let mut vm = crate::new_vm();
            crate::register_vm_stdlib(&mut vm);
            let value = vm.execute(&crate::compile_source(&format!("{EVALUATE}{LOOKUP}")).unwrap()).await.unwrap();
            let value = crate::llm::helpers::vm_value_to_json(&value);
            assert_eq!(value["outcome"]["kind"], "answered");
            assert_eq!(value["lookup"]["kind"], "retained");
            let receipt = &value["lookup"]["receipt"];
            assert_eq!(receipt["reference"], value["outcome"]["receipt"]);
            assert_eq!(receipt["input_tokens"], serde_json::json!(tokens));
            assert_eq!(receipt["physical_attempts"], 1);
            assert_eq!(receipt["accounting_status"], if tokens.is_some() {"settled"} else {"usage_unknown"});
            assert_eq!(receipt["cost_usd"].is_null(), tokens.is_none());
            if tokens == Some(0) { assert_eq!(receipt["cost_usd"], 0.0); }
            assert_eq!(receipt["native_transport"]["provider_attempts_reported"], serde_json::json!(tokens.filter(|tokens| *tokens > 0).map(|_| 1)));
            assert_eq!(receipt["native_transport"]["final_provider_reported"].is_null(), tokens != Some(100));
            assert_eq!(receipt["native_transport"]["data_controls"]["outcome"], "provider_unresearched");
            let reference = serde_json::to_string(&receipt["reference"]).unwrap();
            let lookup = crate::compile_source(&format!("pipeline main(harness: Harness) {{ return harness.llm.evaluation_receipt({reference}) }}")).unwrap();
            let mut child = vm.child_vm();
            assert_eq!(child.execute(&lookup).await.unwrap(), crate::schema::json_to_vm_value(&value["lookup"]));
            let mut foreign = crate::new_vm();
            crate::register_vm_stdlib(&mut foreign);
            let unavailable = foreign.execute(&lookup).await.unwrap();
            assert_eq!(crate::llm::helpers::vm_value_to_json(&unavailable)["reason"], "foreign_execution");
            assert_eq!(backend.request_count(), 1, "receipt reads cannot dispatch a second request");
        }));
    }
}

#[test]
fn public_receipt_access_distinguishes_refusal_absence_and_bounded_retention() {
    let source = format!(
        "{EVALUATE}{}",
        LOOKUP.replace("\"fixture\"", "\"unconfigured-model\"")
    );
    crate::test_support::current_thread_runtime().block_on(harn_clock::test_support::with_local_tasks(async {
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        let value = vm.execute(&crate::compile_source(&source).unwrap()).await.unwrap();
        let value = crate::llm::helpers::vm_value_to_json(&value);
        assert_eq!(value["outcome"]["kind"], "unavailable");
        assert_eq!(value["lookup"]["kind"], "retained");
        assert_eq!(value["lookup"]["receipt"]["physical_attempts"], 0);
        assert_eq!(value["lookup"]["receipt"]["accounting_status"], "not_dispatched");
        for (reference, reason) in [("malformed".to_string(), "malformed_reference"),
            (format!("{}/evaluation/999999", vm.execution_id()), "not_retained")] {
            let source = format!("pipeline main(harness: Harness) {{ return harness.llm.evaluation_receipt({reference:?}) }}");
            let mut child = vm.child_vm();
            let value = child.execute(&crate::compile_source(&source).unwrap()).await.unwrap();
            assert_eq!(crate::llm::helpers::vm_value_to_json(&value)["reason"], reason);
        }
        let source = format!(r#"{EVALUATE}
pipeline main(harness: Harness) {{
  let reference = ""
  for _ in range(0, 1025) {{
    reference = evaluate(harness.llm, "unconfigured-model").receipt
  }}
  return {{lookup: harness.llm.evaluation_receipt(reference)}}
}}
"#);
        let value = vm.execute(&crate::compile_source(&source).unwrap()).await.unwrap();
        assert_eq!(crate::llm::helpers::vm_value_to_json(&value)["lookup"]["reason"], "not_retained");
        let (retained, dropped) = vm.native().evaluation.journal.lock().snapshot();
        assert_eq!(retained.len(), 1024);
        assert_eq!(dropped, 1);
    }));
}
