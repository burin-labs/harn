use harn_kernel::{compile_program, start, DataValue, EntryKind, Execution, GrantSet};

include!("../testdata/portable_conformance.rs");

#[test]
fn canonical_native_kernel_matches_portable_corpus() {
    for case in PURE_CASES {
        let program = compile_program(case.source, case.entry, EntryKind::Function)
            .unwrap_or_else(|diagnostics| panic!("{} did not compile: {diagnostics:?}", case.id));
        let input = DataValue::from_json(serde_json::from_str(case.input_json).unwrap()).unwrap();
        let execution = start(&program, input, &GrantSet::pure());
        let Execution::Completed { value } = execution else {
            panic!("{} did not complete: {execution:?}", case.id);
        };
        let expected: serde_json::Value = serde_json::from_str(case.expected_json).unwrap();
        assert_eq!(value.to_json(), expected, "{} diverged", case.id);
    }
}

#[test]
fn one_immutable_artifact_runs_in_parallel_isolates() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<harn_kernel::ProgramArtifact>();

    let case = &PURE_CASES[0];
    let program = compile_program(case.source, case.entry, EntryKind::Function).unwrap();
    let input = DataValue::from_json(serde_json::from_str(case.input_json).unwrap()).unwrap();
    let expected: serde_json::Value = serde_json::from_str(case.expected_json).unwrap();

    std::thread::scope(|scope| {
        let workers = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let Execution::Completed { value } =
                        start(&program, input.clone(), &GrantSet::pure())
                    else {
                        panic!("parallel isolate did not complete")
                    };
                    value.to_json()
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            assert_eq!(worker.join().unwrap(), expected);
        }
    });
}

#[test]
fn invalid_corpus_has_stable_diagnostics() {
    for case in INVALID_CASES {
        let diagnostics = compile_program(case.source, case.entry, EntryKind::Function)
            .expect_err("invalid case must fail compilation");
        assert_eq!(
            diagnostics[0].code, case.expected_code,
            "{} drifted",
            case.id
        );
    }
}
