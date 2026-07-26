//! Registered provider and session-hook closures under execution policy.
//!
//! A registered closure evaluates under the policy, while a bridged builtin
//! reached from outside one is still rejected. The scope-pin cases matter most:
//! after the VM that registered a closure is dropped, the closure must still
//! resolve its sibling functions, and a genuinely unknown name must still fall
//! through to builtin/host-bridge dispatch instead of resolving in-VM.

use crate::orchestration::*;
use std::sync::Arc;
/// Run `source` through a real VM under an active execution policy. The VM
/// exposes:
///   * `__probe_bridge_gate()` — a stand-in for an app-provided bridged
///     builtin. It runs the exact gate the model's bridged-builtin calls hit
///     at `dispatch.rs` (`enforce_current_policy_for_bridge_builtin`) using the
///     real Burin reminder builtin name from the bug, and counts each call.
///   * `__test_fire_reminders(session_id)` — drives the reminder-provider
///     evaluation seam (`evaluate_and_inject` -> `evaluate_vm_provider`).
///
/// Returns the script result (Ok output / Err message) and the probe count.
fn run_registered_closure_probe(source: &str) -> (Result<String, String>, usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();

    let probe_calls = Arc::new(AtomicUsize::new(0));
    let probe_for_rt = Arc::clone(&probe_calls);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async move {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let chunk = crate::compile_source(source)?;
                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);

                let probe = Arc::clone(&probe_for_rt);
                vm.register_builtin("__probe_bridge_gate", move |_args, _out| {
                    probe.fetch_add(1, Ordering::SeqCst);
                    crate::orchestration::enforce_current_policy_for_bridge_builtin(
                        "evaluate_burin_user_reminder_rules",
                    )?;
                    Ok(crate::value::VmValue::String(arcstr::ArcStr::from("ok")))
                });

                vm.register_async_builtin("__test_fire_reminders", |ctx, args| async move {
                    let session_id = args
                        .first()
                        .map(crate::value::VmValue::display)
                        .unwrap_or_default();
                    let report = crate::llm::reminder_providers::evaluate_and_inject(
                        Some(&ctx),
                        crate::orchestration::HookEvent::SessionStart,
                        &session_id,
                        serde_json::json!({}),
                        serde_json::json!({}),
                    )
                    .await?;
                    Ok(crate::json_to_vm_value(&report))
                });

                push_execution_policy(CapabilityPolicy {
                    tools: vec!["read".to_string()],
                    ..Default::default()
                });
                let outcome = vm.execute(&chunk).await;
                pop_execution_policy();
                outcome
                    .map(|_| vm.output().to_string())
                    .map_err(|error| error.to_string())
            })
            .await
    });

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();
    (result, probe_calls.load(Ordering::SeqCst))
}

// A registered reminder provider AND a registered session hook, both whose
// bodies call a bridged builtin, must evaluate cleanly while the agent loop's
// execution policy is active. Before the trusted-bridge guard was added at the
// provider/hook invocation seams, the first such call died with
// `tool_rejected: ... exceeds execution policy`, taking down every turn.
#[test]
fn registered_provider_and_session_hook_evaluate_under_execution_policy() {
    let script = r#"pipeline main() {
  const session = agent_session_open("trusted-bridge-probe")
  agent_session_reset(session)
  register_reminder_provider({
    id: "probe-provider",
    subscribes_to: ["session_start"],
    evaluate: { ctx ->
      __probe_bridge_gate()
      return []
    },
  })
  register_session_hook("session_start", { _payload ->
    __probe_bridge_gate()
    return {control: "allow"}
  })
  __test_fire_reminders(session)
  __host_fire_session_hook("session_start", {session: {id: session}, event: "session_start"})
}"#;

    let (result, probe_calls) = run_registered_closure_probe(script);
    result.expect(
        "a registered reminder provider and session hook must evaluate under an active \
         execution policy without tripping the bridged-builtin gate",
    );
    assert_eq!(
        probe_calls, 2,
        "both the reminder-provider closure and the session-hook closure must have executed \
         the bridged-builtin probe exactly once each"
    );
}

// Negative control: a bridged builtin invoked OUTSIDE any registered
// provider/hook closure (i.e. at the top of the pipeline, the way a
// model-issued tool/builtin call reaches `dispatch.rs`) must STILL be rejected
// under the same policy. This proves the guard narrows trust to the runtime's
// own registered-closure seams rather than weakening the policy globally.
#[test]
fn bridged_builtin_outside_registered_closure_is_still_rejected_under_policy() {
    let script = r"pipeline main() {
  __probe_bridge_gate()
}";

    let (result, probe_calls) = run_registered_closure_probe(script);
    let error = result.expect_err(
        "a bridged builtin invoked outside a registered provider/hook closure must remain \
         rejected while an execution policy is active",
    );
    assert!(
        error.contains("exceeds execution policy"),
        "rejection must come from the execution-policy gate, got: {error}"
    );
    assert_eq!(
        probe_calls, 1,
        "the probe should have run once and been rejected by the gate"
    );
}

// --- Sibling-fn resolution survives the registering VM's teardown -----------
//
// A registered provider/hook closure is stored in a process/thread-local
// registry (`USER_PROVIDERS`, the session-hook table) that OUTLIVES the VM that
// registered it. Its body may call a sibling `pub fn` defined in the SAME
// pipeline module — exactly how Burin wires
// `register_reminder_provider({ evaluate: { ctx -> evaluate_burin_user_reminder_rules(ctx) } })`
// inside `build_loop_options_base`.
//
// Sibling-fn resolution for a module closure goes through the module's function
// registry, which the closure holds only via a `Weak` (`VmClosure::module_functions`).
// The sole strong owner of that registry is the registering VM's `module_cache`.
// When Burin registers the provider during one agent-loop setup and the runtime
// later fires it from a *different* VM (fresh `module_cache`), the original
// registry has been dropped, the `Weak` is dead, and the sibling call falls
// through to host-bridge dispatch — dying with
// `host bridge tool 'evaluate_burin_user_reminder_rules' is not implemented`.
// harn#4113 fixed the *policy* rejection at the same seam; this is the failure
// that "moved" behind it: a name-resolution misdispatch, not a policy trip.
//
// The reproduction registers the closure in a disposable VM, drops that VM
// (releasing its `module_cache`), then fires the provider/hook from a fresh VM
// — the runtime's real invocation path (`evaluate_and_inject`, child VM).

// The module both tests register from: a sibling `pub fn` the registered
// closure calls, plus the `pub fn` that performs the registration.
const REGISTERED_CLOSURE_MODULE: &str = r#"pub fn compute_provider_reminders(ctx) {
  return []
}

pub fn session_hook_decision(payload) {
  return {control: "allow"}
}

pub fn register_provider_closure() {
  register_reminder_provider({
    id: "fn-provider",
    subscribes_to: ["session_start"],
    evaluate: { ctx -> return compute_provider_reminders(ctx) },
  })
  return nil
}

pub fn register_hook_closure() {
  register_session_hook("session_start", { payload ->
    return session_hook_decision(payload)
  })
  return nil
}
"#;

/// Register a provider/hook by loading [`REGISTERED_CLOSURE_MODULE`] into a
/// throwaway VM, invoking `register_fn`, then dropping that VM so its
/// `module_cache` (the only strong owner of the module's function registry) is
/// released. Mirrors Burin registering a provider in one agent-loop VM whose
/// lifetime ends before the provider fires.
async fn register_from_disposable_vm(register_fn: &str) {
    let mut vm = crate::Vm::new();
    crate::stdlib::register_vm_stdlib(&mut vm);
    let exports = vm
        .load_module_exports_from_source(
            "orchestration/tests/registered_closure_module.harn",
            REGISTERED_CLOSURE_MODULE,
        )
        .await
        .expect("compile registered-closure module");
    let register = exports
        .get(register_fn)
        .unwrap_or_else(|| panic!("module must export {register_fn}"))
        .clone();
    vm.call_closure_pub(&register, &[])
        .await
        .expect("registration closure must run");
    // Drop the VM (and `exports`) so the module's function registry is released,
    // leaving the globally-retained closure's `Weak` dangling — the state Burin
    // is in when a later VM fires the provider/hook.
    drop(exports);
    drop(vm);
}

// A registered reminder provider whose `evaluate` closure calls a sibling
// module `pub fn` must still resolve that function when the runtime fires the
// provider from a VM other than the one that registered it. Before the fix the
// dead `Weak` made the call fall through to host-bridge dispatch, dying with
// `host bridge tool 'compute_provider_reminders' is not implemented`.
#[test]
fn registered_provider_closure_resolves_sibling_fn_after_registering_vm_dropped() {
    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                register_from_disposable_vm("register_provider_closure").await;

                // Fire from a fresh VM whose `module_cache` never loaded the
                // module — the runtime's real invocation path.
                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);
                vm.register_async_builtin("__test_fire_reminders", |ctx, _args| async move {
                    let report = crate::llm::reminder_providers::evaluate_and_inject(
                        Some(&ctx),
                        crate::orchestration::HookEvent::SessionStart,
                        "provider-fn-probe",
                        serde_json::json!({}),
                        serde_json::json!({}),
                    )
                    .await?;
                    Ok(crate::json_to_vm_value(&report))
                });
                let chunk = crate::compile_source(
                    r#"pipeline main() {
  const session = agent_session_open("provider-fn-probe")
  agent_session_reset(session)
  __test_fire_reminders()
}"#,
                )?;
                push_execution_policy(CapabilityPolicy {
                    tools: vec!["read".to_string()],
                    ..Default::default()
                });
                let outcome = vm.execute(&chunk).await;
                pop_execution_policy();
                outcome.map(|_| ()).map_err(|error| error.to_string())
            })
            .await
    });

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();
    result.expect(
        "a registered reminder provider's evaluate closure must resolve its sibling module \
         `pub fn` in-VM even after the registering VM is dropped, not fall through to \
         host-bridge dispatch",
    );
}

// Same invariant for a `register_session_hook` handler closure that calls a
// sibling module `pub fn`, fired from a VM other than the registering one.
#[test]
fn registered_session_hook_closure_resolves_sibling_fn_after_registering_vm_dropped() {
    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                register_from_disposable_vm("register_hook_closure").await;

                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);
                let chunk = crate::compile_source(
                    r#"pipeline main() {
  __host_fire_session_hook("session_start", {session: {id: "hook-fn-probe"}, event: "session_start"})
}"#,
                )?;
                push_execution_policy(CapabilityPolicy {
                    tools: vec!["read".to_string()],
                    ..Default::default()
                });
                let outcome = vm.execute(&chunk).await;
                pop_execution_policy();
                outcome.map(|_| ()).map_err(|error| error.to_string())
            })
            .await
    });

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();
    result.expect(
        "a registered session hook's handler closure must resolve its sibling module `pub fn` \
         in-VM even after the registering VM is dropped, not fall through to host-bridge \
         dispatch",
    );
}

// Negative control: pinning the module scope must NOT turn every unresolved
// name into an in-VM hit. A provider closure that calls a name which is neither
// a sibling module function nor a builtin must STILL fall through to
// builtin/host-bridge dispatch (here surfacing as "Undefined builtin", the
// no-host equivalent of the host's `-32601 host bridge tool not implemented`).
// This proves the fix narrows retention to the closure's real defining scope
// rather than blanket-swallowing unknown names.
#[test]
fn registered_provider_closure_unknown_name_still_falls_through_to_bridge() {
    const MODULE: &str = r#"pub fn register_unknown_call_provider() {
  register_reminder_provider({
    id: "unknown-call-provider",
    subscribes_to: ["session_start"],
    evaluate: { ctx -> return definitely_not_a_defined_name(ctx) },
  })
  return nil
}
"#;

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);
                let exports = vm
                    .load_module_exports_from_source(
                        "orchestration/tests/unknown_call_module.harn",
                        MODULE,
                    )
                    .await
                    .expect("compile unknown-call module");
                let register = exports
                    .get("register_unknown_call_provider")
                    .expect("module must export register fn")
                    .clone();
                vm.call_closure_pub(&register, &[])
                    .await
                    .expect("registration closure must run");
                drop(exports);
                drop(vm);

                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);
                vm.register_async_builtin("__test_fire_reminders", |ctx, _args| async move {
                    let report = crate::llm::reminder_providers::evaluate_and_inject(
                        Some(&ctx),
                        crate::orchestration::HookEvent::SessionStart,
                        "unknown-call-probe",
                        serde_json::json!({}),
                        serde_json::json!({}),
                    )
                    .await?;
                    Ok(crate::json_to_vm_value(&report))
                });
                let chunk = crate::compile_source(
                    r#"pipeline main() {
  const session = agent_session_open("unknown-call-probe")
  agent_session_reset(session)
  __test_fire_reminders()
}"#,
                )?;
                let outcome = vm.execute(&chunk).await;
                outcome.map(|_| ()).map_err(|error| error.to_string())
            })
            .await
    });

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();
    let error = result
        .expect_err("a genuinely unknown name must not resolve in-VM after the scope-pin fix");
    assert!(
        error.contains("definitely_not_a_defined_name"),
        "the unresolved name must still fall through to builtin/host-bridge dispatch, got: {error}"
    );
}
