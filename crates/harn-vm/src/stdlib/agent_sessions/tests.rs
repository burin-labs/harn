use super::build_compact_config;
use crate::value::VmValue;

fn call_agent_session_builtin(name: &str) -> VmValue {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = crate::Vm::new();
                crate::register_vm_stdlib(&mut vm);
                vm.call_named_builtin(name, Vec::new())
                    .await
                    .expect("builtin call")
            })
            .await
    })
}

fn call_current_id_builtin() -> VmValue {
    call_agent_session_builtin("agent_session_current_id")
}

#[test]
fn current_id_returns_nil_outside_active_session() {
    crate::reset_thread_local_state();
    assert!(matches!(call_current_id_builtin(), VmValue::Nil));
}

#[test]
fn current_id_returns_active_session_id() {
    crate::reset_thread_local_state();
    crate::agent_sessions::push_current_session("unit-test-session".to_string());
    let current = call_current_id_builtin();
    crate::agent_sessions::pop_current_session();
    assert!(matches!(current, VmValue::String(value) if value.as_str() == "unit-test-session"));
}

fn call_builtin_with_args(name: &str, args: Vec<VmValue>) -> VmValue {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = crate::Vm::new();
                crate::register_vm_stdlib(&mut vm);
                vm.call_named_builtin(name, args)
                    .await
                    .expect("builtin call")
            })
            .await
    })
}

#[test]
fn record_changed_path_builtin_attributes_to_current_session() {
    // The receipt's `files_written` drains `take_session_changed_paths`, and
    // host-side edits (which bypass the hostlib write chokepoint) must reach
    // that store through this builtin or they vanish from the receipt.
    crate::reset_thread_local_state();
    let session = "record-changed-path-session";
    crate::agent_sessions::clear_session_changed_paths(session);
    crate::agent_sessions::push_current_session(session.to_string());
    let recorded = call_builtin_with_args(
        "agent_session_record_changed_path",
        vec![VmValue::String(arcstr::ArcStr::from(
            "test/users.integration.test.ts",
        ))],
    );
    crate::agent_sessions::pop_current_session();
    assert!(matches!(recorded, VmValue::Bool(true)));
    assert_eq!(
        crate::agent_sessions::take_session_changed_paths(session),
        vec!["test/users.integration.test.ts".to_string()],
        "the builtin must record under the active session so the receipt drains it"
    );
}

#[test]
fn record_changed_path_builtin_no_session_records_nothing() {
    // Outside any active session (and with no explicit id), there is nothing
    // to attribute to: the builtin must be a no-op returning false, never
    // recording under an empty key.
    crate::reset_thread_local_state();
    let recorded = call_builtin_with_args(
        "agent_session_record_changed_path",
        vec![VmValue::String(arcstr::ArcStr::from("test/orphan.ts"))],
    );
    assert!(matches!(recorded, VmValue::Bool(false)));
    assert!(crate::agent_sessions::take_session_changed_paths("").is_empty());
}

#[test]
fn record_changed_path_builtin_honors_explicit_session_argument() {
    crate::reset_thread_local_state();
    let session = "record-changed-path-explicit";
    crate::agent_sessions::clear_session_changed_paths(session);
    let recorded = call_builtin_with_args(
        "agent_session_record_changed_path",
        vec![
            VmValue::String(arcstr::ArcStr::from("src/orders.ts")),
            VmValue::String(arcstr::ArcStr::from(session)),
        ],
    );
    assert!(matches!(recorded, VmValue::Bool(true)));
    assert_eq!(
        crate::agent_sessions::take_session_changed_paths(session),
        vec!["src/orders.ts".to_string()]
    );
}

#[test]
fn actor_chain_returns_current_session_chain() {
    crate::reset_thread_local_state();
    let chain = crate::ActorChain::new("user:kenneth").pushed("agent:root");
    let id = crate::agent_sessions::open_or_create_with_actor_chain(
        Some("actor-chain-current".to_string()),
        Some(chain.clone()),
    );
    crate::agent_sessions::push_current_session(id);
    let current = call_agent_session_builtin("agent_session_actor_chain");
    crate::agent_sessions::pop_current_session();
    assert_eq!(
        crate::llm::helpers::vm_value_to_json(&current),
        chain.to_json_value()
    );
}

#[test]
fn compact_config_rejects_negative_numeric_options() {
    for key in [
        "keep_last",
        "token_threshold",
        "tool_output_max_chars",
        "hard_limit_tokens",
    ] {
        let mut opts = crate::value::DictMap::new();
        opts.insert(crate::value::intern_key(key), VmValue::Int(-1));
        let err = build_compact_config(&opts).expect_err("negative option must fail");
        assert!(err.to_string().contains(key), "{err}");
    }
}
