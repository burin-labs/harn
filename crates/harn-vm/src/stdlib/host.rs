use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value as JsonValue;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{values_equal, VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

/// Audited wrapper for `chrono::Utc::now().to_rfc3339()`. Routes through
/// the testbench leak audit so a paused-clock session can surface every
/// host capability that observed real wall-clock time.
fn audited_utc_now_rfc3339(capability_id: &'static str) -> String {
    let dt: chrono::DateTime<chrono::Utc> =
        crate::clock_mock::leak_audit::wall_now(capability_id).into();
    dt.to_rfc3339()
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_MOCK_BUILTIN_DEF,
    &HOST_MOCK_CLEAR_BUILTIN_DEF,
    &HOST_MOCK_CALLS_BUILTIN_DEF,
    &HOST_MOCK_PUSH_SCOPE_BUILTIN_DEF,
    &HOST_MOCK_POP_SCOPE_BUILTIN_DEF,
    &HOST_CAPABILITIES_BUILTIN_DEF,
    &HOST_HAS_BUILTIN_DEF,
    &HOST_CALL_BUILTIN_DEF,
    &HOST_TOOL_LIST_BUILTIN_DEF,
    &HOST_TOOL_CALL_BUILTIN_DEF,
];

#[derive(Clone)]
struct HostMock {
    capability: String,
    operation: String,
    params: Option<BTreeMap<String, VmValue>>,
    result: Option<VmValue>,
    error: Option<String>,
}

#[derive(Clone)]
struct HostMockCall {
    capability: String,
    operation: String,
    params: BTreeMap<String, VmValue>,
}

thread_local! {
    static HOST_MOCKS: RefCell<Vec<HostMock>> = const { RefCell::new(Vec::new()) };
    static HOST_MOCK_CALLS: RefCell<Vec<HostMockCall>> = const { RefCell::new(Vec::new()) };
    static HOST_MOCK_SCOPES: RefCell<Vec<(Vec<HostMock>, Vec<HostMockCall>)>> =
        const { RefCell::new(Vec::new()) };
}

pub(crate) fn reset_host_state() {
    HOST_MOCKS.with(|mocks| mocks.borrow_mut().clear());
    HOST_MOCK_CALLS.with(|calls| calls.borrow_mut().clear());
    HOST_MOCK_SCOPES.with(|scopes| scopes.borrow_mut().clear());
}

/// Push the current host-mock state onto an internal stack and start a
/// fresh empty scope. Paired with `pop_host_mock_scope`. Used by the
/// `with_host_mocks` helper in `std/testing` to give tests automatic
/// cleanup, including when the body throws.
fn push_host_mock_scope() {
    let mocks = HOST_MOCKS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let calls = HOST_MOCK_CALLS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    HOST_MOCK_SCOPES.with(|v| v.borrow_mut().push((mocks, calls)));
}

/// Restore the most recently pushed host-mock state, replacing any
/// mocks or recorded calls accumulated inside the scope. Returns
/// `false` if there is no saved scope to pop, so callers can surface a
/// clear "imbalanced scope" error rather than silently no-op'ing.
fn pop_host_mock_scope() -> bool {
    let entry = HOST_MOCK_SCOPES.with(|v| v.borrow_mut().pop());
    match entry {
        Some((mocks, calls)) => {
            HOST_MOCKS.with(|v| *v.borrow_mut() = mocks);
            HOST_MOCK_CALLS.with(|v| *v.borrow_mut() = calls);
            true
        }
        None => false,
    }
}

fn capability_manifest_map() -> BTreeMap<String, VmValue> {
    let mut root = BTreeMap::new();
    root.insert(
        "process".to_string(),
        capability(
            "Process execution.",
            &[
                op("exec", "Execute a process in argv or shell mode."),
                op("list_shells", "List shells discovered by the host/session."),
                op(
                    "get_default_shell",
                    "Return the selected default shell for this host/session.",
                ),
                op(
                    "set_default_shell",
                    "Select the default shell for this host/session.",
                ),
                op(
                    "shell_invocation",
                    "Resolve shell selection and login/interactive flags into argv.",
                ),
            ],
        ),
    );
    root.insert(
        "template".to_string(),
        capability(
            "Template rendering.",
            &[op("render", "Render a template file.")],
        ),
    );
    root.insert(
        "interaction".to_string(),
        capability(
            "User interaction.",
            &[op("ask", "Ask the user a question.")],
        ),
    );
    root.insert(
        "memory".to_string(),
        capability(
            "Vector-aware memory: host-provided embeddings.",
            &[op(
                "embed",
                "Embed text for semantic recall. Params: {text, model_hint?}. \
                 Returns {vector: list<float>, model: string, dim: int}.",
            )],
        ),
    );
    root
}

fn mocked_operation_entry() -> VmValue {
    op(
        "mocked",
        "Mocked host operation registered at runtime for tests.",
    )
    .1
}

fn ensure_mocked_capability(
    root: &mut BTreeMap<String, VmValue>,
    capability_name: &str,
    operation_name: &str,
) {
    let Some(existing) = root.get(capability_name).cloned() else {
        root.insert(
            capability_name.to_string(),
            capability(
                "Mocked host capability registered at runtime for tests.",
                &[(operation_name.to_string(), mocked_operation_entry())],
            ),
        );
        return;
    };

    let Some(existing_dict) = existing.as_dict() else {
        return;
    };
    let mut entry = (*existing_dict).clone();
    let mut ops = entry
        .get("ops")
        .and_then(|value| match value {
            VmValue::List(list) => Some((**list).clone()),
            _ => None,
        })
        .unwrap_or_default();
    if !ops.iter().any(|value| value.display() == operation_name) {
        ops.push(VmValue::String(std::sync::Arc::from(
            operation_name.to_string(),
        )));
    }

    let mut operations = entry
        .get("operations")
        .and_then(|value| value.as_dict())
        .map(|dict| (*dict).clone())
        .unwrap_or_default();
    operations
        .entry(operation_name.to_string())
        .or_insert_with(mocked_operation_entry);

    entry.insert("ops".to_string(), VmValue::List(std::sync::Arc::new(ops)));
    entry.insert(
        "operations".to_string(),
        VmValue::Dict(std::sync::Arc::new(operations)),
    );
    root.insert(
        capability_name.to_string(),
        VmValue::Dict(std::sync::Arc::new(entry)),
    );
}

fn capability_manifest_with_mocks() -> VmValue {
    let mut root = capability_manifest_map();
    HOST_MOCKS.with(|mocks| {
        for host_mock in mocks.borrow().iter() {
            ensure_mocked_capability(&mut root, &host_mock.capability, &host_mock.operation);
        }
    });
    VmValue::Dict(std::sync::Arc::new(root))
}

fn op(name: &str, description: &str) -> (String, VmValue) {
    let mut entry = BTreeMap::new();
    entry.insert(
        "description".to_string(),
        VmValue::String(std::sync::Arc::from(description)),
    );
    (name.to_string(), VmValue::Dict(std::sync::Arc::new(entry)))
}

fn capability(description: &str, ops: &[(String, VmValue)]) -> VmValue {
    let mut entry = BTreeMap::new();
    entry.insert(
        "description".to_string(),
        VmValue::String(std::sync::Arc::from(description)),
    );
    entry.insert(
        "ops".to_string(),
        VmValue::List(std::sync::Arc::new(
            ops.iter()
                .map(|(name, _)| VmValue::String(std::sync::Arc::from(name.as_str())))
                .collect(),
        )),
    );
    let mut op_dict = BTreeMap::new();
    for (name, op) in ops {
        op_dict.insert(name.clone(), op.clone());
    }
    entry.insert(
        "operations".to_string(),
        VmValue::Dict(std::sync::Arc::new(op_dict)),
    );
    VmValue::Dict(std::sync::Arc::new(entry))
}

fn require_param(params: &BTreeMap<String, VmValue>, key: &str) -> Result<String, VmError> {
    params
        .get(key)
        .map(|v| v.display())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
                "host_call: missing required parameter '{key}'"
            ))))
        })
}

fn render_template(
    path: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
) -> Result<String, VmError> {
    let asset = crate::stdlib::template::TemplateAsset::render_target(path).map_err(|msg| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "host_call template.render: {msg}"
        ))))
    })?;
    crate::stdlib::template::render_asset_result(&asset, bindings).map_err(VmError::from)
}

fn params_match(
    expected: Option<&BTreeMap<String, VmValue>>,
    actual: &BTreeMap<String, VmValue>,
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    expected.iter().all(|(key, value)| {
        actual
            .get(key)
            .is_some_and(|candidate| values_equal(candidate, value))
    })
}

fn parse_host_mock(args: &[VmValue]) -> Result<HostMock, VmError> {
    let capability = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    let operation = args.get(1).map(|value| value.display()).unwrap_or_default();
    if capability.is_empty() || operation.is_empty() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "host_mock: capability and operation are required",
        ))));
    }

    let mut params = args
        .get(3)
        .and_then(|value| value.as_dict())
        .map(|dict| (*dict).clone());
    let mut result = args.get(2).cloned().or(Some(VmValue::Nil));
    let mut error = None;

    if let Some(config) = args.get(2).and_then(|value| value.as_dict()) {
        if config.contains_key("result")
            || config.contains_key("params")
            || config.contains_key("error")
        {
            params = config
                .get("params")
                .and_then(|value| value.as_dict())
                .map(|dict| (*dict).clone());
            result = config.get("result").cloned();
            error = config
                .get("error")
                .map(|value| value.display())
                .filter(|value| !value.is_empty());
        }
    }

    Ok(HostMock {
        capability,
        operation,
        params,
        result,
        error,
    })
}

fn push_host_mock(host_mock: HostMock) {
    HOST_MOCKS.with(|mocks| mocks.borrow_mut().push(host_mock));
}

fn mock_call_value(call: &HostMockCall) -> VmValue {
    let mut item = BTreeMap::new();
    item.insert(
        "capability".to_string(),
        VmValue::String(std::sync::Arc::from(call.capability.clone())),
    );
    item.insert(
        "operation".to_string(),
        VmValue::String(std::sync::Arc::from(call.operation.clone())),
    );
    item.insert(
        "params".to_string(),
        VmValue::Dict(std::sync::Arc::new(call.params.clone())),
    );
    VmValue::Dict(std::sync::Arc::new(item))
}

fn record_mock_call(capability: &str, operation: &str, params: &BTreeMap<String, VmValue>) {
    HOST_MOCK_CALLS.with(|calls| {
        calls.borrow_mut().push(HostMockCall {
            capability: capability.to_string(),
            operation: operation.to_string(),
            params: params.clone(),
        });
    });
}

pub(crate) fn dispatch_mock_host_call(
    capability: &str,
    operation: &str,
    params: &BTreeMap<String, VmValue>,
) -> Option<Result<VmValue, VmError>> {
    let matched = HOST_MOCKS.with(|mocks| {
        mocks
            .borrow()
            .iter()
            .rev()
            .find(|host_mock| {
                host_mock.capability == capability
                    && host_mock.operation == operation
                    && params_match(host_mock.params.as_ref(), params)
            })
            .cloned()
    })?;

    record_mock_call(capability, operation, params);
    if let Some(error) = matched.error {
        return Some(Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            error,
        )))));
    }
    Some(Ok(matched.result.unwrap_or(VmValue::Nil)))
}

/// Embedder-supplied bridge for `host_call` ops.
///
/// Embedders (debug adapters, CLIs, IDE hosts) implement this trait to
/// satisfy capability/operation pairs that harn-vm itself doesn't know how
/// to handle. Returning `Ok(None)` means "I don't handle this op — fall
/// through to the built-in fallbacks (env-derived defaults, then the
/// `unsupported operation` error)". `Ok(Some(value))` is the result;
/// `Err(VmError::Thrown(_))` surfaces as a Harn exception.
///
/// The trait is intentionally synchronous. Bridges that need async I/O
/// (e.g. DAP reverse requests) should drive their own runtime or use a
/// blocking channel — see `harn-dap`'s `DapHostBridge` for the canonical
/// pattern. Sync keeps the boundary simple and avoids forcing the entire
/// dispatch path into an opaque future.
pub trait HostCallBridge: Send + Sync {
    fn dispatch(
        &self,
        capability: &str,
        operation: &str,
        params: &BTreeMap<String, VmValue>,
    ) -> Result<Option<VmValue>, VmError>;

    fn list_tools(&self) -> Result<Option<VmValue>, VmError> {
        Ok(None)
    }

    fn call_tool(&self, _name: &str, _args: &VmValue) -> Result<Option<VmValue>, VmError> {
        Ok(None)
    }
}

thread_local! {
    static HOST_CALL_BRIDGE: RefCell<Option<Arc<dyn HostCallBridge>>> = const { RefCell::new(None) };
}

/// Install a bridge for the current thread. The bridge is consulted on
/// every `host_call` *after* mock matching but *before* the built-in
/// match arms, so embedders can override anything they like (and equally
/// punt on anything they don't, by returning `Ok(None)`).
pub fn set_host_call_bridge(bridge: Arc<dyn HostCallBridge>) {
    HOST_CALL_BRIDGE.with(|b| *b.borrow_mut() = Some(bridge));
}

/// Remove the current thread's bridge. Idempotent.
pub fn clear_host_call_bridge() {
    HOST_CALL_BRIDGE.with(|b| *b.borrow_mut() = None);
}

/// Dispatch `(capability, operation, params)` to the currently-installed
/// `HostCallBridge`, if any. `Some(Ok(_))` means the bridge handled the
/// call; `Some(Err(_))` means it tried but raised; `None` means there is
/// no bridge or the bridge declined this op (returned `Ok(None)`).
///
/// Mirrors the inner block of `dispatch_host_operation` but without the
/// mock-call check or the built-in fallbacks — useful for callers that
/// want to treat the bridge as one of several sinks (e.g. inbound MCP
/// `elicitation/create` requests).
pub fn dispatch_host_call_bridge(
    capability: &str,
    operation: &str,
    params: &BTreeMap<String, VmValue>,
) -> Option<Result<VmValue, VmError>> {
    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone())?;
    match bridge.dispatch(capability, operation, params) {
        Ok(Some(value)) => Some(Ok(value)),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

fn empty_tool_list_value() -> VmValue {
    VmValue::List(std::sync::Arc::new(Vec::new()))
}

fn current_vm_host_bridge(
    ctx: Option<&AsyncBuiltinCtx>,
) -> Option<std::sync::Arc<crate::bridge::HostBridge>> {
    ctx.and_then(|ctx| ctx.child_vm().bridge.clone())
}

#[cfg(test)]
async fn dispatch_host_tool_list() -> Result<VmValue, VmError> {
    dispatch_host_tool_list_with_ctx(None).await
}

async fn dispatch_host_tool_list_with_ctx(
    ctx: Option<&AsyncBuiltinCtx>,
) -> Result<VmValue, VmError> {
    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone());
    if let Some(bridge) = bridge {
        if let Some(value) = bridge.list_tools()? {
            return Ok(value);
        }
    }

    let Some(bridge) = current_vm_host_bridge(ctx) else {
        return Ok(empty_tool_list_value());
    };
    let tools = bridge.list_host_tools().await?;
    Ok(crate::bridge::json_result_to_vm_value(&JsonValue::Array(
        tools.into_iter().collect(),
    )))
}

pub(crate) async fn dispatch_host_tool_call(
    name: &str,
    args: &VmValue,
) -> Result<VmValue, VmError> {
    dispatch_host_tool_call_with_ctx(None, name, args).await
}

pub(crate) async fn dispatch_host_tool_call_with_ctx(
    ctx: Option<&AsyncBuiltinCtx>,
    name: &str,
    args: &VmValue,
) -> Result<VmValue, VmError> {
    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone());
    if let Some(bridge) = bridge {
        if let Some(value) = bridge.call_tool(name, args)? {
            return Ok(value);
        }
    }

    let Some(bridge) = current_vm_host_bridge(ctx) else {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "host_tool_call: no host bridge is attached",
        ))));
    };

    let result = bridge
        .call(
            "builtin_call",
            serde_json::json!({
                "name": name,
                "args": [crate::llm::vm_value_to_json(args)],
            }),
        )
        .await?;
    Ok(crate::bridge::json_result_to_vm_value(&result))
}

pub(crate) async fn dispatch_host_operation(
    capability: &str,
    operation: &str,
    params: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    dispatch_host_operation_with_ctx(None, capability, operation, params).await
}

pub(crate) async fn dispatch_host_operation_with_ctx(
    ctx: Option<&AsyncBuiltinCtx>,
    capability: &str,
    operation: &str,
    params: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    if let Some(mocked) = dispatch_mock_host_call(capability, operation, params) {
        return mocked;
    }

    if (capability, operation) == ("process", "exec") {
        let caller = serde_json::json!({
            "surface": "host_call",
            "capability": "process",
            "operation": "exec",
            "session_id": crate::llm::current_agent_session_id(),
        });
        return dispatch_process_exec_with_policy(ctx, params, caller).await;
    }

    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone());
    if let Some(bridge) = bridge {
        if let Some(value) = bridge.dispatch(capability, operation, params)? {
            return Ok(value);
        }
    }

    dispatch_builtin_host_operation(capability, operation, params).await
}

async fn dispatch_builtin_host_operation(
    capability: &str,
    operation: &str,
    params: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    match (capability, operation) {
        ("process", "list_shells") => Ok(crate::shells::list_shells_vm_value()),
        ("process", "get_default_shell") => Ok(crate::shells::default_shell_vm_value()),
        ("process", "set_default_shell") => crate::shells::set_default_shell_vm_value(params),
        ("process", "shell_invocation") => crate::shells::shell_invocation_vm_value(params),
        ("template", "render") => {
            let path = require_param(params, "path")?;
            let bindings = params.get("bindings").and_then(|v| v.as_dict());
            Ok(VmValue::String(std::sync::Arc::from(render_template(
                &path, bindings,
            )?)))
        }
        ("interaction", "ask") => {
            let question = require_param(params, "question")?;
            use std::io::BufRead;
            print!("{question}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut input = String::new();
            if std::io::stdin().lock().read_line(&mut input).is_ok() {
                Ok(VmValue::String(std::sync::Arc::from(input.trim_end())))
            } else {
                Ok(VmValue::Nil)
            }
        }
        // Standalone-run fallbacks for capabilities normally supplied by
        // an embedder's JSON-RPC bridge. `runtime.task` lets a debugger or
        // CLI invocation read the pipeline input from `HARN_TASK` without
        // the host explicitly wiring a callback for every op.
        ("runtime", "task") => Ok(VmValue::String(std::sync::Arc::from(
            std::env::var("HARN_TASK").unwrap_or_default(),
        ))),
        ("runtime", "set_result") => {
            // No-op when no host is attached; swallow silently so standalone
            // scripts can still call `set_result` without crashing.
            Ok(VmValue::Nil)
        }
        ("workspace", "project_root") => {
            // Standalone fallback: prefer HARN_PROJECT_ROOT, then the
            // current working directory. Pipelines call this very early so
            // crashing here would block any debug-launched script.
            let path = std::env::var("HARN_PROJECT_ROOT").unwrap_or_else(|_| {
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            });
            Ok(VmValue::String(std::sync::Arc::from(path)))
        }
        ("workspace", "cwd") => {
            let path = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            Ok(VmValue::String(std::sync::Arc::from(path)))
        }
        _ => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("host_call: unsupported operation {capability}.{operation}"),
        )))),
    }
}

pub(crate) async fn dispatch_process_exec(
    params: &BTreeMap<String, VmValue>,
    caller: serde_json::Value,
) -> Result<VmValue, VmError> {
    dispatch_process_exec_with_policy(None, params, caller).await
}

async fn dispatch_process_exec_with_policy(
    ctx: Option<&AsyncBuiltinCtx>,
    params: &BTreeMap<String, VmValue>,
    caller: serde_json::Value,
) -> Result<VmValue, VmError> {
    let (params, command_policy_context, command_policy_decisions) =
        match crate::orchestration::run_command_policy_preflight_with_ctx(ctx, params, caller)
            .await?
        {
            crate::orchestration::CommandPolicyPreflight::Proceed {
                params,
                context,
                decisions,
            } => (params, context, decisions),
            crate::orchestration::CommandPolicyPreflight::Blocked {
                status,
                message,
                context,
                decisions,
            } => {
                return Ok(crate::orchestration::blocked_command_response(
                    params, status, &message, context, decisions,
                ));
            }
        };

    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone());
    if let Some(bridge) = bridge {
        if let Some(value) = bridge.dispatch("process", "exec", &params)? {
            return crate::orchestration::run_command_policy_postflight_with_ctx(
                ctx,
                &params,
                value,
                command_policy_context,
                command_policy_decisions,
            )
            .await;
        }
    }

    dispatch_process_exec_after_policy(
        ctx,
        &params,
        command_policy_context,
        command_policy_decisions,
    )
    .await
}

async fn dispatch_process_exec_after_policy(
    ctx: Option<&AsyncBuiltinCtx>,
    params: &BTreeMap<String, VmValue>,
    command_policy_context: JsonValue,
    command_policy_decisions: Vec<crate::orchestration::CommandPolicyDecision>,
) -> Result<VmValue, VmError> {
    let (program, args) = process_exec_argv(params)?;
    let timeout_ms = optional_i64(params, "timeout")
        .or_else(|| optional_i64(params, "timeout_ms"))
        .filter(|value| *value > 0)
        .map(|value| value as u64);
    // Optional per-call profile override. Pipelines that want to
    // promote a single spawn to `os_hardened` (e.g. running
    // attacker-controlled code) pass `sandbox_profile: "os_hardened"`
    // without having to rewrite the surrounding policy. The override
    // is scoped to this call and pops with the guard at end-of-scope.
    let profile_guard = match optional_string(params, "sandbox_profile") {
        Some(value) => Some(push_sandbox_profile_override(&value)?),
        None => None,
    };
    let mut cmd = crate::process_sandbox::tokio_command_for(&program, &args)
        .map_err(|e| VmError::Runtime(format!("host_call process.exec sandbox setup: {e}")))?;
    if let Some(cwd) = optional_string(params, "cwd") {
        let cwd = resolve_process_exec_cwd(&cwd);
        crate::process_sandbox::enforce_process_cwd(&cwd)
            .map_err(|e| VmError::Runtime(format!("host_call process.exec cwd: {e}")))?;
        cmd.current_dir(cwd);
    }
    if let Some(env) = optional_string_dict(params, "env")? {
        // `env_mode` controls how the provided `env` keys combine with the
        // parent environment:
        //   - "merge" (default): inherit the parent env and overlay the
        //     provided keys. This is the least-surprising behavior — a
        //     caller passing `env: {ONE_VAR: "x"}` keeps PATH/HOME/etc.
        //   - "replace": clear the parent env entirely, then set only the
        //     provided keys. Must be requested explicitly now; previously
        //     this was the (footgun) default whenever `env` was supplied.
        let env_mode = optional_string(params, "env_mode");
        match env_mode.as_deref().unwrap_or("merge") {
            "replace" => {
                cmd.env_clear();
            }
            "merge" => {}
            other => {
                return Err(VmError::Runtime(format!(
                    "host_call process.exec: unknown env_mode {other:?}; expected \"merge\" or \"replace\""
                )));
            }
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
    }
    // env_remove: list of environment variable names to strip before
    // spawning. Applied after `env` so callers can both inherit and
    // selectively unset (e.g. the git stdlib strips `GIT_*` so its
    // operations are self-contained even when Harn is invoked from
    // inside a git hook that sets `GIT_DIR`).
    if let Some(env_remove) = optional_string_list(params, "env_remove") {
        for key in env_remove {
            cmd.env_remove(key);
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let started_at = audited_utc_now_rfc3339("host_call/process.exec.started_at");
    let started = crate::clock_mock::leak_audit::instant_now("host_call/process.exec.started");
    let child = cmd
        .spawn()
        .map_err(|e| VmError::Runtime(format!("host_call process.exec: {e}")))?;
    drop(profile_guard);
    let pid = child.id();
    let timed_out;
    let output_result = if let Some(timeout_ms) = timeout_ms {
        match tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            child.wait_with_output(),
        )
        .await
        {
            Ok(result) => {
                timed_out = false;
                result
            }
            Err(_) => {
                let response = process_exec_response(ProcessExecResponse {
                    pid,
                    started_at,
                    started,
                    stdout: "",
                    stderr: "",
                    exit_code: -1,
                    status: "timed_out",
                    success: false,
                    timed_out: true,
                });
                return crate::orchestration::run_command_policy_postflight_with_ctx(
                    ctx,
                    params,
                    response,
                    command_policy_context,
                    command_policy_decisions,
                )
                .await;
            }
        }
    } else {
        timed_out = false;
        child.wait_with_output().await
    };
    let output =
        output_result.map_err(|e| VmError::Runtime(format!("host_call process.exec: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);
    let response = process_exec_response(ProcessExecResponse {
        pid,
        started_at,
        started,
        stdout: &stdout,
        stderr: &stderr,
        exit_code,
        status: if timed_out { "timed_out" } else { "completed" },
        success: output.status.success(),
        timed_out,
    });
    crate::orchestration::run_command_policy_postflight_with_ctx(
        ctx,
        params,
        response,
        command_policy_context,
        command_policy_decisions,
    )
    .await
}

struct ProcessExecResponse<'a> {
    pid: Option<u32>,
    started_at: String,
    started: Instant,
    stdout: &'a str,
    stderr: &'a str,
    exit_code: i32,
    status: &'a str,
    success: bool,
    timed_out: bool,
}

fn process_exec_response(response: ProcessExecResponse<'_>) -> VmValue {
    let combined = format!("{}{}", response.stdout, response.stderr);
    let mut result = BTreeMap::new();
    result.insert(
        "command_id".to_string(),
        VmValue::String(std::sync::Arc::from(format!(
            "cmd_{}_{}",
            std::process::id(),
            response.started.elapsed().as_nanos()
        ))),
    );
    result.insert(
        "status".to_string(),
        VmValue::String(std::sync::Arc::from(response.status)),
    );
    result.insert(
        "pid".to_string(),
        response
            .pid
            .map(|pid| VmValue::Int(pid as i64))
            .unwrap_or(VmValue::Nil),
    );
    result.insert(
        "process_group_id".to_string(),
        response
            .pid
            .map(|pid| VmValue::Int(pid as i64))
            .unwrap_or(VmValue::Nil),
    );
    result.insert("handle_id".to_string(), VmValue::Nil);
    result.insert(
        "started_at".to_string(),
        VmValue::String(std::sync::Arc::from(response.started_at)),
    );
    result.insert(
        "ended_at".to_string(),
        VmValue::String(std::sync::Arc::from(audited_utc_now_rfc3339(
            "host_call/process.exec.ended_at",
        ))),
    );
    result.insert(
        "duration_ms".to_string(),
        VmValue::Int(response.started.elapsed().as_millis() as i64),
    );
    result.insert(
        "exit_code".to_string(),
        VmValue::Int(response.exit_code as i64),
    );
    result.insert("signal".to_string(), VmValue::Nil);
    result.insert("timed_out".to_string(), VmValue::Bool(response.timed_out));
    result.insert(
        "stdout".to_string(),
        VmValue::String(std::sync::Arc::from(response.stdout.to_string())),
    );
    result.insert(
        "stderr".to_string(),
        VmValue::String(std::sync::Arc::from(response.stderr.to_string())),
    );
    result.insert(
        "combined".to_string(),
        VmValue::String(std::sync::Arc::from(combined)),
    );
    result.insert(
        "exit_status".to_string(),
        VmValue::Int(response.exit_code as i64),
    );
    result.insert(
        "legacy_status".to_string(),
        VmValue::Int(response.exit_code as i64),
    );
    result.insert("success".to_string(), VmValue::Bool(response.success));
    VmValue::Dict(std::sync::Arc::new(result))
}

fn resolve_process_exec_cwd(cwd: &str) -> std::path::PathBuf {
    crate::stdlib::process::resolve_source_relative_path(cwd)
}

fn process_exec_argv(params: &BTreeMap<String, VmValue>) -> Result<(String, Vec<String>), VmError> {
    match optional_string(params, "mode")
        .as_deref()
        .unwrap_or("shell")
    {
        "argv" => {
            let argv = optional_string_list(params, "argv").ok_or_else(|| {
                VmError::Runtime("host_call process.exec missing argv".to_string())
            })?;
            split_argv(argv)
        }
        "shell" => {
            let command = require_param(params, "command")?;
            let mut invocation_params = params.clone();
            invocation_params.insert(
                "command".to_string(),
                VmValue::String(std::sync::Arc::from(command)),
            );
            let invocation =
                crate::shells::resolve_invocation_from_vm_params(&invocation_params)
                    .map_err(|err| VmError::Runtime(format!("host_call process.exec: {err}")))?;
            Ok((invocation.program, invocation.args))
        }
        other => Err(VmError::Runtime(format!(
            "host_call process.exec unsupported mode {other:?}"
        ))),
    }
}

fn split_argv(mut argv: Vec<String>) -> Result<(String, Vec<String>), VmError> {
    if argv.is_empty() {
        return Err(VmError::Runtime(
            "host_call process.exec argv must not be empty".to_string(),
        ));
    }
    let program = argv.remove(0);
    if program.is_empty() {
        return Err(VmError::Runtime(
            "host_call process.exec argv[0] must not be empty".to_string(),
        ));
    }
    Ok((program, argv))
}

/// Push a transient policy onto the execution stack with the
/// requested sandbox profile, returning a guard that pops on drop.
/// Used by `host_call("process", "exec", ...)` to honor a per-call
/// `sandbox_profile` override without rewriting the surrounding
/// orchestration policy.
fn push_sandbox_profile_override(value: &str) -> Result<SandboxProfileGuard, VmError> {
    let profile = crate::orchestration::SandboxProfile::parse(value).ok_or_else(|| {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
            "host_call process.exec: unknown sandbox_profile {value:?}; expected one of \"unrestricted\", \"worktree\", \"os_hardened\", \"wasi\""
        ))))
    })?;
    let mut policy = crate::orchestration::current_execution_policy().unwrap_or_default();
    policy.sandbox_profile = profile;
    crate::orchestration::push_execution_policy(policy);
    Ok(SandboxProfileGuard {
        _private: std::marker::PhantomData,
    })
}

struct SandboxProfileGuard {
    _private: std::marker::PhantomData<*const ()>,
}

impl Drop for SandboxProfileGuard {
    fn drop(&mut self) {
        crate::orchestration::pop_execution_policy();
    }
}

fn optional_i64(params: &BTreeMap<String, VmValue>, key: &str) -> Option<i64> {
    match params.get(key) {
        Some(VmValue::Int(value)) => Some(*value),
        Some(VmValue::Float(value)) if value.fract() == 0.0 => Some(*value as i64),
        _ => None,
    }
}

fn optional_string(params: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    params.get(key).and_then(vm_string).map(ToString::to_string)
}

fn optional_string_list(params: &BTreeMap<String, VmValue>, key: &str) -> Option<Vec<String>> {
    let VmValue::List(values) = params.get(key)? else {
        return None;
    };
    values
        .iter()
        .map(|value| vm_string(value).map(ToString::to_string))
        .collect()
}

fn optional_string_dict(
    params: &BTreeMap<String, VmValue>,
    key: &str,
) -> Result<Option<BTreeMap<String, String>>, VmError> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    let Some(dict) = value.as_dict() else {
        return Err(VmError::Runtime(format!(
            "host_call process.exec {key} must be a dict"
        )));
    };
    let mut out = BTreeMap::new();
    for (key, value) in dict.iter() {
        let Some(value) = vm_string(value) else {
            return Err(VmError::Runtime(format!(
                "host_call process.exec env value for {key:?} must be a string"
            )));
        };
        out.insert(key.clone(), value.to_string());
    }
    Ok(Some(out))
}

fn vm_string(value: &VmValue) -> Option<&str> {
    match value {
        VmValue::String(value) => Some(value.as_ref()),
        _ => None,
    }
}

pub(crate) fn register_host_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "host_mock(capability: string, op: string, response_or_config?: any, params?: dict) -> nil",
    category = "host"
)]
fn host_mock_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let host_mock = parse_host_mock(args)?;
    push_host_mock(host_mock);
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "host_mock_clear() -> nil", category = "host")]
fn host_mock_clear_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    reset_host_state();
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "host_mock_calls() -> list", category = "host")]
fn host_mock_calls_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let calls = HOST_MOCK_CALLS.with(|calls| {
        calls
            .borrow()
            .iter()
            .map(mock_call_value)
            .collect::<Vec<_>>()
    });
    Ok(VmValue::List(std::sync::Arc::new(calls)))
}

#[harn_builtin(sig = "host_mock_push_scope() -> nil", category = "host")]
fn host_mock_push_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    push_host_mock_scope();
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "host_mock_pop_scope() -> nil", category = "host")]
fn host_mock_pop_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if !pop_host_mock_scope() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "host_mock_pop_scope: no scope to pop",
        ))));
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "host_capabilities() -> dict", category = "host")]
fn host_capabilities_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(capability_manifest_with_mocks())
}

#[harn_builtin(
    sig = "host_has(capability: string, op?: string) -> bool",
    category = "host"
)]
fn host_has_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let capability = args.first().map(|a| a.display()).unwrap_or_default();
    let operation = args.get(1).map(|a| a.display());
    let manifest = capability_manifest_with_mocks();
    let has = manifest
        .as_dict()
        .and_then(|d| d.get(&capability))
        .and_then(|v| v.as_dict())
        .is_some_and(|cap| {
            if let Some(operation) = operation {
                cap.get("ops")
                    .and_then(|v| match v {
                        VmValue::List(list) => {
                            Some(list.iter().any(|item| item.display() == operation))
                        }
                        _ => None,
                    })
                    .unwrap_or(false)
            } else {
                true
            }
        });
    Ok(VmValue::Bool(has))
}

#[harn_builtin(
    sig = "host_call(name: string, args?: dict) -> any",
    kind = "async",
    category = "host"
)]
async fn host_call_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let name = args.first().map(|a| a.display()).unwrap_or_default();
    let params = args
        .get(1)
        .and_then(|a| a.as_dict())
        .cloned()
        .unwrap_or_default();
    let Some((capability, operation)) = name.split_once('.') else {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            format!("host_call: unsupported operation name '{name}'"),
        ))));
    };
    dispatch_host_operation_with_ctx(Some(&ctx), capability, operation, &params).await
}

#[harn_builtin(sig = "host_tool_list() -> list", kind = "async", category = "host")]
async fn host_tool_list_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    _args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    dispatch_host_tool_list_with_ctx(Some(&ctx)).await
}

#[harn_builtin(
    sig = "host_tool_call(name: string, args?: any) -> any",
    kind = "async",
    category = "host"
)]
async fn host_tool_call_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let name = args.first().map(|a| a.display()).unwrap_or_default();
    if name.is_empty() {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "host_tool_call: tool name is required",
        ))));
    }
    let call_args = args.get(1).cloned().unwrap_or(VmValue::Nil);
    dispatch_host_tool_call_with_ctx(Some(&ctx), &name, &call_args).await
}

#[cfg(test)]
mod tests {
    use super::{
        capability_manifest_with_mocks, clear_host_call_bridge, dispatch_host_operation,
        dispatch_host_tool_call, dispatch_host_tool_list, dispatch_mock_host_call, push_host_mock,
        reset_host_state, resolve_process_exec_cwd, set_host_call_bridge, HostCallBridge, HostMock,
    };
    use std::collections::BTreeMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use crate::value::{VmError, VmValue};

    #[test]
    fn process_exec_relative_cwd_resolves_against_execution_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(dir.path().to_string_lossy().into_owned()),
                source_dir: Some(dir.path().join("src").to_string_lossy().into_owned()),
                env: BTreeMap::new(),
                adapter: None,
                repo_path: None,
                worktree_path: None,
                branch: None,
                base_ref: None,
                cleanup: None,
            },
        ));

        assert_eq!(
            resolve_process_exec_cwd("subdir"),
            dir.path().join("subdir")
        );

        crate::stdlib::process::set_thread_execution_context(None);
    }

    #[test]
    fn manifest_includes_operation_metadata() {
        let manifest = capability_manifest_with_mocks();
        let process = manifest
            .as_dict()
            .and_then(|d| d.get("process"))
            .and_then(|v| v.as_dict())
            .expect("process capability");
        assert!(process.get("description").is_some());
        let operations = process
            .get("operations")
            .and_then(|v| v.as_dict())
            .expect("operations dict");
        assert!(operations.get("exec").is_some());
    }

    #[test]
    fn mocked_capabilities_appear_in_manifest() {
        reset_host_state();
        push_host_mock(HostMock {
            capability: "project".to_string(),
            operation: "metadata_get".to_string(),
            params: None,
            result: Some(VmValue::Dict(std::sync::Arc::new(BTreeMap::new()))),
            error: None,
        });
        let manifest = capability_manifest_with_mocks();
        let project = manifest
            .as_dict()
            .and_then(|d| d.get("project"))
            .and_then(|v| v.as_dict())
            .expect("project capability");
        let operations = project
            .get("operations")
            .and_then(|v| v.as_dict())
            .expect("operations dict");
        assert!(operations.get("metadata_get").is_some());
        reset_host_state();
    }

    #[test]
    fn mock_host_call_matches_partial_params_and_overrides_order() {
        reset_host_state();
        let mut exact_params = BTreeMap::new();
        exact_params.insert(
            "namespace".to_string(),
            VmValue::String(std::sync::Arc::from("facts")),
        );
        push_host_mock(HostMock {
            capability: "project".to_string(),
            operation: "metadata_get".to_string(),
            params: None,
            result: Some(VmValue::String(std::sync::Arc::from("fallback"))),
            error: None,
        });
        push_host_mock(HostMock {
            capability: "project".to_string(),
            operation: "metadata_get".to_string(),
            params: Some(exact_params),
            result: Some(VmValue::String(std::sync::Arc::from("facts"))),
            error: None,
        });

        let mut call_params = BTreeMap::new();
        call_params.insert(
            "dir".to_string(),
            VmValue::String(std::sync::Arc::from("pkg")),
        );
        call_params.insert(
            "namespace".to_string(),
            VmValue::String(std::sync::Arc::from("facts")),
        );
        let exact = dispatch_mock_host_call("project", "metadata_get", &call_params)
            .expect("expected exact mock")
            .expect("exact mock should succeed");
        assert_eq!(exact.display(), "facts");

        call_params.insert(
            "namespace".to_string(),
            VmValue::String(std::sync::Arc::from("classification")),
        );
        let fallback = dispatch_mock_host_call("project", "metadata_get", &call_params)
            .expect("expected fallback mock")
            .expect("fallback mock should succeed");
        assert_eq!(fallback.display(), "fallback");
        reset_host_state();
    }

    #[test]
    fn mock_host_call_can_throw_errors() {
        reset_host_state();
        push_host_mock(HostMock {
            capability: "project".to_string(),
            operation: "metadata_get".to_string(),
            params: None,
            result: None,
            error: Some("boom".to_string()),
        });
        let params = BTreeMap::new();
        let result = dispatch_mock_host_call("project", "metadata_get", &params)
            .expect("expected mock result");
        match result {
            Err(VmError::Thrown(VmValue::String(message))) => assert_eq!(message.as_ref(), "boom"),
            other => panic!("unexpected result: {other:?}"),
        }
        reset_host_state();
    }

    #[derive(Default)]
    struct TestHostToolBridge;

    impl HostCallBridge for TestHostToolBridge {
        fn dispatch(
            &self,
            _capability: &str,
            _operation: &str,
            _params: &BTreeMap<String, VmValue>,
        ) -> Result<Option<VmValue>, VmError> {
            Ok(None)
        }

        fn list_tools(&self) -> Result<Option<VmValue>, VmError> {
            let tool = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
                (
                    "name".to_string(),
                    VmValue::String(std::sync::Arc::from("Read".to_string())),
                ),
                (
                    "description".to_string(),
                    VmValue::String(std::sync::Arc::from(
                        "Read a file from the host".to_string(),
                    )),
                ),
                (
                    "schema".to_string(),
                    VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                        "type".to_string(),
                        VmValue::String(std::sync::Arc::from("object".to_string())),
                    )]))),
                ),
                ("deprecated".to_string(), VmValue::Bool(false)),
            ])));
            Ok(Some(VmValue::List(std::sync::Arc::new(vec![tool]))))
        }

        fn call_tool(&self, name: &str, args: &VmValue) -> Result<Option<VmValue>, VmError> {
            if name != "Read" {
                return Ok(None);
            }
            let path = args
                .as_dict()
                .and_then(|dict| dict.get("path"))
                .map(|value| value.display())
                .unwrap_or_default();
            Ok(Some(VmValue::String(std::sync::Arc::from(format!(
                "read:{path}"
            )))))
        }
    }

    struct CountingProcessExecBridge {
        calls: Arc<AtomicUsize>,
    }

    impl HostCallBridge for CountingProcessExecBridge {
        fn dispatch(
            &self,
            capability: &str,
            operation: &str,
            _params: &BTreeMap<String, VmValue>,
        ) -> Result<Option<VmValue>, VmError> {
            if (capability, operation) != ("process", "exec") {
                return Ok(None);
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(VmValue::Dict(std::sync::Arc::new(BTreeMap::from([
                (
                    "status".to_string(),
                    VmValue::String(std::sync::Arc::from("completed".to_string())),
                ),
                ("exit_code".to_string(), VmValue::Int(0)),
                ("success".to_string(), VmValue::Bool(true)),
            ])))))
        }
    }

    fn run_host_async_test<F, Fut>(test: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local.run_until(test()).await;
        });
    }

    #[test]
    fn host_tool_list_uses_installed_host_call_bridge() {
        run_host_async_test(|| async {
            reset_host_state();
            set_host_call_bridge(Arc::new(TestHostToolBridge));
            let tools = dispatch_host_tool_list().await.expect("tool list");
            clear_host_call_bridge();

            let VmValue::List(items) = tools else {
                panic!("expected tool list");
            };
            assert_eq!(items.len(), 1);
            let tool = items[0].as_dict().expect("tool dict");
            assert_eq!(tool.get("name").unwrap().display(), "Read");
            assert_eq!(tool.get("deprecated").unwrap().display(), "false");
        });
    }

    #[test]
    fn host_tool_call_uses_installed_host_call_bridge() {
        run_host_async_test(|| async {
            set_host_call_bridge(Arc::new(TestHostToolBridge));
            let args = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                "path".to_string(),
                VmValue::String(std::sync::Arc::from("README.md".to_string())),
            )])));
            let value = dispatch_host_tool_call("Read", &args)
                .await
                .expect("tool call");
            clear_host_call_bridge();
            assert_eq!(value.display(), "read:README.md");
        });
    }

    #[test]
    fn process_exec_bridge_is_gated_by_command_policy() {
        run_host_async_test(|| async {
            crate::orchestration::clear_command_policies();
            let calls = Arc::new(AtomicUsize::new(0));
            set_host_call_bridge(Arc::new(CountingProcessExecBridge {
                calls: calls.clone(),
            }));
            crate::orchestration::push_command_policy(crate::orchestration::CommandPolicy {
                tools: vec!["run".to_string()],
                workspace_roots: Vec::new(),
                default_shell_mode: "shell".to_string(),
                deny_patterns: vec!["cat *".to_string()],
                require_approval: Default::default(),
                pre: None,
                post: None,
                allow_recursive: false,
            });

            let result = dispatch_host_operation(
                "process",
                "exec",
                &BTreeMap::from([
                    (
                        "mode".to_string(),
                        VmValue::String(std::sync::Arc::from("shell")),
                    ),
                    (
                        "command".to_string(),
                        VmValue::String(std::sync::Arc::from("cat Cargo.toml")),
                    ),
                ]),
            )
            .await
            .expect("process.exec result");

            crate::orchestration::clear_command_policies();
            clear_host_call_bridge();

            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "blocked command must not reach host bridge"
            );
            let result = result.as_dict().expect("blocked result dict");
            assert_eq!(result.get("status").unwrap().display(), "blocked");
            assert!(
                result
                    .get("reason")
                    .map(VmValue::display)
                    .unwrap_or_default()
                    .contains("cat *"),
                "blocked result should name the matched policy pattern"
            );
        });
    }

    #[cfg(unix)]
    async fn process_exec_env_probe(env: VmValue, env_mode: Option<&str>) -> (String, String) {
        // Run `sh -c 'printf "%s|%s" "$PARENT_VAR" "$CHILD_VAR"'` so we can
        // observe whether an inherited parent var survives alongside the
        // explicitly-provided child var. The parent var is set on this
        // process's environment immediately before the spawn.
        std::env::set_var("PARENT_VAR", "inherited");
        let mut params = BTreeMap::from([
            (
                "mode".to_string(),
                VmValue::String(std::sync::Arc::from("argv")),
            ),
            (
                "argv".to_string(),
                VmValue::List(std::sync::Arc::new(vec![
                    // Absolute path so the spawn does not depend on PATH,
                    // which the `replace` case intentionally clears.
                    VmValue::String(std::sync::Arc::from("/bin/sh")),
                    VmValue::String(std::sync::Arc::from("-c")),
                    VmValue::String(std::sync::Arc::from(
                        "printf '%s|%s' \"$PARENT_VAR\" \"$CHILD_VAR\"",
                    )),
                ])),
            ),
            ("env".to_string(), env),
        ]);
        if let Some(mode) = env_mode {
            params.insert(
                "env_mode".to_string(),
                VmValue::String(std::sync::Arc::from(mode)),
            );
        }
        let result = super::dispatch_process_exec(&params, serde_json::Value::Null)
            .await
            .expect("process.exec result");
        let dict = result.as_dict().expect("result dict");
        let stdout = dict.get("stdout").map(VmValue::display).unwrap_or_default();
        std::env::remove_var("PARENT_VAR");
        let (parent, child) = stdout.split_once('|').unwrap_or((&stdout, ""));
        (parent.to_string(), child.to_string())
    }

    #[cfg(unix)]
    #[test]
    fn process_exec_env_default_merges_with_parent() {
        run_host_async_test(|| async {
            // No `env_mode`: the provided key must be added WITHOUT clearing
            // the inherited parent environment (the env-clear footgun fix).
            let child_env = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                "CHILD_VAR".to_string(),
                VmValue::String(std::sync::Arc::from("provided")),
            )])));
            let (parent, child) = process_exec_env_probe(child_env, None).await;
            assert_eq!(
                parent, "inherited",
                "default env_mode must inherit parent env"
            );
            assert_eq!(
                child, "provided",
                "default env_mode must apply provided keys"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn process_exec_env_mode_replace_clears_parent() {
        run_host_async_test(|| async {
            // Explicit `replace`: the inherited parent var must be gone and
            // only the provided key survives. This preserves the ability to
            // fully replace the environment when intentionally requested.
            let child_env = VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                "CHILD_VAR".to_string(),
                VmValue::String(std::sync::Arc::from("provided")),
            )])));
            let (parent, child) = process_exec_env_probe(child_env, Some("replace")).await;
            assert_eq!(parent, "", "explicit replace must clear parent env");
            assert_eq!(
                child, "provided",
                "explicit replace must keep provided keys"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn process_exec_env_mode_unknown_is_rejected() {
        run_host_async_test(|| async {
            let params = BTreeMap::from([
                (
                    "mode".to_string(),
                    VmValue::String(std::sync::Arc::from("argv")),
                ),
                (
                    "argv".to_string(),
                    VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                        std::sync::Arc::from("true"),
                    )])),
                ),
                (
                    "env".to_string(),
                    VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                        "CHILD_VAR".to_string(),
                        VmValue::String(std::sync::Arc::from("x")),
                    )]))),
                ),
                (
                    "env_mode".to_string(),
                    VmValue::String(std::sync::Arc::from("bogus")),
                ),
            ]);
            let err = super::dispatch_process_exec(&params, serde_json::Value::Null)
                .await
                .expect_err("unknown env_mode must error");
            assert!(
                format!("{err:?}").contains("env_mode"),
                "error should name env_mode, got {err:?}"
            );
        });
    }

    #[test]
    fn host_tool_list_is_empty_without_bridge() {
        run_host_async_test(|| async {
            clear_host_call_bridge();
            let tools = dispatch_host_tool_list().await.expect("tool list");
            let VmValue::List(items) = tools else {
                panic!("expected tool list");
            };
            assert!(items.is_empty());
        });
    }
}
