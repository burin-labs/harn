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
        let harn_kernel::Execution::Completed { value } = &expected else {
            panic!("{} did not complete in the kernel: {expected:?}", case.id);
        };
        let expected_json: serde_json::Value =
            serde_json::from_str(case.expected_json).expect("expected JSON");
        assert_eq!(
            value.to_json(),
            expected_json,
            "{} kernel result drifted",
            case.id
        );
        let actual =
            harn_vm::portable::start(program.bytes(), input, &harn_kernel::GrantSet::pure())
                .expect("native adapter decodes artifact");
        assert_eq!(actual, expected, "{} diverged in native adapter", case.id);
    }
}

#[test]
fn portable_runtime_failures_reach_the_expected_diagnostic() {
    for case in RUNTIME_FAILURE_CASES {
        let program =
            harn_kernel::compile_program(case.source, case.entry, harn_kernel::EntryKind::Function)
                .unwrap_or_else(|diagnostics| {
                    panic!("{} did not compile: {diagnostics:?}", case.id)
                });
        let input = harn_kernel::DataValue::from_json(
            serde_json::from_str(case.input_json).expect("input JSON"),
        )
        .expect("portable input");
        let actual =
            harn_vm::portable::start(program.bytes(), input, &harn_kernel::GrantSet::pure())
                .expect("native adapter decodes artifact");
        let harn_kernel::Execution::Failed { diagnostic } = actual else {
            panic!("{} completed; expected {}", case.id, case.expected_code);
        };
        assert_eq!(
            diagnostic.code, case.expected_code,
            "{} diagnostic drifted",
            case.id
        );
    }
}

#[test]
fn native_vm_rejects_every_numeric_modulo_zero() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let cases: Vec<_> = RUNTIME_FAILURE_CASES
        .iter()
        .filter(|case| case.expected_code == "division_by_zero")
        .collect();
    assert!(!cases.is_empty(), "modulo zero failure corpus is empty");
    for case in cases {
        let input_literal = serde_json::to_string(case.input_json).expect("input JSON string");
        let source = format!(
            "{}\npipeline default() {{ return {}(json_parse({})) }}",
            case.source, case.entry, input_literal
        );
        let chunk = harn_vm::compile_source(&source).unwrap_or_else(|error| {
            panic!("{} did not compile in the native VM: {error}", case.id)
        });
        let actual = runtime.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut vm = harn_vm::Vm::new();
                    harn_vm::register_vm_stdlib(&mut vm);
                    vm.execute(&chunk).await
                })
                .await
        });
        assert!(
            matches!(&actual, Err(harn_vm::value::VmError::Runtime(message)) if message.starts_with("Division by zero (")),
            "{} did not reach native modulo failure: {actual:?}",
            case.id
        );
    }
}

#[test]
fn native_vm_matches_portable_conformance_corpus() {
    for case in PURE_CASES {
        let input = harn_kernel::DataValue::from_json(
            serde_json::from_str(case.input_json).expect("input JSON"),
        )
        .expect("portable input");
        let source = format!(
            "{}\npipeline default() {{ return {}(__portable_conformance_input) }}",
            case.source, case.entry
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
                    vm.set_global(
                        "__portable_conformance_input",
                        harn_vm::VmValue::from(input),
                    );
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
    for case in package_compilation_cases() {
        let first = case.compile();
        let second = case.compile();
        assert_eq!(
            first.map(|program| program.bytes().to_vec()),
            second.map(|program| program.bytes().to_vec()),
            "{} package compilation was not deterministic",
            case.id,
        );
    }
}
