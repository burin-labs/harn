use crate::orchestration::{
    clear_runtime_hooks, register_vm_hook_lazy, run_lifecycle_hooks_with_ctx, HookEvent,
};
use crate::{register_vm_stdlib, AsyncBuiltinCtx, LazyVmCallable, Vm};

#[tokio::test(flavor = "current_thread")]
async fn registered_host_hook_reaches_bridge_without_granting_the_tool_host_authority() {
    use crate::orchestration::{
        allow_trusted_bridge_calls, pop_execution_policy, push_execution_policy,
        run_post_tool_hooks_with_ctx, CapabilityPolicy,
    };
    use crate::value::{DictMap, VmValue};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct HookBridge(Arc<AtomicUsize>);
    impl crate::HostCallBridge for HookBridge {
        fn dispatch<'a>(
            &'a self,
            capability: &'a str,
            operation: &'a str,
            params: &'a DictMap,
        ) -> crate::HostCallDispatchFuture<'a> {
            assert_eq!((capability, operation), ("runtime", "execute_hook"));
            assert_eq!(
                params.get("event").map(VmValue::display).as_deref(),
                Some("PostToolUse")
            );
            self.0.fetch_add(1, Ordering::SeqCst);
            crate::host_call_ready(Ok(Some(VmValue::dict([(
                "text",
                VmValue::string("host-hook:original"),
            )]))))
        }
    }

    crate::reset_thread_local_state();
    let calls = Arc::new(AtomicUsize::new(0));
    let _bridge = crate::install_host_call_bridge(Arc::new(HookBridge(calls.clone())));
    let dir = tempfile::tempdir().expect("tempdir");
    let module = dir.path().join("host_hook.harn");
    std::fs::write(
        &module,
        r#"
pub fn handle(_harness: Harness, _event: dict) {
  const response = host_call("runtime.execute_hook", {event: "PostToolUse"})
  return {result: response.text}
}
"#,
    )
    .expect("write hook module");
    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.enable_trusted_host_dispatch()
        .expect("host-selected module authority");
    let exports = vm
        .load_module_exports(&module)
        .await
        .expect("load host module");
    let handle = exports.get("handle").expect("hook export");
    let args = [
        vm.root_harness_value().expect("root Harness"),
        VmValue::dict(DictMap::new()),
    ];
    let mut policy = CapabilityPolicy::default();
    policy.restrict_capabilities(std::collections::BTreeMap::new());
    policy.side_effects = vec!["read_only".to_string()];
    push_execution_policy(policy);
    let denied = vm
        .call_closure_pub(handle, &args)
        .await
        .expect_err("ordinary call denied");
    assert!(
        denied.to_string().contains("exceeds capability ceiling"),
        "{denied}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    register_vm_hook_lazy(
        HookEvent::PostToolUse,
        "*",
        "handle",
        LazyVmCallable::new(module, "handle"),
    );
    let ctx = AsyncBuiltinCtx::for_test(vm);
    let result =
        run_post_tool_hooks_with_ctx(Some(&ctx), "look", &serde_json::json!({}), "original")
            .await
            .expect("runtime-selected hook reaches host despite tool ceiling");
    assert_eq!(result.text, "host-hook:original");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let mut child = ctx.child_vm();
    let denied = child
        .call_closure_pub(handle, &args)
        .await
        .expect_err("hook authority unwinds");
    assert!(
        denied.to_string().contains("exceeds capability ceiling"),
        "{denied}"
    );
    {
        let _trusted = allow_trusted_bridge_calls();
        let denied = crate::tool_handler_scope::scope(child.call_closure_pub(handle, &args))
            .await
            .expect_err("tool-handler host-call boundary remains closed");
        assert!(
            denied
                .to_string()
                .contains("unavailable inside a tool handler"),
            "{denied}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    pop_execution_policy();
    clear_runtime_hooks();
}

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

pub fn handle(harness: Harness, _payload: dict) -> nil {{
  counter = counter + 1
  harness.fs.write_text({marker_literal}, to_string(counter))
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

// Class B falsifier (v0.10.18 gate): a manifest hook handler whose BODY calls a
// SIBLING module `pub fn`, fired from a child agent-loop VM. Before the fix the
// lazy resolution returned the handler closure without its module scope retained
// when the resolving child VM was not the strong owner of the module registry,
// so the sibling call fell through name resolution to host-bridge dispatch and
// threw `Undefined builtin: sibling_effect`.
#[tokio::test(flavor = "current_thread")]
async fn lazy_hook_handler_resolves_transitive_sibling_across_repeated_fires() {
    crate::reset_thread_local_state();
    clear_runtime_hooks();
    let dir = tempfile::tempdir().expect("tempdir");
    // Mirror the Burin manifest shape (harn.toml -> lib.harn ->
    // lib/hooks/host-delegates.harn): the registered handler lives in the ENTRY
    // module and calls an IMPORTED `pub fn` from a sibling module, and that
    // imported function in turn calls its OWN same-module `pub fn` sibling.
    // The transitively-imported module's registry is only kept alive by the
    // child VM that first loaded the graph, so the second fire (a fresh child
    // VM hitting the lazy cache) used to throw
    // `Undefined builtin: delegate_effect` on the innermost sibling call.
    let marker = dir.path().join("transitive-marker.txt");
    let marker_literal = serde_json::to_string(&marker.to_string_lossy()).expect("marker literal");
    let delegates_path = dir.path().join("delegates.harn");
    std::fs::write(
        &delegates_path,
        format!(
            r"
pub fn delegate_effect(fs: HarnessFs, n: int) -> int {{
  fs.write_text({marker_literal}, to_string(n))
  return n
}}

pub fn delegate_entry(fs: HarnessFs, n: int) -> int {{
  return delegate_effect(fs, n)
}}
"
        ),
    )
    .expect("write delegates module");
    let module_path = dir.path().join("entry.harn");
    std::fs::write(
        &module_path,
        r#"
import { delegate_entry } from "./delegates"

pub fn handle(harness: Harness, _payload: dict) -> nil {
  delegate_entry(harness.fs, 7)
  return nil
}
"#,
    )
    .expect("write entry module");
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
    let payload = serde_json::json!({"session": {"id": "lazy-hook-transitive-test"}});
    // First fire loads the module graph into the child VM's cache.
    run_lifecycle_hooks_with_ctx(Some(&ctx), HookEvent::SessionStart, &payload)
        .await
        .expect("first fire resolves the transitive sibling");
    std::fs::write(&marker, "0").expect("reset marker");
    // Second fire hits the lazy cache with a fresh child VM: the transitive
    // sibling must still resolve.
    run_lifecycle_hooks_with_ctx(Some(&ctx), HookEvent::SessionStart, &payload)
        .await
        .expect("second fire resolves the transitive sibling");

    assert_eq!(std::fs::read_to_string(marker).expect("marker"), "7");
    clear_runtime_hooks();
}
