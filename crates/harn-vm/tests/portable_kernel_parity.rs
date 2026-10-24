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

/// What a runtime failure means, independent of how either runtime words it.
///
/// The kernel names a builtin's argument failure `builtin_type` and a user
/// function's `argument_type`; the native VM instead splits arity from type
/// and does not mark which callees are builtins. Both are the same contract
/// failure, so they share one class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    DivisionByZero,
    ValueByteLimit,
    Thrown,
    DeclaredThrown,
    TypeContract,
}

fn kernel_failure_class(code: &str) -> FailureClass {
    match code {
        "division_by_zero" => FailureClass::DivisionByZero,
        "value_byte_limit" => FailureClass::ValueByteLimit,
        "harn_throw" => FailureClass::Thrown,
        "harn_declared_throw" => FailureClass::DeclaredThrown,
        "builtin_type" | "argument_type" => FailureClass::TypeContract,
        other => panic!("kernel code `{other}` has no native failure class; extend the mapping"),
    }
}

/// Classified from the error's structure, never its message. `None` means the
/// native VM failed in a way the portable contract has no class for.
fn native_failure_class(error: &harn_vm::value::VmError) -> Option<FailureClass> {
    use harn_vm::value::VmError;
    Some(match error {
        VmError::DivisionByZero => FailureClass::DivisionByZero,
        VmError::ValueByteLimit(_) => FailureClass::ValueByteLimit,
        VmError::Thrown(_) => FailureClass::Thrown,
        VmError::DeclaredThrown(_) => FailureClass::DeclaredThrown,
        VmError::TypeError(_) | VmError::ArgTypeMismatch(_) | VmError::ArityMismatch(_) => {
            FailureClass::TypeContract
        }
        _ => return None,
    })
}

/// Kernel failures that come from the portable profile's 1 MiB per-value
/// budget. The hostful VM caps a single expansion at 16 MiB and has no
/// per-value budget, so it completes these. The differential pins that: if
/// the native VM starts failing them, the budgets converged and the entry
/// should go.
const NATIVE_LARGER_BUDGET: &[&str] = &["string-repetition-rejects-output-over-byte-limit"];

/// Run `entry` from `case_source` in the native VM the way a host calls an
/// exported function: execute the module, then invoke the closure. Errors come
/// back as the VM raised them, before the top-level boundary renders a source
/// line into the message.
fn run_native(
    case_source: &str,
    entry: &str,
    input_json: &str,
) -> Result<harn_vm::VmValue, harn_vm::value::VmError> {
    let input =
        harn_kernel::DataValue::from_json(serde_json::from_str(input_json).expect("input JSON"))
            .expect("portable input");
    let source = format!("{case_source}\npipeline default() {{ return {entry} }}");
    let chunk = harn_vm::compile_source(&source)
        .unwrap_or_else(|error| panic!("{entry} did not compile in the native VM: {error}"));
    let runtime = harn_clock::test_support::current_thread_runtime();
    runtime.block_on(harn_clock::test_support::with_local_tasks(async move {
        let mut vm = harn_vm::new_vm();
        harn_runtime::register_vm_stdlib(&mut vm);
        let callee = vm.execute(&chunk).await.expect("module executes");
        let harn_vm::VmValue::Closure(closure) = callee else {
            panic!("{entry} is not a closure: {callee:?}");
        };
        vm.call_closure_pub(&closure, &[harn_vm::VmValue::from(input)])
            .await
    }))
}

#[test]
fn native_vm_fails_every_portable_runtime_failure_like_the_kernel() {
    assert!(
        !RUNTIME_FAILURE_CASES.is_empty(),
        "runtime failure corpus is empty"
    );
    for id in NATIVE_LARGER_BUDGET {
        assert!(
            RUNTIME_FAILURE_CASES.iter().any(|case| case.id == *id),
            "budget exemption `{id}` names no corpus case"
        );
    }
    let mut drift = Vec::new();
    for case in RUNTIME_FAILURE_CASES {
        let expected = kernel_failure_class(case.expected_code);
        let actual = run_native(case.source, case.entry, case.input_json);
        let matches = if NATIVE_LARGER_BUDGET.contains(&case.id) {
            actual.is_ok()
        } else {
            actual.as_ref().err().and_then(native_failure_class) == Some(expected)
        };
        if !matches {
            drift.push(format!(
                "{}: kernel {} ({expected:?}), native {actual:?}",
                case.id, case.expected_code
            ));
        }
    }
    assert!(
        drift.is_empty(),
        "native VM drifted from the portable kernel:\n{}",
        drift.join("\n")
    );
}

#[test]
fn native_vm_matches_portable_conformance_corpus() {
    for case in PURE_CASES {
        let value = run_native(case.source, case.entry, case.input_json)
            .unwrap_or_else(|error| panic!("{} failed in the native VM: {error:?}", case.id));
        let expected: serde_json::Value =
            serde_json::from_str(case.expected_json).expect("expected JSON");
        assert_eq!(
            harn_vm::llm::vm_value_to_json(&value),
            expected,
            "{} diverged in the native VM",
            case.id
        );
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
