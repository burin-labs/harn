use crate::orchestration::{
    clear_runtime_hooks, register_vm_hook_lazy, run_lifecycle_hooks_with_ctx, HookEvent,
};
use crate::{register_vm_stdlib, AsyncBuiltinCtx, LazyVmCallable, Vm};

#[tokio::test(flavor = "current_thread")]
async fn lazy_hook_module_state_survives_repeated_child_invocations() {
    crate::reset_thread_local_state();
    clear_runtime_hooks();
    let dir = tempfile::tempdir().expect("tempdir");
    let module_path = dir.path().join("lazy_hook.harn");
    let marker = dir.path().join("hook-count.txt");
    let marker_literal = serde_json::to_string(&marker.to_string_lossy()).expect("marker literal");
    std::fs::write(
        &module_path,
        format!(
            r"
let counter = 0

pub fn handle(_payload: dict) -> nil {{
  counter = counter + 1
  write_file({marker_literal}, to_string(counter))
  return nil
}}
"
        ),
    )
    .expect("write lazy hook module");
    register_vm_hook_lazy(
        HookEvent::SessionStart,
        "*",
        "handle",
        LazyVmCallable::new(module_path, "handle"),
    );

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(dir.path());
    let ctx = AsyncBuiltinCtx::for_test(vm);
    let payload = serde_json::json!({"session": {"id": "lazy-hook-test"}});
    run_lifecycle_hooks_with_ctx(Some(&ctx), HookEvent::SessionStart, &payload)
        .await
        .expect("first hook invocation");
    run_lifecycle_hooks_with_ctx(Some(&ctx), HookEvent::SessionStart, &payload)
        .await
        .expect("second hook invocation");

    assert_eq!(std::fs::read_to_string(marker).expect("marker"), "2");
    clear_runtime_hooks();
}
