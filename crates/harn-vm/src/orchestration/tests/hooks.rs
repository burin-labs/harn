//! Tool and lifecycle hooks around a call.
//!
//! A pre-tool hook can deny, pass through, or rewrite the arguments; a post-tool
//! hook can rewrite the result; a hook whose pattern does not match must not
//! fire. Lifecycle hook patterns are matched against payload shapes, an
//! `on_finish` callback can replace the pipeline return value, and both event
//! families round-trip through the session-hook parser.

use crate::orchestration::*;
use std::sync::Arc;
#[tokio::test(flavor = "current_thread")]
async fn pre_tool_hook_deny_blocks_execution() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "dangerous_*".to_string(),
        pre: Some(Arc::new(|_name, _args| {
            PreToolAction::Deny("blocked by policy".to_string())
        })),
        post: None,
    });
    let result = run_pre_tool_hooks("dangerous_delete", &serde_json::json!({}))
        .await
        .expect("hook result");
    clear_tool_hooks();
    assert!(matches!(result, PreToolAction::Deny(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn pre_tool_hook_allow_passes_through() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "safe_*".to_string(),
        pre: Some(Arc::new(|_name, _args| PreToolAction::Allow)),
        post: None,
    });
    let result = run_pre_tool_hooks("safe_read", &serde_json::json!({}))
        .await
        .expect("hook result");
    clear_tool_hooks();
    assert!(matches!(result, PreToolAction::Allow));
}

#[tokio::test(flavor = "current_thread")]
async fn pre_tool_hook_modify_rewrites_args() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "*".to_string(),
        pre: Some(Arc::new(|_name, _args| {
            PreToolAction::Modify(serde_json::json!({"path": "/sanitized"}))
        })),
        post: None,
    });
    let result = run_pre_tool_hooks("read_file", &serde_json::json!({"path": "/etc/passwd"}))
        .await
        .expect("hook result");
    clear_tool_hooks();
    match result {
        PreToolAction::Modify(args) => assert_eq!(args["path"], "/sanitized"),
        _ => panic!("expected Modify"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn post_tool_hook_modifies_result() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "exec".to_string(),
        pre: None,
        post: Some(Arc::new(|_name, result: &str| {
            if result.contains("SECRET") {
                PostToolAction::Modify("[REDACTED]".to_string())
            } else {
                PostToolAction::Pass
            }
        })),
    });
    let result = run_post_tool_hooks("exec", &serde_json::json!({}), "output with SECRET data")
        .await
        .expect("hook result");
    let clean = run_post_tool_hooks("exec", &serde_json::json!({}), "clean output")
        .await
        .expect("hook result");
    clear_tool_hooks();
    assert_eq!(result.text, "[REDACTED]");
    assert_eq!(result.dropped_bytes, 0);
    assert_eq!(clean.text, "clean output");
    assert_eq!(clean.dropped_bytes, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn post_tool_hook_preserves_explicit_truncation_across_later_appends() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "exec".to_string(),
        pre: None,
        post: Some(Arc::new(|_name, _result: &str| PostToolAction::Truncate {
            result: "short".to_string(),
            dropped_bytes: 7,
        })),
    });
    register_tool_hook(ToolHook {
        pattern: "exec".to_string(),
        pre: None,
        post: Some(Arc::new(|_name, result: &str| {
            PostToolAction::Modify(format!("{result} +seven"))
        })),
    });

    let result = run_post_tool_hooks("exec", &serde_json::json!({}), "twelve bytes")
        .await
        .expect("hook result");
    clear_tool_hooks();

    assert_eq!(result.text, "short +seven");
    assert_eq!(
        result.text.len(),
        "twelve bytes".len(),
        "the regression requires a zero net length change"
    );
    assert_eq!(
        result.dropped_bytes, 7,
        "later appended text must not erase the earlier truncation fact"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unmatched_hook_pattern_does_not_fire() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "exec".to_string(),
        pre: Some(Arc::new(|_name, _args| {
            PreToolAction::Deny("should not match".to_string())
        })),
        post: None,
    });
    let result = run_pre_tool_hooks("read_file", &serde_json::json!({}))
        .await
        .expect("hook result");
    clear_tool_hooks();
    assert!(matches!(result, PreToolAction::Allow));
}

#[tokio::test(flavor = "current_thread")]
async fn lifecycle_hook_patterns_match_payload_shapes() {
    clear_runtime_hooks();

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let exports = vm
        .load_module_exports_from_source(
            "orchestration/tests/noop_hook.harn",
            "pub fn noop(event) { return nil }\n",
        )
        .await
        .expect("compile noop hook");
    let closure = exports.get("noop").expect("noop export").clone();

    for pattern in [
        "trigger.script_*",
        "trigger.provider == 'cron'",
        "trigger.kind =~ '^schedule'",
        "trigger.provider != 'webhook'",
        "script.path",
        "trigger.kind =~ '['",
    ] {
        register_vm_hook(
            HookEvent::PreAgentTurn,
            pattern,
            format!("test::{pattern}"),
            std::sync::Arc::clone(&closure),
        );
    }

    let payload = serde_json::json!({
        "target": "trigger.script_001",
        "trigger": {
            "provider": "cron",
            "kind": "schedule.tick",
        },
        "script": {
            "path": "scripts/bench_001.harn",
        },
    });

    assert_eq!(
        matching_vm_lifecycle_hooks(HookEvent::PreAgentTurn, &payload).len(),
        5,
        "valid glob, equality, regex, inequality, and truthy-path patterns should match"
    );
    clear_runtime_hooks();
}

#[tokio::test(flavor = "current_thread")]
async fn pipeline_on_finish_callback_replaces_return_value() {
    crate::reset_thread_local_state();

    let local = tokio::task::LocalSet::new();
    let value = local
        .run_until(async {
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            let chunk = crate::compile_source(
                "pipeline default(harness: Harness) { harness.agent.pipeline_on_finish({ _h, v -> v + 100 }); return 7 }",
            )
            .expect("compile");
            vm.execute(&chunk).await.expect("execute")
        })
        .await;

    match value {
        VmValue::Int(n) => assert_eq!(n, 107, "on_finish callback should add 100 to 7"),
        other => panic!("expected Int, got {}", other.type_name()),
    }

    // One-shot consumption: a second execute without re-registration must
    // not re-invoke the previous callback.
    let next = local
        .run_until(async {
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            let chunk = crate::compile_source("pipeline default(harness: Harness) { return 7 }")
                .expect("compile");
            vm.execute(&chunk).await.expect("execute")
        })
        .await;
    match next {
        VmValue::Int(n) => assert_eq!(n, 7, "no callback registered → value passes through"),
        other => panic!("expected Int, got {}", other.type_name()),
    }
}

#[test]
fn pipeline_finish_events_round_trip_through_session_hook_parser() {
    for (input, expected) in [
        ("pre_finish", HookEvent::PreFinish),
        ("PreFinish", HookEvent::PreFinish),
        ("post_finish", HookEvent::PostFinish),
        ("PostFinish", HookEvent::PostFinish),
        ("on_unsettled_detected", HookEvent::OnUnsettledDetected),
        ("OnUnsettledDetected", HookEvent::OnUnsettledDetected),
    ] {
        let parsed = HookEvent::parse_session_event(input)
            .unwrap_or_else(|err| panic!("{input} should parse as a session event: {err}"));
        assert_eq!(parsed, expected, "{input} should map to {expected:?}");
    }
}

#[test]
fn lifecycle_hook_events_round_trip_through_session_hook_parser() {
    // harn#1859: the 7 new lifecycle events (suspend, resume, drain
    // phases) round-trip through the session-hook parser using both
    // their canonical PascalCase and Harn-facing snake_case spellings.
    for (input, expected) in [
        ("pre_suspend", HookEvent::PreSuspend),
        ("PreSuspend", HookEvent::PreSuspend),
        ("post_suspend", HookEvent::PostSuspend),
        ("PostSuspend", HookEvent::PostSuspend),
        ("pre_resume", HookEvent::PreResume),
        ("PreResume", HookEvent::PreResume),
        ("post_resume", HookEvent::PostResume),
        ("PostResume", HookEvent::PostResume),
        ("pre_drain", HookEvent::PreDrain),
        ("PreDrain", HookEvent::PreDrain),
        ("post_drain", HookEvent::PostDrain),
        ("PostDrain", HookEvent::PostDrain),
        ("on_drain_decision", HookEvent::OnDrainDecision),
        ("OnDrainDecision", HookEvent::OnDrainDecision),
    ] {
        let parsed = HookEvent::parse_session_event(input)
            .unwrap_or_else(|err| panic!("{input} should parse as a session event: {err}"));
        assert_eq!(parsed, expected, "{input} should map to {expected:?}");
    }
}
