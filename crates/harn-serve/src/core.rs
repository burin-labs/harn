use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use harn_vm::event_log::{
    active_event_log, install_active_event_log, install_default_for_base_dir,
};
use harn_vm::llm::vm_value_to_json;
use harn_vm::trust_graph::{append_trust_record, AutonomyTier, TrustOutcome, TrustRecord};
use harn_vm::{TraceId, Vm, VmValue};
use tokio::task::LocalSet;
use tracing::Instrument;

use crate::auth::{AuthPolicy, AuthRequest, AuthorizationDecision};
use crate::replay::{InMemoryReplayCache, ReplayCache, ReplayCacheEntry, ReplayKey};
use crate::{DispatchError, ExportCatalog};

#[derive(Clone, Debug, PartialEq)]
pub enum CallArguments {
    Named(BTreeMap<String, serde_json::Value>),
    Positional(Vec<serde_json::Value>),
}

#[derive(Clone, Debug)]
pub struct CallRequest {
    pub adapter: String,
    pub function: String,
    pub arguments: CallArguments,
    pub auth: AuthRequest,
    pub caller: String,
    pub replay_key: Option<String>,
    pub trace_id: Option<TraceId>,
    pub parent_span_id: Option<String>,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub cancel_token: Option<Arc<AtomicBool>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CallResponse {
    pub function: String,
    pub value: serde_json::Value,
    pub printed_output: String,
    pub trace_id: TraceId,
    pub cached: bool,
    pub duration_ms: u128,
}

#[async_trait(?Send)]
pub trait VmConfigurator: Send + Sync {
    fn configure(&self, _vm: &mut Vm) -> Result<(), DispatchError> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct NoopVmConfigurator;

#[async_trait(?Send)]
impl VmConfigurator for NoopVmConfigurator {}

pub struct DispatchCoreConfig {
    pub script_path: PathBuf,
    pub base_dir: PathBuf,
    pub service_name: String,
    pub autonomy_tier: AutonomyTier,
    pub auth_policy: AuthPolicy,
    pub replay_cache: Arc<dyn ReplayCache>,
    pub vm_configurator: Arc<dyn VmConfigurator>,
}

impl DispatchCoreConfig {
    pub fn for_script(path: impl Into<PathBuf>) -> Self {
        let script_path = path.into();
        let base_dir = script_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let service_name = script_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("harn-serve")
            .to_string();
        Self {
            script_path,
            base_dir,
            service_name,
            autonomy_tier: AutonomyTier::ActAuto,
            auth_policy: AuthPolicy::allow_all(),
            replay_cache: Arc::new(InMemoryReplayCache::new()),
            vm_configurator: Arc::new(NoopVmConfigurator),
        }
    }
}

pub struct DispatchCore {
    config: DispatchCoreConfig,
    catalog: ExportCatalog,
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
}

impl DispatchCore {
    pub fn new(config: DispatchCoreConfig) -> Result<Self, DispatchError> {
        let catalog = ExportCatalog::from_path(&config.script_path)?;
        let event_log = install_default_for_base_dir(&config.base_dir).map_err(|error| {
            DispatchError::Io(format!(
                "failed to initialize event log for {}: {error}",
                config.base_dir.display()
            ))
        })?;
        Ok(Self {
            config,
            catalog,
            event_log,
        })
    }

    pub fn catalog(&self) -> &ExportCatalog {
        &self.catalog
    }

    pub async fn dispatch(&self, request: CallRequest) -> Result<CallResponse, DispatchError> {
        let authorization = self.config.auth_policy.authorize(&request.auth).await;
        let trace_id = request.trace_id.clone().unwrap_or_default();
        match authorization {
            AuthorizationDecision::Authorized(_) => {}
            AuthorizationDecision::Rejected(message) => {
                self.record_trust(
                    &request,
                    &trace_id,
                    TrustOutcome::Denied,
                    Some(message.clone()),
                )
                .await?;
                return Err(DispatchError::Unauthorized(message));
            }
        }

        let function = self.catalog.function(&request.function).ok_or_else(|| {
            DispatchError::MissingExport(format!(
                "function '{}' is not exported by {}",
                request.function,
                self.catalog.script_path.display()
            ))
        })?;

        let replay_key = request
            .replay_key
            .clone()
            .map(ReplayKey)
            .or_else(|| Some(self.default_replay_key(&request)));
        if let Some(key) = replay_key.as_ref() {
            if let Some(cached) = self.config.replay_cache.get(key).await? {
                return Ok(CallResponse {
                    function: request.function.clone(),
                    value: cached.value,
                    printed_output: cached.printed_output,
                    trace_id,
                    cached: true,
                    duration_ms: 0,
                });
            }
        }

        let span = tracing::info_span!(
            target: "harn.serve",
            "harn_serve.dispatch",
            adapter = %request.adapter,
            function = %request.function,
            caller = %request.caller,
            trace_id = %trace_id.0,
        );
        let _ = harn_vm::observability::otel::set_span_parent(
            &span,
            &trace_id,
            request.parent_span_id.as_deref(),
        );

        let started = Instant::now();
        let invocation = async {
            let value = self.invoke_function(&request, function).await?;
            Ok::<_, DispatchError>(value)
        }
        .instrument(span)
        .await;

        match invocation {
            Ok((value, printed_output)) => {
                let duration_ms = started.elapsed().as_millis();
                self.record_trust(&request, &trace_id, TrustOutcome::Success, None)
                    .await?;
                if let Some(key) = replay_key {
                    self.config
                        .replay_cache
                        .put(
                            key,
                            ReplayCacheEntry {
                                value: value.clone(),
                                printed_output: printed_output.clone(),
                            },
                        )
                        .await?;
                }
                Ok(CallResponse {
                    function: request.function,
                    value,
                    printed_output,
                    trace_id,
                    cached: false,
                    duration_ms,
                })
            }
            Err(error) => {
                self.record_trust(
                    &request,
                    &trace_id,
                    TrustOutcome::Failure,
                    Some(error.to_string()),
                )
                .await?;
                Err(error)
            }
        }
    }

    fn default_replay_key(&self, request: &CallRequest) -> ReplayKey {
        let rendered_args = match &request.arguments {
            CallArguments::Named(values) => serde_json::to_string(values).unwrap_or_default(),
            CallArguments::Positional(values) => serde_json::to_string(values).unwrap_or_default(),
        };
        ReplayKey(format!(
            "{}:{}:{}",
            request.adapter, request.function, rendered_args
        ))
    }

    async fn invoke_function(
        &self,
        request: &CallRequest,
        function: &crate::ExportedFunction,
    ) -> Result<(serde_json::Value, String), DispatchError> {
        let source = fs::read_to_string(&self.config.script_path).map_err(|error| {
            DispatchError::Io(format!(
                "failed to read {}: {error}",
                self.config.script_path.display()
            ))
        })?;
        let script_path = self.config.script_path.clone();
        let cancel_token = request
            .cancel_token
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

        let local = LocalSet::new();
        local
            .run_until(async move {
                let previous_log = active_event_log();
                install_active_event_log(self.event_log.clone());

                let mut vm = Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                let store_base = script_path.parent().unwrap_or(Path::new("."));
                harn_vm::register_store_builtins(&mut vm, store_base);
                harn_vm::register_metadata_builtins(&mut vm, store_base);
                vm.set_source_info(&script_path.display().to_string(), &source);
                vm.set_source_dir(store_base);
                vm.install_cancel_token(cancel_token);
                self.config.vm_configurator.configure(&mut vm)?;

                let exports = vm
                    .load_module_exports(&script_path)
                    .await
                    .map_err(|error| DispatchError::Execution(error.to_string()))?;
                let Some(closure) = exports.get(&request.function) else {
                    return Err(DispatchError::MissingExport(format!(
                        "function '{}' is not exported by {}",
                        request.function,
                        script_path.display()
                    )));
                };
                let args = build_vm_args(&request.arguments, function)?;
                let result = vm.call_closure_pub(closure, &args, &[]).await;

                match previous_log {
                    Some(log) => {
                        install_active_event_log(log);
                    }
                    None => {
                        harn_vm::event_log::reset_active_event_log();
                    }
                }

                match result {
                    Ok(value) => Ok((vm_value_to_json(&value), vm.output().to_string())),
                    Err(error) => {
                        let message = error.to_string();
                        if message.contains("cancelled") {
                            Err(DispatchError::Cancelled(message))
                        } else {
                            Err(DispatchError::Execution(message))
                        }
                    }
                }
            })
            .await
    }

    async fn record_trust(
        &self,
        request: &CallRequest,
        trace_id: &TraceId,
        outcome: TrustOutcome,
        error: Option<String>,
    ) -> Result<(), DispatchError> {
        let mut record = TrustRecord::new(
            self.config.service_name.clone(),
            format!("invoke.{}", request.function),
            None,
            outcome,
            trace_id.0.clone(),
            self.config.autonomy_tier,
        );
        record
            .metadata
            .insert("adapter".to_string(), serde_json::json!(request.adapter));
        record
            .metadata
            .insert("caller".to_string(), serde_json::json!(request.caller));
        record
            .metadata
            .insert("function".to_string(), serde_json::json!(request.function));
        if let Some(error) = error {
            record
                .metadata
                .insert("error".to_string(), serde_json::json!(error));
        }
        append_trust_record(&self.event_log, &record)
            .await
            .map(|_| ())
            .map_err(|error| {
                DispatchError::Execution(format!("failed to append trust record: {error}"))
            })
    }
}

fn build_vm_args(
    arguments: &CallArguments,
    function: &crate::ExportedFunction,
) -> Result<Vec<VmValue>, DispatchError> {
    match arguments {
        CallArguments::Positional(values) => Ok(values.iter().map(json_to_vm_value).collect()),
        CallArguments::Named(values) => {
            let mut args = Vec::new();
            let mut saw_gap = false;
            for param in &function.params {
                let value = values.get(&param.name);
                match value {
                    Some(value) => {
                        if saw_gap {
                            return Err(DispatchError::Validation(format!(
                                "named arguments for '{}' skipped '{}' before later arguments",
                                function.name, param.name
                            )));
                        }
                        args.push(json_to_vm_value(value));
                    }
                    None if param.has_default => {
                        saw_gap = true;
                    }
                    None => {
                        return Err(DispatchError::Validation(format!(
                            "missing required argument '{}' for '{}'",
                            param.name, function.name
                        )));
                    }
                }
            }
            Ok(trim_trailing_defaults(args))
        }
    }
}

fn trim_trailing_defaults(mut args: Vec<VmValue>) -> Vec<VmValue> {
    let mut tail = VecDeque::from(args);
    while matches!(tail.back(), Some(VmValue::Nil)) {
        tail.pop_back();
    }
    args = tail.into_iter().collect();
    args
}

fn json_to_vm_value(value: &serde_json::Value) -> VmValue {
    match value {
        serde_json::Value::Null => VmValue::Nil,
        serde_json::Value::Bool(value) => VmValue::Bool(*value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(VmValue::Int)
            .or_else(|| value.as_f64().map(VmValue::Float))
            .unwrap_or(VmValue::Nil),
        serde_json::Value::String(value) => VmValue::String(Rc::from(value.as_str())),
        serde_json::Value::Array(items) => VmValue::List(Rc::new(
            items.iter().map(json_to_vm_value).collect::<Vec<_>>(),
        )),
        serde_json::Value::Object(map) => VmValue::Dict(Rc::new(
            map.iter()
                .map(|(key, value)| (key.clone(), json_to_vm_value(value)))
                .collect(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_executes_exported_function() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
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
            })
            .await
            .expect("dispatch");

        assert_eq!(response.value, serde_json::json!("alice"));
        assert!(!response.cached);
    }

    #[tokio::test]
    async fn dispatch_uses_replay_cache_before_reinvoking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn greet(name: string) -> string {
  return "fresh"
}
"#,
        )
        .expect("write script");

        let cache = Arc::new(InMemoryReplayCache::new());
        cache
            .put(
                ReplayKey("fixed-key".to_string()),
                ReplayCacheEntry {
                    value: serde_json::json!("cached"),
                    printed_output: String::new(),
                },
            )
            .await
            .expect("seed cache");

        let mut config = DispatchCoreConfig::for_script(&script);
        config.replay_cache = cache;
        let core = DispatchCore::new(config).expect("core");
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
                replay_key: Some("fixed-key".to_string()),
                trace_id: None,
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: None,
            })
            .await
            .expect("dispatch");

        assert_eq!(response.value, serde_json::json!("cached"));
        assert!(response.cached);
    }

    #[tokio::test]
    async fn dispatch_records_trust_graph_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn greet(name: string) -> string {
  return name
}
"#,
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
            })
            .await
            .expect("dispatch");

        assert_eq!(response.value, serde_json::json!("stopped"));
    }
}
