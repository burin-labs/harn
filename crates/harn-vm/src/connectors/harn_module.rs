use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::{oneshot, Notify};

use crate::bridge::json_result_to_vm_value;
use crate::event_log::{EventLog, LogEvent, Topic};
use crate::llm::vm_value_to_json;
use crate::orchestration::CapabilityPolicy;
use crate::stdlib::register_vm_stdlib;
use crate::triggers::dispatcher::InboxEnvelope;
use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};
use crate::vm::Vm;
use crate::{
    postprocess_normalized_event, redact_headers, ClientError, Connector, ConnectorClient,
    ConnectorCtx, ConnectorError, ConnectorHttpResponse, ConnectorNormalizeResult,
    HarnConnectorEffectPolicies, HeaderRedactionPolicy, PostNormalizeOutcome, ProviderId,
    ProviderPayload, ProviderPayloadSchema, SignatureStatus, TenantId, TraceId, TriggerBinding,
    TriggerEvent, TriggerEventId, TriggerKind,
};

thread_local! {
    static ACTIVE_HARN_CONNECTOR_CTX: RefCell<Vec<ConnectorCtx>> = const { RefCell::new(Vec::new()) };
}

const HARN_CONNECTOR_POLL_STATE_TOPIC: &str = "connectors.harn.poll.state";
const HARN_CONNECTOR_POLL_STATE_KIND: &str = "harn.poll.state";
const DEFAULT_POLL_INTERVAL: StdDuration = StdDuration::from_secs(300);

#[derive(Clone, Debug, PartialEq)]
pub struct HarnConnectorContract {
    pub module_path: PathBuf,
    pub provider_id: ProviderId,
    pub kinds: Vec<TriggerKind>,
    pub payload_schema: ProviderPayloadSchema,
    pub has_poll_tick: bool,
}

pub struct HarnConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    payload_schema: ProviderPayloadSchema,
    module_path: PathBuf,
    has_poll_tick: bool,
    effect_policies: HarnConnectorEffectPolicies,
    shared: Arc<HarnConnectorShared>,
}

struct HarnConnectorClient {
    shared: Arc<HarnConnectorShared>,
}

struct HarnConnectorShared {
    provider_id: ProviderId,
    worker: Mutex<Option<Arc<HarnConnectorWorker>>>,
    ctx: Mutex<Option<ConnectorCtx>>,
    poll_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    poll_shutdown: Mutex<Arc<PollShutdownSignal>>,
}

struct HarnConnectorWorker {
    tx: mpsc::Sender<WorkerCommand>,
    join: Mutex<Option<JoinHandle<()>>>,
    effect_policies: HarnConnectorEffectPolicies,
}

enum WorkerCommand {
    Init {
        ctx: ConnectorCtx,
        init_payload: JsonValue,
        resp: oneshot::Sender<Result<(), String>>,
    },
    CallExport {
        name: String,
        args: Vec<JsonValue>,
        required: bool,
        policy: Option<CapabilityPolicy>,
        resp: oneshot::Sender<Result<Option<JsonValue>, String>>,
    },
    Shutdown {
        resp: oneshot::Sender<Result<(), String>>,
    },
}

struct LocalHarnConnectorRuntime {
    base_vm: Vm,
    exports: BTreeMap<String, Rc<VmClosure>>,
    ctx: ConnectorCtx,
}

#[derive(Debug, Default)]
struct PollShutdownSignal {
    stopped: AtomicBool,
    notify: Notify,
}

impl PollShutdownSignal {
    fn request_stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        self.notify.notified().await;
    }
}

#[derive(Debug, Deserialize)]
struct HarnNormalizeResult {
    kind: String,
    #[serde(default)]
    occurred_at: Option<String>,
    dedupe_key: String,
    payload: JsonValue,
    #[serde(default)]
    signature_status: Option<SignatureStatus>,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    batch: Option<Vec<JsonValue>>,
}

#[derive(Debug, Deserialize)]
struct HarnHttpResponse {
    #[serde(default = "default_ok_status")]
    status: u16,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: JsonValue,
}

#[derive(Debug, Deserialize)]
struct HarnPollTickResult {
    #[serde(default)]
    events: Vec<HarnNormalizeResult>,
    #[serde(default)]
    cursor: Option<JsonValue>,
    #[serde(default)]
    state: Option<JsonValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct HarnPollStateRecord {
    provider: String,
    binding_id: String,
    state_key: String,
    #[serde(default)]
    cursor: Option<JsonValue>,
    #[serde(default)]
    state: Option<JsonValue>,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct HarnPollBindingConfigEnvelope {
    #[serde(default)]
    poll: HarnPollBindingConfig,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct HarnPollBindingConfig {
    #[serde(default)]
    interval: Option<String>,
    #[serde(default)]
    interval_ms: Option<u64>,
    #[serde(default)]
    interval_secs: Option<u64>,
    #[serde(default)]
    jitter: Option<String>,
    #[serde(default)]
    jitter_ms: Option<u64>,
    #[serde(default)]
    jitter_secs: Option<u64>,
    #[serde(default, alias = "cursor_state_key")]
    state_key: Option<String>,
    #[serde(default)]
    lease_id: Option<String>,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    max_batch_size: Option<usize>,
}

#[derive(Clone, Debug)]
struct ResolvedHarnPollBinding {
    binding: TriggerBinding,
    interval: StdDuration,
    jitter: StdDuration,
    state_key: String,
    lease_id: String,
    tenant_id: Option<TenantId>,
    max_batch_size: Option<usize>,
}

impl HarnConnector {
    pub async fn load(module_path: &Path) -> Result<Self, ConnectorError> {
        Self::load_with_effect_policies(module_path, HarnConnectorEffectPolicies::default()).await
    }

    pub async fn load_with_effect_policies(
        module_path: &Path,
        effect_policies: HarnConnectorEffectPolicies,
    ) -> Result<Self, ConnectorError> {
        let contract = load_contract(module_path).await?;
        let shared = Arc::new(HarnConnectorShared {
            provider_id: contract.provider_id.clone(),
            worker: Mutex::new(None),
            ctx: Mutex::new(None),
            poll_tasks: Mutex::new(Vec::new()),
            poll_shutdown: Mutex::new(Arc::new(PollShutdownSignal::default())),
        });
        Ok(Self {
            provider_id: contract.provider_id,
            kinds: contract.kinds,
            payload_schema: contract.payload_schema,
            module_path: contract.module_path,
            has_poll_tick: contract.has_poll_tick,
            effect_policies,
            shared,
        })
    }
}

impl HarnConnectorShared {
    fn install_worker(&self, worker: Arc<HarnConnectorWorker>) {
        *self.worker.lock().expect("worker mutex poisoned") = Some(worker);
    }

    fn set_ctx(&self, ctx: ConnectorCtx) {
        *self.ctx.lock().expect("ctx mutex poisoned") = Some(ctx);
    }

    fn ctx(&self) -> Result<ConnectorCtx, ConnectorError> {
        self.ctx
            .lock()
            .expect("ctx mutex poisoned")
            .clone()
            .ok_or_else(|| {
                ConnectorError::HarnRuntime(format!(
                    "connector runtime for provider '{}' is not initialized",
                    self.provider_id.as_str()
                ))
            })
    }

    fn start_poll_tasks(&self, tasks: Vec<tokio::task::JoinHandle<()>>) {
        self.poll_tasks
            .lock()
            .expect("poll tasks poisoned")
            .extend(tasks);
    }

    fn reset_poll_shutdown(&self) -> Arc<PollShutdownSignal> {
        let shutdown = Arc::new(PollShutdownSignal::default());
        *self.poll_shutdown.lock().expect("poll shutdown poisoned") = shutdown.clone();
        shutdown
    }

    fn stop_poll_tasks(&self) {
        self.poll_shutdown
            .lock()
            .expect("poll shutdown poisoned")
            .request_stop();
        for task in self
            .poll_tasks
            .lock()
            .expect("poll tasks poisoned")
            .drain(..)
        {
            task.abort();
        }
    }

    fn worker(&self) -> Result<Arc<HarnConnectorWorker>, ConnectorError> {
        self.worker
            .lock()
            .expect("worker mutex poisoned")
            .clone()
            .ok_or_else(|| {
                ConnectorError::HarnRuntime(format!(
                    "connector runtime for provider '{}' is not initialized",
                    self.provider_id.as_str()
                ))
            })
    }

    fn worker_for_client(&self) -> Result<Arc<HarnConnectorWorker>, ClientError> {
        self.worker()
            .map_err(|error| ClientError::Other(error.to_string()))
    }

    fn take_worker(&self) -> Option<Arc<HarnConnectorWorker>> {
        self.worker.lock().expect("worker mutex poisoned").take()
    }
}

impl HarnConnectorWorker {
    fn spawn(
        provider_id: ProviderId,
        module_path: PathBuf,
        effect_policies: HarnConnectorEffectPolicies,
    ) -> Result<Arc<Self>, ConnectorError> {
        let (tx, rx) = mpsc::channel();
        let thread_name = format!("harn-connector-{}", provider_id.as_str());
        let join = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run_worker_loop(module_path, rx))
            .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?;
        Ok(Arc::new(Self {
            tx,
            join: Mutex::new(Some(join)),
            effect_policies,
        }))
    }

    async fn init(&self, ctx: ConnectorCtx, init_payload: JsonValue) -> Result<(), ConnectorError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(WorkerCommand::Init {
                ctx,
                init_payload,
                resp: resp_tx,
            })
            .map_err(worker_send_error)?;
        resp_rx
            .await
            .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?
            .map_err(ConnectorError::HarnRuntime)
    }

    async fn call_export(
        &self,
        name: impl Into<String>,
        args: Vec<JsonValue>,
        required: bool,
    ) -> Result<Option<JsonValue>, ConnectorError> {
        let name = name.into();
        let policy = self.effect_policies.policy_for_export(&name);
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(WorkerCommand::CallExport {
                name,
                args,
                required,
                policy,
                resp: resp_tx,
            })
            .map_err(worker_send_error)?;
        resp_rx
            .await
            .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?
            .map_err(ConnectorError::HarnRuntime)
    }

    async fn shutdown(&self) -> Result<(), ConnectorError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(WorkerCommand::Shutdown { resp: resp_tx })
            .map_err(worker_send_error)?;
        resp_rx
            .await
            .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?
            .map_err(ConnectorError::HarnRuntime)?;
        if let Some(join) = self.join.lock().expect("join mutex poisoned").take() {
            join.join().map_err(|_| {
                ConnectorError::HarnRuntime("connector worker panicked".to_string())
            })?;
        }
        Ok(())
    }
}

fn worker_send_error(error: mpsc::SendError<WorkerCommand>) -> ConnectorError {
    ConnectorError::HarnRuntime(format!("connector worker channel closed: {error}"))
}

fn run_worker_loop(module_path: PathBuf, rx: mpsc::Receiver<WorkerCommand>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");
    let local = tokio::task::LocalSet::new();
    let mut state: Option<LocalHarnConnectorRuntime> = None;
    while let Ok(command) = rx.recv() {
        let should_exit = matches!(command, WorkerCommand::Shutdown { .. });
        local.block_on(&runtime, async {
            handle_worker_command(&module_path, &mut state, command).await;
        });
        if should_exit {
            break;
        }
    }
}

async fn handle_worker_command(
    module_path: &Path,
    state: &mut Option<LocalHarnConnectorRuntime>,
    command: WorkerCommand,
) {
    match command {
        WorkerCommand::Init {
            ctx,
            init_payload,
            resp,
        } => {
            let result = async {
                if state.is_none() {
                    *state = Some(load_runtime_with_ctx(module_path, ctx).await?);
                }
                let runtime = state
                    .as_ref()
                    .expect("runtime initialized before init export");
                call_provider_export(runtime, "init", vec![init_payload], false, None)
                    .await
                    .map(|_| ())
            }
            .await
            .map_err(|error| error.to_string());
            let _ = resp.send(result);
        }
        WorkerCommand::CallExport {
            name,
            args,
            required,
            policy,
            resp,
        } => {
            let result = async {
                let runtime = state.as_ref().ok_or_else(|| {
                    ConnectorError::HarnRuntime("connector runtime is not initialized".to_string())
                })?;
                call_provider_export(runtime, &name, args, required, policy).await
            }
            .await
            .map_err(|error| error.to_string());
            let _ = resp.send(result);
        }
        WorkerCommand::Shutdown { resp } => {
            let result = async {
                if let Some(runtime) = state.as_ref() {
                    call_provider_export(runtime, "shutdown", Vec::new(), false, None)
                        .await
                        .map(|_| ())?;
                }
                *state = None;
                Ok::<(), ConnectorError>(())
            }
            .await
            .map_err(|error| error.to_string());
            let _ = resp.send(result);
        }
    }
}

pub async fn load_contract(module_path: &Path) -> Result<HarnConnectorContract, ConnectorError> {
    let (base_vm, exports) = load_module_runtime(module_path).await?;
    let provider_id =
        parse_provider_id(required_export_call(&base_vm, &exports, "provider_id", &[]).await?)?;
    let kinds = parse_kinds(required_export_call(&base_vm, &exports, "kinds", &[]).await?)?;
    let payload_schema = parse_payload_schema(
        required_export_call(&base_vm, &exports, "payload_schema", &[]).await?,
    )?;
    Ok(HarnConnectorContract {
        module_path: module_path.to_path_buf(),
        provider_id,
        kinds,
        payload_schema,
        has_poll_tick: exports.contains_key("poll_tick"),
    })
}

#[async_trait]
impl Connector for HarnConnector {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn kinds(&self) -> &[TriggerKind] {
        &self.kinds
    }

    async fn init(&mut self, ctx: ConnectorCtx) -> Result<(), ConnectorError> {
        let worker = HarnConnectorWorker::spawn(
            self.provider_id.clone(),
            self.module_path.clone(),
            self.effect_policies.clone(),
        )?;
        self.shared.set_ctx(ctx.clone());
        let init_payload = json!({
            "provider_id": self.provider_id.as_str(),
            "module_path": self.module_path.display().to_string(),
            "capabilities": {
                "secret_get": true,
                "event_log_emit": true,
                "metrics_inc": true,
            }
        });
        worker.init(ctx, init_payload).await?;
        self.shared.install_worker(worker);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[TriggerBinding],
    ) -> Result<crate::ActivationHandle, ConnectorError> {
        let poll_bindings = bindings
            .iter()
            .filter(|binding| binding.kind.as_str() == "poll")
            .map(resolve_poll_binding)
            .collect::<Result<Vec<_>, _>>()?;
        if !poll_bindings.is_empty() && !self.has_poll_tick {
            return Err(ConnectorError::Activation(format!(
                "Harn connector '{}' has poll binding(s) but does not export poll_tick(ctx)",
                self.provider_id.as_str()
            )));
        }
        let bindings_json = JsonValue::Array(bindings.iter().map(binding_to_json).collect());
        self.shared
            .worker()?
            .call_export("activate", vec![bindings_json], false)
            .await?;
        if poll_bindings.is_empty() {
            self.shared.stop_poll_tasks();
        } else {
            self.shared.stop_poll_tasks();
            let ctx = self.shared.ctx()?;
            let worker = self.shared.worker()?;
            let shutdown = self.shared.reset_poll_shutdown();
            let tasks = poll_bindings
                .into_iter()
                .map(|binding| {
                    let worker = worker.clone();
                    let ctx = ctx.clone();
                    let shutdown = shutdown.clone();
                    let provider_id = self.provider_id.clone();
                    tokio::spawn(async move {
                        if let Err(error) =
                            run_poll_loop(provider_id, worker, ctx, binding, shutdown).await
                        {
                            eprintln!("[harn] Harn connector poll warning: {error}");
                        }
                    })
                })
                .collect();
            self.shared.start_poll_tasks(tasks);
        }
        Ok(crate::ActivationHandle::new(
            self.provider_id.clone(),
            bindings.len(),
        ))
    }

    async fn shutdown(&self, deadline: StdDuration) -> Result<(), ConnectorError> {
        self.shared.stop_poll_tasks();
        if let Some(worker) = self.shared.take_worker() {
            if deadline.is_zero() {
                worker.shutdown().await?;
            } else {
                tokio::time::timeout(deadline, worker.shutdown())
                    .await
                    .map_err(|_| {
                        ConnectorError::HarnRuntime(format!(
                            "connector worker shutdown exceeded {}s",
                            deadline.as_secs()
                        ))
                    })??;
            }
        }
        Ok(())
    }

    async fn normalize_inbound(
        &self,
        raw: crate::RawInbound,
    ) -> Result<TriggerEvent, ConnectorError> {
        let result = self.normalize_inbound_result(raw).await?;
        match result {
            ConnectorNormalizeResult::Event(event) => Ok(*event),
            ConnectorNormalizeResult::Batch(events) => {
                Err(ConnectorError::HarnRuntime(format!(
                    "connector '{}' returned a NormalizeResult batch where a single event was expected ({} events)",
                    self.provider_id.as_str(),
                    events.len()
                )))
            }
            ConnectorNormalizeResult::ImmediateResponse { events, .. } => {
                let mut events = events.into_iter();
                let Some(event) = events.next() else {
                    return Err(ConnectorError::HarnRuntime(format!(
                        "connector '{}' returned an immediate_response without an event where a single event was expected",
                        self.provider_id.as_str()
                    )));
                };
                if events.next().is_some() {
                    return Err(ConnectorError::HarnRuntime(format!(
                        "connector '{}' returned an immediate_response with multiple events where a single event was expected",
                        self.provider_id.as_str()
                    )));
                }
                Ok(event)
            }
            ConnectorNormalizeResult::Reject(response) => Err(ConnectorError::Unsupported(format!(
                "connector '{}' rejected inbound request with HTTP {}",
                self.provider_id.as_str(),
                response.status
            ))),
        }
    }

    async fn normalize_inbound_result(
        &self,
        raw: crate::RawInbound,
    ) -> Result<ConnectorNormalizeResult, ConnectorError> {
        let raw_json = raw_inbound_to_json(&raw);
        let value = self
            .shared
            .worker()?
            .call_export("normalize_inbound", vec![raw_json], true)
            .await?
            .expect("required export returns a value");
        parse_normalize_result(&self.provider_id, &raw, value)
    }

    fn payload_schema(&self) -> ProviderPayloadSchema {
        self.payload_schema.clone()
    }

    fn client(&self) -> Arc<dyn ConnectorClient> {
        Arc::new(HarnConnectorClient {
            shared: self.shared.clone(),
        })
    }
}

#[async_trait]
impl ConnectorClient for HarnConnectorClient {
    async fn call(&self, method: &str, args: JsonValue) -> Result<JsonValue, ClientError> {
        let Some(result) = self
            .shared
            .worker_for_client()?
            .call_export(
                "call",
                vec![JsonValue::String(method.to_string()), args],
                false,
            )
            .await
            .map_err(connector_error_to_client)?
        else {
            return Err(ClientError::MethodNotFound(method.to_string()));
        };
        Ok(result)
    }
}

fn parse_normalize_result(
    provider_id: &ProviderId,
    raw: &crate::RawInbound,
    value: JsonValue,
) -> Result<ConnectorNormalizeResult, ConnectorError> {
    let result_type = value
        .get("type")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match result_type {
        Some("event") => {
            let event_value = value.get("event").cloned().unwrap_or(value);
            parse_harn_normalized_event(provider_id, raw, event_value)
                .map(ConnectorNormalizeResult::event)
        }
        Some("batch") => {
            let events = parse_events_field(provider_id, raw, &value, "events")?;
            if events.is_empty() {
                return Err(ConnectorError::HarnRuntime(
                    "NormalizeResult batch must contain at least one event".to_string(),
                ));
            }
            Ok(ConnectorNormalizeResult::Batch(events))
        }
        Some("immediate_response") => {
            let response = parse_http_response(&value, "immediate_response", 200)?;
            let events = parse_optional_embedded_events(provider_id, raw, &value)?;
            Ok(ConnectorNormalizeResult::ImmediateResponse { response, events })
        }
        Some("reject") => {
            parse_http_response(&value, "reject", 400).map(ConnectorNormalizeResult::Reject)
        }
        Some(other) => Err(ConnectorError::HarnRuntime(format!(
            "unsupported NormalizeResult type '{other}'"
        ))),
        None => {
            tracing::warn!(
                provider = provider_id.as_str(),
                "Harn connector normalize_inbound returned a legacy direct event shape; return NormalizeResult v1 instead"
            );
            parse_harn_normalized_event(provider_id, raw, value)
                .map(ConnectorNormalizeResult::event)
        }
    }
}

fn parse_optional_embedded_events(
    provider_id: &ProviderId,
    raw: &crate::RawInbound,
    value: &JsonValue,
) -> Result<Vec<TriggerEvent>, ConnectorError> {
    let has_event = value.get("event").is_some();
    let has_events = value.get("events").is_some();
    if has_event && has_events {
        return Err(ConnectorError::HarnRuntime(
            "NormalizeResult immediate_response must use either 'event' or 'events', not both"
                .to_string(),
        ));
    }
    if has_event {
        let event = value
            .get("event")
            .cloned()
            .expect("checked immediate_response event field");
        return parse_harn_normalized_event(provider_id, raw, event).map(|event| vec![event]);
    }
    if has_events {
        return parse_events_field(provider_id, raw, value, "events");
    }
    Ok(Vec::new())
}

fn parse_events_field(
    provider_id: &ProviderId,
    raw: &crate::RawInbound,
    value: &JsonValue,
    field: &str,
) -> Result<Vec<TriggerEvent>, ConnectorError> {
    let events = value
        .get(field)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| {
            ConnectorError::HarnRuntime(format!("NormalizeResult missing array field '{field}'"))
        })?;
    events
        .iter()
        .cloned()
        .map(|event| parse_harn_normalized_event(provider_id, raw, event))
        .collect()
}

fn parse_harn_normalized_event(
    provider_id: &ProviderId,
    raw: &crate::RawInbound,
    value: JsonValue,
) -> Result<TriggerEvent, ConnectorError> {
    let normalized: HarnNormalizeResult = serde_json::from_value(value)
        .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?;
    let occurred_at = normalized
        .occurred_at
        .as_deref()
        .map(parse_rfc3339)
        .transpose()?;
    let tenant_id = normalized.tenant_id.map(TenantId::new);
    let headers = redact_headers(
        &normalized.headers.unwrap_or_else(|| raw.headers.clone()),
        &HeaderRedactionPolicy::default(),
    );
    let provider_payload = ProviderPayload::normalize(
        provider_id,
        &normalized.kind,
        &raw.headers,
        normalized.payload,
    )
    .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?;
    Ok(TriggerEvent {
        id: TriggerEventId::new(),
        provider: provider_id.clone(),
        kind: normalized.kind,
        received_at: raw.received_at,
        occurred_at,
        dedupe_key: normalized.dedupe_key,
        trace_id: TraceId::new(),
        tenant_id: tenant_id.or_else(|| raw.tenant_id.clone()),
        headers,
        batch: normalized.batch,
        provider_payload,
        raw_body: Some(raw.body.clone()),
        signature_status: normalized
            .signature_status
            .unwrap_or(SignatureStatus::Unsigned),
        dedupe_claimed: false,
    })
}

fn parse_http_response(
    value: &JsonValue,
    nested_field: &str,
    default_status: u16,
) -> Result<ConnectorHttpResponse, ConnectorError> {
    let response_value = value
        .get(nested_field)
        .or_else(|| value.get("response"))
        .unwrap_or(value);
    let source_has_status = response_value.get("status").is_some();
    let mut response: HarnHttpResponse = serde_json::from_value(response_value.clone())
        .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?;
    if !source_has_status {
        response.status = default_status;
    }
    validate_http_status(response.status)?;
    Ok(ConnectorHttpResponse::new(
        response.status,
        response.headers,
        response.body,
    ))
}

fn validate_http_status(status: u16) -> Result<(), ConnectorError> {
    if (100..=599).contains(&status) {
        return Ok(());
    }
    Err(ConnectorError::HarnRuntime(format!(
        "NormalizeResult HTTP status {status} is outside 100..=599"
    )))
}

fn default_ok_status() -> u16 {
    200
}

async fn load_module_runtime(
    module_path: &Path,
) -> Result<(Vm, BTreeMap<String, Rc<VmClosure>>), ConnectorError> {
    let mut base_vm = Vm::new();
    register_vm_stdlib(&mut base_vm);
    let store_base = module_path.parent().unwrap_or_else(|| Path::new("."));
    crate::store::register_store_builtins(&mut base_vm, store_base);
    if let Some(parent) = module_path.parent() {
        base_vm.set_source_dir(parent);
        base_vm.set_project_root(parent);
    }
    let exports = base_vm
        .load_module_exports(module_path)
        .await
        .map_err(vm_error_to_connector)?;
    Ok((base_vm, exports))
}

async fn load_runtime_with_ctx(
    module_path: &Path,
    ctx: ConnectorCtx,
) -> Result<LocalHarnConnectorRuntime, ConnectorError> {
    let (base_vm, exports) = load_module_runtime(module_path).await?;
    Ok(LocalHarnConnectorRuntime {
        base_vm,
        exports,
        ctx,
    })
}

async fn required_export_call(
    base_vm: &Vm,
    exports: &BTreeMap<String, Rc<VmClosure>>,
    name: &str,
    args: &[VmValue],
) -> Result<VmValue, ConnectorError> {
    let Some(closure) = exports.get(name) else {
        return Err(ConnectorError::HarnRuntime(format!(
            "connector module is missing required export '{name}'"
        )));
    };
    let mut child = base_vm.child_vm_for_host();
    child
        .call_closure_pub(closure, args)
        .await
        .map_err(vm_error_to_connector)
}

async fn call_provider_export(
    runtime: &LocalHarnConnectorRuntime,
    name: &str,
    args: Vec<JsonValue>,
    required: bool,
    policy: Option<CapabilityPolicy>,
) -> Result<Option<JsonValue>, ConnectorError> {
    let Some(closure) = runtime.exports.get(name).cloned() else {
        if required {
            return Err(ConnectorError::HarnRuntime(format!(
                "connector module is missing required export '{name}'"
            )));
        }
        return Ok(None);
    };
    let mut child_vm = runtime.base_vm.child_vm_for_host();
    let _policy_guard = ConnectorExecutionPolicyGuard::push(policy);
    let _ctx_guard = ActiveHarnConnectorCtxGuard::push(runtime.ctx.clone());
    let vm_args = args
        .into_iter()
        .map(|value| json_result_to_vm_value(&value))
        .collect::<Vec<_>>();
    let result = child_vm.call_closure_pub(&closure, &vm_args).await;
    result
        .map(|value| Some(vm_value_to_json(&value)))
        .map_err(|error| vm_error_to_connector_for_export(name, error))
}

pub(crate) fn active_harn_connector_ctx() -> Option<ConnectorCtx> {
    ACTIVE_HARN_CONNECTOR_CTX.with(|slot| slot.borrow().last().cloned())
}

struct ConnectorExecutionPolicyGuard {
    active: bool,
}

impl ConnectorExecutionPolicyGuard {
    fn push(policy: Option<CapabilityPolicy>) -> Self {
        if let Some(policy) = policy {
            crate::orchestration::push_execution_policy(policy);
            Self { active: true }
        } else {
            Self { active: false }
        }
    }
}

impl Drop for ConnectorExecutionPolicyGuard {
    fn drop(&mut self) {
        if self.active {
            crate::orchestration::pop_execution_policy();
        }
    }
}

struct ActiveHarnConnectorCtxGuard;

impl ActiveHarnConnectorCtxGuard {
    fn push(ctx: ConnectorCtx) -> Self {
        ACTIVE_HARN_CONNECTOR_CTX.with(|slot| slot.borrow_mut().push(ctx));
        Self
    }
}

impl Drop for ActiveHarnConnectorCtxGuard {
    fn drop(&mut self) {
        ACTIVE_HARN_CONNECTOR_CTX.with(|slot| {
            slot.borrow_mut().pop();
        });
    }
}

fn vm_error_to_connector(error: VmError) -> ConnectorError {
    ConnectorError::HarnRuntime(vm_error_message(error))
}

fn vm_error_to_connector_for_export(export: &str, error: VmError) -> ConnectorError {
    match &error {
        VmError::CategorizedError {
            category: ErrorCategory::ToolRejected,
            message,
        } => ConnectorError::HarnRuntime(format!(
            "connector export '{export}' violated effect policy: {message}"
        )),
        _ => vm_error_to_connector(error),
    }
}

fn connector_error_to_client(error: ConnectorError) -> ClientError {
    match error {
        ConnectorError::HarnRuntime(message) => client_error_from_message(message),
        other => ClientError::Other(other.to_string()),
    }
}

fn client_error_from_message(message: String) -> ClientError {
    if let Some(detail) = message.strip_prefix("method_not_found:") {
        return ClientError::MethodNotFound(detail.trim().to_string());
    }
    if let Some(detail) = message.strip_prefix("invalid_args:") {
        return ClientError::InvalidArgs(detail.trim().to_string());
    }
    if let Some(detail) = message.strip_prefix("rate_limited:") {
        return ClientError::RateLimited(detail.trim().to_string());
    }
    ClientError::Other(message)
}

fn vm_error_message(error: VmError) -> String {
    match error {
        VmError::Thrown(VmValue::String(message)) => message.to_string(),
        VmError::Thrown(value) => vm_value_to_json(&value).to_string(),
        other => other.to_string(),
    }
}

fn parse_provider_id(value: VmValue) -> Result<ProviderId, ConnectorError> {
    match value {
        VmValue::String(value) if !value.trim().is_empty() => {
            Ok(ProviderId::from(value.to_string()))
        }
        other => Err(ConnectorError::HarnRuntime(format!(
            "provider_id() must return a non-empty string, got {}",
            other.type_name()
        ))),
    }
}

fn parse_kinds(value: VmValue) -> Result<Vec<TriggerKind>, ConnectorError> {
    match value {
        VmValue::List(items) => items
            .iter()
            .map(|item| match item {
                VmValue::String(kind) if !kind.trim().is_empty() => {
                    Ok(TriggerKind::from(kind.to_string()))
                }
                other => Err(ConnectorError::HarnRuntime(format!(
                    "kinds() must return a list of strings, found {}",
                    other.type_name()
                ))),
            })
            .collect(),
        other => Err(ConnectorError::HarnRuntime(format!(
            "kinds() must return a list, got {}",
            other.type_name()
        ))),
    }
}

fn parse_payload_schema(value: VmValue) -> Result<ProviderPayloadSchema, ConnectorError> {
    let json = vm_value_to_json(&value);
    if let Some(name) = json.as_str() {
        return Ok(ProviderPayloadSchema::named(name.to_string()));
    }
    serde_json::from_value(json).map_err(|error| {
        ConnectorError::HarnRuntime(format!(
            "payload_schema() must return {{ harn_schema_name, json_schema? }}: {error}"
        ))
    })
}

fn parse_rfc3339(value: &str) -> Result<OffsetDateTime, ConnectorError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))
}

fn binding_to_json(binding: &TriggerBinding) -> JsonValue {
    json!({
        "provider": binding.provider.as_str(),
        "kind": binding.kind.as_str(),
        "id": binding.binding_id,
        "dedupe_key": binding.dedupe_key,
        "dedupe_retention_days": binding.dedupe_retention_days,
        "config": binding.config,
    })
}

fn resolve_poll_binding(
    binding: &TriggerBinding,
) -> Result<ResolvedHarnPollBinding, ConnectorError> {
    let config: HarnPollBindingConfigEnvelope = if binding.config.is_null() {
        HarnPollBindingConfigEnvelope::default()
    } else {
        serde_json::from_value(binding.config.clone()).map_err(|error| {
            ConnectorError::Activation(format!(
                "poll binding '{}' has invalid connector config: {error}",
                binding.binding_id
            ))
        })?
    };
    let interval = duration_from_config(
        config.poll.interval.as_deref(),
        config.poll.interval_ms,
        config.poll.interval_secs,
    )
    .transpose()
    .map_err(|error| {
        ConnectorError::Activation(format!(
            "poll binding '{}' interval {error}",
            binding.binding_id
        ))
    })?
    .unwrap_or(DEFAULT_POLL_INTERVAL);
    if interval.is_zero() {
        return Err(ConnectorError::Activation(format!(
            "poll binding '{}' requires interval > 0",
            binding.binding_id
        )));
    }
    let jitter = duration_from_config(
        config.poll.jitter.as_deref(),
        config.poll.jitter_ms,
        config.poll.jitter_secs,
    )
    .transpose()
    .map_err(|error| {
        ConnectorError::Activation(format!(
            "poll binding '{}' jitter {error}",
            binding.binding_id
        ))
    })?
    .unwrap_or(StdDuration::ZERO);
    let state_key = config
        .poll
        .state_key
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| binding.binding_id.clone());
    let lease_id = config
        .poll
        .lease_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{}:{}", binding.provider.as_str(), binding.binding_id));
    let tenant_id = config
        .poll
        .tenant_id
        .filter(|value| !value.trim().is_empty())
        .map(TenantId::new);
    Ok(ResolvedHarnPollBinding {
        binding: binding.clone(),
        interval,
        jitter,
        state_key,
        lease_id,
        tenant_id,
        max_batch_size: config.poll.max_batch_size,
    })
}

fn duration_from_config(
    text: Option<&str>,
    millis: Option<u64>,
    secs: Option<u64>,
) -> Option<Result<StdDuration, String>> {
    if let Some(text) = text {
        return Some(parse_duration(text));
    }
    if let Some(millis) = millis {
        return Some(Ok(StdDuration::from_millis(millis)));
    }
    secs.map(|secs| Ok(StdDuration::from_secs(secs)))
}

fn parse_duration(raw: &str) -> Result<StdDuration, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("cannot be empty".to_string());
    }
    let (amount, unit) = trimmed
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(index, _)| (&trimmed[..index], trimmed[index..].trim()))
        .unwrap_or((trimmed, "ms"));
    let amount = amount
        .parse::<u64>()
        .map_err(|_| format!("'{raw}' is not a valid duration"))?;
    match unit {
        "ms" => Ok(StdDuration::from_millis(amount)),
        "s" => Ok(StdDuration::from_secs(amount)),
        "m" => Ok(StdDuration::from_secs(amount.saturating_mul(60))),
        "h" => Ok(StdDuration::from_secs(amount.saturating_mul(60 * 60))),
        _ => Err(format!(
            "'{raw}' uses unsupported unit '{unit}'; expected ms, s, m, or h"
        )),
    }
}

async fn run_poll_loop(
    provider_id: ProviderId,
    worker: Arc<HarnConnectorWorker>,
    ctx: ConnectorCtx,
    binding: ResolvedHarnPollBinding,
    shutdown: Arc<PollShutdownSignal>,
) -> Result<(), ConnectorError> {
    let mut first_tick = true;
    loop {
        if shutdown.is_stopped() {
            return Ok(());
        }
        if first_tick {
            first_tick = false;
        } else {
            let delay = binding
                .interval
                .saturating_add(deterministic_jitter(&binding));
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            tokio::select! {
                _ = &mut sleep => {}
                _ = shutdown.cancelled() => return Ok(()),
            }
        }
        if shutdown.is_stopped() {
            return Ok(());
        }
        let tick = run_poll_tick(
            &provider_id,
            worker.clone(),
            &ctx,
            &binding,
            shutdown.clone(),
        );
        tokio::pin!(tick);
        tokio::select! {
            result = &mut tick => result?,
            _ = shutdown.cancelled() => return Ok(()),
        }
    }
}

async fn run_poll_tick(
    provider_id: &ProviderId,
    worker: Arc<HarnConnectorWorker>,
    ctx: &ConnectorCtx,
    binding: &ResolvedHarnPollBinding,
    shutdown: Arc<PollShutdownSignal>,
) -> Result<(), ConnectorError> {
    let prior = load_poll_state(
        ctx.event_log.as_ref(),
        provider_id.as_str(),
        &binding.binding.binding_id,
        &binding.state_key,
    )
    .await?;
    let tick_at = OffsetDateTime::now_utc();
    let input = json!({
        "provider_id": provider_id.as_str(),
        "binding": binding_to_json(&binding.binding),
        "binding_id": binding.binding.binding_id,
        "state_key": binding.state_key,
        "tick_at": tick_at.format(&Rfc3339).ok(),
        "cursor": prior.as_ref().and_then(|record| record.cursor.clone()),
        "state": prior.as_ref().and_then(|record| record.state.clone()),
        "tenant_id": binding.tenant_id.as_ref().map(|tenant| tenant.0.clone()),
        "lease": {
            "id": binding.lease_id,
            "tenant_id": binding.tenant_id.as_ref().map(|tenant| tenant.0.clone()),
        },
        "max_batch_size": binding.max_batch_size,
    });
    let raw_result = worker
        .call_export("poll_tick", vec![input], true)
        .await?
        .expect("required export returns a value");
    if shutdown.is_stopped() {
        return Ok(());
    }
    let result = parse_poll_tick_result(raw_result)?;
    let events = result
        .events
        .into_iter()
        .take(binding.max_batch_size.unwrap_or(usize::MAX))
        .collect::<Vec<_>>();
    for normalized in events {
        let event = trigger_event_from_normalized(
            provider_id,
            normalized,
            tick_at,
            binding.tenant_id.clone(),
            None,
        )?;
        match postprocess_normalized_event(
            ctx.inbox.as_ref(),
            &binding.binding.binding_id,
            binding.binding.dedupe_key.is_some(),
            StdDuration::from_secs(
                u64::from(binding.binding.dedupe_retention_days.max(1)) * 24 * 60 * 60,
            ),
            event,
        )
        .await?
        {
            PostNormalizeOutcome::DuplicateDropped => {
                ctx.metrics
                    .record_trigger_deduped(&binding.binding.binding_id, "inbox_duplicate");
            }
            PostNormalizeOutcome::Ready(event) => {
                enqueue_poll_event(ctx, &binding.binding.binding_id, *event).await?;
            }
        }
    }
    if result.cursor.is_some() || result.state.is_some() {
        persist_poll_state(
            ctx.event_log.as_ref(),
            &HarnPollStateRecord {
                provider: provider_id.as_str().to_string(),
                binding_id: binding.binding.binding_id.clone(),
                state_key: binding.state_key.clone(),
                cursor: result.cursor,
                state: result.state,
                updated_at: OffsetDateTime::now_utc(),
            },
        )
        .await?;
    }
    Ok(())
}

fn deterministic_jitter(binding: &ResolvedHarnPollBinding) -> StdDuration {
    if binding.jitter.is_zero() {
        return StdDuration::ZERO;
    }
    let max_ms = binding.jitter.as_millis();
    if max_ms == 0 {
        return StdDuration::ZERO;
    }
    let seed = binding
        .binding
        .binding_id
        .bytes()
        .chain(binding.state_key.bytes())
        .fold(0u128, |acc, byte| {
            acc.wrapping_mul(16777619) ^ u128::from(byte)
        });
    StdDuration::from_millis((seed % (max_ms + 1)).min(u128::from(u64::MAX)) as u64)
}

fn parse_poll_tick_result(value: JsonValue) -> Result<HarnPollTickResult, ConnectorError> {
    if value.is_array() {
        let events: Vec<HarnNormalizeResult> =
            serde_json::from_value(value).map_err(poll_result_error)?;
        return Ok(HarnPollTickResult {
            events,
            cursor: None,
            state: None,
        });
    }
    serde_json::from_value(value).map_err(poll_result_error)
}

fn poll_result_error(error: serde_json::Error) -> ConnectorError {
    ConnectorError::HarnRuntime(format!(
        "poll_tick(ctx) returned an invalid result: {error}"
    ))
}

fn trigger_event_from_normalized(
    provider_id: &ProviderId,
    normalized: HarnNormalizeResult,
    received_at: OffsetDateTime,
    fallback_tenant_id: Option<TenantId>,
    raw_body: Option<Vec<u8>>,
) -> Result<TriggerEvent, ConnectorError> {
    let occurred_at = normalized
        .occurred_at
        .as_deref()
        .map(parse_rfc3339)
        .transpose()?;
    let tenant_id = normalized.tenant_id.map(TenantId::new);
    let source_headers = normalized.headers.unwrap_or_default();
    let headers = redact_headers(&source_headers, &HeaderRedactionPolicy::default());
    let provider_payload = ProviderPayload::normalize(
        provider_id,
        &normalized.kind,
        &source_headers,
        normalized.payload,
    )
    .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?;
    Ok(TriggerEvent {
        id: TriggerEventId::new(),
        provider: provider_id.clone(),
        kind: normalized.kind,
        received_at,
        occurred_at,
        dedupe_key: normalized.dedupe_key,
        trace_id: TraceId::new(),
        tenant_id: tenant_id.or(fallback_tenant_id),
        headers,
        batch: normalized.batch,
        provider_payload,
        raw_body,
        signature_status: normalized
            .signature_status
            .unwrap_or(SignatureStatus::Unsigned),
        dedupe_claimed: false,
    })
}

async fn enqueue_poll_event(
    ctx: &ConnectorCtx,
    binding_id: &str,
    event: TriggerEvent,
) -> Result<(), ConnectorError> {
    let topic = Topic::new(crate::triggers::TRIGGER_INBOX_ENVELOPES_TOPIC)
        .expect("trigger inbox envelopes topic must be valid");
    let payload = serde_json::to_value(InboxEnvelope {
        trigger_id: Some(binding_id.to_string()),
        binding_version: None,
        event: event.clone(),
    })
    .map_err(ConnectorError::from)?;
    let headers = BTreeMap::from([
        ("event_id".to_string(), event.id.0.clone()),
        ("trace_id".to_string(), event.trace_id.0.clone()),
        ("provider".to_string(), event.provider.as_str().to_string()),
        ("kind".to_string(), event.kind.clone()),
        ("trigger_id".to_string(), binding_id.to_string()),
    ]);
    ctx.event_log
        .append(
            &topic,
            LogEvent::new("event_ingested", payload).with_headers(headers),
        )
        .await
        .map(|_| ())
        .map_err(ConnectorError::from)
}

async fn load_poll_state(
    event_log: &crate::event_log::AnyEventLog,
    provider: &str,
    binding_id: &str,
    state_key: &str,
) -> Result<Option<HarnPollStateRecord>, ConnectorError> {
    let topic = Topic::new(HARN_CONNECTOR_POLL_STATE_TOPIC)
        .expect("Harn connector poll state topic is valid");
    let records = event_log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(ConnectorError::from)?;
    let mut latest = None;
    for (_, event) in records {
        if event.kind != HARN_CONNECTOR_POLL_STATE_KIND {
            continue;
        }
        let record: HarnPollStateRecord =
            serde_json::from_value(event.payload).map_err(ConnectorError::from)?;
        if record.provider == provider
            && record.binding_id == binding_id
            && record.state_key == state_key
        {
            latest = Some(record);
        }
    }
    Ok(latest)
}

async fn persist_poll_state(
    event_log: &crate::event_log::AnyEventLog,
    record: &HarnPollStateRecord,
) -> Result<(), ConnectorError> {
    let topic = Topic::new(HARN_CONNECTOR_POLL_STATE_TOPIC)
        .expect("Harn connector poll state topic is valid");
    let payload = serde_json::to_value(record).map_err(ConnectorError::from)?;
    event_log
        .append(
            &topic,
            LogEvent::new(HARN_CONNECTOR_POLL_STATE_KIND, payload),
        )
        .await
        .map(|_| ())
        .map_err(ConnectorError::from)
}

fn raw_inbound_to_json(raw: &crate::RawInbound) -> JsonValue {
    let binding_id = raw
        .metadata
        .get("binding_id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let binding_version = raw
        .metadata
        .get("binding_version")
        .and_then(JsonValue::as_u64);
    let binding_path = raw
        .metadata
        .get("path")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let mut payload = json!({
        "kind": raw.kind,
        "headers": raw.headers,
        "query": raw.query,
        "received_at": raw.received_at.format(&Rfc3339).ok(),
        "occurred_at": raw.occurred_at.and_then(|value| value.format(&Rfc3339).ok()),
        "tenant_id": raw.tenant_id.as_ref().map(|tenant| tenant.0.clone()),
        "binding_id": binding_id,
        "binding_version": binding_version,
        "binding_path": binding_path,
        "metadata": raw.metadata,
    });
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "body_base64".to_string(),
            JsonValue::String(base64::engine::general_purpose::STANDARD.encode(&raw.body)),
        );
        object.insert(
            "body_text".to_string(),
            std::str::from_utf8(&raw.body)
                .map(|value| JsonValue::String(value.to_string()))
                .unwrap_or(JsonValue::Null),
        );
        if let Ok(body_json) = serde_json::from_slice::<JsonValue>(&raw.body) {
            object.insert("body_json".to_string(), body_json);
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    use crate::event_log::{AnyEventLog, MemoryEventLog};
    use crate::secrets::{
        RotationHandle, SecretBytes, SecretError, SecretId, SecretMeta, SecretProvider,
    };
    use crate::{InboxIndex, MetricsRegistry, RateLimiterFactory};

    fn raw_inbound(body: JsonValue) -> crate::RawInbound {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        raw_inbound_with_headers(body, headers)
    }

    fn raw_inbound_with_headers(
        body: JsonValue,
        headers: BTreeMap<String, String>,
    ) -> crate::RawInbound {
        let mut raw = crate::RawInbound::new(
            "",
            headers,
            serde_json::to_vec(&body).expect("json body serializes"),
        );
        raw.received_at = OffsetDateTime::parse("2026-04-22T12:34:56Z", &Rfc3339).unwrap();
        raw
    }

    async fn normalize_with_harn_connector(
        source: &str,
        body: JsonValue,
        headers: BTreeMap<String, String>,
    ) -> TriggerEvent {
        let (_dir, module_path) = write_connector(source);
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let mut connector = HarnConnector::load(&module_path).await.unwrap();
        connector.init(ctx(log).await).await.unwrap();
        let result = connector
            .normalize_inbound_result(raw_inbound_with_headers(body, headers))
            .await
            .unwrap();
        connector.shutdown(StdDuration::ZERO).await.unwrap();
        let ConnectorNormalizeResult::Event(event) = result else {
            panic!("expected normalized event");
        };
        *event
    }

    fn event_value(kind: &str, dedupe_key: &str, id: &str) -> JsonValue {
        json!({
            "kind": kind,
            "occurred_at": "2026-04-22T12:30:00Z",
            "dedupe_key": dedupe_key,
            "payload": {
                "id": id,
                "type": kind,
            },
            "signature_status": {
                "state": "verified",
            },
        })
    }

    #[test]
    fn normalize_result_v1_event_parses_normal_event() {
        let provider = ProviderId::new("webhook");
        let raw = raw_inbound(json!({"id": "evt-1"}));
        let result = parse_normalize_result(
            &provider,
            &raw,
            json!({
                "type": "event",
                "event": event_value("webhook.received", "webhook:evt-1", "evt-1"),
            }),
        )
        .unwrap();

        let ConnectorNormalizeResult::Event(event) = result else {
            panic!("expected event result");
        };
        assert_eq!(event.provider, provider);
        assert_eq!(event.kind, "webhook.received");
        assert_eq!(event.dedupe_key, "webhook:evt-1");
        assert_eq!(event.signature_status, SignatureStatus::Verified);
        assert!(event.raw_body.is_some());
    }

    #[test]
    fn normalize_result_v1_batch_parses_multiple_events() {
        let provider = ProviderId::new("webhook");
        let raw = raw_inbound(json!({"items": [{"id": "a"}, {"id": "b"}]}));
        let result = parse_normalize_result(
            &provider,
            &raw,
            json!({
                "type": "batch",
                "events": [
                    event_value("webhook.received", "webhook:a", "a"),
                    event_value("webhook.received", "webhook:b", "b"),
                ],
            }),
        )
        .unwrap();

        let ConnectorNormalizeResult::Batch(events) = result else {
            panic!("expected batch result");
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].dedupe_key, "webhook:a");
        assert_eq!(events[1].dedupe_key, "webhook:b");
    }

    #[test]
    fn normalize_result_v1_immediate_response_covers_slack_url_verification_fixture() {
        let provider = ProviderId::new("slack");
        let raw = raw_inbound(json!({
            "type": "url_verification",
            "challenge": "challenge-token",
        }));
        let result = parse_normalize_result(
            &provider,
            &raw,
            json!({
                "type": "immediate_response",
                "immediate_response": {
                    "status": 200,
                    "headers": {
                        "content-type": "text/plain; charset=utf-8",
                    },
                    "body": "challenge-token",
                },
            }),
        )
        .unwrap();

        let ConnectorNormalizeResult::ImmediateResponse { response, events } = result else {
            panic!("expected immediate_response result");
        };
        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers.get("content-type").map(String::as_str),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            response.body,
            JsonValue::String("challenge-token".to_string())
        );
        assert!(events.is_empty());
    }

    #[test]
    fn normalize_result_v1_reject_parses_http_rejection() {
        let provider = ProviderId::new("webhook");
        let raw = raw_inbound(json!({"id": "evt-1"}));
        let result = parse_normalize_result(
            &provider,
            &raw,
            json!({
                "type": "reject",
                "status": 403,
                "body": {
                    "error": "verification_failed",
                },
            }),
        )
        .unwrap();

        let ConnectorNormalizeResult::Reject(response) = result else {
            panic!("expected reject result");
        };
        assert_eq!(response.status, 403);
        assert_eq!(response.body["error"], "verification_failed");
    }

    #[test]
    fn legacy_direct_normalize_result_still_parses_during_transition() {
        let provider = ProviderId::new("webhook");
        let raw = raw_inbound(json!({"id": "legacy"}));
        let result = parse_normalize_result(
            &provider,
            &raw,
            event_value("webhook.received", "webhook:legacy", "legacy"),
        )
        .unwrap();

        let ConnectorNormalizeResult::Event(event) = result else {
            panic!("expected legacy event result");
        };
        assert_eq!(event.kind, "webhook.received");
        assert_eq!(event.dedupe_key, "webhook:legacy");
    }

    struct EmptySecretProvider;

    #[async_trait]
    impl SecretProvider for EmptySecretProvider {
        async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
            Err(SecretError::NotFound {
                provider: self.namespace().to_string(),
                id: id.clone(),
            })
        }

        async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
            Ok(())
        }

        async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
            Ok(RotationHandle {
                provider: self.namespace().to_string(),
                id: id.clone(),
                from_version: None,
                to_version: None,
            })
        }

        async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
            Ok(Vec::new())
        }

        fn namespace(&self) -> &str {
            "test"
        }

        fn supports_versions(&self) -> bool {
            false
        }
    }

    struct StaticSecretProvider;

    #[async_trait]
    impl SecretProvider for StaticSecretProvider {
        async fn get(&self, id: &SecretId) -> Result<SecretBytes, SecretError> {
            if id.to_string().starts_with("test/signing-secret") {
                return Ok(SecretBytes::from("local-secret"));
            }
            Err(SecretError::NotFound {
                provider: self.namespace().to_string(),
                id: id.clone(),
            })
        }

        async fn put(&self, _id: &SecretId, _value: SecretBytes) -> Result<(), SecretError> {
            Ok(())
        }

        async fn rotate(&self, id: &SecretId) -> Result<RotationHandle, SecretError> {
            Ok(RotationHandle {
                provider: self.namespace().to_string(),
                id: id.clone(),
                from_version: None,
                to_version: None,
            })
        }

        async fn list(&self, _prefix: &SecretId) -> Result<Vec<SecretMeta>, SecretError> {
            Ok(Vec::new())
        }

        fn namespace(&self) -> &str {
            "test"
        }

        fn supports_versions(&self) -> bool {
            false
        }
    }

    async fn ctx(log: Arc<AnyEventLog>) -> ConnectorCtx {
        let metrics = Arc::new(MetricsRegistry::default());
        ConnectorCtx {
            inbox: Arc::new(InboxIndex::new(log.clone(), metrics.clone()).await.unwrap()),
            event_log: log,
            secrets: Arc::new(EmptySecretProvider),
            metrics,
            rate_limiter: Arc::new(RateLimiterFactory::default()),
        }
    }

    async fn ctx_with_secrets(
        log: Arc<AnyEventLog>,
        secrets: Arc<dyn SecretProvider>,
    ) -> ConnectorCtx {
        let metrics = Arc::new(MetricsRegistry::default());
        ConnectorCtx {
            inbox: Arc::new(InboxIndex::new(log.clone(), metrics.clone()).await.unwrap()),
            event_log: log,
            secrets,
            metrics,
            rate_limiter: Arc::new(RateLimiterFactory::default()),
        }
    }

    fn write_connector(source: &str) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connector.harn");
        std::fs::write(&path, source).unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn normalize_inbound_default_policy_allows_local_hot_path_work() {
        let (_dir, module_path) = write_connector(
            r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }

pub fn normalize_inbound(raw) {
  let decoded = base64_decode(raw.body_base64)
  let body = json_parse(decoded)
  let secret = secret_get("test/signing-secret")
  let signature = hmac_sha256(secret, decoded)
  metrics_inc("normalize_ok")
  return {
    type: "event",
    event: {
      kind: "webhook.received",
      dedupe_key: "webhook:" + body.id,
      payload: {id: body.id, signature: signature},
      signature_status: {state: "verified"},
    },
  }
}
"#,
        );
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let mut connector = HarnConnector::load(&module_path).await.unwrap();
        connector
            .init(ctx_with_secrets(log, Arc::new(StaticSecretProvider)).await)
            .await
            .unwrap();
        let result = connector
            .normalize_inbound_result(raw_inbound(json!({"id": "evt-1"})))
            .await
            .unwrap();
        connector.shutdown(StdDuration::ZERO).await.unwrap();

        let ConnectorNormalizeResult::Event(event) = result else {
            panic!("expected normalized event");
        };
        assert_eq!(event.kind, "webhook.received");
        assert_eq!(event.signature_status, SignatureStatus::Verified);
        match &event.provider_payload {
            ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::Webhook(
                payload,
            )) => assert_eq!(payload.raw["id"], "evt-1"),
            other => panic!("unexpected provider payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn pure_harn_sunset_connectors_preserve_builtin_provider_payload_shapes() {
        let github_event = normalize_with_harn_connector(
            r#"
pub fn provider_id() { return "github" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GitHubEventPayload" }

pub fn normalize_inbound(raw) {
  let body = raw.body_json
  return {
    type: "event",
    event: {
      kind: raw.headers["X-GitHub-Event"],
      dedupe_key: raw.headers["X-GitHub-Delivery"],
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}
"#,
            json!({
                "action": "opened",
                "installation": {"id": 101},
                "issue": {"number": 42, "title": "Contract drift"}
            }),
            BTreeMap::from([
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-GitHub-Event".to_string(), "issues".to_string()),
                ("X-GitHub-Delivery".to_string(), "delivery-gh-1".to_string()),
            ]),
        )
        .await;
        assert_eq!(github_event.kind, "issues");
        assert_eq!(github_event.dedupe_key, "delivery-gh-1");
        assert_eq!(github_event.signature_status, SignatureStatus::Verified);
        match &github_event.provider_payload {
            ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::GitHub(
                crate::triggers::GitHubEventPayload::Issues(payload),
            )) => {
                assert_eq!(payload.common.event, "issues");
                assert_eq!(payload.common.action.as_deref(), Some("opened"));
                assert_eq!(payload.common.delivery_id.as_deref(), Some("delivery-gh-1"));
                assert_eq!(payload.common.installation_id, Some(101));
                assert_eq!(payload.issue["number"], 42);
            }
            other => panic!("unexpected github payload: {other:?}"),
        }

        let slack_event = normalize_with_harn_connector(
            r#"
pub fn provider_id() { return "slack" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "SlackEventPayload" }

pub fn normalize_inbound(raw) {
  let body = raw.body_json
  return {
    type: "event",
    event: {
      kind: body.event.type + "." + body.event.channel_type,
      dedupe_key: "slack:" + body.event_id,
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}
"#,
            json!({
                "team_id": "T123ABC456",
                "api_app_id": "A123ABC456",
                "type": "event_callback",
                "event_id": "Ev123MESSAGE",
                "event": {
                    "type": "message",
                    "user": "U123ABC456",
                    "text": "hello from a channel",
                    "ts": "1715000000.000100",
                    "channel": "C123ABC456",
                    "channel_type": "channel",
                    "event_ts": "1715000000.000100"
                }
            }),
            BTreeMap::from([("Content-Type".to_string(), "application/json".to_string())]),
        )
        .await;
        assert_eq!(slack_event.kind, "message.channel");
        match &slack_event.provider_payload {
            ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::Slack(
                payload,
            )) => match payload.as_ref() {
                crate::triggers::SlackEventPayload::Message(message) => {
                    assert_eq!(message.common.event, "message.channel");
                    assert_eq!(message.common.event_id.as_deref(), Some("Ev123MESSAGE"));
                    assert_eq!(message.common.team_id.as_deref(), Some("T123ABC456"));
                    assert_eq!(message.common.channel_id.as_deref(), Some("C123ABC456"));
                    assert_eq!(message.channel_type.as_deref(), Some("channel"));
                    assert_eq!(message.text.as_deref(), Some("hello from a channel"));
                }
                other => panic!("unexpected slack variant: {other:?}"),
            },
            other => panic!("unexpected slack payload: {other:?}"),
        }

        let linear_event = normalize_with_harn_connector(
            r#"
pub fn provider_id() { return "linear" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "LinearEventPayload" }

pub fn normalize_inbound(raw) {
  let body = raw.body_json
  return {
    type: "event",
    event: {
      kind: "issue." + body.action,
      dedupe_key: raw.headers["Linear-Delivery"],
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}
"#,
            json!({
                "action": "update",
                "type": "Issue",
                "organizationId": "org_123",
                "webhookTimestamp": 1715000000000i64,
                "webhookId": "wh_123",
                "actor": {"id": "user_1", "name": "Ada"},
                "data": {"id": "ISS-1", "title": "Fix Linear connector"},
                "updatedFrom": {"title": "Previous title", "labelIds": ["lbl_1"]}
            }),
            BTreeMap::from([
                ("Content-Type".to_string(), "application/json".to_string()),
                (
                    "Linear-Delivery".to_string(),
                    "delivery-linear-1".to_string(),
                ),
            ]),
        )
        .await;
        assert_eq!(linear_event.kind, "issue.update");
        match &linear_event.provider_payload {
            ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::Linear(
                crate::triggers::LinearEventPayload::Issue(issue),
            )) => {
                assert_eq!(issue.common.event, "issue");
                assert_eq!(issue.common.action.as_deref(), Some("update"));
                assert_eq!(
                    issue.common.delivery_id.as_deref(),
                    Some("delivery-linear-1")
                );
                assert_eq!(issue.issue["id"], "ISS-1");
                assert!(issue.changes.iter().any(|change| matches!(
                    change,
                    crate::triggers::event::LinearIssueChange::Title { previous: Some(value) }
                        if value == "Previous title"
                )));
            }
            other => panic!("unexpected linear payload: {other:?}"),
        }

        let notion_event = normalize_with_harn_connector(
            r#"
pub fn provider_id() { return "notion" }
pub fn kinds() { return ["webhook", "poll"] }
pub fn payload_schema() { return "NotionEventPayload" }

pub fn normalize_inbound(raw) {
  let body = raw.body_json
  return {
    type: "event",
    event: {
      kind: body.type,
      dedupe_key: "notion:" + body.entity.id,
      payload: body,
      signature_status: {state: "verified"},
    },
  }
}
"#,
            json!({
                "id": "evt_1",
                "type": "page.content_updated",
                "workspace_id": "ws_1",
                "subscription_id": "sub_1",
                "integration_id": "int_1",
                "entity": {"id": "page_1", "type": "page"},
                "api_version": "2022-06-28"
            }),
            BTreeMap::from([
                ("Content-Type".to_string(), "application/json".to_string()),
                ("request-id".to_string(), "req_123".to_string()),
            ]),
        )
        .await;
        assert_eq!(notion_event.kind, "page.content_updated");
        match &notion_event.provider_payload {
            ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::Notion(
                payload,
            )) => {
                assert_eq!(payload.event, "page.content_updated");
                assert_eq!(payload.request_id.as_deref(), Some("req_123"));
                assert_eq!(payload.workspace_id.as_deref(), Some("ws_1"));
                assert_eq!(payload.entity_id.as_deref(), Some("page_1"));
                assert_eq!(payload.entity_type.as_deref(), Some("page"));
                assert_eq!(payload.subscription_id.as_deref(), Some("sub_1"));
            }
            other => panic!("unexpected notion payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn normalize_inbound_default_policy_denies_network_llm_and_file_effects() {
        for (label, source, expected) in [
            (
                "network",
                r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn normalize_inbound(_raw) {
  http_get("https://example.invalid")
  return {type: "reject", status: 400}
}
"#,
                "network ceiling",
            ),
            (
                "llm",
                r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn normalize_inbound(_raw) {
  llm_call("hello", nil, {provider: "mock"})
  return {type: "reject", status: 400}
}
"#,
                "LLM/network ceiling",
            ),
            (
                "file",
                r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn normalize_inbound(_raw) {
  read_file("ambient.txt")
  return {type: "reject", status: 400}
}
"#,
                "workspace.read_text ceiling",
            ),
        ] {
            let (_dir, module_path) = write_connector(source);
            let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
            let mut connector = HarnConnector::load(&module_path).await.unwrap();
            connector.init(ctx(log).await).await.unwrap();
            let error = connector
                .normalize_inbound_result(raw_inbound(json!({"id": label})))
                .await
                .unwrap_err();
            connector.shutdown(StdDuration::ZERO).await.unwrap();
            let message = error.to_string();
            assert!(
                message.contains("connector export 'normalize_inbound' violated effect policy"),
                "{label}: {message}"
            );
            assert!(message.contains(expected), "{label}: {message}");
        }
    }

    fn write_poll_connector() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("poll_connector.harn");
        std::fs::write(
            &path,
            r#"
pub fn provider_id() {
  return "webhook"
}

pub fn kinds() {
  return ["poll"]
}

pub fn payload_schema() {
  return "GenericWebhookPayload"
}

pub fn poll_tick(ctx) {
  var previous = 0
  if ctx.cursor != nil && ctx.cursor.count != nil {
    previous = ctx.cursor.count
  }
  let next = previous + 1
  return {
    cursor: {count: next},
    state: {last_lease_id: ctx.lease.id, tenant_id: ctx.tenant_id},
    events: [
      {
        kind: "webhook.poll",
        dedupe_key: "poll-" + to_string(next),
        payload: {
          count: next,
          previous: previous,
          max_batch_size: ctx.max_batch_size,
          tenant_id: ctx.tenant_id,
          lease_id: ctx.lease.id,
        },
      },
    ],
  }
}
"#,
        )
        .unwrap();
        (dir, path)
    }

    async fn read_topic(
        log: &Arc<AnyEventLog>,
        topic: &str,
    ) -> Vec<(u64, crate::event_log::LogEvent)> {
        let topic = Topic::new(topic).unwrap();
        log.read_range(&topic, None, usize::MAX).await.unwrap()
    }

    async fn wait_for_topic_count(log: &Arc<AnyEventLog>, topic: &str, expected: usize) {
        for _ in 0..1000 {
            if read_topic(log, topic).await.len() >= expected {
                return;
            }
            tokio::time::advance(StdDuration::from_millis(1)).await;
            tokio::task::yield_now().await;
        }
        panic!("topic {topic} did not reach {expected} records");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn poll_tick_emits_inbox_events_and_persists_cursor_state() {
        let (_dir, module_path) = write_poll_connector();
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(128)));
        let mut connector = HarnConnector::load(&module_path).await.unwrap();
        connector.init(ctx(log.clone()).await).await.unwrap();

        let mut binding = TriggerBinding::new(ProviderId::from("webhook"), "poll", "poll-source");
        binding.dedupe_key = Some("event.dedupe_key".to_string());
        binding.config = json!({
            "poll": {
                "interval_ms": 1000,
                "state_key": "tenant-a-source",
                "lease_id": "lease-a",
                "tenant_id": "tenant-a",
                "max_batch_size": 1,
            }
        });

        connector.activate(&[binding]).await.unwrap();
        wait_for_topic_count(&log, crate::triggers::TRIGGER_INBOX_ENVELOPES_TOPIC, 1).await;

        tokio::time::advance(StdDuration::from_millis(1000)).await;
        wait_for_topic_count(&log, crate::triggers::TRIGGER_INBOX_ENVELOPES_TOPIC, 2).await;
        connector.shutdown(StdDuration::ZERO).await.unwrap();

        let inbox = read_topic(&log, crate::triggers::TRIGGER_INBOX_ENVELOPES_TOPIC).await;
        let envelopes = inbox
            .into_iter()
            .map(|(_, event)| serde_json::from_value::<InboxEnvelope>(event.payload).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(envelopes[0].trigger_id.as_deref(), Some("poll-source"));
        assert_eq!(envelopes[0].event.dedupe_key, "poll-1");
        assert_eq!(envelopes[1].event.dedupe_key, "poll-2");
        assert_eq!(
            envelopes[1]
                .event
                .tenant_id
                .as_ref()
                .map(|tenant| tenant.0.as_str()),
            Some("tenant-a")
        );
        match &envelopes[1].event.provider_payload {
            ProviderPayload::Known(crate::triggers::event::KnownProviderPayload::Webhook(
                payload,
            )) => {
                assert_eq!(payload.raw["previous"], 1);
                assert_eq!(payload.raw["max_batch_size"], 1);
                assert_eq!(payload.raw["tenant_id"], "tenant-a");
                assert_eq!(payload.raw["lease_id"], "lease-a");
            }
            other => panic!("unexpected provider payload: {other:?}"),
        }

        let states = read_topic(&log, HARN_CONNECTOR_POLL_STATE_TOPIC).await;
        assert_eq!(states.len(), 2);
        let latest: HarnPollStateRecord =
            serde_json::from_value(states.last().unwrap().1.payload.clone()).unwrap();
        assert_eq!(latest.provider, "webhook");
        assert_eq!(latest.binding_id, "poll-source");
        assert_eq!(latest.state_key, "tenant-a-source");
        assert_eq!(latest.cursor.unwrap()["count"], 2);
        assert_eq!(latest.state.unwrap()["last_lease_id"], "lease-a");
    }

    #[tokio::test]
    async fn poll_binding_requires_poll_tick_export() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing_poll.harn");
        std::fs::write(
            &path,
            r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["poll"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
"#,
        )
        .unwrap();
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
        let mut connector = HarnConnector::load(&path).await.unwrap();
        connector.init(ctx(log).await).await.unwrap();
        let binding = TriggerBinding::new(ProviderId::from("webhook"), "poll", "poll-source");

        let error = connector.activate(&[binding]).await.unwrap_err();
        assert!(
            error.to_string().contains("does not export poll_tick(ctx)"),
            "{error}"
        );
        connector.shutdown(StdDuration::from_secs(1)).await.unwrap();
    }
}
