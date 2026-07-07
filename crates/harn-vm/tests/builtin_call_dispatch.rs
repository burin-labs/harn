#![recursion_limit = "256"]
//! Regression tests for the synchronous-builtin fast path on `Op::CallBuiltin`.
//!
//! `execute_call_builtin_sync` dispatches synchronous builtins directly instead
//! of bailing to the async handler (which would re-run `resolve_named_closure`
//! and spin up the async state machine). These tests pin the *semantics* that
//! optimization must preserve:
//!   - a bare builtin call still produces the builtin's result,
//!   - a user `fn` that shadows a builtin name still wins over the builtin,
//!   - builtin calls keep working inside loops / collection callbacks,
//!   - builtin argument validation still fires on the fast path.

use harn_vm::value::{VmError, VmValue};

fn eval(source: &str) -> Result<VmValue, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"))
            })
            .await
    })
}

#[test]
fn sync_builtin_call_dispatches_inline() {
    // `abs` is a synchronous builtin: the fast path must return its result.
    assert!(matches!(
        eval("pipeline t(task) { return abs(-5) }"),
        Ok(VmValue::Int(5))
    ));
}

#[test]
fn user_fn_shadowing_a_builtin_name_still_wins() {
    // The sync fast path is only reached *after* `resolve_named_closure`
    // confirms the name is not a user closure. A module-level `fn` named like a
    // builtin must therefore still take precedence over the builtin.
    let result = eval(
        r"pipeline t(task) {
            fn abs(x) { return 999 }
            return abs(-5)
        }",
    );
    assert!(
        matches!(result, Ok(VmValue::Int(999))),
        "user fn must shadow the builtin, got {result:?}"
    );
}

#[test]
fn builtin_calls_work_inside_a_loop() {
    let result = eval(
        r"pipeline t(task) {
            let s = 0
            for x in [-1, -2, 3] {
                s = s + abs(x)
            }
            return s
        }",
    );
    assert!(
        matches!(result, Ok(VmValue::Int(6))),
        "expected sum of abs values, got {result:?}"
    );
}

#[test]
fn builtin_argument_validation_still_fires_on_fast_path() {
    // Calling a numeric builtin with a wrong-typed argument must still error
    // (validation runs inside the sync dispatch, not only on the async path).
    let result = eval(r#"pipeline t(task) { return abs("not a number") }"#);
    assert!(
        result.is_err(),
        "expected a validation error for abs(string), got {result:?}"
    );
}

#[test]
fn string_slice_aliases_substring_with_list_semantics() {
    // `slice` is historically a list method; harness authors routinely call it
    // on strings (JS/Python muscle memory). It must now work on strings —
    // char-based, negative-index aware, clamped — mirroring `list.slice`,
    // so "string has no method `slice`" can never crash an agent loop again.
    assert!(matches!(
        eval(r#"pipeline t(task) { return "hello world".slice(0, 5) }"#),
        Ok(VmValue::String(ref s)) if s == "hello"
    ));
    // Negative indices count from the end (like list.slice).
    assert!(matches!(
        eval(r#"pipeline t(task) { return "hello".slice(-3) }"#),
        Ok(VmValue::String(ref s)) if s == "llo"
    ));
    // Out-of-range end clamps instead of panicking.
    assert!(matches!(
        eval(r#"pipeline t(task) { return "hi".slice(0, 999) }"#),
        Ok(VmValue::String(ref s)) if s == "hi"
    ));
    // start > end yields empty, never a slice-index panic.
    assert!(matches!(
        eval(r#"pipeline t(task) { return "hello".slice(4, 1) }"#),
        Ok(VmValue::String(ref s)) if s.is_empty()
    ));
}
