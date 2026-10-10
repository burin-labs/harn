use super::arguments::lift_flat_single_object_arg;
use super::error_classification::budget_category_from_error;
use super::*;
use harn_vm::VmValue;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct TrackingReplayCache {
    inner: InMemoryReplayCache,
    gets: AtomicUsize,
    puts: AtomicUsize,
}

impl TrackingReplayCache {
    fn counts(&self) -> (usize, usize) {
        (
            self.gets.load(Ordering::SeqCst),
            self.puts.load(Ordering::SeqCst),
        )
    }
}

#[async_trait]
impl ReplayCache for TrackingReplayCache {
    async fn get(&self, key: &ReplayKey) -> Result<Option<ReplayCacheEntry>, DispatchError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(key).await
    }

    async fn put(&self, key: ReplayKey, value: ReplayCacheEntry) -> Result<(), DispatchError> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        self.inner.put(key, value).await
    }
}

struct CountingVmConfigurator {
    calls: Arc<AtomicUsize>,
}

impl VmConfigurator for CountingVmConfigurator {
    fn configure(&self, vm: &mut Vm) -> Result<(), DispatchError> {
        let calls = self.calls.clone();
        vm.register_builtin("test_increment_call_count", move |_args, _output| {
            let count = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(VmValue::Int(
                count.try_into().expect("test call count fits in i64"),
            ))
        });
        Ok(())
    }
}

fn replay_test_fixture() -> (
    tempfile::TempDir,
    DispatchCore,
    Arc<AtomicUsize>,
    Arc<TrackingReplayCache>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn observe_execution() -> int {
  return test_increment_call_count()
}
",
    )
    .expect("write script");

    let calls = Arc::new(AtomicUsize::new(0));
    let cache = Arc::new(TrackingReplayCache::default());
    let mut config = DispatchCoreConfig::for_script(&script);
    config.replay_cache = cache.clone();
    config.vm_configurator = Arc::new(CountingVmConfigurator {
        calls: calls.clone(),
    });
    let core = DispatchCore::new(config).expect("core");
    (dir, core, calls, cache)
}

fn replay_test_request(replay_key: Option<&str>) -> CallRequest {
    CallRequest {
        adapter: "mcp".to_string(),
        function: "observe_execution".to_string(),
        arguments: CallArguments::Named(BTreeMap::new()),
        auth: AuthRequest::default(),
        caller: "tester".to_string(),
        replay_key: replay_key.map(str::to_string),
        trace_id: None,
        parent_span_id: None,
        metadata: BTreeMap::new(),
        cancel_token: None,
        agent_session_id: None,
        agent_event_sink: None,
        actor_chain: None,
        actor_chain_hop: None,
        progress: None,
        tenant_id: None,
        request_id: None,
        auth_context: None,
        auth_principal: None,
    }
}

#[tokio::test]
async fn dispatch_executes_exported_function() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn greet(name: string) -> string {
  return name
}
",
    )
    .expect("write script");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let response = core
        .dispatch(CallRequest {
            adapter: "mcp".to_string(),
            function: "greet".to_string(),
            arguments: CallArguments::Named(BTreeMap::from([(
                "name".to_string(),
                serde_json::json!("alice"),
            )])),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect("dispatch");

    assert_eq!(response.value, serde_json::json!("alice"));
    assert!(!response.cached);
}

/// harn#5039: a single-object-param tool (the `pub fn tool(params: {..})`
/// convention) whose MCP `inputSchema` nests fields under `params` must
/// still bind when a client emits those fields FLAT at the top level, the
/// exact 6/6-failing shape the local model produced against
/// `burin-harness-debugger` (`{events_dir: ".."}` rather than
/// `{params: {events_dir: ".."}}`). The flat keys must reach the parameter
/// instead of being dropped so the tool falls back to its default.
async fn dispatch_echo_params(arguments: CallArguments) -> serde_json::Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn stage_triage(
  params: {events_dir: string, verdict_path?: string} = {events_dir: ""},
) -> dict {
  return params
}
"#,
    )
    .expect("write script");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    core.dispatch(CallRequest {
        adapter: "mcp".to_string(),
        function: "stage_triage".to_string(),
        arguments,
        auth: AuthRequest::default(),
        caller: "tester".to_string(),
        replay_key: None,
        trace_id: None,
        parent_span_id: None,
        metadata: BTreeMap::new(),
        cancel_token: None,
        agent_session_id: None,
        agent_event_sink: None,
        actor_chain: None,
        actor_chain_hop: None,
        progress: None,
        tenant_id: None,
        request_id: None,
        auth_context: None,
        auth_principal: None,
    })
    .await
    .expect("dispatch")
    .value
}

#[tokio::test]
async fn flat_single_object_arg_binds_like_nested_5039() {
    // FLAT emission (local-model shape) now reaches the `params` parameter.
    let flat = dispatch_echo_params(CallArguments::Named(BTreeMap::from([(
        "events_dir".to_string(),
        serde_json::json!("/runs/20260717-175909"),
    )])))
    .await;
    assert_eq!(
        flat,
        serde_json::json!({ "events_dir": "/runs/20260717-175909" }),
        "flat top-level args must be lifted into the single object parameter",
    );

    // Correctly-nested emission (Claude shape) is untouched — idempotent.
    let nested = dispatch_echo_params(CallArguments::Named(BTreeMap::from([(
        "params".to_string(),
        serde_json::json!({ "events_dir": "/runs/nested" }),
    )])))
    .await;
    assert_eq!(
        nested,
        serde_json::json!({ "events_dir": "/runs/nested" }),
        "a correctly-nested call must not be double-lifted",
    );

    // No arguments falls back to the declared default (no spurious lift).
    let empty = dispatch_echo_params(CallArguments::Named(BTreeMap::new())).await;
    assert_eq!(empty, serde_json::json!({ "events_dir": "" }));
}

#[test]
fn flat_lift_is_scoped_to_single_object_param() {
    use crate::ExportedParam;

    let obj_param = |name: &str| ExportedParam {
        name: name.to_string(),
        type_expr: None,
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        has_default: true,
        rest: false,
    };
    let scalar_param = |name: &str| ExportedParam {
        name: name.to_string(),
        type_expr: None,
        input_schema: serde_json::json!({ "type": "string" }),
        has_default: false,
        rest: false,
    };
    let flat = BTreeMap::from([("events_dir".to_string(), serde_json::json!("x"))]);

    // Single object param, flat args, param name absent -> lift.
    let params = [obj_param("params")];
    let lifted = lift_flat_single_object_arg(&params, &flat).expect("lift");
    assert_eq!(lifted["params"], serde_json::json!({ "events_dir": "x" }));

    // Param name already present -> no lift (idempotent).
    let nested = BTreeMap::from([("params".to_string(), serde_json::json!({ "a": 1 }))]);
    assert!(lift_flat_single_object_arg(&params, &nested).is_none());

    // Scalar single param -> no lift (preserves clear "missing required arg").
    assert!(lift_flat_single_object_arg(&[scalar_param("name")], &flat).is_none());

    // Multiple params -> no lift (normal named binding).
    assert!(lift_flat_single_object_arg(&[obj_param("a"), obj_param("b")], &flat).is_none());

    // Empty args -> no lift (default applies).
    assert!(lift_flat_single_object_arg(&params, &BTreeMap::new()).is_none());
}

#[cfg(feature = "hostlib")]
#[tokio::test]
async fn dispatch_exported_function_can_use_deterministic_tools_hostlib() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
import { command_run } from "std/command"

pub fn run_help(harness: Harness, binary: string) -> int {
  const result = command_run(
    harness.tools,
    {argv: [binary, "--help"]},
    {capture: {max_inline_bytes: 256}, timeout_ms: 5000},
  )
  return result.exit_code
}
"#,
    )
    .expect("write script");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let response = core
        .dispatch(CallRequest {
            adapter: "mcp".to_string(),
            function: "run_help".to_string(),
            arguments: CallArguments::Named(BTreeMap::from([(
                "binary".to_string(),
                serde_json::json!(std::env::current_exe()
                    .expect("current executable")
                    .to_string_lossy()),
            )])),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect("dispatch");

    assert_eq!(response.value, serde_json::json!(0));
}

#[tokio::test]
async fn dispatch_executes_legacy_pipeline_when_no_public_exports() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pipeline default(harness: Harness, task: unknown) {
  harness.stdio.println(json_stringify({task: task}))
}
",
    )
    .expect("write script");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let response = core
        .dispatch(CallRequest {
            adapter: "a2a".to_string(),
            function: "default".to_string(),
            arguments: CallArguments::Named(BTreeMap::from([(
                "task".to_string(),
                serde_json::json!("payload"),
            )])),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect("dispatch");

    assert_eq!(
        response.value,
        serde_json::json!("{\"task\":\"payload\"}\n")
    );
    assert_eq!(response.printed_output, "{\"task\":\"payload\"}\n");
}

#[tokio::test]
async fn dispatch_without_replay_key_executes_each_request_without_cache_access() {
    let (_dir, core, calls, cache) = replay_test_fixture();

    let first = core
        .dispatch(replay_test_request(None))
        .await
        .expect("first dispatch");
    let second = core
        .dispatch(replay_test_request(None))
        .await
        .expect("second dispatch");

    assert_eq!(
        [first.value, second.value],
        [serde_json::json!(1), serde_json::json!(2)]
    );
    assert_eq!([first.cached, second.cached], [false, false]);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(cache.counts(), (0, 0));
}

#[tokio::test]
async fn dispatch_with_same_explicit_replay_key_executes_once_and_replays_once() {
    let (_dir, core, calls, cache) = replay_test_fixture();

    let first = core
        .dispatch(replay_test_request(Some("fixed-key")))
        .await
        .expect("first dispatch");
    let second = core
        .dispatch(replay_test_request(Some("fixed-key")))
        .await
        .expect("second dispatch");

    assert_eq!(
        [first.value, second.value],
        [serde_json::json!(1), serde_json::json!(1)]
    );
    assert_eq!([first.cached, second.cached], [false, true]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(cache.counts(), (2, 1));
}

#[tokio::test]
async fn dispatch_records_trust_graph_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn greet(name: string) -> string {
  return name
}
",
    )
    .expect("write script");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let response = core
        .dispatch(CallRequest {
            adapter: "mcp".to_string(),
            function: "greet".to_string(),
            arguments: CallArguments::Named(BTreeMap::from([(
                "name".to_string(),
                serde_json::json!("alice"),
            )])),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: Some("trust-key".to_string()),
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect("dispatch");

    let records =
        harn_vm::query_trust_records(&core.event_log, &harn_vm::TrustQueryFilters::default())
            .await
            .expect("records");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].trace_id, response.trace_id.0);
    assert_eq!(records[0].metadata["adapter"], "mcp");
}

#[tokio::test]
async fn dispatch_propagates_cancelled_execution() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn spin() -> string {
  while true {
    if is_cancelled() {
      return "stopped"
    }
  }
}
"#,
    )
    .expect("write script");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let cancel_token = Arc::new(AtomicBool::new(true));
    let response = core
        .dispatch(CallRequest {
            adapter: "acp".to_string(),
            function: "spin".to_string(),
            arguments: CallArguments::Positional(Vec::new()),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: Some("cancel-key".to_string()),
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: Some(cancel_token),
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect("dispatch");

    assert_eq!(response.value, serde_json::json!("stopped"));
}

/// `.harn` callees see the tenant the host bound via
/// `AuthPolicy` — the `ApiKeyEntry` was configured with a tenant,
/// the principal carries it forward, and `DispatchCore::dispatch`
/// installs the [`harn_vm::enter_tenant`] guard so the script's
/// `harness.tenant.id()` returns the same id end-to-end.
#[tokio::test]
async fn dispatch_threads_api_key_tenant_into_harness_and_trust_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn whoami(harness: Harness) -> string {
  return harness.tenant.id()
}
",
    )
    .expect("write script");

    let mut config = DispatchCoreConfig::for_script(&script);
    config.auth_policy = crate::auth::AuthPolicy {
        methods: vec![crate::auth::AuthMethodConfig::ApiKey(
            crate::auth::ApiKeyAuthConfig {
                keys: vec![crate::auth::ApiKeyEntry::new("alice-key", []).with_tenant("acme-corp")],
            },
        )],
        mcp_allowlist: None,
    };
    let core = DispatchCore::new(config).expect("core");

    let response = core
        .dispatch(CallRequest {
            adapter: "mcp".to_string(),
            function: "whoami".to_string(),
            arguments: CallArguments::Positional(Vec::new()),
            auth: AuthRequest {
                headers: BTreeMap::from([(
                    "authorization".to_string(),
                    "Bearer alice-key".to_string(),
                )]),
                ..AuthRequest::default()
            },
            caller: "tester".to_string(),
            replay_key: Some("tenant-whoami".to_string()),
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect("dispatch");

    assert_eq!(response.value, serde_json::json!("acme-corp"));

    let records =
        harn_vm::query_trust_records(&core.event_log, &harn_vm::TrustQueryFilters::default())
            .await
            .expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].metadata["tenant_id"], "acme-corp");
}

#[tokio::test]
async fn dispatch_threads_actor_chain_into_agent_session() {
    harn_vm::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn actor_chain(harness: Harness) -> any {
  return harness.agent.actor_chain()
}
",
    )
    .expect("write script");

    let mut config = DispatchCoreConfig::for_script(&script);
    config.auth_policy = crate::auth::AuthPolicy {
        methods: vec![crate::auth::AuthMethodConfig::ApiKey(
            crate::auth::ApiKeyAuthConfig {
                keys: vec![crate::auth::ApiKeyEntry::new("actor-key", [])],
            },
        )],
        mcp_allowlist: None,
    };
    let core = DispatchCore::new(config).expect("core");

    let response = core
        .dispatch(CallRequest {
            adapter: "a2a".to_string(),
            function: "actor_chain".to_string(),
            arguments: CallArguments::Positional(Vec::new()),
            auth: AuthRequest {
                headers: BTreeMap::from([(
                    "authorization".to_string(),
                    "Bearer actor-key".to_string(),
                )]),
                ..AuthRequest::default()
            },
            caller: "tester".to_string(),
            replay_key: Some("actor-chain".to_string()),
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: Some("dispatch-actor-chain".to_string()),
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: Some("agent:merge-captain".to_string()),
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect("dispatch");

    let expected = serde_json::json!({
        "sub": "api-key",
        "act": {
            "sub": "agent:merge-captain"
        }
    });
    assert_eq!(response.value, expected);
    assert_eq!(
        harn_vm::agent_sessions::actor_chain("dispatch-actor-chain")
            .map(|chain| chain.to_json_value()),
        Some(expected)
    );
}

/// `harness.tenant.id()` raises a typed runtime error (categorized
/// as `auth`) when the dispatch was not bound to a tenant. The
/// dispatch surface then maps it through the standard `Execution`
/// error envelope so callers see the canonical message.
#[tokio::test]
async fn dispatch_missing_tenant_raises_typed_runtime_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn whoami(harness: Harness) -> string {
  return harness.tenant.id()
}
",
    )
    .expect("write script");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let error = core
        .dispatch(CallRequest {
            adapter: "mcp".to_string(),
            function: "whoami".to_string(),
            arguments: CallArguments::Positional(Vec::new()),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: Some("missing-tenant".to_string()),
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect_err("missing tenant should error");

    let message = error.message();
    assert!(
        message.contains("harness.tenant.id()"),
        "expected typed tenant error, got: {message}"
    );
}

/// `CallRequest.tenant_id` overrides the principal-supplied tenant
/// — covers the case where an upstream gateway already resolved
/// tenancy out-of-band and hands the answer to harn-serve.
#[tokio::test]
async fn dispatch_request_tenant_overrides_principal_tenant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub fn whoami(harness: Harness) -> string {
  return harness.tenant.id()
}
",
    )
    .expect("write script");

    let mut config = DispatchCoreConfig::for_script(&script);
    config.auth_policy = crate::auth::AuthPolicy {
        methods: vec![crate::auth::AuthMethodConfig::ApiKey(
            crate::auth::ApiKeyAuthConfig {
                keys: vec![crate::auth::ApiKeyEntry::new("key", []).with_tenant("principal-tenant")],
            },
        )],
        mcp_allowlist: None,
    };
    let core = DispatchCore::new(config).expect("core");

    let response = core
        .dispatch(CallRequest {
            adapter: "mcp".to_string(),
            function: "whoami".to_string(),
            arguments: CallArguments::Positional(Vec::new()),
            auth: AuthRequest {
                headers: BTreeMap::from([("authorization".to_string(), "Bearer key".to_string())]),
                ..AuthRequest::default()
            },
            caller: "tester".to_string(),
            replay_key: Some("override-tenant".to_string()),
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: Some(harn_vm::TenantId::new("override-tenant")),
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect("dispatch");

    assert_eq!(response.value, serde_json::json!("override-tenant"));
}

mod dispatch_error_tests;
mod prepared_generation_tests;
mod prepared_tools_tests;
mod trusted_host_dispatch_tests;
mod typed_pipeline_tests;
