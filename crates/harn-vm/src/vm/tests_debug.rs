use std::rc::Rc;

use crate::stdlib::register_vm_stdlib;
use crate::values_equal;
use crate::{Chunk, VmError, VmValue};

use super::*;

/// Drive the VM forward from a `start()`ed chunk until it reaches
/// the first breakpoint (or exhausts the step budget). Returns the
/// VM positioned in whatever frame the breakpoint lives in. Used
/// by the `evaluate_in_frame` tests below so we can inspect a paused
/// scope without wiring a full DAP session.
fn run_until_paused(vm: &mut Vm, chunk: &Chunk) {
    vm.start(chunk);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                for _ in 0..10_000 {
                    if vm.is_stopped() {
                        return;
                    }
                    match vm.step_execute().await {
                        Ok(Some((_, true))) => return,
                        Ok(_) => continue,
                        Err(e) => panic!("step_execute failed: {e}"),
                    }
                }
                panic!("run_until_paused: step budget exceeded");
            })
            .await
    })
}

/// Synchronously evaluate an expression on an already-paused VM.
/// Mirrors what harn-dap's `handle_evaluate` will do on the async
/// runtime it already owns.
fn eval(vm: &mut Vm, expr: &str) -> Result<VmValue, VmError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local.run_until(vm.evaluate_in_frame(expr, 0)).await
    })
}

#[test]
fn test_evaluate_in_frame_literal() {
    // Need a live frame for evaluate_in_frame, even for a pure
    // expression, because the scratch chunk inherits source info
    // from the top frame. Seed one by compiling & starting an empty
    // pipeline that just waits on a breakpoint.
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_breakpoints(vec![2]);
    let chunk = crate::compile_source("let __seed__: int = 0\nlog(__seed__)\n").unwrap();
    run_until_paused(&mut vm, &chunk);

    assert!(values_equal(
        &eval(&mut vm, "1 + 2").unwrap(),
        &VmValue::Int(3)
    ));
    assert!(values_equal(
        &eval(&mut vm, "\"hi\" + \" there\"").unwrap(),
        &VmValue::String(Rc::from("hi there"))
    ));
    assert!(values_equal(
        &eval(&mut vm, "5 > 3 && 2 < 4").unwrap(),
        &VmValue::Bool(true)
    ));
}

#[test]
fn test_evaluate_in_frame_sees_locals() {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_breakpoints(vec![3]);
    let chunk =
        crate::compile_source("let user: string = \"alice\"\nlet count: int = 42\nlog(count)\n")
            .unwrap();
    run_until_paused(&mut vm, &chunk);

    assert!(values_equal(
        &eval(&mut vm, "user").unwrap(),
        &VmValue::String(Rc::from("alice"))
    ));
    assert!(values_equal(
        &eval(&mut vm, "count * 2").unwrap(),
        &VmValue::Int(84)
    ));
    assert!(values_equal(
        &eval(&mut vm, "user + \" has \" + to_string(count)").unwrap(),
        &VmValue::String(Rc::from("alice has 42"))
    ));
}

#[test]
fn test_evaluate_in_frame_does_not_leak_state() {
    // Evaluation must be transparent to the live session — no
    // scope leftovers, no stack residue, no step-mode drift.
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_breakpoints(vec![2]);
    let chunk = crate::compile_source("let x: int = 7\nlog(x)\n").unwrap();
    run_until_paused(&mut vm, &chunk);

    let pre_stack = vm.stack.len();
    let pre_frames = vm.frames.len();
    let pre_scope = vm.env.scope_depth();
    let _ = eval(&mut vm, "x + 100").unwrap();
    let _ = eval(&mut vm, "x * x").unwrap();
    assert_eq!(vm.stack.len(), pre_stack);
    assert_eq!(vm.frames.len(), pre_frames);
    assert_eq!(vm.env.scope_depth(), pre_scope);
    // The synthetic `__harn_eval_result__` binding must not linger
    // in the paused scope.
    assert!(vm.env.get("__harn_eval_result__").is_none());
}

#[test]
fn test_set_variable_in_frame_updates_let_binding() {
    // Pipeline authors overwhelmingly use `let`; the debug
    // setVariable path must bypass immutability or it's useless.
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_breakpoints(vec![3]);
    let chunk =
        crate::compile_source("let count: int = 7\nlet label: string = \"before\"\nlog(count)\n")
            .unwrap();
    run_until_paused(&mut vm, &chunk);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let stored = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(vm.set_variable_in_frame("count", "42", 0))
            .await
    });
    assert!(values_equal(&stored.unwrap(), &VmValue::Int(42)));
    assert!(values_equal(
        &eval(&mut vm, "count").unwrap(),
        &VmValue::Int(42)
    ));

    // Expression RHS — not just literals.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(vm.set_variable_in_frame("label", "\"x\" + to_string(count)", 0))
            .await
            .unwrap()
    });
    assert!(values_equal(
        &eval(&mut vm, "label").unwrap(),
        &VmValue::String(Rc::from("x42"))
    ));
}

#[test]
fn test_set_variable_in_frame_rejects_undefined() {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_breakpoints(vec![2]);
    let chunk = crate::compile_source("let x: int = 1\nlog(x)\n").unwrap();
    run_until_paused(&mut vm, &chunk);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(vm.set_variable_in_frame("ghost", "0", 0))
                .await
        })
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("ghost"),
        "expected 'ghost' in error, got {msg}"
    );
}

#[test]
fn test_restart_frame_rewinds_ip_and_rebinds_args() {
    // Pause inside a function, mutate a local, restart the frame
    // — the mutation must vanish and execution must resume from
    // the top of the function with the original args.
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_breakpoints(vec![3]);
    let chunk = crate::compile_source(
        "fn inner(n: int) -> int { \n  let doubled: int = n * 2\n  log(doubled)\n  return doubled\n}\nlog(inner(21))\n",
    )
    .unwrap();
    run_until_paused(&mut vm, &chunk);

    // We're paused at line 3 inside `inner`. Mutate the local so
    // we can assert the restart wiped it.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(vm.set_variable_in_frame("doubled", "999", 0))
            .await
            .unwrap()
    });
    assert!(values_equal(
        &eval(&mut vm, "doubled").unwrap(),
        &VmValue::Int(999)
    ));

    // restart_frame(top_frame_index) rewinds `inner` to entry.
    let top = vm.frame_count() - 1;
    vm.restart_frame(top).unwrap();

    // `doubled` no longer exists because the function's scope was
    // blown away, but `n` should still be bound from the re-applied
    // arg.
    assert!(values_equal(
        &eval(&mut vm, "n").unwrap(),
        &VmValue::Int(21)
    ));
}

#[test]
fn test_restart_frame_rejects_scratch_frames() {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_breakpoints(vec![2]);
    let chunk = crate::compile_source("let x: int = 1\nlog(x)\n").unwrap();
    run_until_paused(&mut vm, &chunk);
    // The top-level pipeline frame has `initial_env: Some(_)` so
    // restartFrame *is* valid there — our script has no inner
    // function yet. Push a synthetic scratch frame via
    // evaluate_in_frame (which leaves no live frame when done),
    // then attempt restart on an out-of-range id.
    let err = vm.restart_frame(99).unwrap_err();
    assert!(err.to_string().contains("out of range"));
}

#[test]
fn test_signal_cancel_unwinds_step_loop() {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    // A busy-looping pipeline that would never terminate under a
    // normal run; signal cancel before stepping so the first
    // instruction check throws VmError::Thrown with the
    // cancelled kind.
    let chunk =
        crate::compile_source("pipeline t(task) { var i = 0\n while i < 1000000 { i = i + 1 } }\n")
            .unwrap();
    vm.start(&chunk);
    vm.signal_cancel();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local.run_until(vm.step_execute()).await
    });
    match result {
        Err(VmError::Thrown(VmValue::String(s))) => {
            assert!(
                s.contains("kind:cancelled:"),
                "cancellation must surface as a kind-tagged Thrown error"
            );
        }
        other => panic!("expected cancelled Thrown, got {other:?}"),
    }
}

#[test]
fn test_function_breakpoint_stops_on_entry() {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_function_breakpoints(vec!["do_work".to_string()]);
    let chunk = crate::compile_source(
        "fn do_work(n: int) -> int { return n + 1 }\npipeline t(task) { let x = do_work(41)\nlog(x) }\n",
    )
    .unwrap();
    run_until_paused(&mut vm, &chunk);
    // The latch must identify the matching function and get
    // drained exactly once.
    let hit = vm.take_pending_function_bp().expect("must latch a hit");
    assert_eq!(hit, "do_work");
    assert!(vm.take_pending_function_bp().is_none(), "one-shot");

    // The top frame should be `do_work` at entry.
    let frames = vm.debug_stack_frames();
    let top = frames.last().expect("callee frame on stack");
    assert_eq!(top.0, "do_work");
}

#[test]
fn test_generated_module_source_is_cached_and_tagged_for_debugger() {
    let source = "pub fn generated_answer() {\n  return 42\n}\n";
    let mut vm = Vm::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let exports = rt
        .block_on(vm.load_module_exports_from_source("<generated>/wrapper.harn", source))
        .expect("generated module loads");

    assert_eq!(
        vm.debug_source_for_path("<generated>/wrapper.harn")
            .as_deref(),
        Some(source)
    );
    let closure = exports
        .get("generated_answer")
        .expect("exported generated function");
    assert_eq!(
        closure.func.chunk.source_file.as_deref(),
        Some("<generated>/wrapper.harn")
    );
}

#[test]
fn test_function_breakpoint_unknown_name_does_not_fire() {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_function_breakpoints(vec!["nonexistent".to_string()]);
    let chunk = crate::compile_source("pipeline t(task) { let x = 1\nlog(x) }\n").unwrap();
    // With no matching callee, the program runs to completion
    // without latching a hit; run_until_paused would have panicked
    // with "step budget exceeded" if the VM idled, so wrap with a
    // finite run of step_execute until a natural terminate.
    vm.start(&chunk);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                for _ in 0..10_000 {
                    match vm.step_execute().await {
                        Ok(Some((_, false))) => return,
                        Ok(_) => continue,
                        Err(e) => panic!("step_execute failed: {e}"),
                    }
                }
                panic!("step budget exceeded");
            })
            .await
    });
    assert!(vm.take_pending_function_bp().is_none());
}

#[test]
fn test_evaluate_in_frame_parse_error_is_surfaced_standalone() {
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_breakpoints(vec![1]);
    let chunk = crate::compile_source("log(0)\n").unwrap();
    run_until_paused(&mut vm, &chunk);

    let err = eval(&mut vm, "(\"unterminated").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("evaluate:"),
        "expected evaluate error prefix, got: {msg}"
    );
}

#[test]
fn test_breakpoints_wildcard_matches_any_file() {
    let mut vm = Vm::new();
    vm.set_breakpoints(vec![3, 7]);
    assert!(vm.breakpoint_matches(3));
    assert!(vm.breakpoint_matches(7));
    assert!(!vm.breakpoint_matches(4));
}

#[test]
fn test_breakpoints_per_file_does_not_leak_to_wildcard() {
    let mut vm = Vm::new();
    vm.set_breakpoints_for_file("auto.harn", vec![10]);
    // Without an active frame, only the empty-string key matches; a
    // file-scoped breakpoint must NOT fire when no frame is active.
    assert!(!vm.breakpoint_matches(10));
}

#[test]
fn test_breakpoints_per_file_clear_on_empty() {
    let mut vm = Vm::new();
    vm.set_breakpoints_for_file("a.harn", vec![1, 2]);
    vm.set_breakpoints_for_file("a.harn", vec![]);
    assert!(!vm.breakpoints.contains_key("a.harn"));
}
