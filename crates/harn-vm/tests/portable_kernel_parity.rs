//! Differential proof that the hostful native VM remains aligned with the
//! portable kernel for the language surface promised by portable v1.

include!("../../harn-kernel/testdata/portable_conformance.rs");

#[test]
fn native_adapter_executes_the_exact_portable_artifact_bytes() {
    for case in PURE_CASES {
        let program =
            harn_kernel::compile_program(case.source, case.entry, harn_kernel::EntryKind::Function)
                .unwrap_or_else(|diagnostics| {
                    panic!("{} did not compile: {diagnostics:?}", case.id)
                });
        let input = harn_kernel::DataValue::from_json(
            serde_json::from_str(case.input_json).expect("input JSON"),
        )
        .expect("portable input");
        let expected = harn_kernel::start(&program, input.clone(), &harn_kernel::GrantSet::pure());
        let actual =
            harn_vm::portable::start(program.bytes(), input, &harn_kernel::GrantSet::pure())
                .expect("native adapter decodes artifact");
        assert_eq!(actual, expected, "{} diverged in native adapter", case.id);
    }
}

#[test]
fn native_vm_matches_portable_conformance_corpus() {
    for case in PURE_CASES {
        let input_literal = serde_json::to_string(case.input_json).expect("input JSON string");
        let source = format!(
            "{}\npipeline default() {{ return {}(json_parse({})) }}",
            case.source, case.entry, input_literal
        );
        let chunk = harn_vm::compile_source(&source).unwrap_or_else(|error| {
            panic!("{} did not compile in the native VM: {error}", case.id)
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime");
        let actual = runtime.block_on(async move {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async move {
                    let mut vm = harn_vm::Vm::new();
                    harn_vm::register_vm_stdlib(&mut vm);
                    let value = vm.execute(&chunk).await.expect("native VM execution");
                    harn_vm::llm::vm_value_to_json(&value)
                })
                .await
        });
        let expected: serde_json::Value =
            serde_json::from_str(case.expected_json).expect("expected JSON");
        assert_eq!(actual, expected, "{} diverged in the native VM", case.id);
    }
}

#[test]
fn native_adapter_preserves_invalid_diagnostics_exactly() {
    for case in INVALID_CASES {
        let first =
            harn_kernel::compile_program(case.source, case.entry, harn_kernel::EntryKind::Function)
                .expect_err("invalid case must fail");
        let second =
            harn_kernel::compile_program(case.source, case.entry, harn_kernel::EntryKind::Function)
                .expect_err("invalid case must fail deterministically");
        assert_eq!(
            first, second,
            "{} diagnostics were not deterministic",
            case.id
        );
        assert_eq!(first[0].code, case.expected_code, "{} drifted", case.id);
    }
}
