//! Language-neutral executable tool-composition contract.
//!
//! A composition run is a tiny program over already-typed tool bindings. The
//! runtime must expose it as a parent run with child tool operations, not as an
//! opaque "execute code" blob, so policy, transcript, replay, and host approval
//! surfaces can keep reasoning about each child call normally.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agent_events::{ToolCallErrorCategory, ToolCallStatus};
use crate::tool_annotations::SideEffectLevel;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

mod crystallization;
mod events;
mod hosts;
mod manifest;
mod types;
mod typescript;

#[cfg(test)]
mod tests;

pub use crystallization::composition_crystallization_trace;
pub use events::composition_report_events;
pub use hosts::{ClosureCompositionToolHost, StaticCompositionToolHost};
pub use manifest::{
    binding_manifest_from_tool_surface, binding_manifest_hash, BindingManifest,
    BindingManifestEntry, BindingManifestOptions, BindingPolicyDisposition, BindingPolicyStatus,
    BINDING_MANIFEST_SCHEMA_VERSION,
};
pub use types::{
    CompositionChildCall, CompositionChildResult, CompositionExecutionLimits,
    CompositionExecutionReport, CompositionExecutionRequest, CompositionFailureCategory,
    CompositionRunEnvelope, CompositionToolHost, CompositionToolOutput,
    COMPOSITION_EXECUTION_SCHEMA_VERSION,
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
        });
    }

    fn push_result(
        &mut self,
        call: &CompositionChildCall,
        output: &CompositionToolOutput,
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
            status: if output.error.is_some() {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            },
            raw_output: output.value.clone(),
            error: output.error.clone(),
            error_category: output.error_category,
            executor: output.executor.clone(),
            duration_ms: Some(elapsed_ms),
            execution_duration_ms: Some(elapsed_ms),
        });
    }
}

/// Execute a read-only Harn-native composition snippet against a manifest.
pub async fn execute_harn_composition(
    mut request: CompositionExecutionRequest,
    host: Rc<dyn CompositionToolHost>,
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
    host: Rc<dyn CompositionToolHost>,
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
    let state = Rc::new(RefCell::new(ExecutionState {
        request,
        calls: Vec::new(),
        results: Vec::new(),
        clock: execution_clock,
        started_ms: execution_started_ms,
    }));
    let mut vm = Vm::new();
    crate::register_core_stdlib(&mut vm);
    register_composition_call_builtin(&mut vm, state.clone(), host);
    if let Some(timeout_ms) = state.borrow().request.limits.timeout_ms {
        vm.push_deadline_after(std::time::Duration::from_millis(timeout_ms));
    }
    vm.set_source_info("composition://snippet.harn", &source);
    match vm.execute(&chunk).await {
        Ok(value) => {
            let json = crate::llm::vm_value_to_json(&value);
            let stdout = vm.output().to_string();
            let state = state.borrow();
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
            let state = state.borrow();
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

fn register_composition_call_builtin(
    vm: &mut Vm,
    state: Rc<RefCell<ExecutionState>>,
    host: Rc<dyn CompositionToolHost>,
) {
    vm.register_async_builtin("__composition_call", move |args| {
        let state = state.clone();
        let host = host.clone();
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
                let mut state = state.borrow_mut();
                let (binding, call) = state.next_call(&tool_name, input.clone())?;
                (binding, call, state.clock.clone())
            };
            let started_ms = clock.monotonic_ms();
            let output = host.call(&binding, input).await;
            {
                let mut state = state.borrow_mut();
                state.push_result(&call, &output, elapsed_ms(&*clock, started_ms));
            }
            if let Some(error) = output.error {
                return Err(VmError::Runtime(error));
            }
            Ok(crate::json_to_vm_value(
                &output.value.unwrap_or(Value::Null),
            ))
        }
    });
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
    value.replace('\\', "\\\\").replace('"', "\\\"")
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
            "snippet": "let readme = read_file({path: \"README.md\"})\nlet spec = read_file({path: \"spec/HARN_SPEC.md\", limit: 80})\nreturn {readme: readme, spec_excerpt: spec}",
            "required_side_effect_level": "read_only",
            "tools": ["read_file"]
        }),
        serde_json::json!({
            "id": "search-then-read",
            "title": "Search first, then read the best candidate",
            "language": "harn",
            "snippet": "let hits = search({query: \"CompositionRunEnvelope\"})\nreturn hits",
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
        Ok(VmValue::String(Rc::from(
            composition_typescript_declarations(&manifest),
        )))
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

    vm.register_async_builtin("composition_execute", |args| async move {
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
        }
        let host: Rc<dyn CompositionToolHost> = match dispatcher {
            Some(closure) => {
                let outer_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
                    VmError::Runtime(
                        "composition_execute: dispatcher requires an async builtin VM context"
                            .into(),
                    )
                })?;
                Rc::new(ClosureCompositionToolHost::new(closure, outer_vm))
            }
            None => Rc::new(StaticCompositionToolHost::new(BTreeMap::new())),
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
