use super::*;
use crate::llm::decision::replay::{EvaluationFixture, EvaluationReplayScope, FixtureAnswer};

fn args(state: &str) -> Vec<VmValue> {
    vec![
        VmValue::string("replay.site"),
        VmValue::string(state),
        questions_value(),
        policy_value(0.5, 1.0),
    ]
}

#[test]
fn replay_and_cache_inherit_into_children_and_keep_occurrences_and_cost_separate() {
    let backend = Arc::new(MockDecisionBackend::scripted(vec![
        Ok(answering(0.95)),
        Ok(answering(0.95)),
    ]));
    let _backend = install_backend(backend.clone());
    let _route = install_route("mock", "fixture", contract(4096, None));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut vm = crate::Vm::new();
            let recording = EvaluationReplayScope::record();
            vm.set_evaluation_replay(recording.clone());
            let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm.child_vm());
            let live = evaluate_internal(&ctx, &args("state")).await.unwrap();
            assert_eq!(backend.request_count(), 1);
            let tape = recording.finish().unwrap().unwrap();
            let replay = EvaluationReplayScope::replay(&tape).unwrap();
            vm.set_evaluation_replay(replay.clone());
            let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm.child_vm());
            let reused = evaluate_internal(&ctx, &args("state")).await.unwrap();
            replay.finish().unwrap();
            assert_eq!(backend.request_count(), 1, "tape never dispatched");
            assert_eq!(live.3.evaluation_id, reused.3.evaluation_id);
            assert_ne!(live.3.invocation_id, reused.3.invocation_id);
            assert_eq!(reused.3.reused_from.as_deref(), Some(&live.3));
            assert_eq!(reused.3.physical_attempts, 0);
            assert_eq!(reused.3.cost_usd, Some(0.0));
            assert_eq!(reused.3.budget_charge_usd, Some(0.0));
            assert_eq!(reused.3.source, EvaluationSource::Tape);

            let cache = EvaluationReplayScope::cache();
            vm.set_evaluation_replay(cache.clone());
            let first = evaluate_internal(
                &crate::vm::AsyncBuiltinCtx::for_test(vm.child_vm()),
                &args("cached"),
            )
            .await
            .unwrap();
            let second = evaluate_internal(
                &crate::vm::AsyncBuiltinCtx::for_test(vm.child_vm()),
                &args("cached"),
            )
            .await
            .unwrap();
            assert_eq!(
                backend.request_count(),
                2,
                "first cache call is a measured dispatch"
            );
            assert_ne!(first.3.invocation_id, second.3.invocation_id);
            assert_eq!(second.3.source, EvaluationSource::Cache);
            assert_eq!(second.3.physical_attempts, 0);
            cache.finish().unwrap();
        });
}

#[test]
fn missing_changed_extra_and_caught_mismatches_cannot_succeed_or_dispatch() {
    let backend = Arc::new(MockDecisionBackend::scripted(vec![Ok(answering(0.95))]));
    let _backend = install_backend(backend.clone());
    let _route = install_route("mock", "fixture", contract(4096, None));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut vm = crate::Vm::new();
            let recording = EvaluationReplayScope::record();
            vm.set_evaluation_replay(recording.clone());
            evaluate_internal(
                &crate::vm::AsyncBuiltinCtx::for_test(vm.child_vm()),
                &args("state"),
            )
            .await
            .unwrap();
            let tape = recording.finish().unwrap().unwrap();
            let extra = EvaluationReplayScope::replay(&tape).unwrap();
            assert!(
                extra.finish().is_err(),
                "unconsumed records are not success"
            );
            vm.set_evaluation_replay(extra.clone());
            assert!(evaluate_internal(
                &crate::vm::AsyncBuiltinCtx::for_test(vm.child_vm()),
                &args("changed")
            )
            .await
            .is_err());
            assert!(
                extra.finish().is_err(),
                "catching an error cannot hide a changed fixture"
            );
            let mut missing = tape;
            missing.records.clear();
            let missing = EvaluationReplayScope::replay(&missing).unwrap();
            vm.set_evaluation_replay(missing.clone());
            assert!(evaluate_internal(
                &crate::vm::AsyncBuiltinCtx::for_test(vm.child_vm()),
                &args("state")
            )
            .await
            .is_err());
            assert!(missing.finish().is_err());
            assert_eq!(
                backend.request_count(),
                1,
                "only the positive recording dispatched"
            );
        });
}

#[test]
fn typed_fixture_receipt_reports_no_dispatch_or_invented_live_source() {
    let backend = Arc::new(MockDecisionBackend::scripted(vec![]));
    let _backend = install_backend(backend.clone());
    let _route = install_route("mock", "fixture", contract(4096, None));
    let arguments = args("state");
    let fixture = EvaluationFixture {
        site_id: "replay.site".into(),
        state: crate::llm::helpers::vm_value_to_json(&arguments[1]),
        questions: crate::llm::helpers::vm_value_to_json(&arguments[2]),
        policy: crate::llm::helpers::vm_value_to_json(&arguments[3]),
        answers: BTreeMap::from([(
            "safe".into(),
            FixtureAnswer::Boolean {
                verdict: true,
                confidence: 0.95,
                evidence: "declared by the test".into(),
            },
        )]),
    };
    let scope = EvaluationReplayScope::fixtures(vec![fixture]).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut vm = crate::Vm::new();
            vm.set_evaluation_replay(scope.clone());
            let result = evaluate_internal(
                &crate::vm::AsyncBuiltinCtx::for_test(vm.child_vm()),
                &arguments,
            )
            .await
            .unwrap();
            scope.finish().unwrap();
            assert_eq!(result.0.kind, "answered");
            assert_eq!(result.3.source, EvaluationSource::Fixture);
            assert!(result.3.reused_from.is_none());
            assert_eq!(result.3.physical_attempts, 0);
            assert_eq!(result.3.cost_usd, Some(0.0));
            assert_eq!(result.3.budget_charge_usd, Some(0.0));
            let evidence = vm.execution_evidence(None, Vec::new());
            assert_eq!(evidence.evaluation_receipts.unwrap().len(), 1);
            assert_eq!(backend.request_count(), 0);
        });
}

#[test]
fn every_checked_in_evaluation_tape_decodes_with_the_current_decoder() {
    // A change to how recordings serialize must fail here, where it is made,
    // rather than in the slow end-to-end suite that replays one example.
    use crate::testbench::tape::{EventTape, TapeRecordKind};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let listed = std::process::Command::new("git")
        .current_dir(&root)
        .args(["ls-files", "-z", "--", "*.tape"])
        .output()
        .expect("git lists the checked-in tapes");
    assert!(listed.status.success(), "git ls-files failed: {listed:?}");
    let tapes: Vec<_> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| root.join(String::from_utf8_lossy(path).as_ref()))
        .collect();
    let mut evaluation_tapes = 0;
    for path in &tapes {
        let tape = EventTape::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if tape
            .records
            .iter()
            .any(|record| matches!(record.kind, TapeRecordKind::DecisionEvaluation { .. }))
        {
            EvaluationReplayScope::replay(&tape)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            evaluation_tapes += 1;
        }
    }
    // An empty listing must not read as every tape decoding.
    assert!(
        evaluation_tapes > 0,
        "no checked-in evaluation tape was found among {} tapes",
        tapes.len()
    );
}
