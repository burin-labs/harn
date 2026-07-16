use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value as JsonValue;
use tokio::io::AsyncReadExt;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{values_equal, VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

mod operation_registry;

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
}

pub(crate) fn reset_scoped_host_state() {
    operation_registry::clear_scoped_mockable();
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

fn async_builtin_cancel_token(
    ctx: Option<&AsyncBuiltinCtx>,
) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
    ctx.and_then(|ctx| ctx.child_vm().cancel_token.clone())
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

fn apply_mockable_operations(root: &mut crate::value::DictMap) {
    operation_registry::apply_mockable(root);
}

fn capability_manifest_with_mocks() -> VmValue {
    let mut root = capability_manifest_map();
    apply_registered_operations(&mut root);
    HOST_MOCKS.with(|mocks| {
        for host_mock in mocks.borrow().iter() {
            ensure_mocked_capability(&mut root, &host_mock.capability, &host_mock.operation);
        }
    });
    VmValue::dict(root)
}

fn known_host_operations() -> Vec<(String, String)> {
    let mut root = capability_manifest_map();
    apply_registered_operations(&mut root);
    apply_mockable_operations(&mut root);
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

fn host_operation_is_registered(capability: &str, operation: &str) -> bool {
    known_host_operations()
        .iter()
        .any(|(known_capability, known_operation)| {
            known_capability == capability && known_operation == operation
        })
}

fn edit_distance(a: &str, b: &str) -> usize {
    let mut previous: Vec<usize> = (0..=b.chars().count()).collect();
    let mut current = vec![0; previous.len()];
    for (i, ca) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current[j + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.chars().count()]
}

fn closest_host_operation(capability: &str, operation: &str) -> Option<(String, String)> {
    let requested = format!("{capability}.{operation}");
    known_host_operations()
        .into_iter()
        .map(|(candidate_capability, candidate_operation)| {
            let candidate = format!("{candidate_capability}.{candidate_operation}");
            let distance = edit_distance(&requested, &candidate);
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
        params: &crate::value::DictMap,
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
    params: &crate::value::DictMap,
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

pub(crate) async fn dispatch_host_operation(
    capability: &str,
    operation: &str,
    params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    dispatch_host_operation_with_ctx(None, capability, operation, params).await
}

pub(crate) async fn dispatch_host_operation_with_ctx(
    ctx: Option<&AsyncBuiltinCtx>,
    capability: &str,
    operation: &str,
    params: &crate::value::DictMap,
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
            async_builtin_cancel_token(ctx),
        )
        .await
        {
            return result;
        }
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
        // Standalone-run fallbacks for capabilities normally supplied by
        // an embedder's JSON-RPC bridge. `runtime.task` lets a debugger or
        // CLI invocation read the pipeline input from `HARN_TASK` without
        // the host explicitly wiring a callback for every op.
        ("runtime", "task") => Ok(VmValue::String(arcstr::ArcStr::from(
            std::env::var("HARN_TASK").unwrap_or_default(),
        ))),
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

pub(crate) async fn dispatch_process_exec(
    params: &crate::value::DictMap,
    caller: serde_json::Value,
) -> Result<VmValue, VmError> {
    dispatch_process_exec_with_policy(None, params, caller).await
}

async fn dispatch_process_exec_with_policy(
    ctx: Option<&AsyncBuiltinCtx>,
    params: &crate::value::DictMap,
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

/// Apply the command-policy preflight (deny-patterns, approval gating,
/// sandbox decisions) and then spawn the process non-blocking. Mirrors
/// [`dispatch_process_exec_with_policy`] so spawn is gated identically to
/// exec. There is no postflight here: spawn returns a handle immediately,
/// not a completed command result; completion is observed later via
/// poll/wait, which are not themselves command executions.
async fn dispatch_process_spawn_with_policy(
    ctx: Option<&AsyncBuiltinCtx>,
    params: &crate::value::DictMap,
    caller: serde_json::Value,
) -> Result<VmValue, VmError> {
    let params =
        match crate::orchestration::run_command_policy_preflight_with_ctx(ctx, params, caller)
            .await?
        {
            crate::orchestration::CommandPolicyPreflight::Proceed { params, .. } => params,
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

    match crate::stdlib::process_spawn::dispatch("spawn", &params, async_builtin_cancel_token(ctx))
        .await
    {
        Some(result) => result,
        None => Err(VmError::Runtime(
            "host_call process.spawn: dispatch returned None".to_string(),
        )),
    }
}

async fn dispatch_process_exec_after_policy(
    ctx: Option<&AsyncBuiltinCtx>,
    params: &crate::value::DictMap,
    command_policy_context: JsonValue,
    command_policy_decisions: Vec<crate::orchestration::CommandPolicyDecision>,
) -> Result<VmValue, VmError> {
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
    let mut cmd = build_sandboxed_command(params, "process.exec")?;
    crate::op_interrupt::configure_tokio_kill_group(&mut cmd);
    let cleanup_token = crate::op_interrupt::new_process_cleanup_token();
    cmd.env(
        crate::op_interrupt::PROCESS_CLEANUP_TOKEN_ENV,
        &cleanup_token,
    );
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let started_at = audited_utc_now_rfc3339("host_call/process.exec.started_at");
    let started = crate::clock_mock::leak_audit::instant_now("host_call/process.exec.started");
    let mut child = cmd
        .spawn()
        .map_err(|e| VmError::Runtime(format!("host_call process.exec: {e}")))?;
    drop(profile_guard);
    let pid = child.id();
    let cleanup_registration = crate::op_interrupt::register_active_process_cleanup(
        pid,
        &cleanup_token,
        async_builtin_cancel_token(ctx),
    );
    let stdout_pipe = match child.stdout.take() {
        Some(pipe) => pipe,
        None => {
            terminate_process_exec_child(&mut child, pid, &cleanup_token, "missing_stdout_pipe")
                .await;
            drop(cleanup_registration);
            return Err(VmError::Runtime(
                "host_call process.exec stdout pipe was not captured".to_string(),
            ));
        }
    };
    let stderr_pipe = match child.stderr.take() {
        Some(pipe) => pipe,
        None => {
            terminate_process_exec_child(&mut child, pid, &cleanup_token, "missing_stderr_pipe")
                .await;
            drop(cleanup_registration);
            return Err(VmError::Runtime(
                "host_call process.exec stderr pipe was not captured".to_string(),
            ));
        }
    };
    let stdout_task = tokio::spawn(read_process_exec_pipe(stdout_pipe));
    let stderr_task = tokio::spawn(read_process_exec_pipe(stderr_pipe));

    enum ProcessExecWait {
        Exited(std::io::Result<std::process::ExitStatus>),
        TimedOut,
    }

    let exec_deadline = timeout_ms.map(|timeout_ms| {
        tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms)
    });
    let wait_result = {
        let wait = child.wait();
        tokio::pin!(wait);
        if let Some(deadline) = exec_deadline {
            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);
            tokio::select! {
                result = &mut wait => ProcessExecWait::Exited(result),
                _ = &mut sleep => ProcessExecWait::TimedOut,
            }
        } else {
            ProcessExecWait::Exited(wait.await)
        }
    };

    let (mut status, mut success, mut timed_out, mut exit_code) = match wait_result {
        ProcessExecWait::Exited(result) => {
            let status =
                result.map_err(|e| VmError::Runtime(format!("host_call process.exec: {e}")))?;
            let exit_code = status.code().unwrap_or(-1);
            ("completed", status.success(), false, exit_code)
        }
        ProcessExecWait::TimedOut => {
            terminate_process_exec_child(&mut child, pid, &cleanup_token, "timeout").await;
            ("timed_out", false, true, -1)
        }
    };

    let drain_pipes = async {
        let stdout = collect_process_exec_pipe(stdout_task, "stdout").await?;
        let stderr = collect_process_exec_pipe(stderr_task, "stderr").await?;
        Ok::<_, VmError>((stdout, stderr))
    };
    tokio::pin!(drain_pipes);
    let (stdout, stderr) = if !timed_out {
        if let Some(deadline) = exec_deadline {
            tokio::select! {
                result = &mut drain_pipes => result?,
                _ = tokio::time::sleep_until(deadline) => {
                    terminate_process_exec_child(
                        &mut child,
                        pid,
                        &cleanup_token,
                        "pipe_drain_timeout",
                    )
                    .await;
                    status = "timed_out";
                    success = false;
                    timed_out = true;
                    exit_code = -1;
                    drain_pipes.await?
                }
            }
        } else {
            drain_pipes.await?
        }
    } else {
        drain_pipes.await?
    };
    drop(cleanup_registration);

    let stdout_utf8_valid = std::str::from_utf8(&stdout).is_ok();
    let stderr_utf8_valid = std::str::from_utf8(&stderr).is_ok();
    let stdout = String::from_utf8_lossy(&stdout).to_string();
    let stderr = String::from_utf8_lossy(&stderr).to_string();
    let response = process_exec_response(ProcessExecResponse {
        pid,
        started_at,
        started,
        stdout: &stdout,
        stderr: &stderr,
        exit_code,
        status,
        success,
        timed_out,
        stdout_utf8_valid,
        stderr_utf8_valid,
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

async fn read_process_exec_pipe<R>(mut pipe: R) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

async fn collect_process_exec_pipe(
    task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    name: &str,
) -> Result<Vec<u8>, VmError> {
    match task.await {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(VmError::Runtime(format!(
            "host_call process.exec read {name}: {error}"
        ))),
        Err(error) => Err(VmError::Runtime(format!(
            "host_call process.exec join {name} reader: {error}"
        ))),
    }
}

async fn terminate_process_exec_child(
    child: &mut tokio::process::Child,
    pid: Option<u32>,
    cleanup_token: &str,
    reason: &'static str,
) {
    if let Some(pid) = pid {
        let mut report = crate::op_interrupt::signal_pid_tree_group_and_token_with_report(
            pid,
            Some(cleanup_token),
            9,
        );
        report.refresh_survivor_status();
        tracing::warn!(
            pid,
            children = report.children.len(),
            reason,
            "host_call process.exec signalled child process tree"
        );
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Build a sandboxed `tokio::process::Command` from process-call params,
/// applying argv/shell resolution, the active sandbox policy via
/// [`crate::process_sandbox::tokio_command_for`], cwd enforcement, and
/// env/env_mode/env_remove handling.
///
/// Shared by `process.exec` (synchronous) and `process.spawn`
/// (non-blocking) so both go through the identical sandbox-gated build
/// path. The caller is responsible for any `sandbox_profile` override
/// guard (it must be live across this call) and for setting stdio/kill
/// behaviour on the returned command. `label` ("process.exec" or
/// "process.spawn") is woven into error messages.
pub(crate) fn build_sandboxed_command(
    params: &crate::value::DictMap,
    label: &str,
) -> Result<tokio::process::Command, VmError> {
    let (program, args) = process_exec_argv(params)?;
    let mut cmd = crate::process_sandbox::tokio_command_for(&program, &args)
        .map_err(|e| VmError::Runtime(format!("host_call {label} sandbox setup: {e}")))?;
    if let Some(cwd) = optional_string(params, "cwd") {
        let cwd = resolve_process_exec_cwd(&cwd);
        crate::process_sandbox::enforce_process_cwd(&cwd)
            .map_err(|e| VmError::Runtime(format!("host_call {label} cwd: {e}")))?;
        cmd.current_dir(cwd);
    }
    // Track keys the caller set explicitly so the sandbox-local TMPDIR overlay
    // below never clobbers an intentional per-call value.
    let mut caller_env_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(env) = optional_string_dict(params, "env")? {
        // `env_mode` controls how the provided `env` keys combine with the
        // parent environment:
        //   - "merge" (default): inherit the parent env and overlay the
        //     provided keys. This is the least-surprising behavior — a
        //     caller passing `env: {ONE_VAR: "x"}` keeps PATH/HOME/etc.
        //   - "replace": clear the parent env entirely, then set only the
        //     provided keys. This is the footgun shape and must be requested
        //     explicitly whenever `env` is supplied.
        let env_mode = optional_string(params, "env_mode");
        match env_mode.as_deref().unwrap_or("merge") {
            "replace" => {
                cmd.env_clear();
            }
            "merge" => {}
            other => {
                return Err(VmError::Runtime(format!(
                    "host_call {label}: unknown env_mode {other:?}; expected \"merge\" or \"replace\""
                )));
            }
        }
        for (key, value) in env {
            caller_env_keys.insert(key.clone());
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
            caller_env_keys.insert(key.clone());
            cmd.env_remove(key);
        }
    }
    // Point the child's temp dir at a sandbox-writable, workspace-local
    // location so compiler linkers (rustc/cc/ld, Go, Swift, …) and other
    // toolchains that honor TMPDIR/TMP/TEMP don't false-fail trying to write
    // intermediates to the unwritable system /tmp. A key the caller set (via
    // `env`) or explicitly stripped (via `env_remove`) is left as the caller
    // intended; only keys the caller did not touch receive the overlay. No-op
    // when the active profile is unrestricted or no writable workspace root is
    // available.
    for (key, value) in crate::process_sandbox::active_workspace_tmpdir_env() {
        if caller_env_keys.contains(&key) {
            continue;
        }
        cmd.env(key, value);
    }
    // Pin tool *message* output to a deterministic English/UTF-8 locale so
    // downstream English-diagnostic matchers (deterministic syntax repair,
    // error-signature grounding, completion/pass-fail classification) do not
    // misfire for a non-Anglosphere user whose shell localizes compiler/test
    // output. A user-inherited `LC_ALL` overrides `LC_MESSAGES`, so strip it
    // first — unless the caller pinned it via `env`/`env_remove` — then apply
    // the overlay with the same caller-wins rule as the TMPDIR overlay above.
    if !caller_env_keys.contains(crate::process_sandbox::MESSAGE_LOCALE_OVERRIDE_ENV) {
        cmd.env_remove(crate::process_sandbox::MESSAGE_LOCALE_OVERRIDE_ENV);
    }
    for (key, value) in crate::process_sandbox::deterministic_message_locale_env() {
        if caller_env_keys.contains(&key) {
            continue;
        }
        cmd.env(key, value);
    }
    Ok(cmd)
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
    stdout_utf8_valid: bool,
    stderr_utf8_valid: bool,
}

fn process_exec_response(response: ProcessExecResponse<'_>) -> VmValue {
    let combined = format!("{}{}", response.stdout, response.stderr);
    let mut result = crate::value::DictMap::new();
    result.put_str(
        "command_id",
        format!(
            "cmd_{}_{}",
            std::process::id(),
            response.started.elapsed().as_nanos()
        ),
    );
    result.put_str("status", response.status);
    result.insert(
        crate::value::intern_key("pid"),
        response
            .pid
            .map(|pid| VmValue::Int(pid as i64))
            .unwrap_or(VmValue::Nil),
    );
    result.insert(
        crate::value::intern_key("process_group_id"),
        response
            .pid
            .map(|pid| VmValue::Int(pid as i64))
            .unwrap_or(VmValue::Nil),
    );
    result.insert(crate::value::intern_key("handle_id"), VmValue::Nil);
    result.put_str("started_at", response.started_at);
    result.put_str(
        "ended_at",
        audited_utc_now_rfc3339("host_call/process.exec.ended_at"),
    );
    result.insert(
        crate::value::intern_key("duration_ms"),
        VmValue::Int(response.started.elapsed().as_millis() as i64),
    );
    result.insert(
        crate::value::intern_key("exit_code"),
        VmValue::Int(response.exit_code as i64),
    );
    result.insert(crate::value::intern_key("signal"), VmValue::Nil);
    result.insert(
        crate::value::intern_key("timed_out"),
        VmValue::Bool(response.timed_out),
    );
    result.put_str("stdout", response.stdout);
    result.put_str("stderr", response.stderr);
    result.insert(
        crate::value::intern_key("stdout_utf8_valid"),
        VmValue::Bool(response.stdout_utf8_valid),
    );
    result.insert(
        crate::value::intern_key("stderr_utf8_valid"),
        VmValue::Bool(response.stderr_utf8_valid),
    );
    result.put_str("combined", combined);
    result.insert(
        crate::value::intern_key("exit_status"),
        VmValue::Int(response.exit_code as i64),
    );
    result.insert(
        crate::value::intern_key("legacy_status"),
        VmValue::Int(response.exit_code as i64),
    );
    result.insert(
        crate::value::intern_key("success"),
        VmValue::Bool(response.success),
    );
    VmValue::dict(result)
}

fn resolve_process_exec_cwd(cwd: &str) -> std::path::PathBuf {
    crate::stdlib::process::resolve_source_relative_path(cwd)
}

fn process_exec_argv(params: &crate::value::DictMap) -> Result<(String, Vec<String>), VmError> {
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
            invocation_params.put_str("command", command);
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
pub(crate) fn push_sandbox_profile_override(value: &str) -> Result<SandboxProfileGuard, VmError> {
    let profile = crate::orchestration::SandboxProfile::parse(value).ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
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

pub(crate) struct SandboxProfileGuard {
    _private: std::marker::PhantomData<*const ()>,
}

impl Drop for SandboxProfileGuard {
    fn drop(&mut self) {
        crate::orchestration::pop_execution_policy();
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
    sig = "host_mock(capability: string, op: string, response_or_config?: any, params?: dict) -> nil",
    category = "host"
)]
fn host_mock_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let host_mock = parse_host_mock(args)?;
    validate_host_mock_registration(&host_mock)?;
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
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
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
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "host_tool_call: tool name is required",
        ))));
    }
    let call_args = args.get(1).cloned().unwrap_or(VmValue::Nil);
    dispatch_host_tool_call_with_ctx(Some(&ctx), &name, &call_args).await
}

#[cfg(test)]
mod tests {
    use super::{
        build_sandboxed_command, capability_manifest_with_mocks, clear_host_call_bridge,
        dispatch_host_operation, dispatch_host_tool_call, dispatch_host_tool_list,
        dispatch_mock_host_call, dispatch_mock_hostlib_call, host_has_builtin,
        host_mock_clear_builtin, parse_host_mock, push_host_mock, register_mockable_host_operation,
        register_scoped_mockable_host_operation, reset_host_state, reset_scoped_host_state,
        resolve_process_exec_cwd, set_host_call_bridge, validate_host_mock_registration,
        HostCallBridge, HostMock,
    };
    use crate::value::VmDictExt;

    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use crate::value::{VmError, VmValue};

    /// Collect a built command's env mutations as `(name, Option<value>)`,
    /// where `None` marks a variable the command removes from the inherited
    /// environment.
    fn command_env(
        cmd: &tokio::process::Command,
    ) -> std::collections::BTreeMap<String, Option<String>> {
        cmd.as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn build_sandboxed_command_forces_deterministic_message_locale() {
        // A verify command spawned by a non-Anglosphere user whose *shell*
        // exports LC_ALL (inherited via the parent env, NOT pinned by the
        // caller's `env` dict) must still emit English diagnostics, or the
        // downstream English-keyed matchers (syntax repair, error grounding,
        // pass/fail classification) misfire. In merge mode the child inherits
        // the parent env implicitly, so the builder must issue an explicit
        // LC_ALL removal — observable here as a `(key, None)` mutation — and
        // pin LC_MESSAGES=C + DOTNET_CLI_UI_LANGUAGE=en. The caller pins no
        // locale key here, so the overlay engages.
        let mut params = crate::value::DictMap::new();
        params.put_str("mode", "argv");
        params.put(
            "argv",
            VmValue::List(Arc::new(vec![VmValue::string("/bin/true")])),
        );
        params.put_str("env_mode", "merge");
        let mut caller_env = crate::value::DictMap::new();
        // An innocuous caller env key that must NOT suppress the locale overlay.
        caller_env.put_str("CARGO_TARGET_DIR", "/tmp/target");
        params.put("env", VmValue::dict_map(caller_env));

        let cmd = build_sandboxed_command(&params, "process.exec").expect("build command");
        let env = command_env(&cmd);

        assert_eq!(
            env.get("LC_ALL"),
            Some(&None),
            "the builder must remove LC_ALL from the child so an inherited shell \
             value cannot override the forced LC_MESSAGES"
        );
        assert_eq!(
            env.get("LC_MESSAGES"),
            Some(&Some("C".to_string())),
            "LC_MESSAGES must be pinned to C for untranslated (English) tool output"
        );
        assert_eq!(
            env.get("DOTNET_CLI_UI_LANGUAGE"),
            Some(&Some("en".to_string())),
            ".NET ignores LC_* and needs its own UI-language override"
        );
    }

    #[test]
    fn build_sandboxed_command_respects_a_caller_pinned_locale() {
        // A caller that explicitly pins the locale keys (or LC_ALL) wins over
        // the deterministic overlay — same caller-wins rule as TMPDIR.
        let mut params = crate::value::DictMap::new();
        params.put_str("mode", "argv");
        params.put(
            "argv",
            VmValue::List(Arc::new(vec![VmValue::string("/bin/true")])),
        );
        params.put_str("env_mode", "merge");
        let mut caller_env = crate::value::DictMap::new();
        caller_env.put_str("LC_ALL", "fr_FR.UTF-8");
        caller_env.put_str("LC_MESSAGES", "fr_FR.UTF-8");
        params.put("env", VmValue::dict_map(caller_env));

        let cmd = build_sandboxed_command(&params, "process.exec").expect("build command");
        let env = command_env(&cmd);

        assert_eq!(
            env.get("LC_ALL"),
            Some(&Some("fr_FR.UTF-8".to_string())),
            "a caller that pins LC_ALL keeps it — the overlay must not strip an explicit value"
        );
        assert_eq!(
            env.get("LC_MESSAGES"),
            Some(&Some("fr_FR.UTF-8".to_string())),
            "a caller-pinned LC_MESSAGES wins over the C overlay"
        );
    }

    #[test]
    fn process_exec_relative_cwd_resolves_against_execution_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(dir.path().to_string_lossy().into_owned()),
                project_root: None,
                source_dir: Some(dir.path().join("src").to_string_lossy().into_owned()),
                env: std::collections::BTreeMap::new(),
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
    fn workspace_project_root_fallback_prefers_execution_context_project_root() {
        run_host_async_test(|| async {
            let project = tempfile::tempdir().expect("project root");
            let cwd = tempfile::tempdir().expect("cwd");
            crate::stdlib::process::set_thread_execution_context(Some(
                crate::orchestration::RunExecutionRecord {
                    cwd: Some(cwd.path().to_string_lossy().into_owned()),
                    project_root: Some(project.path().to_string_lossy().into_owned()),
                    source_dir: None,
                    env: std::collections::BTreeMap::new(),
                    adapter: None,
                    repo_path: None,
                    worktree_path: None,
                    branch: None,
                    base_ref: None,
                    cleanup: None,
                },
            ));

            let result =
                dispatch_host_operation("workspace", "project_root", &crate::value::DictMap::new())
                    .await
                    .expect("workspace.project_root result");

            crate::stdlib::process::set_thread_execution_context(None);
            assert_eq!(result.display(), project.path().display().to_string());
        });
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
            result: Some(VmValue::dict(crate::value::DictMap::new())),
            error: None,
            unregistered_ok: false,
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
        let mut exact_params = crate::value::DictMap::new();
        exact_params.put_str("namespace", "facts");
        push_host_mock(HostMock {
            capability: "project".to_string(),
            operation: "metadata_get".to_string(),
            params: None,
            result: Some(VmValue::String(arcstr::ArcStr::from("fallback"))),
            error: None,
            unregistered_ok: false,
        });
        push_host_mock(HostMock {
            capability: "project".to_string(),
            operation: "metadata_get".to_string(),
            params: Some(exact_params),
            result: Some(VmValue::String(arcstr::ArcStr::from("facts"))),
            error: None,
            unregistered_ok: false,
        });

        let mut call_params = crate::value::DictMap::new();
        call_params.put_str("dir", "pkg");
        call_params.put_str("namespace", "facts");
        let exact = dispatch_mock_host_call("project", "metadata_get", &call_params)
            .expect("expected exact mock")
            .expect("exact mock should succeed");
        assert_eq!(exact.display(), "facts");

        call_params.put_str("namespace", "classification");
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
            unregistered_ok: false,
        });
        let params = crate::value::DictMap::new();
        let result = dispatch_mock_host_call("project", "metadata_get", &params)
            .expect("expected mock result");
        match result {
            Err(VmError::Thrown(VmValue::String(message))) => assert_eq!(message.as_str(), "boom"),
            other => panic!("unexpected result: {other:?}"),
        }
        reset_host_state();
    }

    #[test]
    fn host_mock_registration_rejects_unknown_operations_by_default() {
        let host_mock = HostMock {
            capability: "runtime".to_string(),
            operation: "tas".to_string(),
            params: None,
            result: Some(VmValue::Nil),
            error: None,
            unregistered_ok: false,
        };
        let error = validate_host_mock_registration(&host_mock)
            .expect_err("unknown host operation should fail at registration");
        match error {
            VmError::Thrown(VmValue::String(message)) => {
                assert!(message.contains("runtime.tas"));
                assert!(message.contains("unregistered_ok"));
                assert!(message.contains("runtime.task"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn host_mock_registration_allows_explicit_test_local_operations() {
        let host_mock = HostMock {
            capability: "synthetic".to_string(),
            operation: "op".to_string(),
            params: None,
            result: Some(VmValue::Nil),
            error: None,
            unregistered_ok: true,
        };
        validate_host_mock_registration(&host_mock)
            .expect("explicit unregistered_ok should permit synthetic mocks");
    }

    #[test]
    fn host_mock_registration_accepts_runtime_registered_operations() {
        register_mockable_host_operation(
            "code_index",
            "stats",
            "Hostlib schema-backed operation registered at runtime.",
        );
        let host_mock = HostMock {
            capability: "code_index".to_string(),
            operation: "stats".to_string(),
            params: None,
            result: Some(VmValue::Nil),
            error: None,
            unregistered_ok: false,
        };
        validate_host_mock_registration(&host_mock)
            .expect("registered hostlib operations should be mockable");
    }

    #[test]
    fn clearing_live_mocks_preserves_scoped_manifest_declarations() {
        reset_scoped_host_state();
        register_scoped_mockable_host_operation(
            "scoped_clear_fixture",
            "answer",
            "Test-scoped manifest declaration.",
        );
        let host_mock = HostMock {
            capability: "scoped_clear_fixture".to_string(),
            operation: "answer".to_string(),
            params: None,
            result: Some(VmValue::Nil),
            error: None,
            unregistered_ok: false,
        };

        validate_host_mock_registration(&host_mock).expect("scoped declaration is registered");
        host_mock_clear_builtin(&[], &mut String::new()).expect("clear live mocks");
        validate_host_mock_registration(&host_mock)
            .expect("clearing live mocks must preserve manifest declarations");
        reset_scoped_host_state();
    }

    #[tokio::test]
    async fn declared_mockable_operation_is_not_reported_as_callable() {
        std::thread::spawn(|| {
            register_mockable_host_operation(
                "async_host_registration",
                "cross_thread",
                "Embedding operation registered before async worker migration.",
            );
        })
        .join()
        .expect("registration worker should finish");

        std::thread::spawn(|| {
            let host_mock = HostMock {
                capability: "async_host_registration".to_string(),
                operation: "cross_thread".to_string(),
                params: None,
                result: Some(VmValue::Nil),
                error: None,
                unregistered_ok: false,
            };
            validate_host_mock_registration(&host_mock)
                .expect("process host registration should be visible after worker migration");

            let typo = HostMock {
                operation: "cross_tread".to_string(),
                ..host_mock
            };
            validate_host_mock_registration(&typo)
                .expect_err("an undeclared operation should still fail closed");
        })
        .join()
        .expect("validation worker should finish");

        assert!(matches!(
            host_has_builtin(
                &[
                    VmValue::string("async_host_registration"),
                    VmValue::string("cross_thread"),
                ],
                &mut String::new(),
            )
            .expect("host_has should succeed"),
            VmValue::Bool(false)
        ));
        dispatch_host_operation(
            "async_host_registration",
            "cross_thread",
            &crate::value::DictMap::new(),
        )
        .await
        .expect_err("an unmocked declaration must remain unsupported at dispatch");
    }

    #[test]
    fn host_mock_parse_preserves_unregistered_ok_config() {
        let config = VmValue::dict(crate::value::DictMap::from_iter([
            (crate::value::intern_key("result"), VmValue::string("ok")),
            (
                crate::value::intern_key("unregistered_ok"),
                VmValue::Bool(true),
            ),
        ]));
        let host_mock =
            parse_host_mock(&[VmValue::string("synthetic"), VmValue::string("op"), config])
                .expect("parse host mock config");
        assert!(host_mock.unregistered_ok);
    }

    #[test]
    fn hostlib_mock_dispatch_matches_module_method_and_params() {
        reset_host_state();
        let mut mock_params = crate::value::DictMap::new();
        mock_params.put(
            "argv",
            VmValue::List(Arc::new(vec![VmValue::string("echo")])),
        );
        push_host_mock(HostMock {
            capability: "tools".to_string(),
            operation: "run_command".to_string(),
            params: Some(mock_params),
            result: Some(VmValue::String(arcstr::ArcStr::from("direct"))),
            error: None,
            unregistered_ok: false,
        });

        let mut call_params = crate::value::DictMap::new();
        call_params.put(
            "argv",
            VmValue::List(Arc::new(vec![VmValue::string("echo")])),
        );
        call_params.put_str("cwd", "/tmp/not-used");
        let value = dispatch_mock_hostlib_call("tools", "run_command", &call_params)
            .expect("expected hostlib mock")
            .expect("hostlib mock should succeed");
        assert_eq!(value.display(), "direct");
        reset_host_state();
    }

    #[test]
    fn hostlib_run_command_falls_back_to_process_exec_mocks() {
        reset_host_state();
        let mut mock_params = crate::value::DictMap::new();
        mock_params.put(
            "argv",
            VmValue::List(Arc::new(vec![
                VmValue::string("cargo"),
                VmValue::string("test"),
            ])),
        );
        push_host_mock(HostMock {
            capability: "process".to_string(),
            operation: "exec".to_string(),
            params: Some(mock_params),
            result: Some(VmValue::String(arcstr::ArcStr::from("legacy"))),
            error: None,
            unregistered_ok: false,
        });

        let mut call_params = crate::value::DictMap::new();
        call_params.put(
            "argv",
            VmValue::List(Arc::new(vec![
                VmValue::string("cargo"),
                VmValue::string("test"),
            ])),
        );
        call_params.put_str("cwd", "/tmp/not-used");
        let value = dispatch_mock_hostlib_call("tools", "run_command", &call_params)
            .expect("expected legacy process.exec mock")
            .expect("legacy mock should succeed");
        assert_eq!(value.display(), "legacy");
        reset_host_state();
    }

    #[test]
    fn hostlib_run_command_prefers_exact_mock_over_process_exec_alias() {
        reset_host_state();
        let mut params = crate::value::DictMap::new();
        params.put(
            "argv",
            VmValue::List(Arc::new(vec![
                VmValue::string("npm"),
                VmValue::string("test"),
            ])),
        );
        push_host_mock(HostMock {
            capability: "process".to_string(),
            operation: "exec".to_string(),
            params: Some(params.clone()),
            result: Some(VmValue::String(arcstr::ArcStr::from("legacy"))),
            error: None,
            unregistered_ok: false,
        });
        push_host_mock(HostMock {
            capability: "tools".to_string(),
            operation: "run_command".to_string(),
            params: Some(params.clone()),
            result: Some(VmValue::String(arcstr::ArcStr::from("direct"))),
            error: None,
            unregistered_ok: false,
        });

        let value = dispatch_mock_hostlib_call("tools", "run_command", &params)
            .expect("expected exact hostlib mock")
            .expect("exact mock should succeed");
        assert_eq!(value.display(), "direct");
        reset_host_state();
    }

    #[derive(Default)]
    struct TestHostToolBridge;

    impl HostCallBridge for TestHostToolBridge {
        fn dispatch(
            &self,
            _capability: &str,
            _operation: &str,
            _params: &crate::value::DictMap,
        ) -> Result<Option<VmValue>, VmError> {
            Ok(None)
        }

        fn list_tools(&self) -> Result<Option<VmValue>, VmError> {
            let tool = VmValue::dict(crate::value::DictMap::from_iter([
                (
                    crate::value::intern_key("name"),
                    VmValue::String(arcstr::ArcStr::from("Read".to_string())),
                ),
                (
                    crate::value::intern_key("description"),
                    VmValue::String(arcstr::ArcStr::from(
                        "Read a file from the host".to_string(),
                    )),
                ),
                (
                    crate::value::intern_key("schema"),
                    VmValue::dict(crate::value::DictMap::from_iter([(
                        crate::value::intern_key("type"),
                        VmValue::String(arcstr::ArcStr::from("object".to_string())),
                    )])),
                ),
                (crate::value::intern_key("deprecated"), VmValue::Bool(false)),
            ]));
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
            Ok(Some(VmValue::String(arcstr::ArcStr::from(format!(
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
            _params: &crate::value::DictMap,
        ) -> Result<Option<VmValue>, VmError> {
            if (capability, operation) != ("process", "exec") {
                return Ok(None);
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(VmValue::dict(crate::value::DictMap::from_iter([
                (
                    crate::value::intern_key("status"),
                    VmValue::String(arcstr::ArcStr::from("completed".to_string())),
                ),
                (crate::value::intern_key("exit_code"), VmValue::Int(0)),
                (crate::value::intern_key("success"), VmValue::Bool(true)),
            ]))))
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
            let args = VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("path"),
                VmValue::String(arcstr::ArcStr::from("README.md".to_string())),
            )]));
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
                deny_labels: Default::default(),
                pre: None,
                post: None,
                consent: None,
                allow_recursive: false,
            });

            let result = dispatch_host_operation(
                "process",
                "exec",
                &crate::value::DictMap::from_iter([
                    (
                        crate::value::intern_key("mode"),
                        VmValue::String(arcstr::ArcStr::from("shell")),
                    ),
                    (
                        crate::value::intern_key("command"),
                        VmValue::String(arcstr::ArcStr::from("cat Cargo.toml")),
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
        let mut params = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("mode"),
                VmValue::String(arcstr::ArcStr::from("argv")),
            ),
            (
                crate::value::intern_key("argv"),
                VmValue::List(std::sync::Arc::new(vec![
                    // Absolute path so the spawn does not depend on PATH,
                    // which the `replace` case intentionally clears.
                    VmValue::String(arcstr::ArcStr::from("/bin/sh")),
                    VmValue::String(arcstr::ArcStr::from("-c")),
                    VmValue::String(arcstr::ArcStr::from(
                        "printf '%s|%s' \"$PARENT_VAR\" \"$CHILD_VAR\"",
                    )),
                ])),
            ),
            (crate::value::intern_key("env"), env),
        ]);
        if let Some(mode) = env_mode {
            params.put_str("env_mode", mode);
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
            let child_env = VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("CHILD_VAR"),
                VmValue::String(arcstr::ArcStr::from("provided")),
            )]));
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
            let child_env = VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("CHILD_VAR"),
                VmValue::String(arcstr::ArcStr::from("provided")),
            )]));
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
            let params = crate::value::DictMap::from_iter([
                (
                    crate::value::intern_key("mode"),
                    VmValue::String(arcstr::ArcStr::from("argv")),
                ),
                (
                    crate::value::intern_key("argv"),
                    VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                        arcstr::ArcStr::from("true"),
                    )])),
                ),
                (
                    crate::value::intern_key("env"),
                    VmValue::dict(crate::value::DictMap::from_iter([(
                        crate::value::intern_key("CHILD_VAR"),
                        VmValue::String(arcstr::ArcStr::from("x")),
                    )])),
                ),
                (
                    crate::value::intern_key("env_mode"),
                    VmValue::String(arcstr::ArcStr::from("bogus")),
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

    // Drive the real `host_call("process","exec")` builder under a restricted
    // policy and read back the `$TMPDIR` the child actually saw. This is the
    // agent-facing path; the assertion is OS-independent (it observes the
    // injected env, not OS-sandbox enforcement), so it pins the mechanism on
    // every CI host while the live OS-level link proof runs on tornadough.
    #[cfg(unix)]
    async fn process_exec_tmpdir_probe(
        workspace: &std::path::Path,
        caller_env: Option<VmValue>,
    ) -> String {
        let mut env_pairs = vec![(
            crate::value::intern_key("mode"),
            VmValue::String(arcstr::ArcStr::from("argv")),
        )];
        env_pairs.push((
            crate::value::intern_key("argv"),
            VmValue::List(std::sync::Arc::new(vec![
                VmValue::String(arcstr::ArcStr::from("/bin/sh")),
                VmValue::String(arcstr::ArcStr::from("-c")),
                VmValue::String(arcstr::ArcStr::from("printf '%s' \"$TMPDIR\"")),
            ])),
        ));
        if let Some(env) = caller_env {
            env_pairs.push((crate::value::intern_key("env"), env));
        }
        let params = crate::value::DictMap::from_iter(env_pairs);

        crate::orchestration::push_execution_policy(crate::orchestration::CapabilityPolicy {
            sandbox_profile: crate::orchestration::SandboxProfile::Worktree,
            workspace_roots: vec![workspace.to_string_lossy().into_owned()],
            // Keep OS confinement out of this unit assertion regardless of host
            // Landlock/seatbelt availability; we are pinning the env injection,
            // not OS enforcement (which the tornadough run proves end-to-end).
            ..crate::orchestration::CapabilityPolicy::default()
        });
        std::env::set_var("HARN_HANDLER_SANDBOX", "off");
        let result = super::dispatch_process_exec(&params, serde_json::Value::Null)
            .await
            .expect("process.exec result");
        std::env::remove_var("HARN_HANDLER_SANDBOX");
        crate::orchestration::pop_execution_policy();
        result
            .as_dict()
            .and_then(|d| d.get("stdout"))
            .map(VmValue::display)
            .unwrap_or_default()
    }

    #[cfg(unix)]
    #[test]
    fn process_exec_injects_workspace_local_tmpdir() {
        run_host_async_test(|| async {
            let workspace = tempfile::tempdir().expect("workspace");
            let tmpdir = process_exec_tmpdir_probe(workspace.path(), None).await;

            assert!(
                !tmpdir.is_empty(),
                "sandboxed child must receive a non-empty TMPDIR"
            );
            let tmpdir_path = std::path::PathBuf::from(&tmpdir);
            let canonical_tmpdir = std::fs::canonicalize(&tmpdir_path)
                .expect("workspace-local TMPDIR should canonicalize");
            let canonical_workspace =
                std::fs::canonicalize(workspace.path()).expect("workspace should canonicalize");
            assert!(
                canonical_tmpdir.starts_with(&canonical_workspace),
                "child TMPDIR {tmpdir:?} must live inside the workspace {:?}",
                workspace.path()
            );
            assert!(
                tmpdir_path.ends_with(".harn-tmp"),
                "child TMPDIR {tmpdir:?} must be the workspace-local .harn-tmp dir"
            );
            assert!(
                tmpdir_path.is_dir(),
                "the workspace-local TMPDIR must have been created on disk"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn process_exec_respects_caller_pinned_tmpdir() {
        run_host_async_test(|| async {
            let workspace = tempfile::tempdir().expect("workspace");
            let caller_tmp = workspace.path().join("caller-chosen");
            std::fs::create_dir_all(&caller_tmp).unwrap();
            let caller_env = VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("TMPDIR"),
                VmValue::String(arcstr::ArcStr::from(
                    caller_tmp.to_string_lossy().into_owned(),
                )),
            )]));

            let tmpdir = process_exec_tmpdir_probe(workspace.path(), Some(caller_env)).await;

            assert_eq!(
                std::path::PathBuf::from(&tmpdir),
                caller_tmp,
                "an explicit caller TMPDIR must override the workspace-local default"
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
