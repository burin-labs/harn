//! Tests for [`crate::vm::state`].
//!
//! Split out of `state.rs` to keep that file under the source-length cap; they
//! are still its `tests` child module, so private items stay reachable through
//! `use super::*`.

use super::*;

fn baseline_with_stdlib(source: &str) -> VmBaseline {
    let mut vm = super::super::test_runtime::new_vm();
    vm.set_source_info("baseline_test.harn", source);
    vm.set_global(
        "stable_global",
        VmValue::String(arcstr::ArcStr::from("baseline")),
    );
    vm.baseline()
}

#[test]
fn vm_baseline_instantiates_clean_mutable_execution_state() {
    let baseline = baseline_with_stdlib(
        "pipeline main(harness: Harness) { harness.stdio.println(stable_global) }",
    );

    let mut dirty = baseline.instantiate();
    dirty.stack.push(VmValue::Int(42));
    dirty.output.push_str("dirty");
    dirty.task_counter = 9;
    dirty.runtime_context_counter = 7;
    dirty
        .error_stack_trace
        .push(("main".to_string(), 1, 1, None));

    let clean = baseline.instantiate();
    assert!(clean.stack.is_empty());
    assert!(clean.output.is_empty());
    assert!(clean.frames.is_empty());
    assert!(clean.exception_handlers.is_empty());
    assert!(clean.spawned_tasks.is_empty());
    assert!(clean.held_sync_guards.is_empty());
    assert_eq!(clean.task_counter, 0);
    assert_eq!(clean.runtime_context_counter, 0);
    assert!(clean.deadlines.is_empty());
    assert!(clean.cancel_token.is_none());
    assert!(clean.interrupt_handlers.is_empty());
    assert!(clean.error_stack_trace.is_empty());
    assert!(clean
        .globals
        .get("stable_global")
        .is_some_and(|value| value.display() == "baseline"));
}

#[tokio::test]
async fn inline_child_inherits_held_lock_keys_but_concurrent_child_does_not() {
    let mut parent = super::super::test_runtime::new_vm();
    let permit = parent
        .sync_runtime
        .acquire("mutex", "v:test", 1, 1, None, None)
        .await
        .unwrap()
        .unwrap();
    parent
        .held_sync_guards
        .push(crate::synchronization::VmSyncHeldGuard {
            _permit: permit,
            frame_depth: 0,
            env_scope_depth: 0,
        });
    assert_eq!(parent.held_permits_for("mutex", "v:test"), 1);

    // An inline child (async builtin awaited while the parent is parked, or
    // a closure the builtin runs inline) inherits the held key, so a
    // re-acquire is caught as a cross-context self-deadlock (HARN-ORC-011)
    // — even transitively through a further inline child.
    let inline = parent.child_vm_inline();
    assert!(Arc::ptr_eq(
        &inline.execution_deadline,
        &parent.execution_deadline
    ));
    assert_eq!(inline.held_permits_for("mutex", "v:test"), 1);
    assert_eq!(
        inline.child_vm_inline().held_permits_for("mutex", "v:test"),
        1
    );

    // A new concurrent task (spawn / parallel / trigger) does NOT inherit:
    // blocking on a parent-held lock there is legitimately resolvable, so
    // flagging it would be a false positive.
    let concurrent = parent.child_vm();
    assert_eq!(concurrent.held_permits_for("mutex", "v:test"), 0);
}

#[test]
fn vm_reports_effective_runtime_limits() {
    let vm = super::super::test_runtime::new_vm();

    assert_eq!(vm.runtime_limits(), RuntimeLimits::default());
    assert_eq!(
        vm.runtime_limit_report().entries.len(),
        crate::runtime_limits::RUNTIME_LIMIT_DESCRIPTIONS.len()
    );
    assert_eq!(vm.child_vm().runtime_limits(), vm.runtime_limits());
    assert_eq!(
        vm.baseline().instantiate().runtime_limits(),
        vm.runtime_limits()
    );
}
