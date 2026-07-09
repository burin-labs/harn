use super::*;

pub(super) fn trigger_event(kind: &str, dedupe_key: &str) -> TriggerEvent {
    TriggerEvent::new(
        ProviderId::from("github"),
        kind,
        None,
        dedupe_key,
        None,
        BTreeMap::new(),
        ProviderPayload::Known(KnownProviderPayload::GitHub(Box::new(
            GitHubEventPayload::Issues(crate::triggers::event::GitHubIssuesEventPayload {
                common: crate::triggers::event::GitHubEventCommon {
                    event: "issues".to_string(),
                    action: Some("opened".to_string()),
                    delivery_id: Some(dedupe_key.to_string()),
                    installation_id: Some(42),
                    topic: None,
                    reaction_topics: Vec::new(),
                    repository: None,
                    repo: None,
                    raw: serde_json::json!({"action":"opened"}),
                },
                issue: serde_json::json!({}),
            }),
        ))),
        SignatureStatus::Verified,
    )
}

pub(super) async fn compile_trigger_expr(
    vm: &mut Vm,
    dir: &std::path::Path,
    label: &str,
    expr: &str,
) -> crate::triggers::TriggerExpressionSpec {
    let source = format!(
        "import \"std/triggers\"\n\npub fn __expr(event: TriggerEvent) -> any {{\n  return {expr}\n}}\n"
    );
    let exports = vm
        .load_module_exports_from_source(dir.join(format!("{label}.harn")), &source)
        .await
        .expect("compile trigger expression");
    crate::triggers::TriggerExpressionSpec {
        raw: expr.to_string(),
        closure: exports["__expr"].clone(),
    }
}

pub(super) async fn dispatcher_fixture(
    source: &str,
    handler_name: &str,
    when_name: Option<&str>,
    retry: TriggerRetryConfig,
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    dispatcher_fixture_with_options(
        source,
        handler_name,
        when_name,
        None,
        None,
        retry,
        crate::triggers::TriggerFlowControlConfig::default(),
    )
    .await
}

pub(super) async fn dispatcher_fixture_with_flow_control(
    source: &str,
    handler_name: &str,
    when_name: Option<&str>,
    retry: TriggerRetryConfig,
    flow_control: crate::triggers::TriggerFlowControlConfig,
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    dispatcher_fixture_with_options(
        source,
        handler_name,
        when_name,
        None,
        None,
        retry,
        flow_control,
    )
    .await
}

pub(super) async fn dispatcher_fixture_with_options(
    source: &str,
    handler_name: &str,
    when_name: Option<&str>,
    when_budget: Option<TriggerPredicateBudget>,
    daily_cost_usd: Option<f64>,
    retry: TriggerRetryConfig,
    flow_control: crate::triggers::TriggerFlowControlConfig,
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    dispatcher_fixture_with_budget_strategy(
        source,
        handler_name,
        when_name,
        when_budget,
        daily_cost_usd,
        None,
        crate::TriggerBudgetExhaustionStrategy::False,
        retry,
        flow_control,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatcher_fixture_with_budget_strategy(
    source: &str,
    handler_name: &str,
    when_name: Option<&str>,
    when_budget: Option<TriggerPredicateBudget>,
    daily_cost_usd: Option<f64>,
    hourly_cost_usd: Option<f64>,
    on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy,
    retry: TriggerRetryConfig,
    flow_control: crate::triggers::TriggerFlowControlConfig,
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    crate::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_test_event_log();
    let lib_path = dir.path().join("lib.harn");
    std::fs::write(&lib_path, source).expect("write module source");

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(dir.path());
    let exports = vm
        .load_module_exports(&lib_path)
        .await
        .expect("load handler exports");

    let handler = exports
        .get(handler_name)
        .unwrap_or_else(|| panic!("missing handler export {handler_name}"))
        .clone();
    let when = when_name.map(|name| TriggerPredicateSpec {
        raw: name.to_string(),
        closure: exports
            .get(name)
            .unwrap_or_else(|| panic!("missing predicate export {name}"))
            .clone(),
    });

    install_manifest_triggers(vec![TriggerBindingSpec {
        id: "github-new-issue".to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Local {
            raw: handler_name.to_string(),
            closure: handler,
        },
        dispatch_priority: crate::WorkerQueuePriority::Normal,
        when,
        when_budget,
        retry,
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd,
        hourly_cost_usd,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted,
        max_concurrent: None,
        flow_control,
        aggregation: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: format!("fp:{handler_name}"),
    }])
    .await
    .expect("install test trigger binding");

    (dir, log.clone(), Dispatcher::with_event_log(vm, log))
}

pub(super) async fn a2a_dispatcher_fixture(
    target: String,
    retry: TriggerRetryConfig,
    allow_cleartext: bool,
    a2a_client: Arc<dyn crate::a2a::A2aClient>,
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    crate::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_test_event_log();

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(dir.path());

    install_manifest_triggers(vec![TriggerBindingSpec {
        id: "github-a2a-review".to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::A2a {
            target: target.clone(),
            allow_cleartext,
        },
        dispatch_priority: crate::WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry,
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        hourly_cost_usd: None,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
        max_concurrent: None,
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: format!("fp:{target}"),
    }])
    .await
    .expect("install test trigger binding");

    let dispatcher = Dispatcher::with_event_log(vm, log.clone()).with_a2a_client(a2a_client);
    (dir, log, dispatcher)
}

pub(super) async fn worker_dispatcher_fixture(
    queue: String,
    retry: TriggerRetryConfig,
    dispatch_priority: crate::WorkerQueuePriority,
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    crate::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_test_event_log();

    let mut vm = Vm::new();
    register_vm_stdlib(&mut vm);
    vm.set_source_dir(dir.path());

    install_manifest_triggers(vec![TriggerBindingSpec {
        id: "github-worker-review".to_string(),
        source: TriggerBindingSource::Manifest,
        kind: "webhook".to_string(),
        provider: ProviderId::from("github"),
        autonomy_tier: crate::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Worker { queue },
        dispatch_priority,
        when: None,
        when_budget: None,
        retry,
        match_events: vec!["issues.opened".to_string()],
        dedupe_key: Some("event.dedupe_key".to_string()),
        dedupe_retention_days: crate::triggers::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        hourly_cost_usd: None,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted: crate::TriggerBudgetExhaustionStrategy::False,
        max_concurrent: None,
        flow_control: crate::triggers::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: "fp:worker-review".to_string(),
    }])
    .await
    .expect("install test trigger binding");

    (dir, log.clone(), Dispatcher::with_event_log(vm, log))
}

pub(super) async fn read_topic(
    log: Arc<crate::event_log::AnyEventLog>,
    topic: &str,
) -> Vec<(u64, crate::event_log::LogEvent)> {
    let topic = Topic::new(topic).expect("valid topic");
    log.read_range(&topic, None, usize::MAX)
        .await
        .expect("read topic events")
}

pub(super) async fn wait_for_dispatcher_in_flight(dispatcher: &Dispatcher, expected: u64) {
    for _ in 0..1_000 {
        if dispatcher.snapshot().in_flight >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "timed out waiting for {expected} in-flight dispatches; snapshot={:?}",
        dispatcher.snapshot()
    );
}

pub(super) fn test_cancel_requested_at() -> time::OffsetDateTime {
    time::OffsetDateTime::UNIX_EPOCH
}

pub(super) async fn await_wait_event(
    log: Arc<crate::event_log::AnyEventLog>,
    wait_id: &str,
    kind: &str,
) {
    let topic =
        Topic::new(crate::waitpoints::WAITPOINT_WAITS_TOPIC).expect("valid waitpoint waits topic");
    let mut stream = log
        .clone()
        .subscribe(&topic, None)
        .await
        .expect("subscribe to waitpoint waits");
    let matched = tokio::time::timeout(TEST_DEFAULT_TIMEOUT, async {
        while let Some(item) = stream.next().await {
            let (_, event) = item.expect("waitpoint wait event");
            if event.kind == kind
                && event.headers.get("wait_id").map(String::as_str) == Some(wait_id)
            {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if matched {
        return;
    }

    let events = read_topic(log.clone(), crate::waitpoints::WAITPOINT_WAITS_TOPIC).await;
    let observed = events
        .iter()
        .map(|(_, event)| {
            let observed_wait_id = event
                .headers
                .get("wait_id")
                .map(String::as_str)
                .unwrap_or("<missing>");
            format!("{}:{observed_wait_id}", event.kind)
        })
        .collect::<Vec<_>>();
    panic!("timed out waiting for {kind} for {wait_id}; observed={observed:?}");
}

pub(super) fn flatten_action_graph(
    events: &[(u64, crate::event_log::LogEvent)],
) -> (Vec<String>, Vec<String>) {
    let mut node_kinds = Vec::new();
    let mut edge_kinds = Vec::new();
    for (_, event) in events {
        let observability = &event.payload["observability"];
        if let Some(nodes) = observability["action_graph_nodes"].as_array() {
            node_kinds.extend(nodes.iter().filter_map(|node| {
                node.get("kind")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            }));
        }
        if let Some(edges) = observability["action_graph_edges"].as_array() {
            edge_kinds.extend(edges.iter().filter_map(|edge| {
                edge.get("kind")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            }));
        }
    }
    (node_kinds, edge_kinds)
}

pub(super) fn lifecycle_payloads(
    events: &[(u64, crate::event_log::LogEvent)],
    kind: &str,
) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter(|(_, event)| event.kind == kind)
        .map(|(_, event)| event.payload.clone())
        .collect()
}

/// Parameters captured from a single call to [`InProcessMockA2aClient::dispatch`].
pub(super) struct MockA2aCall {
    pub(super) target: String,
    pub(super) allow_cleartext: bool,
    pub(super) event_trace_id: String,
}

/// What the mock should return when [`A2aClient::dispatch`] is called.
pub(super) enum MockA2aResponse {
    Inline {
        task_id: String,
        result: serde_json::Value,
    },
    Pending {
        task_id: String,
        state: String,
        handle: serde_json::Value,
    },
    Timeout(String),
    Denied(String),
    /// Return a protocol error carrying `message`.
    Protocol(String),
    /// Block until the cancel broadcast fires, then return `Cancelled`.
    WaitForCancel,
}

/// In-process A2A client mock — no TCP, no TLS, no OS threads, no real I/O.
pub(super) struct InProcessMockA2aClient {
    response: MockA2aResponse,
    pub(super) calls: tokio::sync::Mutex<Vec<MockA2aCall>>,
}

impl InProcessMockA2aClient {
    pub(super) fn new(response: MockA2aResponse) -> Arc<Self> {
        Arc::new(Self {
            response,
            calls: tokio::sync::Mutex::new(Vec::new()),
        })
    }

    pub(super) async fn take_calls(&self) -> Vec<MockA2aCall> {
        std::mem::take(&mut *self.calls.lock().await)
    }
}

#[async_trait]
impl crate::a2a::A2aClient for InProcessMockA2aClient {
    async fn dispatch(
        &self,
        target: &str,
        allow_cleartext: bool,
        _binding_id: &str,
        _binding_key: &str,
        event: &crate::triggers::TriggerEvent,
        cancel_rx: &mut tokio::sync::broadcast::Receiver<()>,
    ) -> Result<
        (crate::a2a::ResolvedA2aEndpoint, crate::a2a::DispatchAck),
        crate::a2a::A2aClientError,
    > {
        self.calls.lock().await.push(MockA2aCall {
            target: target.to_string(),
            allow_cleartext,
            event_trace_id: event.trace_id.0.clone(),
        });

        let endpoint = crate::a2a::ResolvedA2aEndpoint {
            card_url: format!("https://{target}/.well-known/agent-card.json"),
            rpc_url: format!("https://{target}/rpc"),
            agent_id: Some(format!("agent:{target}")),
            target_agent: crate::a2a::target_agent_label(target),
        };

        match &self.response {
            MockA2aResponse::Inline { task_id, result } => Ok((
                endpoint,
                crate::a2a::DispatchAck::InlineResult {
                    task_id: task_id.clone(),
                    result: result.clone(),
                },
            )),
            MockA2aResponse::Pending {
                task_id,
                state,
                handle,
            } => Ok((
                endpoint,
                crate::a2a::DispatchAck::PendingTask {
                    task_id: task_id.clone(),
                    state: state.clone(),
                    handle: handle.clone(),
                },
            )),
            MockA2aResponse::Timeout(message) => {
                Err(crate::a2a::A2aClientError::Timeout(message.clone()))
            }
            MockA2aResponse::Denied(message) => {
                Err(crate::a2a::A2aClientError::Denied(message.clone()))
            }
            MockA2aResponse::Protocol(message) => {
                Err(crate::a2a::A2aClientError::Protocol(message.clone()))
            }
            MockA2aResponse::WaitForCancel => {
                let _ = cancel_rx.recv().await;
                Err(crate::a2a::A2aClientError::Cancelled(
                    "A2A dispatch cancelled by shutdown".to_string(),
                ))
            }
        }
    }
}
