use std::cell::RefCell;
use std::collections::BTreeMap;
use std::process::Stdio;
use std::rc::Rc;

use serde_json::Value as JsonValue;

use crate::value::{values_equal, VmError, VmValue};
use crate::vm::clone_async_builtin_child_vm;
use crate::vm::Vm;

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
}

pub(crate) fn reset_host_state() {
    HOST_MOCKS.with(|mocks| mocks.borrow_mut().clear());
    HOST_MOCK_CALLS.with(|calls| calls.borrow_mut().clear());
}

fn capability_manifest_map() -> BTreeMap<String, VmValue> {
    let mut root = BTreeMap::new();
    root.insert(
        "process".to_string(),
        capability(
            "Process execution.",
            &[op("exec", "Execute a shell command.")],
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
        ops.push(VmValue::String(Rc::from(operation_name.to_string())));
    }

    let mut operations = entry
        .get("operations")
        .and_then(|value| value.as_dict())
        .map(|dict| (*dict).clone())
        .unwrap_or_default();
    operations
        .entry(operation_name.to_string())
        .or_insert_with(mocked_operation_entry);

    entry.insert("ops".to_string(), VmValue::List(Rc::new(ops)));
    entry.insert("operations".to_string(), VmValue::Dict(Rc::new(operations)));
    root.insert(capability_name.to_string(), VmValue::Dict(Rc::new(entry)));
}

fn capability_manifest_with_mocks() -> VmValue {
    let mut root = capability_manifest_map();
    HOST_MOCKS.with(|mocks| {
        for host_mock in mocks.borrow().iter() {
            ensure_mocked_capability(&mut root, &host_mock.capability, &host_mock.operation);
        }
    });
    VmValue::Dict(Rc::new(root))
}

fn op(name: &str, description: &str) -> (String, VmValue) {
    let mut entry = BTreeMap::new();
    entry.insert(
        "description".to_string(),
        VmValue::String(Rc::from(description)),
    );
    (name.to_string(), VmValue::Dict(Rc::new(entry)))
}

fn capability(description: &str, ops: &[(String, VmValue)]) -> VmValue {
    let mut entry = BTreeMap::new();
    entry.insert(
        "description".to_string(),
        VmValue::String(Rc::from(description)),
    );
    entry.insert(
        "ops".to_string(),
        VmValue::List(Rc::new(
            ops.iter()
                .map(|(name, _)| VmValue::String(Rc::from(name.as_str())))
                .collect(),
        )),
    );
    let mut op_dict = BTreeMap::new();
    for (name, op) in ops {
        op_dict.insert(name.clone(), op.clone());
    }
    entry.insert("operations".to_string(), VmValue::Dict(Rc::new(op_dict)));
    VmValue::Dict(Rc::new(entry))
}

fn require_param(params: &BTreeMap<String, VmValue>, key: &str) -> Result<String, VmError> {
    params
        .get(key)
        .map(|v| v.display())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "host_call: missing required parameter '{key}'"
            ))))
        })
}

fn render_template(
    path: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
) -> Result<String, VmError> {
    let resolved = crate::stdlib::process::resolve_source_asset_path(path);
    let template = std::fs::read_to_string(&resolved).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "host_call template.render: failed to read template {}: {e}",
            resolved.display()
        ))))
    })?;
    let base = resolved.parent();
    crate::stdlib::template::render_template_result(&template, bindings, base, Some(&resolved))
        .map_err(VmError::from)
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
        return Err(VmError::Thrown(VmValue::String(Rc::from(
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
        VmValue::String(Rc::from(call.capability.clone())),
    );
    item.insert(
        "operation".to_string(),
        VmValue::String(Rc::from(call.operation.clone())),
    );
    item.insert(
        "params".to_string(),
        VmValue::Dict(Rc::new(call.params.clone())),
    );
    VmValue::Dict(Rc::new(item))
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

fn mock_host_call(
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
        return Some(Err(VmError::Thrown(VmValue::String(Rc::from(error)))));
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
pub trait HostCallBridge {
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
    static HOST_CALL_BRIDGE: RefCell<Option<Rc<dyn HostCallBridge>>> = const { RefCell::new(None) };
}

/// Install a bridge for the current thread. The bridge is consulted on
/// every `host_call` *after* mock matching but *before* the built-in
/// match arms, so embedders can override anything they like (and equally
/// punt on anything they don't, by returning `Ok(None)`).
pub fn set_host_call_bridge(bridge: Rc<dyn HostCallBridge>) {
    HOST_CALL_BRIDGE.with(|b| *b.borrow_mut() = Some(bridge));
}

/// Remove the current thread's bridge. Idempotent.
pub fn clear_host_call_bridge() {
    HOST_CALL_BRIDGE.with(|b| *b.borrow_mut() = None);
}

fn empty_tool_list_value() -> VmValue {
    VmValue::List(Rc::new(Vec::new()))
}

fn current_vm_host_bridge() -> Option<Rc<crate::bridge::HostBridge>> {
    clone_async_builtin_child_vm().and_then(|vm| vm.bridge.clone())
}

async fn dispatch_host_tool_list() -> Result<VmValue, VmError> {
    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone());
    if let Some(bridge) = bridge {
        if let Some(value) = bridge.list_tools()? {
            return Ok(value);
        }
    }

    let Some(bridge) = current_vm_host_bridge() else {
        return Ok(empty_tool_list_value());
    };
    let tools = bridge.list_host_tools().await?;
    Ok(crate::bridge::json_result_to_vm_value(&JsonValue::Array(
        tools.into_iter().collect(),
    )))
}

async fn dispatch_host_tool_call(name: &str, args: &VmValue) -> Result<VmValue, VmError> {
    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone());
    if let Some(bridge) = bridge {
        if let Some(value) = bridge.call_tool(name, args)? {
            return Ok(value);
        }
    }

    let Some(bridge) = current_vm_host_bridge() else {
        return Err(VmError::Thrown(VmValue::String(Rc::from(
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

async fn dispatch_host_operation(
    capability: &str,
    operation: &str,
    params: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    if let Some(mocked) = mock_host_call(capability, operation, params) {
        return mocked;
    }

    let bridge = HOST_CALL_BRIDGE.with(|b| b.borrow().clone());
    if let Some(bridge) = bridge {
        if let Some(value) = bridge.dispatch(capability, operation, params)? {
            return Ok(value);
        }
    }

    match (capability, operation) {
        ("process", "exec") => {
            let command = require_param(params, "command")?;
            let output = tokio::process::Command::new("/bin/sh")
                .arg("-lc")
                .arg(&command)
                .stdin(Stdio::null())
                .output()
                .await
                .map_err(|e| VmError::Runtime(format!("host_call process.exec: {e}")))?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let mut result = BTreeMap::new();
            result.insert(
                "stdout".to_string(),
                VmValue::String(Rc::from(stdout.clone())),
            );
            result.insert(
                "stderr".to_string(),
                VmValue::String(Rc::from(stderr.clone())),
            );
            result.insert(
                "combined".to_string(),
                VmValue::String(Rc::from(format!("{stdout}{stderr}"))),
            );
            let status = output.status.code().unwrap_or(-1);
            result.insert("status".to_string(), VmValue::Int(status as i64));
            result.insert(
                "success".to_string(),
                VmValue::Bool(output.status.success()),
            );
            Ok(VmValue::Dict(Rc::new(result)))
        }
        ("template", "render") => {
            let path = require_param(params, "path")?;
            let bindings = params.get("bindings").and_then(|v| v.as_dict());
            Ok(VmValue::String(Rc::from(render_template(&path, bindings)?)))
        }
        ("interaction", "ask") => {
            let question = require_param(params, "question")?;
            use std::io::BufRead;
            print!("{question}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut input = String::new();
            if std::io::stdin().lock().read_line(&mut input).is_ok() {
                Ok(VmValue::String(Rc::from(input.trim_end())))
            } else {
                Ok(VmValue::Nil)
            }
        }
        // Standalone-run fallbacks for capabilities normally supplied by
        // an embedder's JSON-RPC bridge. `runtime.task` lets a debugger or
        // CLI invocation read the pipeline input from `HARN_TASK` without
        // the host explicitly wiring a callback for every op.
        ("runtime", "task") => Ok(VmValue::String(Rc::from(
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
            Ok(VmValue::String(Rc::from(path)))
        }
        ("workspace", "cwd") => {
            let path = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            Ok(VmValue::String(Rc::from(path)))
        }
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "host_call: unsupported operation {capability}.{operation}"
        ))))),
    }
}

pub(crate) fn register_host_builtins(vm: &mut Vm) {
    vm.register_builtin("host_mock", |args, _out| {
        let host_mock = parse_host_mock(args)?;
        push_host_mock(host_mock);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("host_mock_clear", |_args, _out| {
        reset_host_state();
        Ok(VmValue::Nil)
    });

    vm.register_builtin("host_mock_calls", |_args, _out| {
        let calls = HOST_MOCK_CALLS.with(|calls| {
            calls
                .borrow()
                .iter()
                .map(mock_call_value)
                .collect::<Vec<_>>()
        });
        Ok(VmValue::List(Rc::new(calls)))
    });

    vm.register_builtin("host_capabilities", |_args, _out| {
        Ok(capability_manifest_with_mocks())
    });

    vm.register_builtin("host_has", |args, _out| {
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
    });

    vm.register_async_builtin("host_call", |args| async move {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        let params = args
            .get(1)
            .and_then(|a| a.as_dict())
            .cloned()
            .unwrap_or_default();
        let Some((capability, operation)) = name.split_once('.') else {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "host_call: unsupported operation name '{name}'"
            )))));
        };
        dispatch_host_operation(capability, operation, &params).await
    });

    vm.register_async_builtin("host_tool_list", |_args| async move {
        dispatch_host_tool_list().await
    });

    vm.register_async_builtin("host_tool_call", |args| async move {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        if name.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "host_tool_call: tool name is required",
            ))));
        }
        let call_args = args.get(1).cloned().unwrap_or(VmValue::Nil);
        dispatch_host_tool_call(&name, &call_args).await
    });
}

#[cfg(test)]
mod tests {
    use super::{
        capability_manifest_with_mocks, clear_host_call_bridge, dispatch_host_tool_call,
        dispatch_host_tool_list, mock_host_call, push_host_mock, reset_host_state,
        set_host_call_bridge, HostCallBridge, HostMock,
    };
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::value::{VmError, VmValue};

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
            result: Some(VmValue::Dict(Rc::new(BTreeMap::new()))),
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
        exact_params.insert("namespace".to_string(), VmValue::String(Rc::from("facts")));
        push_host_mock(HostMock {
            capability: "project".to_string(),
            operation: "metadata_get".to_string(),
            params: None,
            result: Some(VmValue::String(Rc::from("fallback"))),
            error: None,
        });
        push_host_mock(HostMock {
            capability: "project".to_string(),
            operation: "metadata_get".to_string(),
            params: Some(exact_params),
            result: Some(VmValue::String(Rc::from("facts"))),
            error: None,
        });

        let mut call_params = BTreeMap::new();
        call_params.insert("dir".to_string(), VmValue::String(Rc::from("pkg")));
        call_params.insert("namespace".to_string(), VmValue::String(Rc::from("facts")));
        let exact = mock_host_call("project", "metadata_get", &call_params)
            .expect("expected exact mock")
            .expect("exact mock should succeed");
        assert_eq!(exact.display(), "facts");

        call_params.insert(
            "namespace".to_string(),
            VmValue::String(Rc::from("classification")),
        );
        let fallback = mock_host_call("project", "metadata_get", &call_params)
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
        let result =
            mock_host_call("project", "metadata_get", &params).expect("expected mock result");
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
            let tool = VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "name".to_string(),
                    VmValue::String(Rc::from("Read".to_string())),
                ),
                (
                    "description".to_string(),
                    VmValue::String(Rc::from("Read a file from the host".to_string())),
                ),
                (
                    "schema".to_string(),
                    VmValue::Dict(Rc::new(BTreeMap::from([(
                        "type".to_string(),
                        VmValue::String(Rc::from("object".to_string())),
                    )]))),
                ),
                ("deprecated".to_string(), VmValue::Bool(false)),
            ])));
            Ok(Some(VmValue::List(Rc::new(vec![tool]))))
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
            Ok(Some(VmValue::String(Rc::from(format!("read:{path}")))))
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
            set_host_call_bridge(Rc::new(TestHostToolBridge));
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
            set_host_call_bridge(Rc::new(TestHostToolBridge));
            let args = VmValue::Dict(Rc::new(BTreeMap::from([(
                "path".to_string(),
                VmValue::String(Rc::from("README.md".to_string())),
            )])));
            let value = dispatch_host_tool_call("Read", &args)
                .await
                .expect("tool call");
            clear_host_call_bridge();
            assert_eq!(value.display(), "read:README.md");
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
