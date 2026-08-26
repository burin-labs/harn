use super::*;

#[tokio::test]
async fn top_level_execution_owns_one_isolated_cancellation_registry() {
    crate::tool_call_cancellations::clear_registry_for_test();
    crate::llm::clear_current_host_bridge();
    let outer = crate::tool_call_cancellations::register("session", "call", "outer")
        .expect("outer registration");
    let bridge = std::sync::Arc::new(crate::bridge::HostBridge::from_parts_with_writer(
        std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        std::sync::Arc::new(|_| Ok(())),
        1,
    ));
    crate::llm::install_current_host_bridge(bridge.clone());
    let scope = AmbientExecutionScope::capture_for_top_level_execution(
        std::sync::Arc::from("isolated-execution"),
        LlmMockContext::default(),
        crate::stdlib::agents::agents_workers::fresh_worker_registry(),
        crate::stdlib::agents_daemon::fresh_daemon_registry(),
        crate::triggers::registry::runtime::fresh_trigger_registry(),
        crate::agent_sessions::fresh_session_runtime(),
        crate::tracing::fresh_tracing_runtime(),
        crate::llm::agent_session_host::fresh_agent_host_session_runtime(),
    );

    scope_ambient(scope, async {
        assert!(crate::tool_call_cancellations::lookup("session", "call").is_none());
        let inner = crate::tool_call_cancellations::register("session", "call", "inner")
            .expect("top-level registration");
        assert_eq!(
            bridge
                .tool_call_cancellation_registry()
                .cancel("session", "call", "host stop", false)
                .status,
            crate::tool_call_cancellations::CancelStatus::Cancelled
        );
        assert!(inner.0.is_cancelled());

        scope_ambient(AmbientExecutionScope::capture_for_inline_subtask(), async {
            assert_eq!(
                crate::tool_call_cancellations::lookup("session", "call")
                    .as_ref()
                    .map(|handle| handle.tool_name.as_str()),
                Some("inner")
            );
        })
        .await;
        drop(inner);
    })
    .await;

    assert_eq!(
        crate::tool_call_cancellations::lookup("session", "call")
            .as_ref()
            .map(|handle| handle.tool_name.as_str()),
        Some("outer")
    );
    drop(outer);
    assert!(crate::tool_call_cancellations::lookup("session", "call").is_none());
    crate::llm::clear_current_host_bridge();
}
