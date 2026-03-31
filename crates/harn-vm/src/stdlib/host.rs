use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn capability_manifest() -> VmValue {
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
                op("list", "List direct children of a directory."),
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
    PathBuf::from(path)
}

fn render_template(
    path: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
) -> Result<String, VmError> {
    let template = std::fs::read_to_string(path).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "host_invoke render: failed to read template {path}: {e}"
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

pub(crate) fn register_host_builtins(vm: &mut Vm) {
    vm.register_builtin("host_capabilities", |_args, _out| Ok(capability_manifest()));

    vm.register_builtin("host_has", |args, _out| {
        let capability = args.first().map(|a| a.display()).unwrap_or_default();
        let operation = args.get(1).map(|a| a.display());
        let manifest = capability_manifest();
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
    use super::capability_manifest;

    #[test]
    fn manifest_includes_operation_metadata() {
        let manifest = capability_manifest();
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
        assert!(operations.get("list").is_some());
    }
}
