//! Language-neutral executable tool-composition contract.
//!
//! A composition run is a tiny program over already-typed tool bindings. The
//! runtime must expose it as a parent run with child tool operations, not as an
//! opaque "execute code" blob, so policy, transcript, replay, and host approval
//! surfaces can keep reasoning about each child call normally.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::agent_events::{ToolCallErrorCategory, ToolCallStatus};
use crate::tool_annotations::SideEffectLevel;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

mod crystallization;
mod events;
mod harn_api;
mod hosts;
mod manifest;
mod types;
mod typescript;

#[cfg(test)]
mod tests;

pub use crystallization::composition_crystallization_trace;
pub use events::composition_report_events;
pub use harn_api::composition_harn_api;
pub use hosts::{ClosureCompositionToolHost, StaticCompositionToolHost};
pub use manifest::{
    binding_manifest_from_tool_surface, binding_manifest_hash, BindingManifest,
    BindingManifestEntry, BindingManifestOptions, BindingPolicyDisposition, BindingPolicyStatus,
    BINDING_MANIFEST_SCHEMA_VERSION,
};
pub use types::{
    CompositionChildCall, CompositionChildResult, CompositionExecutionLimits,
    CompositionExecutionReport, CompositionExecutionRequest, CompositionFailureCategory,
    CompositionMcpPolicy, CompositionRetryPolicy, CompositionRunEnvelope, CompositionToolHost,
    CompositionToolOutput, COMPOSITION_EXECUTION_SCHEMA_VERSION,
};
pub use typescript::composition_typescript_declarations;

/// Stable digest for the prompt-visible snippet body.
pub fn composition_snippet_hash(language: &str, snippet: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"harn.composition.snippet.v1\0");
    hasher.update(language.as_bytes());
    hasher.update(b"\0");
    hasher.update(snippet.as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

struct ExecutionState {
    request: CompositionExecutionRequest,
    calls: Vec<CompositionChildCall>,
    results: Vec<CompositionChildResult>,
    clock: Arc<dyn harn_clock::Clock>,
    started_ms: i64,
}

impl ExecutionState {
    fn next_call(
        &mut self,
        tool_name: &str,
        input: Value,
    ) -> Result<(BindingManifestEntry, CompositionChildCall), VmError> {
        if self.results.len() as u64 >= self.request.limits.max_operations {
            return Err(VmError::Runtime(format!(
                "composition exceeded max_operations={}",
                self.request.limits.max_operations
            )));
        }
        if let Some(timeout_ms) = self.request.limits.timeout_ms {
            if elapsed_ms(&*self.clock, self.started_ms) > timeout_ms {
                return Err(VmError::Runtime(format!(
                    "composition exceeded timeout_ms={timeout_ms}"
                )));
            }
        }
        let binding = self
            .request
            .manifest
            .find_by_name(tool_name)
            .or_else(|| self.request.manifest.find_by_binding(tool_name))
            .cloned()
            .ok_or_else(|| {
                VmError::Runtime(format!("composition binding '{tool_name}' not found"))
            })?;
        let call = self.push_call(&binding, input);
        if binding.policy.disposition == BindingPolicyDisposition::Denied {
            let message = format!(
                "composition binding '{}' denied{}",
                binding.name,
                binding
                    .policy
                    .reason
                    .as_deref()
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default()
            );
            self.push_failed_result(&call, &message, ToolCallErrorCategory::PermissionDenied);
            return Err(VmError::Runtime(message));
        }
        if binding.policy.disposition == BindingPolicyDisposition::Gated {
            let message = format!(
                "composition binding '{}' requires approval and cannot run in read-only mode",
                binding.name
            );
            self.push_failed_result(&call, &message, ToolCallErrorCategory::PermissionDenied);
            return Err(VmError::Runtime(message));
        }
        if binding.side_effect_level.rank() > self.request.requested_side_effect_ceiling.rank() {
            let message = format!(
                "composition binding '{}' requires side-effect level '{}' above requested ceiling '{}'",
                binding.name,
                binding.side_effect_level.as_str(),
                self.request.requested_side_effect_ceiling.as_str()
            );
            self.push_failed_result(&call, &message, ToolCallErrorCategory::PermissionDenied);
            return Err(VmError::Runtime(message));
        }
        Ok((binding, call))
    }

    fn push_call(&mut self, binding: &BindingManifestEntry, input: Value) -> CompositionChildCall {
        let operation_index = self.calls.len() as u64;
        let call = CompositionChildCall {
            run_id: self.request.run_id.clone(),
            tool_call_id: format!("{}:{operation_index}", self.request.run_id),
            tool_name: binding.name.clone(),
            operation_index,
            annotations: Some(binding.annotations.clone()),
            requested_side_effect_level: binding.side_effect_level,
            policy_context: serde_json::json!({
                "disposition": binding.policy.disposition,
                "reason": binding.policy.reason,
                "ceiling": self.request.requested_side_effect_ceiling,
            }),
            raw_input: input,
        };
        self.calls.push(call.clone());
        call
    }

    fn push_failed_result(
        &mut self,
        call: &CompositionChildCall,
        message: &str,
        category: ToolCallErrorCategory,
    ) {
        self.results.push(CompositionChildResult {
            run_id: call.run_id.clone(),
            tool_call_id: call.tool_call_id.clone(),
            tool_name: call.tool_name.clone(),
            operation_index: call.operation_index,
            status: ToolCallStatus::Failed,
            raw_output: None,
            error: Some(message.to_string()),
            error_category: Some(category),
            executor: Some(crate::agent_events::ToolExecutor::HarnBuiltin),
            duration_ms: Some(0),
            execution_duration_ms: Some(0),
            attempt: 1,
            retry_attempts: 0,
            retry_errors: Vec::new(),
            retry_delays_ms: Vec::new(),
        });
    }

    fn push_result(
        &mut self,
        call: &CompositionChildCall,
        outcome: &CompositionDispatchOutcome,
        elapsed_ms: u64,
    ) {
        if self
            .results
            .iter()
            .any(|result| result.tool_call_id == call.tool_call_id)
        {
            return;
        }
        self.results.push(CompositionChildResult {
            run_id: call.run_id.clone(),
            tool_call_id: call.tool_call_id.clone(),
            tool_name: call.tool_name.clone(),
            operation_index: call.operation_index,
            status: if outcome.output.error.is_some() {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            },
            raw_output: outcome.output.value.clone(),
            error: outcome.output.error.clone(),
            error_category: outcome.output.error_category,
            executor: outcome.output.executor.clone(),
            duration_ms: Some(elapsed_ms),
            execution_duration_ms: Some(elapsed_ms),
            attempt: outcome.attempt,
            retry_attempts: outcome.retry_attempts,
            retry_errors: outcome.retry_errors.clone(),
            retry_delays_ms: outcome.retry_delays_ms.clone(),
        });
    }
}

#[derive(Clone)]
struct CompositionRuntime {
    state: Arc<parking_lot::Mutex<ExecutionState>>,
    host: Arc<dyn CompositionToolHost>,
    bulkheads: Arc<CompositionBulkheads>,
}

struct CompositionBulkheads {
    global: Arc<Semaphore>,
    per_server: parking_lot::Mutex<HashMap<String, Arc<Semaphore>>>,
    per_server_limit: usize,
}

impl CompositionBulkheads {
    fn new(limits: &CompositionExecutionLimits) -> Self {
        Self {
            global: Arc::new(Semaphore::new(
                limits
                    .max_concurrent_operations
                    .clamp(1, Semaphore::MAX_PERMITS),
            )),
            per_server: parking_lot::Mutex::new(HashMap::new()),
            per_server_limit: limits
                .max_concurrent_per_server
                .clamp(1, Semaphore::MAX_PERMITS),
        }
    }

    async fn acquire(
        &self,
        binding: &BindingManifestEntry,
    ) -> Result<(OwnedSemaphorePermit, Option<OwnedSemaphorePermit>), VmError> {
        let global = self
            .global
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| VmError::Runtime("composition bulkhead closed".to_string()))?;
        let server = mcp_server_name(binding);
        let per_server = match server {
            Some(server) => {
                let semaphore = {
                    let mut semaphores = self.per_server.lock();
                    semaphores
                        .entry(server)
                        .or_insert_with(|| Arc::new(Semaphore::new(self.per_server_limit)))
                        .clone()
                };
                Some(semaphore.acquire_owned().await.map_err(|_| {
                    VmError::Runtime("composition per-server bulkhead closed".to_string())
                })?)
            }
            None => None,
        };
        Ok((global, per_server))
    }
}

struct CompositionDispatchOutcome {
    output: CompositionToolOutput,
    attempt: u32,
    retry_attempts: u32,
    retry_errors: Vec<String>,
    retry_delays_ms: Vec<u64>,
}

/// Execute a read-only Harn-native composition snippet against a manifest.
pub async fn execute_harn_composition(
    mut request: CompositionExecutionRequest,
    host: Arc<dyn CompositionToolHost>,
) -> CompositionExecutionReport {
    if request.run_id.trim().is_empty() {
        request.run_id = uuid::Uuid::now_v7().to_string();
    }
    if request.language.trim().is_empty() {
        request.language = "harn".to_string();
    }
    let manifest_hash = request
        .manifest
        .hash()
        .unwrap_or_else(|_| "sha256:manifest_hash_error".to_string());
    let snippet_hash = composition_snippet_hash(&request.language, &request.snippet);
    let mut run = CompositionRunEnvelope::read_only(
        request.run_id.clone(),
        request.language.clone(),
        snippet_hash,
        manifest_hash,
    );
    let session_id = request.session_id.clone();
    run.requested_side_effect_ceiling = request.requested_side_effect_ceiling;
    run.metadata = request.metadata.clone();
    if !run.metadata.is_object() {
        run.metadata = Value::Object(serde_json::Map::new());
    }
    if let Some(session_id) = &session_id {
        run.metadata["session_id"] = Value::String(session_id.clone());
    }
    let clock = harn_clock::RealClock::arc();
    let started_ms = clock.monotonic_ms();

    let result = if request.language != "harn" {
        Err((
            CompositionFailureCategory::UnsupportedLanguage,
            format!("unsupported composition language '{}'", request.language),
            Vec::new(),
            Vec::new(),
        ))
    } else if request.requested_side_effect_ceiling.rank() > SideEffectLevel::ReadOnly.rank() {
        Err((
            CompositionFailureCategory::PolicyDenied,
            "read-only composition executor refuses side-effect ceilings above read_only"
                .to_string(),
            Vec::new(),
            Vec::new(),
        ))
    } else {
        execute_harn_composition_inner(request, host).await
    };

    let report = match result {
        Ok((value, stdout, calls, results)) => {
            run.result = Some(value);
            run.stdout = (!stdout.is_empty()).then_some(stdout);
            run.duration_ms = Some(elapsed_ms(&*clock, started_ms));
            CompositionExecutionReport {
                schema_version: COMPOSITION_EXECUTION_SCHEMA_VERSION,
                ok: true,
                summary: format!(
                    "composition completed with {} child operation(s)",
                    results.len()
                ),
                run,
                child_calls: calls,
                child_results: results,
            }
        }
        Err((category, error, calls, results)) => {
            run.failure_category = Some(category);
            run.error = Some(error.clone());
            run.duration_ms = Some(elapsed_ms(&*clock, started_ms));
            CompositionExecutionReport {
                schema_version: COMPOSITION_EXECUTION_SCHEMA_VERSION,
                ok: false,
                summary: error,
                run,
                child_calls: calls,
                child_results: results,
            }
        }
    };
    if let Some(session_id) = session_id {
        events::emit_composition_report_events(&session_id, &report);
    }
    report
}

async fn execute_harn_composition_inner(
    request: CompositionExecutionRequest,
    host: Arc<dyn CompositionToolHost>,
) -> Result<
    (
        Value,
        String,
        Vec<CompositionChildCall>,
        Vec<CompositionChildResult>,
    ),
    (
        CompositionFailureCategory,
        String,
        Vec<CompositionChildCall>,
        Vec<CompositionChildResult>,
    ),
> {
    let validation_source = composition_validation_source(&request.snippet);
    let validation_program = harn_parser::parse_source(&validation_source).map_err(|error| {
        (
            CompositionFailureCategory::SchemaValidation,
            format!("composition parse error: {error}"),
            Vec::new(),
            Vec::new(),
        )
    })?;
    validate_composition_program(&validation_program, &request.manifest).map_err(|error| {
        (
            CompositionFailureCategory::PolicyDenied,
            error,
            Vec::new(),
            Vec::new(),
        )
    })?;

    let source = composition_source(&request.manifest, &request.snippet);
    let program = harn_parser::parse_source(&source).map_err(|error| {
        (
            CompositionFailureCategory::SchemaValidation,
            format!("composition parse error: {error}"),
            Vec::new(),
            Vec::new(),
        )
    })?;
    let chunk = crate::Compiler::new()
        .compile_named(&program, "main")
        .map_err(|error| {
            (
                CompositionFailureCategory::SchemaValidation,
                format!("composition compile error: {error}"),
                Vec::new(),
                Vec::new(),
            )
        })?;

    let execution_clock = harn_clock::RealClock::arc();
    let execution_started_ms = execution_clock.monotonic_ms();
    let state = Arc::new(parking_lot::Mutex::new(ExecutionState {
        request,
        calls: Vec::new(),
        results: Vec::new(),
        clock: execution_clock,
        started_ms: execution_started_ms,
    }));
    let mut vm = Vm::new();
    crate::register_core_stdlib(&mut vm);
    let limits = state.lock().request.limits.clone();
    let runtime = CompositionRuntime {
        state: state.clone(),
        host,
        bulkheads: Arc::new(CompositionBulkheads::new(&limits)),
    };
    register_composition_call_builtin(&mut vm, runtime.clone());
    register_composition_map_bounded_builtin(&mut vm, runtime);
    if let Some(timeout_ms) = state.lock().request.limits.timeout_ms {
        vm.push_deadline_after(std::time::Duration::from_millis(timeout_ms));
    }
    vm.set_source_info("composition://snippet.harn", &source);
    match vm.execute(&chunk).await {
        Ok(value) => {
            let json = crate::llm::vm_value_to_json(&value);
            let stdout = vm.output().to_string();
            let state = state.lock();
            let result_size = serde_json::to_vec(&json)
                .map(|bytes| bytes.len())
                .unwrap_or(0);
            let output_size = result_size.saturating_add(stdout.len());
            if output_size as u64 > state.request.limits.max_output_bytes {
                return Err((
                    CompositionFailureCategory::ExecutionError,
                    format!(
                        "composition output exceeded max_output_bytes={}",
                        state.request.limits.max_output_bytes
                    ),
                    state.calls.clone(),
                    state.results.clone(),
                ));
            }
            Ok((json, stdout, state.calls.clone(), state.results.clone()))
        }
        Err(error) => {
            let state = state.lock();
            let category = if error.to_string().contains("denied")
                || error.to_string().contains("side-effect")
                || error.to_string().contains("approval")
            {
                CompositionFailureCategory::PolicyDenied
            } else if error.to_string().contains("Deadline exceeded")
                || error.to_string().contains("max_operations")
                || error.to_string().contains("timeout_ms")
                || error.to_string().contains("max_output_bytes")
            {
                CompositionFailureCategory::Timeout
            } else if state
                .results
                .iter()
                .any(|result| result.status == ToolCallStatus::Failed)
            {
                CompositionFailureCategory::ChildToolError
            } else {
                CompositionFailureCategory::ExecutionError
            };
            Err((
                category,
                error.to_string(),
                state.calls.clone(),
                state.results.clone(),
            ))
        }
    }
}

fn register_composition_call_builtin(vm: &mut Vm, runtime: CompositionRuntime) {
    vm.register_async_builtin("__composition_call", move |_ctx, args| {
        let runtime = runtime.clone();
        async move {
            let tool_name = args
                .first()
                .map(VmValue::display)
                .ok_or_else(|| VmError::Runtime("__composition_call: missing tool name".into()))?;
            let input = args
                .get(1)
                .map(crate::llm::vm_value_to_json)
                .unwrap_or_else(|| serde_json::json!({}));
            let (binding, call, clock) = {
                let mut state = runtime.state.lock();
                let (binding, call) = state.next_call(&tool_name, input.clone())?;
                (binding, call, state.clock.clone())
            };
            let started_ms = clock.monotonic_ms();
            let outcome = dispatch_binding_with_policy(&runtime, &binding, input).await?;
            {
                let mut state = runtime.state.lock();
                state.push_result(&call, &outcome, elapsed_ms(&*clock, started_ms));
            }
            if let Some(error) = outcome.output.error {
                return Err(VmError::Runtime(error));
            }
            Ok(crate::json_to_vm_value(
                &outcome.output.value.unwrap_or(Value::Null),
            ))
        }
    });
}

async fn dispatch_binding_with_policy(
    runtime: &CompositionRuntime,
    binding: &BindingManifestEntry,
    input: Value,
) -> Result<CompositionDispatchOutcome, VmError> {
    let policy = runtime.state.lock().request.mcp_policy.clone();
    let retry = policy.retry.clone();
    let max_attempts = retry.max_attempts.max(1);
    let can_retry = retry_allowed(binding, &input, &policy);
    let mut attempt = 1u32;
    let mut retry_errors = Vec::new();
    let mut retry_delays_ms = Vec::new();

    loop {
        let (_global_permit, _server_permit) = runtime.bulkheads.acquire(binding).await?;
        let call = runtime.host.call(binding, input.clone());
        let mut output = if let Some(timeout_ms) = policy.call_timeout_ms.filter(|ms| *ms > 0) {
            match tokio::time::timeout(Duration::from_millis(timeout_ms), call).await {
                Ok(output) => output,
                Err(_) => CompositionToolOutput::error(
                    format!(
                        "composition binding '{}' timed out after {timeout_ms}ms",
                        binding.name
                    ),
                    ToolCallErrorCategory::Timeout,
                ),
            }
        } else {
            call.await
        };
        drop((_global_permit, _server_permit));

        if output.error.is_none() {
            if let Some(value) = output.value.take() {
                match validate_binding_output(binding, value) {
                    Ok(value) => output.value = Some(value),
                    Err(message) => {
                        output = CompositionToolOutput::error(
                            message,
                            ToolCallErrorCategory::SchemaValidation,
                        );
                    }
                }
            }
        }

        if output.error.is_none()
            || attempt >= max_attempts
            || !can_retry
            || !is_retryable_child_error(&output)
        {
            return Ok(CompositionDispatchOutcome {
                output,
                attempt,
                retry_attempts: attempt.saturating_sub(1),
                retry_errors,
                retry_delays_ms,
            });
        }

        let error = output
            .error
            .clone()
            .unwrap_or_else(|| "composition child call failed".to_string());
        let delay_ms = compute_retry_delay_ms(binding, &input, attempt, &retry, &error);
        retry_errors.push(error);
        retry_delays_ms.push(delay_ms);
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        attempt = attempt.saturating_add(1);
    }
}

fn validate_binding_output(binding: &BindingManifestEntry, value: Value) -> Result<Value, String> {
    let Some(schema) = &binding.output_schema else {
        return Ok(value);
    };
    let value_vm = crate::json_to_vm_value(&value);
    let schema_vm = crate::json_to_vm_value(schema);
    crate::schema::schema_expect_value(&value_vm, &schema_vm, false)
        .map(|value| crate::llm::vm_value_to_json(&value))
        .map_err(|error| {
            format!(
                "composition binding '{}' outputSchema validation failed: {error}",
                binding.name
            )
        })
}

fn retry_allowed(
    binding: &BindingManifestEntry,
    input: &Value,
    policy: &CompositionMcpPolicy,
) -> bool {
    if idempotency_key_present(input) {
        return true;
    }
    if binding.source == "mcp_server" {
        if !mcp_binding_trusted(binding, policy) {
            return false;
        }
        return binding.annotations.destructive_hint != Some(true)
            && (binding.annotations.read_only_hint == Some(true)
                || binding.annotations.idempotent_hint == Some(true));
    }
    binding.side_effect_level == SideEffectLevel::ReadOnly
        && binding.annotations.kind.is_read_only()
}

fn mcp_binding_trusted(binding: &BindingManifestEntry, policy: &CompositionMcpPolicy) -> bool {
    policy.trust_annotations
        || mcp_server_name(binding)
            .as_ref()
            .is_some_and(|server| policy.trusted_servers.contains(server))
}

fn mcp_server_name(binding: &BindingManifestEntry) -> Option<String> {
    binding
        .metadata
        .get("_mcp_server")
        .or_else(|| binding.metadata.get("mcp_server"))
        .or_else(|| binding.metadata.pointer("/server/name"))
        .and_then(Value::as_str)
        .filter(|server| !server.is_empty())
        .map(ToOwned::to_owned)
}

fn idempotency_key_present(input: &Value) -> bool {
    for pointer in [
        "/idempotency_key",
        "/idempotencyKey",
        "/_idempotency_key",
        "/_meta/idempotencyKey",
        "/_meta/harn/idempotencyKey",
    ] {
        if input.pointer(pointer).is_some_and(|value| match value {
            Value::String(value) => !value.trim().is_empty(),
            Value::Null => false,
            _ => true,
        }) {
            return true;
        }
    }
    false
}

fn is_retryable_child_error(output: &CompositionToolOutput) -> bool {
    if matches!(
        output.error_category,
        Some(
            ToolCallErrorCategory::Network
                | ToolCallErrorCategory::Timeout
                | ToolCallErrorCategory::McpServerError
                | ToolCallErrorCategory::ResourceBusy
        )
    ) {
        return true;
    }
    let Some(error) = &output.error else {
        return false;
    };
    let error = error.to_ascii_lowercase();
    [
        "429",
        "503",
        "retry-after",
        "rate limit",
        "rate-limit",
        "timeout",
        "timed out",
        "transient",
        "overloaded",
        "server closed connection",
        "disconnected",
        "mcp read error",
        "mcp write error",
        "connection reset",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn compute_retry_delay_ms(
    binding: &BindingManifestEntry,
    input: &Value,
    attempt: u32,
    retry: &CompositionRetryPolicy,
    error: &str,
) -> u64 {
    if retry.max_delay_ms == 0 {
        return 0;
    }
    if retry.honor_retry_after {
        if let Some(delay) = retry_after_ms_from_error(error) {
            return delay.min(retry.max_delay_ms);
        }
    }
    let shift = attempt.saturating_sub(1).min(20);
    let multiplier = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    let base = retry.base_delay_ms.saturating_mul(multiplier);
    if base == 0 {
        return 0;
    }
    let jitter_span = (base / 2).max(1);
    let mut hasher = Sha256::new();
    hasher.update(binding.name.as_bytes());
    hasher.update(b"\0");
    hasher.update(attempt.to_le_bytes());
    hasher.update(b"\0");
    if let Ok(bytes) = serde_json::to_vec(input) {
        hasher.update(bytes);
    }
    let digest = hasher.finalize();
    let jitter = u64::from_le_bytes(digest[..8].try_into().unwrap_or([0; 8])) % (jitter_span + 1);
    base.saturating_add(jitter).min(retry.max_delay_ms)
}

fn retry_after_ms_from_error(error: &str) -> Option<u64> {
    let lower = error.to_ascii_lowercase();
    let (_, tail) = lower.split_once("retry-after")?;
    let value = tail
        .trim_start_matches(|c: char| c == ':' || c == '=' || c.is_whitespace())
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .filter(|value| !value.is_empty())?;
    value
        .parse::<u64>()
        .ok()
        .map(|seconds| seconds.saturating_mul(1000))
}

fn register_composition_map_bounded_builtin(vm: &mut Vm, runtime: CompositionRuntime) {
    vm.register_async_builtin("map_bounded", move |ctx, args| {
        let runtime = runtime.clone();
        async move {
            let items = match args.first() {
                Some(VmValue::List(items)) => items.as_ref().clone(),
                Some(other) => {
                    return Err(VmError::TypeError(format!(
                        "map_bounded: first argument must be a list, got {}",
                        other.type_name()
                    )))
                }
                None => {
                    return Err(VmError::Runtime(
                        "map_bounded: first argument must be a list".to_string(),
                    ))
                }
            };
            let closure = match args.get(1) {
                Some(VmValue::Closure(closure)) => closure.clone(),
                Some(other) => {
                    return Err(VmError::TypeError(format!(
                        "map_bounded: second argument must be a closure, got {}",
                        other.type_name()
                    )))
                }
                None => {
                    return Err(VmError::Runtime(
                        "map_bounded: second argument must be a closure".to_string(),
                    ))
                }
            };
            let options = args
                .get(2)
                .map(crate::llm::vm_value_to_json)
                .unwrap_or_else(|| serde_json::json!({}));
            let default_cap = runtime
                .state
                .lock()
                .request
                .limits
                .max_concurrent_operations
                .max(1);
            let cap = options
                .get("concurrency")
                .or_else(|| options.get("max_concurrent"))
                .and_then(Value::as_u64)
                .map(|value| value.max(1) as usize)
                .unwrap_or(default_cap)
                .min(items.len().max(1));

            let total = items.len();
            let mut pending = items.into_iter().enumerate();
            let mut in_flight = FuturesUnordered::new();
            let mut results: Vec<Option<VmValue>> = vec![None; total];
            let mut succeeded = 0i64;
            let mut failed = 0i64;

            while in_flight.len() < cap {
                let Some((index, item)) = pending.next() else {
                    break;
                };
                in_flight.push(run_map_bounded_item(
                    ctx.clone(),
                    closure.clone(),
                    index,
                    item,
                ));
            }
            while let Some((index, output, result)) = in_flight.next().await {
                ctx.forward_output(&output);
                match result {
                    Ok(value) => {
                        succeeded += 1;
                        results[index] = Some(VmValue::enum_variant("Result", "Ok", vec![value]));
                    }
                    Err(error) => {
                        failed += 1;
                        results[index] = Some(VmValue::enum_variant(
                            "Result",
                            "Err",
                            vec![VmValue::String(arcstr::ArcStr::from(error.to_string()))],
                        ));
                    }
                }
                if let Some((next_index, next_item)) = pending.next() {
                    in_flight.push(run_map_bounded_item(
                        ctx.clone(),
                        closure.clone(),
                        next_index,
                        next_item,
                    ));
                }
            }

            let mut dict = BTreeMap::new();
            dict.insert(
                "results".to_string(),
                VmValue::List(std::sync::Arc::new(
                    results
                        .into_iter()
                        .map(|value| {
                            value.unwrap_or_else(|| {
                                VmValue::enum_variant(
                                    "Result",
                                    "Err",
                                    vec![VmValue::String(arcstr::ArcStr::from(
                                        "map_bounded: task did not produce a result",
                                    ))],
                                )
                            })
                        })
                        .collect(),
                )),
            );
            dict.insert("succeeded".to_string(), VmValue::Int(succeeded));
            dict.insert("failed".to_string(), VmValue::Int(failed));
            Ok(VmValue::dict(dict))
        }
    });
}

async fn run_map_bounded_item(
    ctx: crate::vm::AsyncBuiltinCtx,
    closure: std::sync::Arc<crate::VmClosure>,
    index: usize,
    item: VmValue,
) -> (usize, String, Result<VmValue, VmError>) {
    let mut vm = ctx.child_vm();
    let result = vm.call_closure_pub(&closure, &[item]).await;
    let output = vm.take_output();
    (index, output, result)
}

fn elapsed_ms(clock: &dyn harn_clock::Clock, started_ms: i64) -> u64 {
    clock.monotonic_ms().saturating_sub(started_ms).max(0) as u64
}

fn composition_validation_source(snippet: &str) -> String {
    let mut source = String::from("pipeline main() {\n");
    source.push_str(snippet);
    if !snippet.ends_with('\n') {
        source.push('\n');
    }
    source.push_str("}\n");
    source
}

fn composition_source(manifest: &BindingManifest, snippet: &str) -> String {
    let mut source = String::new();
    for binding in &manifest.bindings {
        source.push_str(&format!(
            "fn {}(args = {{}}) {{ return __composition_call(\"{}\", args) }}\n",
            binding.binding,
            escape_harn_string(&binding.name)
        ));
    }
    source.push_str("pipeline main() {\n");
    source.push_str(snippet);
    if !snippet.ends_with('\n') {
        source.push('\n');
    }
    source.push_str("}\n");
    source
}

fn escape_harn_string(value: &str) -> String {
    harn_lexer::escape_string_literal(value)
}

fn validate_composition_program(
    program: &[harn_parser::SNode],
    manifest: &BindingManifest,
) -> Result<(), String> {
    use harn_parser::visit::walk_program;
    use harn_parser::Node;

    let bindings = manifest
        .bindings
        .iter()
        .map(|entry| entry.binding.clone())
        .collect::<BTreeSet<_>>();
    let mut local_functions = BTreeSet::from(["__composition_call".to_string()]);
    walk_program(program, &mut |node| {
        if let Node::FnDecl { name, .. } = &node.node {
            local_functions.insert(name.clone());
        }
    });

    let mut error = None;
    walk_program(program, &mut |node| {
        if error.is_some() {
            return;
        }
        match &node.node {
            Node::ImportDecl { .. } | Node::SelectiveImport { .. } => {
                error = Some("composition snippets cannot import modules".to_string());
            }
            Node::SpawnExpr { .. } | Node::Parallel { .. } => {
                error = Some("composition snippets cannot spawn or parallelize work".to_string());
            }
            Node::HitlExpr { .. } => {
                error = Some("composition snippets cannot request HITL directly".to_string());
            }
            Node::CostRoute { .. } => {
                error = Some("composition snippets cannot open LLM routing blocks".to_string());
            }
            Node::FunctionCall { name, .. } => {
                if DENIED_COMPOSITION_CALLS.contains(&name.as_str()) && !bindings.contains(name) {
                    error = Some(format!("composition snippets cannot call `{name}`"));
                } else if !bindings.contains(name)
                    && !local_functions.contains(name)
                    && !PURE_COMPOSITION_CALLS.contains(&name.as_str())
                {
                    error = Some(format!(
                        "composition call target `{name}` is not a manifest binding or pure helper"
                    ));
                }
            }
            _ => {}
        }
    });
    error.map_or(Ok(()), Err)
}

const DENIED_COMPOSITION_CALLS: &[&str] = &[
    "append_file",
    "append_file_locked",
    "ask_user",
    "connector_call",
    "copy_file",
    "delete_file",
    "dual_control",
    "escalate_to",
    "event_log_emit",
    "event_log.emit",
    "exec",
    "host_call",
    "host_tool_call",
    "http_delete",
    "http_download",
    "http_get",
    "http_patch",
    "http_post",
    "http_put",
    "http_request",
    "llm_call",
    "mcp_call",
    "mcp_connect",
    "pg_execute",
    "pg_query",
    "request_approval",
    "secret_get",
    "write_file",
    "write_file_bytes",
    "replace_file",
    "replace_file_result",
    "replace_file_bytes",
    "replace_file_bytes_result",
];

const PURE_COMPOSITION_CALLS: &[&str] = &[
    "Ok",
    "Err",
    "abs",
    "assert",
    "assert_eq",
    "assert_ne",
    "base64_decode",
    "base64_encode",
    "ceil",
    "contains",
    "dedup_by",
    "dirname",
    "entries",
    "ends_with",
    "flat_map",
    "floor",
    "format",
    "group_by",
    "hash_value",
    "hex_decode",
    "hex_encode",
    "is_err",
    "is_ok",
    "join",
    "jq",
    "jq_first",
    "json_extract",
    "json_parse",
    "json_pointer",
    "json_stringify",
    "keys",
    "len",
    "lower",
    "map_bounded",
    "parse_float_or",
    "parse_int_or",
    "split",
    "starts_with",
    "to_float",
    "to_int",
    "to_string",
    "trim",
    "upper",
    "values",
];

pub fn composition_search_examples(query: &str, limit: usize) -> Value {
    let mut examples = vec![
        serde_json::json!({
            "id": "read-summarize",
            "title": "Read two files and return a compact summary",
            "language": "harn",
            "snippet": "const readme = read_file({path: \"README.md\"})\nconst spec = read_file({path: \"spec/HARN_SPEC.md\", limit: 80})\nreturn {readme: readme, spec_excerpt: spec}",
            "required_side_effect_level": "read_only",
            "tools": ["read_file"]
        }),
        serde_json::json!({
            "id": "search-then-read",
            "title": "Search first, then read the best candidate",
            "language": "harn",
            "snippet": "const hits = search({query: \"CompositionRunEnvelope\"})\nreturn hits",
            "required_side_effect_level": "read_only",
            "tools": ["search"]
        }),
    ];
    if !query.trim().is_empty() {
        let q = query.to_ascii_lowercase();
        examples.retain(|example| {
            example
                .to_string()
                .to_ascii_lowercase()
                .contains(q.as_str())
        });
    }
    examples.truncate(limit.max(1));
    Value::Array(examples)
}

pub fn register_composition_builtins(vm: &mut Vm) {
    vm.register_builtin("composition_binding_manifest", |args, _out| {
        let tools = args
            .first()
            .map(crate::llm::vm_value_to_json)
            .unwrap_or(Value::Null);
        let options_json = args
            .get(1)
            .map(crate::llm::vm_value_to_json)
            .unwrap_or(Value::Null);
        let mut options = BindingManifestOptions::default();
        if let Some(ceiling) = options_json
            .get("side_effect_ceiling")
            .and_then(Value::as_str)
        {
            options.side_effect_ceiling = SideEffectLevel::parse(ceiling);
        }
        if let Some(include_denied) = options_json.get("include_denied").and_then(Value::as_bool) {
            options.include_denied = include_denied;
        }
        options.denied_tools = string_set_option(&options_json, "denied_tools");
        options.gated_tools = string_set_option(&options_json, "gated_tools");
        let manifest = binding_manifest_from_tool_surface(&tools, options);
        let value = if options_json.get("form").and_then(Value::as_str) == Some("compact") {
            manifest.to_compact_value()
        } else {
            manifest.to_value()
        };
        Ok(crate::json_to_vm_value(&value))
    });

    vm.register_builtin("composition_search_examples", |args, _out| {
        let query = args.first().map(VmValue::display).unwrap_or_default();
        let limit = args
            .get(1)
            .and_then(|value| match value {
                VmValue::Int(n) => Some((*n).max(1) as usize),
                _ => None,
            })
            .unwrap_or(10);
        Ok(crate::json_to_vm_value(&composition_search_examples(
            &query, limit,
        )))
    });

    vm.register_builtin("composition_typescript_declarations", |args, _out| {
        let manifest_value = args
            .first()
            .map(crate::llm::vm_value_to_json)
            .ok_or_else(|| {
                VmError::Runtime("composition_typescript_declarations: manifest is required".into())
            })?;
        let manifest: BindingManifest =
            serde_json::from_value(manifest_value).map_err(|error| {
                VmError::Runtime(format!(
                    "composition_typescript_declarations: invalid manifest: {error}"
                ))
            })?;
        Ok(VmValue::String(arcstr::ArcStr::from(
            composition_typescript_declarations(&manifest),
        )))
    });

    vm.register_builtin("composition_harn_api", |args, _out| {
        let manifest_value = args
            .first()
            .map(crate::llm::vm_value_to_json)
            .ok_or_else(|| VmError::Runtime("composition_harn_api: manifest is required".into()))?;
        let manifest: BindingManifest =
            serde_json::from_value(manifest_value).map_err(|error| {
                VmError::Runtime(format!("composition_harn_api: invalid manifest: {error}"))
            })?;
        Ok(VmValue::String(arcstr::ArcStr::from(composition_harn_api(
            &manifest,
        ))))
    });

    vm.register_builtin("composition_crystallization_trace", |args, _out| {
        let report_value = args
            .first()
            .map(crate::llm::vm_value_to_json)
            .ok_or_else(|| {
                VmError::Runtime("composition_crystallization_trace: report is required".into())
            })?;
        let report: CompositionExecutionReport =
            serde_json::from_value(report_value).map_err(|error| {
                VmError::Runtime(format!(
                    "composition_crystallization_trace: invalid report: {error}"
                ))
            })?;
        let options = args
            .get(1)
            .map(crate::llm::vm_value_to_json)
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        Ok(crate::json_to_vm_value(&composition_crystallization_trace(
            &report, &options,
        )))
    });

    vm.register_async_builtin("composition_execute", |ctx, args| async move {
        let snippet = args
            .first()
            .map(VmValue::display)
            .ok_or_else(|| VmError::Runtime("composition_execute: snippet is required".into()))?;
        let manifest_value = args
            .get(1)
            .map(crate::llm::vm_value_to_json)
            .ok_or_else(|| VmError::Runtime("composition_execute: manifest is required".into()))?;
        let dispatcher = args.get(2).and_then(|value| match value {
            VmValue::Closure(closure) => Some((**closure).clone()),
            VmValue::Dict(dict) => match dict.get("dispatcher") {
                Some(VmValue::Closure(closure)) => Some((**closure).clone()),
                _ => None,
            },
            _ => None,
        });
        let mut request = CompositionExecutionRequest {
            snippet,
            manifest: serde_json::from_value(manifest_value).map_err(|error| {
                VmError::Runtime(format!("composition_execute: invalid manifest: {error}"))
            })?,
            ..CompositionExecutionRequest::default()
        };
        if let Some(options) = args.get(2).map(crate::llm::vm_value_to_json) {
            if let Some(session_id) = options.get("session_id").and_then(Value::as_str) {
                request.session_id = Some(session_id.to_string());
            }
            if let Some(run_id) = options.get("run_id").and_then(Value::as_str) {
                request.run_id = run_id.to_string();
            }
            if let Some(max_operations) = options.get("max_operations").and_then(Value::as_u64) {
                request.limits.max_operations = max_operations;
            }
            if let Some(timeout_ms) = options.get("timeout_ms").and_then(Value::as_u64) {
                request.limits.timeout_ms = Some(timeout_ms);
            }
            if let Some(max_output_bytes) = options.get("max_output_bytes").and_then(Value::as_u64)
            {
                request.limits.max_output_bytes = max_output_bytes;
            }
            if let Some(max_concurrent) = options
                .get("max_concurrent_operations")
                .or_else(|| options.get("max_concurrent"))
                .and_then(Value::as_u64)
            {
                request.limits.max_concurrent_operations =
                    usize::try_from(max_concurrent).unwrap_or(usize::MAX).max(1);
            }
            if let Some(per_server) = options
                .get("max_concurrent_per_server")
                .or_else(|| options.get("per_server_concurrency"))
                .and_then(Value::as_u64)
            {
                request.limits.max_concurrent_per_server =
                    usize::try_from(per_server).unwrap_or(usize::MAX).max(1);
            }
            let trusted_servers = string_set_option(&options, "trusted_servers");
            let trusted_mcp_servers = string_set_option(&options, "trusted_mcp_servers");
            if !trusted_servers.is_empty() || !trusted_mcp_servers.is_empty() {
                request
                    .mcp_policy
                    .trusted_servers
                    .extend(trusted_servers.into_iter().chain(trusted_mcp_servers));
            }
            if let Some(trust_annotations) = options
                .get("trust_annotations")
                .or_else(|| options.get("trust_mcp_annotations"))
                .and_then(Value::as_bool)
            {
                request.mcp_policy.trust_annotations = trust_annotations;
            }
            if let Some(call_timeout_ms) = options.get("call_timeout_ms").and_then(Value::as_u64) {
                request.mcp_policy.call_timeout_ms = Some(call_timeout_ms);
            }
            if let Some(retry_options) = options.get("retry") {
                if let Some(max_attempts) =
                    retry_options.get("max_attempts").and_then(Value::as_u64)
                {
                    request.mcp_policy.retry.max_attempts =
                        u32::try_from(max_attempts).unwrap_or(u32::MAX).max(1);
                }
                if let Some(base_delay_ms) =
                    retry_options.get("base_delay_ms").and_then(Value::as_u64)
                {
                    request.mcp_policy.retry.base_delay_ms = base_delay_ms;
                }
                if let Some(max_delay_ms) =
                    retry_options.get("max_delay_ms").and_then(Value::as_u64)
                {
                    request.mcp_policy.retry.max_delay_ms = max_delay_ms;
                }
                if let Some(honor_retry_after) = retry_options
                    .get("honor_retry_after")
                    .and_then(Value::as_bool)
                {
                    request.mcp_policy.retry.honor_retry_after = honor_retry_after;
                }
            }
        }
        let host: Arc<dyn CompositionToolHost> = match dispatcher {
            Some(closure) => Arc::new(ClosureCompositionToolHost::new(closure, ctx.clone())),
            None => Arc::new(StaticCompositionToolHost::new(BTreeMap::new())),
        };
        let report = execute_harn_composition(request, host).await;
        Ok(crate::json_to_vm_value(
            &serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({"ok": false})),
        ))
    });
}

fn string_set_option(value: &Value, key: &str) -> BTreeSet<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
