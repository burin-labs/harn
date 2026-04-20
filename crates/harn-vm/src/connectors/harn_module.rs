use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::oneshot;

use crate::bridge::json_result_to_vm_value;
use crate::llm::vm_value_to_json;
use crate::stdlib::register_vm_stdlib;
use crate::value::{VmClosure, VmError, VmValue};
use crate::vm::Vm;
use crate::{
    redact_headers, ClientError, Connector, ConnectorClient, ConnectorCtx, ConnectorError,
    HeaderRedactionPolicy, ProviderId, ProviderPayload, ProviderPayloadSchema, SignatureStatus,
    TenantId, TraceId, TriggerBinding, TriggerEvent, TriggerEventId, TriggerKind,
};

thread_local! {
    static ACTIVE_HARN_CONNECTOR_CTX: RefCell<Vec<ConnectorCtx>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Debug, PartialEq)]
pub struct HarnConnectorContract {
    pub module_path: PathBuf,
    pub provider_id: ProviderId,
    pub kinds: Vec<TriggerKind>,
    pub payload_schema: ProviderPayloadSchema,
}

pub struct HarnConnector {
    provider_id: ProviderId,
    kinds: Vec<TriggerKind>,
    payload_schema: ProviderPayloadSchema,
    module_path: PathBuf,
    shared: Arc<HarnConnectorShared>,
}

struct HarnConnectorClient {
    shared: Arc<HarnConnectorShared>,
}

struct HarnConnectorShared {
    provider_id: ProviderId,
    worker: Mutex<Option<Arc<HarnConnectorWorker>>>,
}

struct HarnConnectorWorker {
    tx: mpsc::Sender<WorkerCommand>,
    join: Mutex<Option<JoinHandle<()>>>,
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

impl HarnConnector {
    pub async fn load(module_path: &Path) -> Result<Self, ConnectorError> {
        let contract = load_contract(module_path).await?;
        let shared = Arc::new(HarnConnectorShared {
            provider_id: contract.provider_id.clone(),
            worker: Mutex::new(None),
        });
        Ok(Self {
            provider_id: contract.provider_id,
            kinds: contract.kinds,
            payload_schema: contract.payload_schema,
            module_path: contract.module_path,
            shared,
        })
    }
}

impl HarnConnectorShared {
    fn install_worker(&self, worker: Arc<HarnConnectorWorker>) {
        *self.worker.lock().expect("worker mutex poisoned") = Some(worker);
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
    fn spawn(provider_id: ProviderId, module_path: PathBuf) -> Result<Arc<Self>, ConnectorError> {
        let (tx, rx) = mpsc::channel();
        let thread_name = format!("harn-connector-{}", provider_id.as_str());
        let join = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run_worker_loop(module_path, rx))
            .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?;
        Ok(Arc::new(Self {
            tx,
            join: Mutex::new(Some(join)),
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
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(WorkerCommand::CallExport {
                name: name.into(),
                args,
                required,
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
                call_provider_export(runtime, "init", vec![init_payload], false)
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
            resp,
        } => {
            let result = async {
                let runtime = state.as_ref().ok_or_else(|| {
                    ConnectorError::HarnRuntime("connector runtime is not initialized".to_string())
                })?;
                call_provider_export(runtime, &name, args, required).await
            }
            .await
            .map_err(|error| error.to_string());
            let _ = resp.send(result);
        }
        WorkerCommand::Shutdown { resp } => {
            let result = async {
                if let Some(runtime) = state.as_ref() {
                    call_provider_export(runtime, "shutdown", Vec::new(), false)
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
        let worker =
            HarnConnectorWorker::spawn(self.provider_id.clone(), self.module_path.clone())?;
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
        let bindings_json = JsonValue::Array(bindings.iter().map(binding_to_json).collect());
        self.shared
            .worker()?
            .call_export("activate", vec![bindings_json], false)
            .await?;
        Ok(crate::ActivationHandle::new(
            self.provider_id.clone(),
            bindings.len(),
        ))
    }

    async fn shutdown(&self, _deadline: StdDuration) -> Result<(), ConnectorError> {
        if let Some(worker) = self.shared.take_worker() {
            worker.shutdown().await?;
        }
        Ok(())
    }

    async fn normalize_inbound(
        &self,
        raw: crate::RawInbound,
    ) -> Result<TriggerEvent, ConnectorError> {
        let raw_json = raw_inbound_to_json(&raw);
        let event = self
            .shared
            .worker()?
            .call_export("normalize_inbound", vec![raw_json], true)
            .await?
            .expect("required export returns a value");
        let normalized: HarnNormalizeResult = serde_json::from_value(event)
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
            &self.provider_id,
            &normalized.kind,
            &raw.headers,
            normalized.payload,
        )
        .map_err(|error| ConnectorError::HarnRuntime(error.to_string()))?;
        Ok(TriggerEvent {
            id: TriggerEventId::new(),
            provider: self.provider_id.clone(),
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

async fn load_module_runtime(
    module_path: &Path,
) -> Result<(Vm, BTreeMap<String, Rc<VmClosure>>), ConnectorError> {
    let mut base_vm = Vm::new();
    register_vm_stdlib(&mut base_vm);
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
        .call_closure_pub(closure, args, &[])
        .await
        .map_err(vm_error_to_connector)
}

async fn call_provider_export(
    runtime: &LocalHarnConnectorRuntime,
    name: &str,
    args: Vec<JsonValue>,
    required: bool,
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
    ACTIVE_HARN_CONNECTOR_CTX.with(|slot| slot.borrow_mut().push(runtime.ctx.clone()));
    let vm_args = args
        .into_iter()
        .map(|value| json_result_to_vm_value(&value))
        .collect::<Vec<_>>();
    let result = child_vm.call_closure_pub(&closure, &vm_args, &[]).await;
    ACTIVE_HARN_CONNECTOR_CTX.with(|slot| {
        slot.borrow_mut().pop();
    });
    result
        .map(|value| Some(vm_value_to_json(&value)))
        .map_err(vm_error_to_connector)
}

pub(crate) fn active_harn_connector_ctx() -> Option<ConnectorCtx> {
    ACTIVE_HARN_CONNECTOR_CTX.with(|slot| slot.borrow().last().cloned())
}

fn vm_error_to_connector(error: VmError) -> ConnectorError {
    ConnectorError::HarnRuntime(vm_error_message(error))
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
