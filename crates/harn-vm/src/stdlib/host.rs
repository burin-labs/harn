use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{values_equal, VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

mod bridge;
pub(crate) mod fixtured_operations;
mod operation_registry;
pub mod process_admission;
pub(crate) mod process_dispatch;
mod process_exec;
// Public so tests and embedders can share the per-turn memo allowlist even
// when probing host_call outside the stdlib builtin (harn#5190).
pub mod turn_cache;

use bridge::HOST_CALL_BRIDGE;
pub use bridge::{
    clear_host_call_bridge, dispatch_host_call_bridge, host_call_ready, install_host_call_bridge,
    set_host_call_bridge, HostCallBridge, HostCallBridgeGuard, HostCallDispatchFuture,
};

use process_dispatch::dispatch_process_exec_with_policy;
pub(crate) use process_dispatch::{dispatch_process_exec, dispatch_reviewed_git_push_with_lease};
pub(crate) use process_exec::build_sandboxed_command;
use process_exec::dispatch_process_spawn_with_policy;

/// Audited wrapper for `chrono::Utc::now().to_rfc3339()`. Routes through
/// the testbench leak audit so a paused-clock session can surface every
/// host capability that observed real wall-clock time.
pub(crate) fn audited_utc_now_rfc3339(capability_id: &'static str) -> String {
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
    params: Option<crate::value::DictMap>,
    result: Option<VmValue>,
    error: Option<String>,
    unregistered_ok: bool,
}

#[derive(Clone)]
struct HostMockCall {
    capability: String,
    operation: String,
    params: crate::value::DictMap,
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
    // Thread-local clear only: this hook runs per-test across the whole crate,
    // so it must not bump the process-global turn epoch. See
    // [`turn_cache::reset_local`].
    turn_cache::reset_local();
}

pub(crate) fn reset_scoped_host_state() {
    operation_registry::clear_scoped_mockable();
}

/// Push the current host-mock state onto an internal stack and start a
/// fresh empty scope. Paired with `pop_host_mock_scope` for privileged
/// runtime tests that still exercise the wire layer directly. Script tests use
/// per-Harness capability fixtures instead.
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

fn capability_manifest_map() -> crate::value::DictMap {
    let mut root = crate::value::DictMap::new();
    root.insert(
        crate::value::intern_key("process"),
        capability(
            "Process execution.",
            &[
                op("exec", "Execute a process in argv or shell mode."),
                op(
                    "spawn",
                    "Spawn a process non-blocking; returns a handle immediately for poll/wait/kill.",
                ),
                op(
                    "poll",
                    "Non-blocking snapshot of a spawned process: status, captured stdout/stderr.",
                ),
                op(
                    "wait",
                    "Await a spawned process to completion (optional timeout_ms); returns final result.",
                ),
                op(
                    "kill",
                    "Terminate a spawned process by handle and await the status transition.",
                ),
                op(
                    "release",
                    "Release a spawned-process handle and free its retained output.",
                ),
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
        crate::value::intern_key("template"),
        capability(
            "Template rendering.",
            &[op("render", "Render a template file.")],
        ),
    );
    root.insert(
        crate::value::intern_key("interaction"),
        capability(
            "User interaction.",
            &[op("ask", "Ask the user a question.")],
        ),
    );
    root.insert(
        crate::value::intern_key("memory"),
        capability(
            "Vector-aware memory: host-provided embeddings.",
            &[op(
                "embed",
                "Embed text for semantic recall. Params: {text, model_hint?}. \
                 Returns {vector: list<float>, model: string, dim: int}.",
            )],
        ),
    );
    root.insert(
        crate::value::intern_key("project"),
        capability(
            "Project metadata and durable project facts.",
            &[
                op("metadata_get", "Read project metadata."),
                op("metadata_inspect", "Inspect project metadata provenance."),
                op("metadata_set", "Write project metadata."),
                op("metadata_save", "Persist pending project metadata changes."),
                op("metadata_stale", "Check whether project metadata is stale."),
                op(
                    "metadata_refresh_hashes",
                    "Refresh project metadata content hashes.",
                ),
            ],
        ),
    );
    root.insert(
        crate::value::intern_key("runtime"),
        capability(
            "Runtime task context and run metadata supplied by the active host.",
            &[
                op("task", "Read the current runtime task."),
                op("pipeline_input", "Read the active pipeline input payload."),
                op("prompt_content", "Read the active session prompt content."),
                op("dry_run", "Read whether the runtime is in dry-run mode."),
                op("approved_plan", "Read the approved plan text."),
                op("record_run", "Record run metadata with the host."),
                op("set_result", "Write the runtime result payload."),
            ],
        ),
    );
    root.insert(
        crate::value::intern_key("workspace"),
        capability(
            "Workspace facts and file access supplied by the active host.",
            &[
                op("project_root", "Return the active project root."),
                op("cwd", "Return the active current working directory."),
                op("read_text", "Read a workspace text file."),
                op("list", "List workspace files or directories."),
                op("exists", "Check whether a workspace path exists."),
            ],
        ),
    );
    root.insert(
        crate::value::intern_key("oauth_storage"),
        capability(
            "Host-managed OAuth token storage.",
            &[
                op("cloud_get", "Read a cloud-managed token set."),
                op("cloud_set", "Write a cloud-managed token set."),
                op("cloud_delete", "Delete a cloud-managed token set."),
                op(
                    "cloud_acquire_refresh_lock",
                    "Acquire an OAuth refresh lock.",
                ),
                op(
                    "cloud_release_refresh_lock",
                    "Release an OAuth refresh lock.",
                ),
            ],
        ),
    );
    root.insert(
        crate::value::intern_key("mcp"),
        capability(
            "MCP host interactions.",
            &[op("elicit", "Ask the connected MCP client for input.")],
        ),
    );
    root.insert(
        crate::value::intern_key("hitl"),
        capability(
            "Human-in-the-loop host interactions.",
            &[
                op(
                    "question",
                    "Ask a human a question through the active host.",
                ),
                op(
                    "approval",
                    "Request a human approval through the active host.",
                ),
                op(
                    "dual_control",
                    "Request quorum approval from multiple human reviewers.",
                ),
                op(
                    "escalation",
                    "Escalate a task to a human role through the active host.",
                ),
            ],
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
    root: &mut crate::value::DictMap,
    capability_name: &str,
    operation_name: &str,
) {
    let Some(existing) = root.get(capability_name).cloned() else {
        root.insert(
            crate::value::intern_key(capability_name),
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
        ops.push(VmValue::String(arcstr::ArcStr::from(
            operation_name.to_string(),
        )));
    }

    let mut operations = entry
        .get("operations")
        .and_then(|value| value.as_dict())
        .map(|dict| (*dict).clone())
        .unwrap_or_default();
    operations
        .entry(crate::value::intern_key(operation_name))
        .or_insert_with(mocked_operation_entry);

    entry.insert(
        crate::value::intern_key("ops"),
        VmValue::List(std::sync::Arc::new(ops)),
    );
    entry.insert(
        crate::value::intern_key("operations"),
        VmValue::dict(operations),
    );
    root.insert(
        crate::value::intern_key(capability_name),
        VmValue::dict(entry),
    );
}

fn ensure_registered_operation(
    root: &mut crate::value::DictMap,
    capability_name: &str,
    operation_name: &str,
    description: &str,
) {
    let operation = op(operation_name, description);
    let Some(existing) = root.get(capability_name).cloned() else {
        root.insert(
            crate::value::intern_key(capability_name),
            capability(description, &[operation]),
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
        ops.push(VmValue::String(arcstr::ArcStr::from(
            operation_name.to_string(),
        )));
    }

    let mut operations = entry
        .get("operations")
        .and_then(|value| value.as_dict())
        .map(|dict| (*dict).clone())
        .unwrap_or_default();
    operations
        .entry(crate::value::intern_key(operation_name))
        .or_insert(operation.1);

    entry.insert(
        crate::value::intern_key("ops"),
        VmValue::List(std::sync::Arc::new(ops)),
    );
    entry.insert(
        crate::value::intern_key("operations"),
        VmValue::dict(operations),
    );
    root.insert(
        crate::value::intern_key(capability_name),
        VmValue::dict(entry),
    );
}

pub fn register_mockable_host_operation(
    capability_name: impl AsRef<str>,
    operation_name: impl AsRef<str>,
    description: impl AsRef<str>,
) {
    operation_registry::register_mockable(capability_name, operation_name, description);
}

/// Register a mock-validation declaration scoped to the current test thread.
pub fn register_scoped_mockable_host_operation(
    capability_name: impl AsRef<str>,
    operation_name: impl AsRef<str>,
    description: impl AsRef<str>,
) {
    operation_registry::register_scoped_mockable(capability_name, operation_name, description);
}

pub fn register_callable_host_operation(
    capability_name: impl AsRef<str>,
    operation_name: impl AsRef<str>,
    description: impl AsRef<str>,
) {
    operation_registry::register_callable(capability_name, operation_name, description);
}

fn apply_registered_operations(root: &mut crate::value::DictMap) {
    operation_registry::apply_callable(root);
}

fn capability_manifest_with_mocks() -> VmValue {
    let mut root = capability_manifest_map();
    apply_registered_operations(&mut root);
    HOST_MOCKS.with(|mocks| {
        for host_mock in mocks.borrow().iter() {
            ensure_mocked_capability(&mut root, &host_mock.capability, &host_mock.operation);
        }
    });
    fixtured_operations::apply_to_manifest(&mut root, ensure_mocked_capability);
    VmValue::dict(root)
}

fn known_host_operations() -> Vec<(String, String)> {
    let mut root = capability_manifest_map();
    apply_registered_operations(&mut root);
    operation_registry::apply_mockable(&mut root);
    root.into_iter()
        .flat_map(|(capability_name, capability)| {
            let capability_name = capability_name.to_string();
            capability
                .as_dict()
                .and_then(|dict| dict.get("ops"))
                .and_then(|value| match value {
                    VmValue::List(list) => Some((**list).clone()),
                    _ => None,
                })
                .unwrap_or_default()
                .into_iter()
                .map(move |operation| (capability_name.clone(), operation.display()))
        })
        .collect()
}

pub(crate) fn host_operation_is_registered(capability: &str, operation: &str) -> bool {
    known_host_operations()
        .iter()
        .any(|(known_capability, known_operation)| {
            known_capability == capability && known_operation == operation
        })
}

fn closest_host_operation(capability: &str, operation: &str) -> Option<(String, String)> {
    let requested = format!("{capability}.{operation}");
    known_host_operations()
        .into_iter()
        .map(|(candidate_capability, candidate_operation)| {
            let candidate = format!("{candidate_capability}.{candidate_operation}");
            let distance = strsim::levenshtein(&requested, &candidate);
            (distance, candidate_capability, candidate_operation)
        })
        .filter(|(distance, _, _)| *distance <= 4)
        .min_by_key(|(distance, _, _)| *distance)
        .map(|(_, candidate_capability, candidate_operation)| {
            (candidate_capability, candidate_operation)
        })
}

fn validate_host_mock_registration(host_mock: &HostMock) -> Result<(), VmError> {
    if host_mock.unregistered_ok
        || host_operation_is_registered(&host_mock.capability, &host_mock.operation)
    {
        return Ok(());
    }

    let mut message = format!(
        "host_mock: unregistered host operation {}.{}; register the capability/operation on \
         the host or pass {{unregistered_ok: true}} for a test-local mock",
        host_mock.capability, host_mock.operation
    );
    if let Some((capability, operation)) =
        closest_host_operation(&host_mock.capability, &host_mock.operation)
    {
        message.push_str(&format!(". Did you mean {capability}.{operation}?"));
    }
    Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
        message,
    ))))
}

fn op(name: &str, description: &str) -> (String, VmValue) {
    let mut entry = crate::value::DictMap::new();
    entry.put_str("description", description);
    (name.to_string(), VmValue::dict(entry))
}

fn capability(description: &str, ops: &[(String, VmValue)]) -> VmValue {
    let mut entry = crate::value::DictMap::new();
    entry.put_str("description", description);
    entry.insert(
        crate::value::intern_key("ops"),
        VmValue::List(std::sync::Arc::new(
            ops.iter()
                .map(|(name, _)| VmValue::String(arcstr::ArcStr::from(name.as_str())))
                .collect(),
        )),
    );
    let mut op_dict = crate::value::DictMap::new();
    for (name, op) in ops {
        op_dict.insert(crate::value::intern_key(name), op.clone());
    }
    entry.insert(
        crate::value::intern_key("operations"),
        VmValue::dict(op_dict),
    );
    VmValue::dict(entry)
}

pub(crate) fn require_param(params: &crate::value::DictMap, key: &str) -> Result<String, VmError> {
    params
        .get(key)
        .map(|v| v.display())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "host_call: missing required parameter '{key}'"
            ))))
        })
}

fn render_template(
    path: &str,
    bindings: Option<&crate::value::DictMap>,
) -> Result<String, VmError> {
    let asset = crate::stdlib::template::TemplateAsset::render_target(path).map_err(|msg| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "host_call template.render: {msg}"
        ))))
    })?;
    crate::stdlib::template::render_asset_result(&asset, bindings).map_err(VmError::from)
}

fn params_match(expected: Option<&crate::value::DictMap>, actual: &crate::value::DictMap) -> bool {
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
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "host_mock: capability and operation are required",
        ))));
    }

    let mut params = args
        .get(3)
        .and_then(|value| value.as_dict())
        .map(|dict| (*dict).clone());
    let mut result = args.get(2).cloned().or(Some(VmValue::Nil));
    let mut error = None;
    let mut unregistered_ok = false;

    if let Some(config) = args.get(2).and_then(|value| value.as_dict()) {
        if config.contains_key("result")
            || config.contains_key("params")
            || config.contains_key("error")
            || config.contains_key("unregistered_ok")
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
            unregistered_ok = matches!(config.get("unregistered_ok"), Some(VmValue::Bool(true)));
        }
    }

    Ok(HostMock {
        capability,
        operation,
        params,
        result,
        error,
        unregistered_ok,
    })
}

fn push_host_mock(host_mock: HostMock) {
    HOST_MOCKS.with(|mocks| mocks.borrow_mut().push(host_mock));
}

fn mock_call_value(call: &HostMockCall) -> VmValue {
    let mut item = crate::value::DictMap::new();
    item.put_str("capability", call.capability.clone());
    item.put_str("operation", call.operation.clone());
    item.insert(
        crate::value::intern_key("params"),
        VmValue::dict(call.params.clone()),
    );
    VmValue::dict(item)
}

fn record_mock_call(capability: &str, operation: &str, params: &crate::value::DictMap) {
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
    params: &crate::value::DictMap,
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
        return Some(Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            error,
        )))));
    }
    Some(Ok(matched.result.unwrap_or(VmValue::Nil)))
}

/// Dispatch a hostlib builtin through the same scoped mock registry used by
/// `host_call`.
///
/// Hostlib builtins are addressed by their schema module/method pair, so a test
/// can mock `hostlib_tools_run_command(...)` with
/// `{capability: "tools", operation: "run_command", ...}`. During the
/// `process.exec` -> hostlib `run_command` migration we also honor existing
/// `{capability: "process", operation: "exec", ...}` command mocks after the
/// canonical `tools.run_command` lookup, preserving last-write-wins within each
/// mock lane and giving explicit hostlib mocks precedence.
pub fn dispatch_mock_hostlib_call(
    module: &str,
    method: &str,
    params: &crate::value::DictMap,
) -> Option<Result<VmValue, VmError>> {
    if let Some(mocked) = dispatch_mock_host_call(module, method, params) {
        return Some(mocked);
    }

    if (module, method) == ("tools", "run_command") {
        return dispatch_mock_host_call("process", "exec", params);
    }

    None
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
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
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

/// Public entry for canonical `host_call` dispatch without an async-builtin
/// context. Used by embedder tests and by MCP/host plumbing that already sits
/// outside a VM builtin frame.
pub async fn dispatch_host_operation(
    capability: &str,
    operation: &str,
    params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    dispatch_host_operation_with_ctx(None, capability, operation, params).await
}

/// Canonical `host_call` dispatch.
///
/// Embedders reach this path through the stdlib `host_call` builtin after
/// installing a [`HostCallBridge`]. ACP installs that bridge and keeps the
/// stdlib builtin; it no longer re-registers `host_call` by name (harn#5523).
/// Cross-cutting behaviour added here therefore reaches editor-hosted sessions
/// automatically — mocks, command-policy preflight, the process-handle
/// registry, and the per-turn memo included.
/// Editor-owned *builtins* (`exec`, `shell`, `run_command`) remain ACP
/// overrides; that intentional ownership is separate from `host_call` routing.
/// Direct `host_call("process.exec", ...)` always passes through the policy gates.
pub async fn dispatch_host_operation_with_ctx(
    ctx: Option<&AsyncBuiltinCtx>,
    capability: &str,
    operation: &str,
    params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    let _invalidation = turn_cache::invalidation_scope(capability, operation);
    if let Some(ctx) = ctx {
        let vm = ctx.child_vm();
        if let Some(fixtured) = vm.harness().and_then(|harness| {
            harness
                .inner()
                .fixtures()
                .dispatch_host(capability, operation, params)
        }) {
            return fixtured;
        }
    }
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

    // process.spawn is the non-blocking sibling of exec. Route it through the
    // SAME command-policy preflight so deny-patterns/approval/sandbox gating
    // are identical; only the completion semantics differ (returns a handle
    // immediately instead of awaiting). poll/wait/kill/release are pure
    // registry operations on an already-gated spawn, so they bypass the
    // command policy.
    if (capability, operation) == ("process", "spawn") {
        let caller = serde_json::json!({
            "surface": "host_call",
            "capability": "process",
            "operation": "spawn",
            "session_id": crate::llm::current_agent_session_id(),
        });
        return dispatch_process_spawn_with_policy(ctx, params, caller).await;
    }
    if capability == "process" && matches!(operation, "poll" | "wait" | "kill" | "release") {
        if let Some(result) = crate::stdlib::process_spawn::dispatch(
            operation,
            params,
            process_exec::async_builtin_cancel_token(ctx),
        )
        .await
        {
            return result;
        }
    }

    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone());
    if let Some(bridge) = bridge {
        // Turn-stable reads share a memo; metadata writes invalidate it on both
        // sides of dispatch. harn#5190, harn#6914, harn#7172.
        let dispatched = bridge::dispatch_cached(bridge, capability, operation, params).await?;
        if let Some(value) = dispatched {
            return Ok(value);
        }
    }

    dispatch_builtin_host_operation(capability, operation, params).await
}

async fn dispatch_builtin_host_operation(
    capability: &str,
    operation: &str,
    params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    match (capability, operation) {
        ("process", "list_shells") => Ok(crate::shells::list_shells_vm_value()),
        ("process", "get_default_shell") => Ok(crate::shells::default_shell_vm_value()),
        ("process", "set_default_shell") => crate::shells::set_default_shell_vm_value(params),
        ("process", "shell_invocation") => crate::shells::shell_invocation_vm_value(params),
        ("template", "render") => {
            let path = require_param(params, "path")?;
            let bindings = params.get("bindings").and_then(|v| v.as_dict());
            Ok(VmValue::String(arcstr::ArcStr::from(render_template(
                &path, bindings,
            )?)))
        }
        ("interaction", "ask") => {
            let question = require_param(params, "question")?;
            super::io::prompt_user_value(&[VmValue::string(question)], &mut String::new())
        }
        ("project", "metadata_get") => crate::metadata::project_metadata_host_get(params),
        ("project", "metadata_inspect") => crate::metadata::project_metadata_host_inspect(params),
        ("project", "metadata_set") => crate::metadata::project_metadata_host_set(params),
        ("project", "metadata_save") => crate::metadata::project_metadata_host_save(params),
        ("project", "metadata_stale") => crate::metadata::project_metadata_host_stale(params),
        ("project", "metadata_refresh_hashes") => {
            crate::metadata::project_metadata_host_refresh_hashes(params)
        }
        // Standalone fallbacks for host-supplied capabilities. `HARN_TASK`
        // backs `runtime.task` for debugger and CLI invocations.
        ("runtime", "task") => Ok(VmValue::String(arcstr::ArcStr::from(
            std::env::var("HARN_TASK").unwrap_or_default(),
        ))),
        ("runtime", "prompt_content") => Ok(VmValue::List(Arc::new(Vec::new()))),
        ("runtime", "set_result") => {
            // No-op when no host is attached; swallow silently so standalone
            // scripts can still call `set_result` without crashing.
            Ok(VmValue::Nil)
        }
        ("workspace", "project_root") => {
            // Standalone fallback: prefer the typed execution project root,
            // then the legacy env root, then the current working directory.
            // Pipelines call this very early, so crashing here would block any
            // debug-launched script.
            let path = crate::stdlib::process::project_root_path()
                .map(|root| root.display().to_string())
                .or_else(|| std::env::var("HARN_PROJECT_ROOT").ok())
                .unwrap_or_else(|| {
                    std::env::current_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                });
            Ok(VmValue::String(arcstr::ArcStr::from(path)))
        }
        ("workspace", "cwd") => {
            let path = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            Ok(VmValue::String(arcstr::ArcStr::from(path)))
        }
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("host_call: unsupported operation {capability}.{operation}"),
        )))),
    }
}

pub(crate) fn optional_i64(params: &crate::value::DictMap, key: &str) -> Option<i64> {
    match params.get(key) {
        Some(VmValue::Int(value)) => Some(*value),
        Some(VmValue::Float(value)) if value.fract() == 0.0 => Some(*value as i64),
        _ => None,
    }
}

pub(crate) fn optional_string(params: &crate::value::DictMap, key: &str) -> Option<String> {
    params.get(key).and_then(vm_string).map(ToString::to_string)
}

fn optional_string_list(params: &crate::value::DictMap, key: &str) -> Option<Vec<String>> {
    let VmValue::List(values) = params.get(key)? else {
        return None;
    };
    values
        .iter()
        .map(|value| vm_string(value).map(ToString::to_string))
        .collect()
}

fn optional_string_dict(
    params: &crate::value::DictMap,
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
    let mut out = std::collections::BTreeMap::new();
    for (key, value) in dict.iter() {
        let Some(value) = vm_string(value) else {
            return Err(VmError::Runtime(format!(
                "host_call process.exec env value for {key:?} must be a string"
            )));
        };
        out.insert(key.to_string(), value.to_string());
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

pub(crate) fn register_missing_host_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        if vm.builtin_metadata_for(def.sig.name).is_none() {
            vm.register_builtin_def(def);
        }
    }
}

#[harn_builtin(
    exposure = "privileged_wire",
    effects = ["host.mutate@arg0"],
    sig = "host_mock(capability: string, op: string, response_or_config?: any, params?: dict) -> nil",
    category = "host"
)]
pub(crate) fn host_mock_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let host_mock = parse_host_mock(args)?;
    validate_host_mock_registration(&host_mock)?;
    push_host_mock(host_mock);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "privileged_wire",
    effects = [],
    sig = "host_mock_clear() -> nil", category = "host"
)]
fn host_mock_clear_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    reset_host_state();
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "host_mock_calls() -> list", category = "host"
)]
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

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "host_mock_push_scope() -> nil", category = "host"
)]
fn host_mock_push_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    push_host_mock_scope();
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "host_mock_pop_scope() -> nil", category = "host"
)]
fn host_mock_pop_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if !pop_host_mock_scope() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "host_mock_pop_scope: no scope to pop",
        ))));
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "host_capabilities() -> dict", category = "host"
)]
fn host_capabilities_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(capability_manifest_with_mocks())
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "host_has(capability: string, op?: string) -> bool",
    category = "host"
)]
fn host_has_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let capability = args.first().map(|a| a.display()).unwrap_or_default();
    let operation = args.get(1).map(|a| a.display());
    let manifest = capability_manifest_with_mocks();
    let has = manifest
        .as_dict()
        .and_then(|d| d.get(capability.as_str()))
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
    exposure = "privileged_wire",
    effects = [],
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
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("host_call: unsupported operation name '{name}'"),
        ))));
    };
    dispatch_host_operation_with_ctx(Some(&ctx), capability, operation, &params).await
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "host_tool_list() -> list", kind = "async", category = "host"
)]
async fn host_tool_list_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    _args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    dispatch_host_tool_list_with_ctx(Some(&ctx)).await
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
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
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "host_tool_call: tool name is required",
        ))));
    }
    let call_args = args.get(1).cloned().unwrap_or(VmValue::Nil);
    dispatch_host_tool_call_with_ctx(Some(&ctx), &name, &call_args).await
}

#[cfg(test)]
#[path = "host/tests.rs"]
mod tests;
