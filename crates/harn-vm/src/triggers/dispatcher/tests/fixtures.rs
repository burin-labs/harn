use super::*;

pub(super) fn trigger_event(kind: &str, dedupe_key: &str) -> TriggerEvent {
    TriggerEvent::new(
        ProviderId::from("github"),
        kind,
        None,
        dedupe_key,
        None,
        BTreeMap::new(),
        ProviderPayload::Known(KnownProviderPayload::GitHub(GitHubEventPayload::Issues(
            crate::triggers::event::GitHubIssuesEventPayload {
                common: crate::triggers::event::GitHubEventCommon {
                    event: "issues".to_string(),
                    action: Some("opened".to_string()),
                    delivery_id: Some(dedupe_key.to_string()),
                    installation_id: Some(42),
                    raw: serde_json::json!({"action":"opened"}),
                },
                issue: serde_json::json!({}),
            },
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
    let log = install_default_for_base_dir(dir.path()).expect("install event log");
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
) -> (
    tempfile::TempDir,
    Arc<crate::event_log::AnyEventLog>,
    Dispatcher,
) {
    crate::reset_thread_local_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let log = install_default_for_base_dir(dir.path()).expect("install event log");

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
        manifest_path: None,
        package_name: Some("workspace".to_string()),
        definition_fingerprint: format!("fp:{target}"),
    }])
    .await
    .expect("install test trigger binding");

    (dir, log.clone(), Dispatcher::with_event_log(vm, log))
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
    let log = install_default_for_base_dir(dir.path()).expect("install event log");

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

pub(super) async fn await_test_signal(label: &str, rx: oneshot::Receiver<()>) {
    tokio::time::timeout(TEST_DEFAULT_TIMEOUT, rx)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {label}"))
        .unwrap_or_else(|_| panic!("{label} sender dropped before firing"));
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

pub(super) struct MockA2aServer {
    pub(super) authority: String,
    requests: Receiver<MockA2aRequest>,
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
}

pub(super) struct MockA2aRequest {
    pub(super) headers: BTreeMap<String, String>,
    pub(super) body: serde_json::Value,
}

impl MockA2aServer {
    pub(super) fn next_request(&self) -> MockA2aRequest {
        self.request_within(TEST_DEFAULT_TIMEOUT)
            .expect("mock A2A request")
    }

    pub(super) fn request_within(&self, timeout: Duration) -> Option<MockA2aRequest> {
        self.requests.recv_timeout(timeout).ok()
    }

    pub(super) fn finish(self) {
        self.stop.store(true, Ordering::SeqCst);
        self.join.join().expect("mock A2A thread");
    }
}

pub(super) fn spawn_mock_a2a_server(task_result: serde_json::Value) -> MockA2aServer {
    spawn_mock_a2a_server_with_schemes(task_result, "https", "https")
}

pub(super) fn spawn_mock_https_a2a_server_with_card_scheme(
    task_result: serde_json::Value,
    card_scheme: &'static str,
) -> MockA2aServer {
    spawn_mock_a2a_server_with_schemes(task_result, "https", card_scheme)
}

pub(super) fn spawn_mock_http_a2a_server(task_result: serde_json::Value) -> MockA2aServer {
    spawn_mock_a2a_server_with_schemes(task_result, "http", "http")
}

pub(super) fn spawn_mock_a2a_server_with_schemes(
    task_result: serde_json::Value,
    listener_scheme: &'static str,
    card_scheme: &'static str,
) -> MockA2aServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock A2A listener");
    listener
        .set_nonblocking(true)
        .expect("set mock A2A listener nonblocking");
    let addr = listener.local_addr().expect("mock A2A addr");
    let authority = format!("127.0.0.1:{}", addr.port());
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let tls_config = (listener_scheme == "https").then(mock_a2a_tls_config);
    let max_connections = if listener_scheme == "http" && card_scheme == "http" {
        // HTTPS discovery probes the canonical card path plus legacy
        // aliases before loopback HTTP fallback. Then the successful HTTP
        // card fetch and JSON-RPC dispatch each use a connection.
        6
    } else {
        2
    };
    let join = thread::spawn(move || {
        let mut handled_requests = 0;
        while handled_requests < max_connections {
            if stop_thread.load(Ordering::SeqCst) {
                break;
            }
            let (stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(FILE_WATCH_FALLBACK_POLL);
                    continue;
                }
                Err(error) => panic!("accept mock A2A request: {error}"),
            };
            stream
                .set_nonblocking(false)
                .expect("set mock A2A stream blocking");
            stream
                .set_read_timeout(Some(TEST_DEFAULT_TIMEOUT))
                .expect("set read timeout");
            stream
                .set_write_timeout(Some(TEST_DEFAULT_TIMEOUT))
                .expect("set write timeout");
            if let Some(tls_config) = &tls_config {
                let connection = ServerConnection::new(tls_config.clone())
                    .expect("construct mock A2A TLS connection");
                let mut stream = StreamOwned::new(connection, stream);
                handle_mock_a2a_connection(
                    &mut stream,
                    card_scheme,
                    addr.port(),
                    &tx,
                    &task_result,
                );
            } else {
                let mut stream = stream;
                let mut first = [0u8; 1];
                let read = stream.peek(&mut first).expect("peek mock A2A stream");
                if read == 0 || !matches!(first[0], b'G' | b'P') {
                    handled_requests += 1;
                    continue;
                }
                handle_mock_a2a_connection(
                    &mut stream,
                    card_scheme,
                    addr.port(),
                    &tx,
                    &task_result,
                );
            }
            handled_requests += 1;
        }
    });
    MockA2aServer {
        authority,
        requests: rx,
        stop,
        join,
    }
}

pub(super) fn handle_mock_a2a_connection<T: Read + Write>(
    stream: &mut T,
    card_scheme: &str,
    port: u16,
    tx: &mpsc::Sender<MockA2aRequest>,
    task_result: &serde_json::Value,
) {
    let (request_line, headers, body) = read_http_request(stream);
    if request_line.starts_with("GET /.well-known/agent-card.json ") {
        write_json_response(
            stream,
            &serde_json::json!({
                "name": "mock-a2a",
                "description": "Mock A2A peer",
                "version": "1.0.0",
                "supportedInterfaces": [{
                    "url": format!("{card_scheme}://127.0.0.1:{port}/rpc"),
                    "protocolBinding": "JSONRPC",
                    "protocolVersion": "0.3.0"
                }],
                "capabilities": {
                    "streaming": true,
                    "pushNotifications": true,
                    "extendedAgentCard": false
                },
                "securitySchemes": {},
                "security": [],
                "defaultInputModes": ["application/json", "text/plain"],
                "defaultOutputModes": ["application/json", "text/plain"],
                "skills": [{
                    "id": "triage",
                    "name": "triage",
                    "description": "Triage mock events",
                    "tags": ["test"]
                }],
            }),
        );
        return;
    }
    assert!(
        request_line.starts_with("POST /rpc "),
        "unexpected request line: {request_line}"
    );
    let payload =
        serde_json::from_slice::<serde_json::Value>(&body).expect("mock A2A request json");
    tx.send(MockA2aRequest {
        headers,
        body: payload.clone(),
    })
    .expect("capture mock A2A request");
    let rpc_id = payload["id"].clone();
    write_json_response(
        stream,
        &crate::jsonrpc::response(rpc_id, task_result.clone()),
    );
}

pub(super) fn mock_a2a_tls_config() -> Arc<ServerConfig> {
    install_rustls_provider();
    let cert = generate_simple_self_signed(vec!["127.0.0.1".to_string(), "localhost".to_string()])
        .expect("generate mock A2A certificate");
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let key_der = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
    Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der.into())
            .expect("build mock A2A TLS server config"),
    )
}

pub(super) fn install_rustls_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

pub(super) fn read_http_request<T: Read>(
    stream: &mut T,
) -> (String, BTreeMap<String, String>, Vec<u8>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end;
    let content_length;
    loop {
        let read = stream.read(&mut chunk).expect("read mock A2A request");
        assert!(read > 0, "mock A2A request closed before headers");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = find_header_end(&buffer) {
            header_end = end;
            content_length = parse_content_length(&buffer[..header_end]);
            break;
        }
    }
    while buffer.len() < header_end + content_length {
        let read = stream.read(&mut chunk).expect("read mock A2A body");
        assert!(read > 0, "mock A2A request closed before body");
        buffer.extend_from_slice(&chunk[..read]);
    }
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let request_line = headers.lines().next().unwrap_or_default().to_string();
    let mut parsed_headers = BTreeMap::new();
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        parsed_headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let body = buffer[header_end..header_end + content_length].to_vec();
    (request_line, parsed_headers, body)
}

pub(super) fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

pub(super) fn parse_content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

pub(super) fn write_json_response<T: Write>(stream: &mut T, body: &serde_json::Value) {
    let payload = serde_json::to_vec(body).expect("serialize mock A2A response");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        payload.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write mock A2A headers");
    stream.write_all(&payload).expect("write mock A2A body");
    stream.flush().expect("flush mock A2A response");
}
