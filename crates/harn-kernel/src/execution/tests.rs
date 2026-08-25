use super::snapshot::SNAPSHOT_TAG_BYTES;
use super::*;
use crate::{compile_program, EntryKind};

#[test]
fn reducer_executes_and_round_trips_json() {
    let source = "fn reduce(input: {count: int, reset: bool, tags: list<string>}) {\n  if input.reset { return {count: 0, tags: input.tags} }\n  return {count: input.count + 1, tags: input.tags + [\"increment\"]}\n}";
    let program = compile_program(source, "reduce", EntryKind::Function).unwrap();
    let input =
        DataValue::from_json(serde_json::json!({"count": 2, "reset": false, "tags": []})).unwrap();
    let Execution::Completed { value } = start(&program, input, &GrantSet::pure()) else {
        panic!("reducer did not complete")
    };
    assert_eq!(
        value.to_json(),
        serde_json::json!({"count": 3, "tags": ["increment"]})
    );
}

#[test]
fn equality_is_structural_across_unlike_types() {
    let program = compile_program(
        "fn reduce(input: string) { return [input != nil, input != 7, 1 == 1.0, nil == nil] }",
        "reduce",
        EntryKind::Function,
    )
    .unwrap();

    assert_eq!(
        start(
            &program,
            DataValue::String("ui://portable".to_string()),
            &GrantSet::pure(),
        ),
        Execution::Completed {
            value: DataValue::List(vec![
                DataValue::Bool(true),
                DataValue::Bool(true),
                DataValue::Bool(true),
                DataValue::Bool(true),
            ])
        }
    );
}

#[test]
fn known_unimplemented_builtin_fails_only_when_execution_reaches_it() {
    let source = r#"
        fn reduce(input: string) {
            if input == "parse" { return json_parse("{}") }
            return "pure"
        }
    "#;
    let program = compile_program(source, "reduce", EntryKind::Function).unwrap();
    assert_eq!(
        start(
            &program,
            DataValue::String("skip".into()),
            &GrantSet::pure()
        ),
        Execution::Completed {
            value: DataValue::String("pure".into())
        }
    );
    let Execution::Failed { diagnostic } = start(
        &program,
        DataValue::String("parse".into()),
        &GrantSet::pure(),
    ) else {
        panic!("unimplemented builtin did not fail at its actual trigger")
    };
    assert_eq!(diagnostic.code, "unsupported_builtin");
    assert!(diagnostic.message.contains("json_parse"));
}

#[test]
fn entry_parameter_types_are_enforced_at_the_host_boundary() {
    let source =
        "fn reduce(input: { count: int, tags: list<string> }) -> int { return input.count }";
    let program = compile_program(source, "reduce", EntryKind::Function).unwrap();
    let invalid = DataValue::from_json(serde_json::json!({
        "count": "three",
        "tags": ["portable"]
    }))
    .unwrap();

    let Execution::Failed { diagnostic } = start(&program, invalid, &GrantSet::pure()) else {
        panic!("typed entry accepted an invalid host value")
    };
    assert_eq!(diagnostic.code, "argument_type");
    assert_eq!(
        diagnostic.message,
        "function `reduce` parameter `input` rejected dict"
    );
}

#[test]
fn nested_calls_share_type_checks_and_rest_binding() {
    let source = r"
        fn collect(...values: int) -> list<int> { return values }
        fn reduce(input: int) -> list<int> { return collect(input, 2) }
    ";
    let program = compile_program(source, "reduce", EntryKind::Function).unwrap();
    let execution = start(&program, DataValue::Int(7), &GrantSet::pure());
    let reduce_chunk = program
        .image()
        .functions
        .iter()
        .find(|function| function.name == "reduce")
        .expect("reduce closure")
        .chunk
        .disassemble("reduce");
    assert_eq!(
        execution,
        Execution::Completed {
            value: DataValue::List(vec![DataValue::Int(7), DataValue::Int(2)])
        },
        "{}\n{reduce_chunk}",
        program.image().disassemble("portable entry")
    );

    let Execution::Failed { diagnostic } = start(
        &program,
        DataValue::String("not an int".to_string()),
        &GrantSet::pure(),
    ) else {
        panic!("nested typed rest call accepted an invalid value")
    };
    assert_eq!(diagnostic.code, "argument_type");
}

#[test]
fn named_calls_support_recursion_and_mutual_recursion() {
    let source = r"
        fn fib(n: int) -> int {
            if n <= 1 { return n }
            return fib(n - 1) + fib(n - 2)
        }
        fn even(n: int) -> bool {
            if n == 0 { return true }
            return odd(n - 1)
        }
        fn odd(n: int) -> bool {
            if n == 0 { return false }
            return even(n - 1)
        }
    ";
    let fib = compile_program(source, "fib", EntryKind::Function).unwrap();
    assert_eq!(
        start(&fib, DataValue::Int(7), &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::Int(13)
        }
    );

    let even = compile_program(source, "even", EntryKind::Function).unwrap();
    assert_eq!(
        start(&even, DataValue::Int(8), &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::Bool(true)
        }
    );
}

#[test]
fn enums_and_result_propagation_share_native_bytecode_semantics() {
    let source = r#"
        fn divide(value: int, divisor: int) -> Result<int, string> {
            if divisor == 0 { return Result.Err("division by zero") }
            return Result.Ok(value / divisor)
        }
        fn halve(value: int, divisor: int) -> Result<int, string> {
            const divided: int = divide(value, divisor)?
            return Result.Ok(divided / 2)
        }
        fn reduce(input: int) {
            const result = halve(12, input)
            match result {
                Result.Ok(value) -> {
                    return [result.variant, result.fields, value, result == Result.Ok(value)]
                }
                Result.Err(message) -> {
                    return [result.variant, result.fields, message, result == Result.Err(message)]
                }
            }
        }
    "#;
    let program = compile_program(source, "reduce", EntryKind::Function).unwrap();

    assert_eq!(
        start(&program, DataValue::Int(3), &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::from_json(serde_json::json!(["Ok", [2], 2, true])).unwrap()
        }
    );
    assert_eq!(
        start(&program, DataValue::Int(0), &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::from_json(serde_json::json!([
                "Err",
                ["division by zero"],
                "division by zero",
                true,
            ]))
            .unwrap()
        }
    );
}

#[test]
fn spread_named_calls_resolve_lexical_functions_before_builtins() {
    let source = r"
        fn collect(...values: int) -> list<int> { return values }
        fn reduce(input: list<int>) -> list<int> { return collect(...input) }
    ";
    let program = compile_program(source, "reduce", EntryKind::Function).unwrap();

    assert_eq!(
        start(
            &program,
            DataValue::List(vec![DataValue::Int(3), DataValue::Int(5)]),
            &GrantSet::pure(),
        ),
        Execution::Completed {
            value: DataValue::List(vec![DataValue::Int(3), DataValue::Int(5)])
        }
    );
}

#[test]
fn closure_capture_arena_drops_without_rc_cycles() {
    let program = compile_program(
        "fn reduce<T>(input: T) -> T {\n  fn identity<U>(value: U) -> U { return value }\n  return identity(input)\n}",
        "reduce",
        EntryKind::Function,
    )
    .unwrap();
    let grants = GrantSet::pure();
    let root = Env::root();
    let root_weak = Rc::downgrade(&root);

    {
        let mut machine = Machine::new(&program, root.clone(), &grants, Vec::new(), 0);
        let _ = machine.execute(program.image().clone(), root.clone(), Vec::new());
    }
    drop(root);

    assert!(
        root_weak.upgrade().is_none(),
        "closure environments must be owned by the execution arena, not Rc cycles"
    );
}

#[test]
fn escaping_closures_keep_their_environment_until_execution_ends() {
    let source = r"
        fn make_adder(increment: int) {
            fn add(value: int) -> int { return value + increment }
            return add
        }
        fn reduce(input: int) -> int {
            let add = make_adder(input)
            return add(2)
        }
    ";
    let program = compile_program(source, "reduce", EntryKind::Function).unwrap();

    assert_eq!(
        start(&program, DataValue::Int(3), &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::Int(5)
        }
    );
}

#[test]
fn shared_value_graphs_are_bounded_by_logical_work() {
    let source = r"
        fn reduce(input: int) {
            let value = [input]
            let index = 0
            while index < 16 {
                value = value + value
                index = index + 1
            }
            let shared = [value]
            return shared + shared
        }
    ";
    let program = compile_program(source, "reduce", EntryKind::Function).unwrap();
    let Execution::Failed { diagnostic } = start(&program, DataValue::Int(1), &GrantSet::pure())
    else {
        panic!("shared graph bypassed the logical node limit")
    };
    assert_eq!(diagnostic.code, "value_node_limit");
}

#[test]
fn recursive_equality_work_consumes_deterministic_fuel() {
    let source = r"
        fn reduce(input: int) {
            let value = [input]
            let index = 0
            while index < 15 {
                value = value + value
                index = index + 1
            }
            index = 0
            while index < 40 {
                let same = value == value
                index = index + 1
            }
            return true
        }
    ";
    let program = compile_program(source, "reduce", EntryKind::Function).unwrap();
    let Execution::Failed { diagnostic } = start(&program, DataValue::Int(1), &GrantSet::pure())
    else {
        panic!("recursive equality bypassed deterministic fuel accounting")
    };
    assert_eq!(diagnostic.code, "execution_fuel");
}

#[test]
fn capability_suspend_resume_is_deterministic() {
    let source =
        "fn greet(harness: Harness, input: string) {\n  return harness.interaction.ask(input)\n}";
    let program = compile_program(source, "greet", EntryKind::Function).unwrap();
    let grants = GrantSet::from_names(["interaction.ask".to_string()])
        .unwrap()
        .with_snapshot_key([7; 32]);
    let Execution::Suspended { request, snapshot } =
        start(&program, DataValue::String("name".into()), &grants)
    else {
        panic!("did not suspend")
    };
    let result = CapabilityResult::Ok {
        request_id: request.id.clone(),
        value: DataValue::String("Ada".into()),
    };
    let first = resume(&program, &snapshot, result.clone(), &grants);
    let second = resume(&program, &snapshot, result, &grants);
    let uninterrupted = replay(
        &program,
        DataValue::String("name".into()),
        &grants,
        vec![CapabilityResult::Ok {
            request_id: request.id,
            value: DataValue::String("Ada".into()),
        }],
    );
    assert_eq!(first, second);
    assert_eq!(first, uninterrupted);
    assert_eq!(
        first,
        Execution::Completed {
            value: DataValue::String("Ada".into())
        }
    );
}

#[test]
fn resume_replays_consumed_prefix_without_double_charging_fuel() {
    let source = r#"
        fn run(harness: Harness, input: int) {
            let value = [input]
            let index = 0
            while index < 16 {
                value = value + value
                index = index + 1
            }
            index = 0
            while index < 8 {
                let same = value == value
                index = index + 1
            }
            let answer = harness.interaction.ask("continue")
            index = 0
            while index < 4 {
                let same = value == value
                index = index + 1
            }
            return answer
        }
    "#;
    let program = compile_program(source, "run", EntryKind::Function).unwrap();
    let grants = GrantSet::from_names(["interaction.ask".to_string()])
        .unwrap()
        .with_snapshot_key([17; 32]);
    let Execution::Suspended { request, snapshot } = start(&program, DataValue::Int(1), &grants)
    else {
        panic!("fuel-heavy prefix did not reach its capability request")
    };

    assert_eq!(
        resume(
            &program,
            &snapshot,
            CapabilityResult::Ok {
                request_id: request.id,
                value: DataValue::String("done".to_string()),
            },
            &grants,
        ),
        Execution::Completed {
            value: DataValue::String("done".to_string())
        }
    );
}

#[test]
fn denied_capability_fails_before_suspending() {
    let source =
        "fn greet(harness: Harness, input: unknown) {\n  return harness.interaction.ask(input)\n}";
    let program = compile_program(source, "greet", EntryKind::Function).unwrap();
    let Execution::Failed { diagnostic } = start(&program, DataValue::Nil, &GrantSet::pure())
    else {
        panic!("denial did not fail")
    };
    assert_eq!(diagnostic.code, "capability_denied");
}

#[test]
fn capability_contracts_reject_unknown_grants_and_wrong_results() {
    let error = GrantSet::from_names(["stdio.not_a_method".to_string()]).unwrap_err();
    assert_eq!(error.code, "unknown_capability_grant");

    let source = "fn width(harness: Harness, _input: unknown) {\n  return harness.term.width()\n}";
    let program = compile_program(source, "width", EntryKind::Function).unwrap();
    let grants = GrantSet::from_names(["term.width".to_string()])
        .unwrap()
        .with_snapshot_key([11; 32]);
    let Execution::Suspended { request, snapshot } = start(&program, DataValue::Nil, &grants)
    else {
        panic!("typed capability did not suspend")
    };
    assert_eq!(request.expected, ValueShape::Int);
    let result = CapabilityResult::Ok {
        request_id: request.id,
        value: DataValue::String("wide".to_string()),
    };
    let Execution::Failed { diagnostic } = resume(&program, &snapshot, result, &grants) else {
        panic!("wrong capability result type was accepted")
    };
    assert_eq!(diagnostic.code, "capability_result_type");
}

#[test]
fn capability_grants_reject_nonportable_registry_types() {
    for grant in ["fs.read_text_result", "llm.stream"] {
        let diagnostic = GrantSet::from_names([grant.to_string()]).unwrap_err();
        assert_eq!(diagnostic.code, "unsupported_portable_capability_type");
        assert!(diagnostic.message.contains(grant));
    }

    GrantSet::from_names(["interaction.ask".to_string(), "llm.call".to_string()])
        .expect("JSON-shaped nested capability contracts remain portable");
}

#[test]
fn host_grants_use_one_strict_record_contract() {
    let pure = GrantSet::from_host_json(r#"{"capabilities":[]}"#).unwrap();
    assert!(!pure.allows("interaction", "ask"));

    let suspendable = GrantSet::from_host_json(
        r#"{"capabilities":["interaction.ask"],"snapshotKey":[7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7]}"#,
    )
    .unwrap();
    assert!(suspendable.allows("interaction", "ask"));

    assert_eq!(
        GrantSet::from_host_json("[]").unwrap_err().code,
        "invalid_capability_grants"
    );
    assert_eq!(
        GrantSet::from_host_json(r#"{"capabilities":[],"extra":true}"#)
            .unwrap_err()
            .code,
        "invalid_capability_grants"
    );
}

#[test]
fn capability_arguments_use_the_full_canonical_type_contract() {
    let source = r#"
        // Deliberate unchecked host boundary: this test exercises the runtime
        // capability contract, not static call-site assignability.
        fn call(harness: Harness, input: any) {
            return harness.llm.call("hello", nil, input)
        }
    "#;
    let program = compile_program(source, "call", EntryKind::Function).unwrap();
    let grants = GrantSet::from_names(["llm.call".to_string()]).unwrap();
    let input = DataValue::from_json(serde_json::json!({"max_tokens": "many"})).unwrap();

    let Execution::Failed { diagnostic } = start(&program, input, &grants) else {
        panic!("nested capability argument mismatch was accepted")
    };
    assert_eq!(diagnostic.code, "capability_argument_type");
    assert!(diagnostic.message.contains("argument `options`"));
}

#[test]
fn capability_results_use_the_full_canonical_type_contract() {
    let source = r#"
        fn call(harness: Harness, _input: unknown) {
            return harness.llm.call("hello", nil, {})
        }
    "#;
    let program = compile_program(source, "call", EntryKind::Function).unwrap();
    let grants = GrantSet::from_names(["llm.call".to_string()])
        .unwrap()
        .with_snapshot_key([29; 32]);
    let Execution::Suspended { request, snapshot } = start(&program, DataValue::Nil, &grants)
    else {
        panic!("well-typed capability call did not suspend")
    };
    let malformed = DataValue::from_json(serde_json::json!({"text": "incomplete"})).unwrap();

    let Execution::Failed { diagnostic } = resume(
        &program,
        &snapshot,
        CapabilityResult::Ok {
            request_id: request.id,
            value: malformed,
        },
        &grants,
    ) else {
        panic!("nested capability result mismatch was accepted")
    };
    assert_eq!(diagnostic.code, "capability_result_type");
}

#[test]
fn snapshots_are_authenticated_and_bind_the_grant_ceiling() {
    let source = "fn width(harness: Harness, _input: unknown) {\n  return harness.term.width()\n}";
    let program = compile_program(source, "width", EntryKind::Function).unwrap();
    let grants = GrantSet::from_names(["term.width".to_string()])
        .unwrap()
        .with_snapshot_key([13; 32]);
    let Execution::Suspended {
        request,
        mut snapshot,
    } = start(&program, DataValue::Nil, &grants)
    else {
        panic!("typed capability did not suspend")
    };
    let result = CapabilityResult::Ok {
        request_id: request.id,
        value: DataValue::Int(120),
    };

    let payload_byte = snapshot.len() - SNAPSHOT_TAG_BYTES - 1;
    snapshot[payload_byte] ^= 1;
    let Execution::Failed { diagnostic } = resume(&program, &snapshot, result.clone(), &grants)
    else {
        panic!("tampered snapshot was accepted")
    };
    assert_eq!(diagnostic.code, "snapshot_authentication");

    let Execution::Suspended { snapshot, .. } = start(&program, DataValue::Nil, &grants) else {
        panic!("typed capability did not suspend")
    };
    let different_grants = GrantSet::from_names(["term.height".to_string()])
        .unwrap()
        .with_snapshot_key([13; 32]);
    let Execution::Failed { diagnostic } = resume(&program, &snapshot, result, &different_grants)
    else {
        panic!("grant escalation was accepted")
    };
    assert_eq!(diagnostic.code, "snapshot_grant_mismatch");
}

#[test]
fn tagged_json_preserves_portable_edge_values() {
    for value in [
        DataValue::Int(i64::MIN),
        DataValue::Int(i64::MAX),
        DataValue::Float(f64::INFINITY),
        DataValue::Float(f64::NEG_INFINITY),
        DataValue::Bytes(vec![0, 127, 255]),
    ] {
        let decoded = DataValue::from_json(value.to_json()).unwrap();
        assert_eq!(decoded, value);
    }
    let nan = DataValue::from_json(DataValue::Float(f64::NAN).to_json()).unwrap();
    assert!(matches!(nan, DataValue::Float(value) if value.is_nan()));

    for value in [
        DataValue::Float(f64::INFINITY),
        DataValue::Float(f64::NEG_INFINITY),
        DataValue::Int(i64::MAX),
        DataValue::Bytes(vec![0, 127, 255]),
    ] {
        let encoded = serde_json::to_vec(&value).unwrap();
        let decoded: DataValue = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, value);
    }
    let encoded_nan = serde_json::to_vec(&DataValue::Float(f64::NAN)).unwrap();
    let decoded_nan: DataValue = serde_json::from_slice(&encoded_nan).unwrap();
    assert!(matches!(decoded_nan, DataValue::Float(value) if value.is_nan()));
}

#[test]
fn non_finite_values_round_trip_through_requests_and_snapshots() {
    let source =
        "fn echo(harness: Harness, input: unknown) { return harness.interaction.ask(input) }";
    let program = compile_program(source, "echo", EntryKind::Function).unwrap();
    let grants = GrantSet::from_names(["interaction.ask".to_string()])
        .unwrap()
        .with_snapshot_key([23; 32]);
    let Execution::Suspended { request, snapshot } =
        start(&program, DataValue::Float(f64::INFINITY), &grants)
    else {
        panic!("non-finite input did not reach the capability seam")
    };
    assert_eq!(
        serde_json::to_value(&request.arguments).unwrap(),
        serde_json::json!([{"$float": "infinity"}])
    );

    assert_eq!(
        resume(
            &program,
            &snapshot,
            CapabilityResult::Ok {
                request_id: request.id,
                value: DataValue::String("finite result".to_string()),
            },
            &grants,
        ),
        Execution::Completed {
            value: DataValue::String("finite result".to_string())
        }
    );
}

/// The binding check is versioned artifact behavior, not a native-only feature.
/// The portable kernel must reach the same verdict the native VM does, or an
/// artifact would mean different things depending on where it runs.
#[test]
fn the_portable_kernel_enforces_a_binding_annotation() {
    let program = compile_program(
        "fn reduce(input: dict) {\n  const name: string = input.name\n  return name\n}",
        "reduce",
        EntryKind::Function,
    )
    .unwrap();

    let good = DataValue::from_json(serde_json::json!({"name": "ada"})).unwrap();
    assert_eq!(
        start(&program, good, &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::String("ada".to_string())
        }
    );

    let bad = DataValue::from_json(serde_json::json!({"name": 12345})).unwrap();
    let Execution::Failed { diagnostic } = start(&program, bad, &GrantSet::pure()) else {
        panic!("an int must not satisfy a `string` binding")
    };
    assert_eq!(diagnostic.code, "binding_type");
    assert!(
        diagnostic.message.contains("binding `name`"),
        "diagnostic must name the binding, got: {}",
        diagnostic.message
    );
}

/// Struct field annotations are versioned artifact behavior. The portable
/// kernel must reject a bad field at construction the same way the native VM
/// does (harn#6268).
#[test]
fn the_portable_kernel_enforces_a_struct_field_annotation() {
    let program = compile_program(
        "struct User { name: string }\nfn reduce(input: dict) {\n  return User { name: input.name }\n}",
        "reduce",
        EntryKind::Function,
    )
    .unwrap();

    let good = DataValue::from_json(serde_json::json!({"name": "ada"})).unwrap();
    assert_eq!(
        start(&program, good, &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::from_json(serde_json::json!({"name": "ada"})).unwrap()
        }
    );

    let bad = DataValue::from_json(serde_json::json!({"name": 12345})).unwrap();
    let Execution::Failed { diagnostic } = start(&program, bad, &GrantSet::pure()) else {
        panic!("an int must not satisfy a `string` struct field")
    };
    assert_eq!(diagnostic.code, "binding_type");
    assert!(
        diagnostic.message.contains("binding `User`"),
        "diagnostic must name the struct, got: {}",
        diagnostic.message
    );
}

/// `unknown` keeps a host boundary explicit while an unannotated local remains
/// inferred and `any` remains the written opt-out from local checks.
#[test]
fn unknown_parameter_and_any_binding_stay_unchecked() {
    let program = compile_program(
        "fn reduce(input: unknown) {\n  const loose = input\n  const opted: any = input\n  return [loose, opted]\n}",
        "reduce",
        EntryKind::Function,
    )
    .unwrap();

    assert_eq!(
        start(&program, DataValue::Int(7), &GrantSet::pure()),
        Execution::Completed {
            value: DataValue::List(vec![DataValue::Int(7), DataValue::Int(7)])
        }
    );
}
