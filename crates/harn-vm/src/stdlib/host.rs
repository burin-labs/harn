use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::rc::Rc;

use crate::value::{values_equal, VmError, VmValue};
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
        "workspace".to_string(),
        capability(
            "Workspace file and directory operations.",
            &[
                op("read_text", "Read a UTF-8 text file."),
                op(
                    "write_text",
                    "Write a UTF-8 text file, creating parents as needed.",
                ),
                op("apply_edit", "Replace a substring in a text file."),
                op("delete", "Delete a file."),
                op("exists", "Check whether a path exists."),
                op("file_exists", "Alias for exists, for host parity."),
                op("list", "List direct children of a directory."),
                op(
                    "project_root",
                    "Return the resolved project root for the current execution context.",
                ),
                op(
                    "roots",
                    "Return workspace roots; local execution reports a single root.",
                ),
            ],
        ),
    );
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
                "host_invoke: missing required parameter '{key}'"
            ))))
        })
}

fn resolve_path(path: &str) -> PathBuf {
    crate::stdlib::process::resolve_source_relative_path(path)
}

fn current_project_root() -> Option<PathBuf> {
    crate::stdlib::process::current_execution_context()
        .and_then(|context| context.cwd.map(PathBuf::from))
        .or_else(|| crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone()))
        .or_else(|| std::env::current_dir().ok())
        .and_then(|base| crate::stdlib::process::find_project_root(&base).or(Some(base)))
}

fn render_template(
    path: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
) -> Result<String, VmError> {
    let resolved = crate::stdlib::process::resolve_source_relative_path(path);
    let template = std::fs::read_to_string(&resolved).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "host_invoke render: failed to read template {}: {e}",
            resolved.display()
        ))))
    })?;
    if let Some(bindings) = bindings {
        let mut result = template;
        for (key, val) in bindings {
            result = result.replace(&format!("{{{{{key}}}}}"), &val.display());
        }
        Ok(result)
    } else {
        Ok(template)
    }
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

fn mock_host_invoke(
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

    vm.register_async_builtin("host_invoke", |args| async move {
        let capability = args.first().map(|a| a.display()).unwrap_or_default();
        let operation = args.get(1).map(|a| a.display()).unwrap_or_default();
        let params = args
            .get(2)
            .and_then(|a| a.as_dict())
            .cloned()
            .unwrap_or_default();

        if let Some(mocked) = mock_host_invoke(&capability, &operation, &params) {
            return mocked;
        }

        match (capability.as_str(), operation.as_str()) {
            ("workspace", "read_text") => {
                let path = require_param(&params, "path")?;
                let content = std::fs::read_to_string(resolve_path(&path)).map_err(|e| {
                    VmError::Runtime(format!("host_invoke workspace.read_text: {e}"))
                })?;
                Ok(VmValue::String(Rc::from(content)))
            }
            ("workspace", "write_text") => {
                let path = require_param(&params, "path")?;
                let content = params
                    .get("content")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let full_path = resolve_path(&path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        VmError::Runtime(format!("host_invoke workspace.write_text: {e}"))
                    })?;
                }
                std::fs::write(&full_path, content).map_err(|e| {
                    VmError::Runtime(format!("host_invoke workspace.write_text: {e}"))
                })?;
                Ok(VmValue::Nil)
            }
            ("workspace", "apply_edit") => {
                let path = require_param(&params, "path")?;
                let old_string = require_param(&params, "old_string")?;
                let new_string = params
                    .get("new_string")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let full_path = resolve_path(&path);
                let content = std::fs::read_to_string(&full_path).map_err(|e| {
                    VmError::Runtime(format!("host_invoke workspace.apply_edit: {e}"))
                })?;
                if !content.contains(&old_string) {
                    return Err(VmError::Runtime(format!(
                        "host_invoke workspace.apply_edit: '{old_string}' not found in {path}"
                    )));
                }
                let updated = content.replacen(&old_string, &new_string, 1);
                std::fs::write(&full_path, updated).map_err(|e| {
                    VmError::Runtime(format!("host_invoke workspace.apply_edit: {e}"))
                })?;
                Ok(VmValue::Nil)
            }
            ("workspace", "delete") => {
                let path = require_param(&params, "path")?;
                let full_path = resolve_path(&path);
                if full_path.exists() {
                    std::fs::remove_file(full_path).map_err(|e| {
                        VmError::Runtime(format!("host_invoke workspace.delete: {e}"))
                    })?;
                }
                Ok(VmValue::Nil)
            }
            ("workspace", "exists") => {
                let path = require_param(&params, "path")?;
                Ok(VmValue::Bool(resolve_path(&path).exists()))
            }
            ("workspace", "file_exists") => {
                let path = require_param(&params, "path")?;
                Ok(VmValue::Bool(resolve_path(&path).exists()))
            }
            ("workspace", "list") => {
                let path = params
                    .get("path")
                    .map(|v| v.display())
                    .unwrap_or_else(|| ".".to_string());
                let entries = std::fs::read_dir(resolve_path(&path))
                    .map_err(|e| VmError::Runtime(format!("host_invoke workspace.list: {e}")))?;
                let mut values = Vec::new();
                for entry in entries.flatten() {
                    let path = entry.path();
                    let file_type = entry.file_type().ok();
                    let mut item = BTreeMap::new();
                    item.insert(
                        "name".to_string(),
                        VmValue::String(Rc::from(entry.file_name().to_string_lossy().to_string())),
                    );
                    item.insert(
                        "path".to_string(),
                        VmValue::String(Rc::from(path.to_string_lossy().to_string())),
                    );
                    item.insert(
                        "is_dir".to_string(),
                        VmValue::Bool(file_type.as_ref().is_some_and(|ft| ft.is_dir())),
                    );
                    values.push(VmValue::Dict(Rc::new(item)));
                }
                values.sort_by_key(|v| {
                    v.as_dict()
                        .and_then(|d| d.get("name"))
                        .map(|v| v.display())
                        .unwrap_or_default()
                });
                Ok(VmValue::List(Rc::new(values)))
            }
            ("workspace", "project_root") => Ok(current_project_root()
                .map(|root| VmValue::String(Rc::from(root.to_string_lossy().to_string())))
                .unwrap_or(VmValue::Nil)),
            ("workspace", "roots") => {
                let roots = current_project_root()
                    .map(|root| {
                        vec![VmValue::String(Rc::from(
                            root.to_string_lossy().to_string(),
                        ))]
                    })
                    .unwrap_or_default();
                Ok(VmValue::List(Rc::new(roots)))
            }
            ("process", "exec") => {
                let command = require_param(&params, "command")?;
                let output = tokio::process::Command::new("/bin/sh")
                    .arg("-lc")
                    .arg(&command)
                    .stdin(Stdio::null())
                    .output()
                    .await
                    .map_err(|e| VmError::Runtime(format!("host_invoke process.exec: {e}")))?;
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
                let path = require_param(&params, "path")?;
                let bindings = params.get("bindings").and_then(|v| v.as_dict());
                Ok(VmValue::String(Rc::from(render_template(&path, bindings)?)))
            }
            ("interaction", "ask") => {
                let question = require_param(&params, "question")?;
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
            _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "host_invoke: unsupported operation {capability}.{operation}"
            ))))),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        capability_manifest_with_mocks, mock_host_invoke, push_host_mock, reset_host_state,
        HostMock,
    };
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::value::{VmError, VmValue};

    #[test]
    fn manifest_includes_operation_metadata() {
        let manifest = capability_manifest_with_mocks();
        let workspace = manifest
            .as_dict()
            .and_then(|d| d.get("workspace"))
            .and_then(|v| v.as_dict())
            .expect("workspace capability");
        assert!(workspace.get("description").is_some());
        let operations = workspace
            .get("operations")
            .and_then(|v| v.as_dict())
            .expect("operations dict");
        assert!(operations.get("read_text").is_some());
        assert!(operations.get("file_exists").is_some());
        assert!(operations.get("list").is_some());
        assert!(operations.get("project_root").is_some());
        assert!(operations.get("roots").is_some());
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
    fn mock_host_invoke_matches_partial_params_and_overrides_order() {
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
        let exact = mock_host_invoke("project", "metadata_get", &call_params)
            .expect("expected exact mock")
            .expect("exact mock should succeed");
        assert_eq!(exact.display(), "facts");

        call_params.insert(
            "namespace".to_string(),
            VmValue::String(Rc::from("classification")),
        );
        let fallback = mock_host_invoke("project", "metadata_get", &call_params)
            .expect("expected fallback mock")
            .expect("fallback mock should succeed");
        assert_eq!(fallback.display(), "fallback");
        reset_host_state();
    }

    #[test]
    fn mock_host_invoke_can_throw_errors() {
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
            mock_host_invoke("project", "metadata_get", &params).expect("expected mock result");
        match result {
            Err(VmError::Thrown(VmValue::String(message))) => assert_eq!(message.as_ref(), "boom"),
            other => panic!("unexpected result: {other:?}"),
        }
        reset_host_state();
    }
}
