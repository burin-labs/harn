//! Reach the canonical consent dispatcher and actual prepared process effect.

use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc, Mutex as StdMutex};

use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::bridge::HostBridge;
use crate::tool_registry::preparation_scope::{current_invocation, enforce_contract, preparation};
use crate::value::VmValue;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_verify_child_interpreter_preserves_read_only_restriction() {
    // Ordinary execution policy can be bypassed by trusted host adapters. The
    // mandatory preparation restriction must survive the real worker boundary
    // even when no ordinary policy is installed in this control.
    assert!(enforce_contract("unknown_host_effect", None).is_ok());
    for independent in [false, true] {
        let result = preparation(async {
            let registry = Arc::new(crate::stdlib::pool::PoolRegistry::default());
            let future = async { enforce_contract("unknown_host_effect", None) };
            let child = if independent {
                crate::vm::subtask::spawn_inherited_child(registry, future)
            } else {
                crate::vm::subtask::spawn_child(registry, future)
            };
            child.await.unwrap()
        })
        .await;
        assert!(result.is_err(), "child must retain preparation restriction");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_verify_independent_session_cannot_reuse_invocation() {
    let fixture = Fixture::new();
    let (ctx, registry) = fixture.registry(false).await;
    let binding = super::prepare(&ctx, Some(&registry), "verify", &json!({}), "test-session")
        .await
        .unwrap()
        .unwrap();
    super::scope(Some(binding), async {
        assert!(current_invocation().is_some());
        assert!(
            super::validate_handler(Some(&ctx), Some(&registry), "different", &json!({}))
                .await
                .is_err()
        );
        let child = crate::vm::subtask::spawn_inherited_child(
            Arc::new(crate::stdlib::pool::PoolRegistry::default()),
            async { current_invocation() },
        );
        assert!(child.await.unwrap().is_none());
    })
    .await;
}

struct BridgeGuard(Option<Arc<HostBridge>>);
impl Drop for BridgeGuard {
    fn drop(&mut self) {
        crate::llm::swap_current_host_bridge(self.0.take());
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    root: std::path::PathBuf,
    other: std::path::PathBuf,
    facts_path: std::path::PathBuf,
    facts: Value,
    policy: Option<crate::orchestration::CapabilityPolicy>,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let root = directory.path().join("approved");
        let other = directory.path().join("replacement");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&other).unwrap();
        let facts_path = directory.path().join("current-verifier.json");
        let facts = json!({"operation": {"command": "printf verified > reached; pwd", "cwd": root}, "goal": "original"});
        std::fs::write(&facts_path, serde_json::to_vec(&facts).unwrap()).unwrap();
        Self {
            _directory: directory,
            root,
            other,
            facts_path,
            facts,
            policy: None,
        }
    }

    async fn registry(&self, preparing_effect: bool) -> (crate::vm::AsyncBuiltinCtx, VmValue) {
        let path = serde_json::to_string(&self.facts_path).unwrap();
        let preparation = if preparing_effect {
            format!("harness.process.run({{program: \"sh\", args: [\"-c\", \"touch reached\"], cwd: {}}})", serde_json::to_string(&self.root).unwrap())
        } else {
            format!("json_parse(harness.fs.read_text({path}))")
        };
        let source = include_str!("fixtures/prepared_verify.harn.template")
            .replace("{{preparation}}", &preparation);
        let chunk = crate::compile_source(&source).expect("compile preparation fixture");
        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        vm.set_harness(crate::Harness::real());
        let registry = vm.execute(&chunk).await.expect("register prepared tool");
        (crate::vm::AsyncBuiltinCtx::for_test(vm), registry)
    }

    fn no_effect(&self) {
        assert!(!self.root.join("reached").exists());
        assert!(!self.other.join("reached").exists());
    }
}

async fn dispatch(
    fixture: &Fixture,
    response: Value,
    replacement: Option<Value>,
    arguments: Value,
    preparing_effect: bool,
) -> (Value, Vec<Value>) {
    crate::reset_thread_local_state();
    let (ctx, registry) = fixture.registry(preparing_effect).await;
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let observed = captured.clone();
    let pending = Arc::new(Mutex::new(HashMap::<
        u64,
        tokio::sync::oneshot::Sender<Value>,
    >::new()));
    let response_pending = pending.clone();
    let facts_path = fixture.facts_path.clone();
    let writer = Arc::new(move |line: &str| {
        let request: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
        observed.lock().unwrap().push(request.clone());
        assert_eq!(request["method"], "session/request_permission");
        if let Some(replacement) = &replacement {
            std::fs::write(&facts_path, serde_json::to_vec(replacement).unwrap()).unwrap();
        }
        let id = request["id"].as_u64().unwrap();
        response_pending
            .try_lock()
            .unwrap()
            .remove(&id)
            .unwrap()
            .send(json!({"jsonrpc": "2.0", "id": id, "result": response}))
            .map_err(|_| "permission caller dropped".to_string())
    });
    let bridge = Arc::new(HostBridge::from_parts_with_writer(
        pending,
        Arc::new(AtomicBool::new(false)),
        writer,
        1,
    ));
    let _bridge = BridgeGuard(crate::llm::swap_current_host_bridge(Some(bridge)));
    let policy = serde_json::from_value(
        json!({"rules": [{"ask": {"tool": "verify"}, "reason": "verify requires consent"}]}),
    )
    .unwrap();
    let call = crate::stdlib::json_to_vm_value(
        &json!({"name": "verify", "id": "prepared-verify", "arguments": arguments}),
    );
    let options =
        crate::stdlib::json_to_vm_value(&json!({"tool_retries": 1, "tool_backoff_ms": 1}));
    let permission = async {
        crate::orchestration::scope_approval_policy(
            policy,
            super::super::agent_host_primitives::host_agent_dispatch_tool_call(
                ctx,
                call,
                Some(&registry),
                options.as_dict().unwrap(),
            ),
        )
        .await
    };
    let outcome = if let Some(policy) = &fixture.policy {
        crate::orchestration::scope_execution_policy(policy.clone(), permission).await
    } else {
        permission.await
    }
    .expect("dispatch returns a tool outcome");
    let requests = captured.lock().unwrap().clone();
    (crate::llm::vm_value_to_json(&outcome), requests)
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_verify_consent_matches_the_actual_process_command_and_root() {
    let fixture = Fixture::new();
    let (outcome, requests) = dispatch(
        &fixture,
        crate::llm::acp_permission::allow_response(),
        None,
        json!({}),
        false,
    )
    .await;
    assert_eq!(requests.len(), 1, "{outcome}");
    assert_eq!(
        requests[0]["params"]["toolCall"]["rawInput"],
        fixture.facts["operation"]
    );
    assert_eq!(outcome["ok"], true, "{outcome}");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("reached")).unwrap(),
        "verified"
    );
    assert!(!fixture.other.join("reached").exists());
    assert!(outcome["rendered_result"]
        .as_str()
        .unwrap()
        .contains(fixture.root.to_str().unwrap()));
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_verify_refusal_runs_no_process() {
    let fixture = Fixture::new();
    let (outcome, requests) = dispatch(
        &fixture,
        crate::llm::acp_permission::reject_response(Some("declined".into())),
        None,
        json!({}),
        false,
    )
    .await;
    assert_eq!(requests.len(), 1, "{outcome}");
    assert_eq!(outcome["ok"], false);
    fixture.no_effect();
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_verify_rejects_changed_goal_command_or_root_after_consent() {
    for key in ["goal", "command", "cwd"] {
        let fixture = Fixture::new();
        let mut changed = fixture.facts.clone();
        match key {
            "goal" => changed["goal"] = json!("replacement"),
            "command" => changed["operation"]["command"] = json!("touch reached"),
            _ => changed["operation"]["cwd"] = json!(fixture.other),
        }
        let (outcome, requests) = dispatch(
            &fixture,
            crate::llm::acp_permission::allow_response(),
            Some(changed),
            json!({}),
            false,
        )
        .await;
        assert_eq!(requests.len(), 1, "{key}: {outcome}");
        assert_eq!(
            requests[0]["params"]["toolCall"]["rawInput"],
            fixture.facts["operation"]
        );
        assert_eq!(outcome["ok"], false, "{key}: {outcome}");
        fixture.no_effect();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_verify_refuses_model_execution_facts_and_preparation_effects() {
    for (arguments, preparing_effect) in [
        (json!({"command": "touch reached"}), false),
        (json!({"cwd": "/"}), false),
        (json!({}), true),
    ] {
        let fixture = Fixture::new();
        let (outcome, requests) = dispatch(
            &fixture,
            crate::llm::acp_permission::allow_response(),
            None,
            arguments,
            preparing_effect,
        )
        .await;
        assert!(
            requests.is_empty(),
            "refusal must precede consent: {requests:?}"
        );
        assert_eq!(outcome["ok"], false);
        fixture.no_effect();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_verify_retries_unchanged_facts_but_refuses_changed_facts() {
    for change_goal in [false, true] {
        let mut fixture = Fixture::new();
        let change = if change_goal {
            let mut changed = fixture.facts.clone();
            changed["goal"] = json!("replacement");
            format!(
                "printf '%s' '{}' > '{}' ; ",
                serde_json::to_string(&changed).unwrap(),
                fixture.facts_path.display()
            )
        } else {
            String::new()
        };
        fixture.facts["operation"]["command"] = json!(format!(
            "printf attempt >> attempts; if [ ! -e retried ]; then touch retried; {change}exit 1; fi; printf verified > reached; pwd"
        ));
        std::fs::write(
            &fixture.facts_path,
            serde_json::to_vec(&fixture.facts).unwrap(),
        )
        .unwrap();
        let (outcome, requests) = dispatch(
            &fixture,
            crate::llm::acp_permission::allow_response(),
            None,
            json!({}),
            false,
        )
        .await;
        assert_eq!(requests.len(), 1, "{outcome}");
        assert_eq!(outcome["ok"], !change_goal, "{outcome}");
        let attempts = std::fs::read_to_string(fixture.root.join("attempts")).unwrap();
        assert_eq!(
            attempts,
            if change_goal {
                "attempt"
            } else {
                "attemptattempt"
            }
        );
        assert_eq!(fixture.root.join("reached").exists(), !change_goal);
        assert!(!fixture.other.join("reached").exists());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_verify_keeps_parent_workspace_scope_and_refuses_approval_rewrites() {
    let mut fixture = Fixture::new();
    fixture.policy = Some(
        serde_json::from_value(json!({
            "workspace_roots": [fixture.other],
            "capabilities": {"workspace": ["read_text"]}
        }))
        .unwrap(),
    );
    let (outcome, requests) = dispatch(
        &fixture,
        crate::llm::acp_permission::allow_response(),
        None,
        json!({}),
        false,
    )
    .await;
    assert_eq!(outcome["ok"], false, "{outcome}");
    assert!(requests.is_empty(), "parent refusal precedes consent");
    fixture.no_effect();

    let fixture = Fixture::new();
    let mut response = crate::llm::acp_permission::allow_response();
    response["args"] = json!({"reason": "rewritten after preparation"});
    let (outcome, requests) = dispatch(&fixture, response, None, json!({}), false).await;
    assert_eq!(requests.len(), 1, "{outcome}");
    assert_eq!(outcome["ok"], false, "{outcome}");
    fixture.no_effect();
}
