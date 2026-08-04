use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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

use crate::event_log::{EventLog, LogEvent, Topic};
use crate::llm::vm_value_to_json;
use crate::orchestration::CapabilityPolicy;
use crate::stdlib::register_vm_stdlib;
#[cfg(test)]
use crate::triggers::dispatcher::InboxEnvelope;
use crate::triggers::test_util::clock;
use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};
use crate::vm::Vm;
use crate::{
    postprocess_normalized_event, redact_headers, ClientError, Connector, ConnectorClient,
    ConnectorCtx, ConnectorError, ConnectorHttpResponse, ConnectorNormalizeResult,
    HarnConnectorEffectPolicies, HeaderRedactionPolicy, PostNormalizeOutcome, ProviderId,
    ProviderPayload, ProviderPayloadSchema, SignatureStatus, TenantId, TraceId, TriggerBinding,
    TriggerEvent, TriggerEventId, TriggerKind,
};

pub mod abi;

thread_local! {
    static ACTIVE_HARN_CONNECTOR_CTX: RefCell<Vec<ConnectorCtx>> = const { RefCell::new(Vec::new()) };
}

const HARN_CONNECTOR_POLL_STATE_TOPIC: &str = "connectors.harn.poll.state";
const HARN_CONNECTOR_POLL_STATE_KIND: &str = "harn.poll.state";
const DEFAULT_POLL_INTERVAL: StdDuration = StdDuration::from_mins(5);

// `ProviderPayloadSchema` carries types that aren't `Eq` (e.g. JSON values),
// so `HarnConnectorContract` can only be `PartialEq`.
#[allow(clippy::derive_partial_eq_without_eq)]
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
        policy: Option<Box<CapabilityPolicy>>,
        resp: oneshot::Sender<Result<Option<JsonValue>, String>>,
    },
    Shutdown {
        resp: oneshot::Sender<Result<(), String>>,
    },
}

struct LocalHarnConnectorRuntime {
    base_vm: Vm,
    exports: BTreeMap<String, Arc<VmClosure>>,
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
        let run = crate::egress::bind_policy_context(move || run_worker_loop(module_path, rx));
        let join = std::thread::Builder::new()
            .name(format!("harn-connector-{}", provider_id.as_str()))
            // The worker loop loads and executes a Harn connector module, so
            // this thread drives the VM and needs its stack.
            .stack_size(crate::RUNTIME_STACK_SIZE)
            .spawn(run)
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
                policy: policy.map(Box::new),
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
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            // Log + bail. The mpsc::Receiver drops with the thread, so the
            // first command send from the caller will surface a clean
            // channel-closed ConnectorError rather than a thread panic.
            crate::events::log_warn(
                "connector.worker.runtime_init_failed",
                &format!("tokio current-thread runtime build failed: {error}"),
            );
            return;
        }
    };
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
    abi::validate_runtime_export_abi(&exports)?;
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
            let payload_schema = self.payload_schema.clone();
            let tasks = poll_bindings
                .into_iter()
                .map(|binding| {
                    let worker = worker.clone();
                    let ctx = ctx.clone();
                    let shutdown = shutdown.clone();
                    let provider_id = self.provider_id.clone();
                    let payload_schema = payload_schema.clone();
                    tokio::spawn(async move {
                        if let Err(error) = run_poll_loop(
                            provider_id,
                            payload_schema,
                            worker,
                            ctx,
                            binding,
                            shutdown,
                        )
                        .await
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
            .ok_or_else(|| {
                ConnectorError::HarnRuntime(
                    "connector module 'normalize_inbound' export returned no value".to_string(),
                )
            })?;
        parse_normalize_result(&self.provider_id, &self.payload_schema, &raw, value)
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
    payload_schema: &ProviderPayloadSchema,
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
            parse_harn_normalized_event(provider_id, payload_schema, raw, event_value)
                .map(ConnectorNormalizeResult::event)
        }
        Some("batch") => {
            let events = parse_events_field(provider_id, payload_schema, raw, &value, "events")?;
            if events.is_empty() {
                return Err(ConnectorError::HarnRuntime(
                    "NormalizeResult batch must contain at least one event".to_string(),
                ));
            }
            Ok(ConnectorNormalizeResult::Batch(events))
        }
        Some("immediate_response") => {
            let response = parse_http_response(&value, "immediate_response", 200)?;
            let events = parse_optional_embedded_events(provider_id, payload_schema, raw, &value)?;
            Ok(ConnectorNormalizeResult::ImmediateResponse { response, events })
        }
        Some("reject") => {
            parse_http_response(&value, "reject", 400).map(ConnectorNormalizeResult::Reject)
        }
        Some(other) => Err(ConnectorError::HarnRuntime(format!(
            "unsupported NormalizeResult type '{other}'"
        ))),
        None => Err(ConnectorError::HarnRuntime(
            "connector normalize_inbound must return NormalizeResult v1 with a `type` field"
                .to_string(),
        )),
    }
}

fn parse_optional_embedded_events(
    provider_id: &ProviderId,
    payload_schema: &ProviderPayloadSchema,
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
        return parse_harn_normalized_event(provider_id, payload_schema, raw, event)
            .map(|event| vec![event]);
    }
    if has_events {
        return parse_events_field(provider_id, payload_schema, raw, value, "events");
    }
    Ok(Vec::new())
}

fn parse_events_field(
    provider_id: &ProviderId,
    payload_schema: &ProviderPayloadSchema,
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
        .map(|event| parse_harn_normalized_event(provider_id, payload_schema, raw, event))
        .collect()
}

fn parse_harn_normalized_event(
    provider_id: &ProviderId,
    payload_schema: &ProviderPayloadSchema,
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
    let provider_payload = ProviderPayload::Extension(crate::ExtensionProviderPayload {
        provider: provider_id.as_str().to_string(),
        schema_name: payload_schema.harn_schema_name.clone(),
        raw: normalized.payload,
    });
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
) -> Result<(Vm, BTreeMap<String, Arc<VmClosure>>), ConnectorError> {
    // `set_source_dir` writes the *shared* thread-local source dir. This
    // connector module lives outside the caller's project (e.g. under
    // a dependency package generation), so leaking that write would re-anchor
    // the caller's top-level `render("@alias/...")` / source-relative asset
    // resolution on the dependency's `harn.toml` instead of the project root.
    // Snapshot and restore the thread-local around the load so the isolated
    // `base_vm` gets its own source dir without mutating the caller's resting
    // context. The `base_vm`'s per-instance `source_dir` field is what drives
    // this module's own import resolution, so keeping the thread-local pinned
    // is purely a caller-facing concern.
    let _source_dir_guard = crate::stdlib::process::SourceDirGuard::capture();
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
    exports: &BTreeMap<String, Arc<VmClosure>>,
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
    policy: Option<Box<CapabilityPolicy>>,
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
    let vm_args = abi::runtime_export_args(&child_vm, name, args)?;
    let result = child_vm.call_closure_pub(&closure, &vm_args).await;
    result
        .map(|value| Some(vm_value_to_json(&value)))
        .map_err(|error| vm_error_to_connector_for_export(name, error))
}

pub(crate) fn active_harn_connector_ctx() -> Option<ConnectorCtx> {
    ACTIVE_HARN_CONNECTOR_CTX.with(|slot| slot.borrow().last().cloned())
}

/// Per-task ambient-scope swap of the active connector context. See
/// `orchestration::ambient_scope`: `ActiveHarnConnectorCtxGuard` is held across
/// the connector export's `.await`, so concurrent exports on one LocalSet would
/// otherwise read a sibling's provider/binding/tenant identity.
pub(crate) fn swap_active_harn_connector_ctx(next: Vec<ConnectorCtx>) -> Vec<ConnectorCtx> {
    ACTIVE_HARN_CONNECTOR_CTX.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

struct ConnectorExecutionPolicyGuard {
    active: bool,
}

impl ConnectorExecutionPolicyGuard {
    fn push(policy: Option<Box<CapabilityPolicy>>) -> Self {
        if let Some(policy) = policy {
            crate::orchestration::push_execution_policy(*policy);
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
    if raw.trim().is_empty() {
        return Err("cannot be empty".to_string());
    }
    let (amount, unit) = crate::duration_parse::split_amount_unit(raw)
        .ok_or_else(|| format!("'{raw}' is not a valid duration"))?;
    let amount = amount
        .parse::<u64>()
        .map_err(|_| format!("'{raw}' is not a valid duration"))?;
    // Bare numbers are milliseconds; this binding accepts only ms/s/m/h.
    let unit = if unit.is_empty() { "ms" } else { unit.as_str() };
    match unit {
        "ms" | "s" | "m" | "h" => Ok(StdDuration::from_millis(
            amount.saturating_mul(crate::duration_parse::unit_to_millis(unit).unwrap()),
        )),
        _ => Err(format!(
            "'{raw}' uses unsupported unit '{unit}'; expected ms, s, m, or h"
        )),
    }
}

async fn run_poll_loop(
    provider_id: ProviderId,
    payload_schema: ProviderPayloadSchema,
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
            &payload_schema,
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
    payload_schema: &ProviderPayloadSchema,
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
    let tick_at = clock::now_utc();
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
        .ok_or_else(|| {
            ConnectorError::HarnRuntime(
                "connector module 'poll_tick' export returned no value".to_string(),
            )
        })?;
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
            payload_schema,
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
                updated_at: clock::now_utc(),
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
    payload_schema: &ProviderPayloadSchema,
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
    let provider_payload = ProviderPayload::Extension(crate::ExtensionProviderPayload {
        provider: provider_id.as_str().to_string(),
        schema_name: payload_schema.harn_schema_name.clone(),
        raw: normalized.payload,
    });
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
    let headers = BTreeMap::from([
        ("event_id".to_string(), event.id.0.clone()),
        ("trace_id".to_string(), event.trace_id.0.clone()),
        ("provider".to_string(), event.provider.as_str().to_string()),
        ("kind".to_string(), event.kind.clone()),
        ("trigger_id".to_string(), binding_id.to_string()),
    ]);
    crate::triggers::dispatcher::append_trigger_inbox_envelope(
        ctx.event_log.as_ref(),
        Some(binding_id.to_string()),
        None,
        &event,
        headers,
        crate::triggers::dispatcher::TriggerInboxTopicScope::Shared,
    )
    .await
    .map(|_| ())
    .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))
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
#[path = "harn_module/tests.rs"]
mod tests;
