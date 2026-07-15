use crate::orchestration::{
    clear_runtime_hooks, register_vm_hook_lazy, run_lifecycle_hooks_with_ctx,
    run_pre_tool_hooks_with_ctx, HookEvent, PreToolAction,
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

#[tokio::test(flavor = "current_thread")]
async fn lazy_hook_handler_resolves_sibling_pub_fn_from_child_vm() {
    crate::reset_thread_local_state();
    clear_runtime_hooks();
    let dir = tempfile::tempdir().expect("tempdir");
    let module_path = dir.path().join("lazy_hook.harn");
    // The handler body calls a `pub fn` imported from a SIBLING module. The
    // handler fires from a VM that never imported the handler's module graph
    // (an inner agent-loop VM dispatching a tool). If the handler closure does
    // not keep a live view of its module's import scope, the imported name
    // falls through to host-bridge dispatch and surfaces as `Undefined
    // builtin: hook_response_with_effects`.
    // `hook_response_with_effects` itself calls a sibling `pub fn` in its own
    // module. Resolving that inner call needs the effects module's function
    // registry to stay alive for as long as the retained handler closure that
    // transitively references it — even after the VM that first loaded the
    // module graph has been torn down.
    std::fs::write(
        dir.path().join("effects.harn"),
        r#"
pub fn deny_dict(reason: string) -> dict {
  return { "deny": reason }
}

pub fn hook_response_with_effects(reason: string) -> dict {
  return deny_dict(reason)
}
"#,
    )
    .expect("write sibling module");
    std::fs::write(
        &module_path,
        r#"
import { hook_response_with_effects } from "./effects"

pub fn handle(_payload: dict) -> dict {
  return hook_response_with_effects("blocked by sibling")
}
"#,
    )
    .expect("write lazy hook module");
    register_vm_hook_lazy(
        HookEvent::PreToolUse,
        "*",
        "handle",
        LazyVmCallable::new(module_path, "handle"),
    );

    // A fresh VM root that never imported the handler's module — mirrors an
    // inner agent-loop VM dispatching a tool. Each dispatch fires the hook on a
    // fresh child VM that is torn down when the hook returns; the second fire
    // exercises the cached resolution after the first firing VM (and the module
    // graph it loaded) has been dropped.
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(dir.path());
    let ctx = AsyncBuiltinCtx::for_test(vm);
    let args = serde_json::json!({});
    for fire in 1..=2 {
        let action = run_pre_tool_hooks_with_ctx(Some(&ctx), "read_file", &args)
            .await
            .unwrap_or_else(|error| panic!("pre-tool hook fire {fire} errored: {error}"));
        match action {
            PreToolAction::Deny(reason) => assert_eq!(reason, "blocked by sibling"),
            other => panic!("fire {fire}: expected Deny from sibling-fn handler, got {other:?}"),
        }
    }
    clear_runtime_hooks();
}
