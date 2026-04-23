pub mod backend;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use backend::{
    BackendScope, BackendWriteOptions, ConflictPolicy, DurableStateBackend, FilesystemBackend,
    WriterIdentity,
};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const HANDLE_TYPE: &str = "state_handle";
const HANDOFF_KEY: &str = "__handoff.json";

pub use backend::{
    BackendWriteOutcome, ConflictRecord, DurableStateBackend as AgentStateBackend,
    FilesystemBackend as AgentStateFilesystemBackend,
};

pub fn register_agent_state_builtins(vm: &mut Vm) {
    register_init(vm);
    register_resume(vm);
    register_write(vm);
    register_read(vm);
    register_list(vm);
    register_delete(vm);
    register_handoff(vm);
}

fn register_init(vm: &mut Vm) {
    vm.register_builtin("__agent_state_init", |args, _out| {
        let backend = FilesystemBackend::new();
        let (scope, writer, conflict_policy) = parse_init_request(args)?;
        backend.ensure_scope(&scope)?;
        Ok(handle_value(&backend, &scope, &writer, conflict_policy))
    });
}

fn register_resume(vm: &mut Vm) {
    vm.register_builtin("__agent_state_resume", |args, _out| {
        let backend = FilesystemBackend::new();
        let (scope, writer, conflict_policy) = parse_resume_request(args)?;
        backend.resume_scope(&scope)?;
        Ok(handle_value(&backend, &scope, &writer, conflict_policy))
    });
}

fn register_write(vm: &mut Vm) {
    vm.register_builtin("__agent_state_write", |args, _out| {
        let backend = FilesystemBackend::new();
        let handle = handle_from_args(args, "__agent_state_write")?;
        let key = required_arg_string(args, 1, "__agent_state_write", "key")?;
        let content = required_arg_string(args, 2, "__agent_state_write", "content")?;
        let scope = scope_from_handle(handle)?;
        let options = write_options_from_handle(handle)?;
        let outcome = backend.write(&scope, &key, &content, &options)?;
        enforce_conflict_policy(handle, &outcome)?;
        Ok(VmValue::Nil)
    });
}

fn register_read(vm: &mut Vm) {
    vm.register_builtin("__agent_state_read", |args, _out| {
        let backend = FilesystemBackend::new();
        let handle = handle_from_args(args, "__agent_state_read")?;
        let key = required_arg_string(args, 1, "__agent_state_read", "key")?;
        let scope = scope_from_handle(handle)?;
        match backend.read(&scope, &key)? {
            Some(content) => Ok(VmValue::String(Rc::from(content))),
            None => Ok(VmValue::Nil),
        }
    });
}

fn register_list(vm: &mut Vm) {
    vm.register_builtin("__agent_state_list", |args, _out| {
        let backend = FilesystemBackend::new();
        let handle = handle_from_args(args, "__agent_state_list")?;
        let scope = scope_from_handle(handle)?;
        let items = backend
            .list(&scope)?
            .into_iter()
            .map(|key| VmValue::String(Rc::from(key)))
            .collect();
        Ok(VmValue::List(Rc::new(items)))
    });
}

fn register_delete(vm: &mut Vm) {
    vm.register_builtin("__agent_state_delete", |args, _out| {
        let backend = FilesystemBackend::new();
        let handle = handle_from_args(args, "__agent_state_delete")?;
        let key = required_arg_string(args, 1, "__agent_state_delete", "key")?;
        let scope = scope_from_handle(handle)?;
        backend.delete(&scope, &key)?;
        Ok(VmValue::Nil)
    });
}

fn register_handoff(vm: &mut Vm) {
    vm.register_builtin("__agent_state_handoff", |args, _out| {
        let backend = FilesystemBackend::new();
        let handle = handle_from_args(args, "__agent_state_handoff")?;
        let summary = args.get(1).ok_or_else(|| {
            VmError::Runtime("__agent_state_handoff: `summary` is required".to_string())
        })?;
        let summary_json = crate::llm::vm_value_to_json(summary);
        let serde_json::Value::Object(_) = summary_json else {
            return Err(VmError::Runtime(
                "__agent_state_handoff: `summary` must be a JSON object".to_string(),
            ));
        };
        let typed_handoff =
            crate::orchestration::normalize_handoff_artifact_json(summary_json.clone()).ok();
        let scope = scope_from_handle(handle)?;
        let writer = writer_from_handle(handle);
        let envelope = serde_json::json!({
            "_type": "agent_state_handoff",
            "version": 1,
            "session_id": scope.namespace.clone(),
            "root": scope.root.to_string_lossy(),
            "key": HANDOFF_KEY,
            "summary": summary_json,
            "handoff": typed_handoff,
            "writer": writer_json(&writer),
            "written_at": now_epoch_seconds(),
        });
        let content = serde_json::to_string_pretty(&envelope).map_err(|error| {
            VmError::Runtime(format!("agent_state.handoff: encode error: {error}"))
        })?;
        let options = write_options_from_handle(handle)?;
        let outcome = backend.write(&scope, HANDOFF_KEY, &content, &options)?;
        enforce_conflict_policy(handle, &outcome)?;
        Ok(VmValue::Nil)
    });
}

fn handle_from_args<'a>(
    args: &'a [VmValue],
    fn_name: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    let handle = args
        .first()
        .ok_or_else(|| VmError::Runtime(format!("{fn_name}: `handle` is required")))?;
    let dict = handle.as_dict().ok_or_else(|| {
        VmError::Runtime(format!("{fn_name}: `handle` must be a state_handle dict"))
    })?;
    match dict.get("_type").map(VmValue::display).as_deref() {
        Some(HANDLE_TYPE) => Ok(dict),
        _ => Err(VmError::Runtime(format!(
            "{fn_name}: `handle` must be a state_handle dict"
        ))),
    }
}

fn required_arg_string(
    args: &[VmValue],
    idx: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<String, VmError> {
    match args.get(idx) {
        Some(VmValue::String(value)) => Ok(value.to_string()),
        Some(value) if !value.display().is_empty() => Ok(value.display()),
        _ => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` must be a non-empty string"
        ))),
    }
}

fn parse_init_request(
    args: &[VmValue],
) -> Result<(BackendScope, WriterIdentity, ConflictPolicy), VmError> {
    let (root, options, explicit_session_id) =
        match (args.first(), args.get(1), args.get(2)) {
            (Some(VmValue::String(root)), Some(VmValue::Dict(options)), _) => {
                (root.to_string(), Some((**options).clone()), None)
            }
            (Some(VmValue::String(root)), None | Some(VmValue::Nil), _) => {
                (root.to_string(), None, None)
            }
            (Some(VmValue::String(session_id)), Some(VmValue::String(root)), maybe_options) => {
                let options = maybe_options.and_then(VmValue::as_dict).cloned();
                (root.to_string(), options, Some(session_id.to_string()))
            }
            _ => return Err(VmError::Runtime(
                "__agent_state_init: expected `(root, options?)` or `(session_id, root, options?)`"
                    .to_string(),
            )),
        };
    let root = resolve_root(&root);
    let session_id = explicit_session_id
        .or_else(|| option_string(options.as_ref(), "session_id"))
        .unwrap_or_else(default_session_id);
    let writer = writer_identity(options.as_ref(), Some(&session_id));
    let conflict_policy = conflict_policy(options.as_ref())?;
    Ok((
        BackendScope {
            root,
            namespace: session_id,
        },
        writer,
        conflict_policy,
    ))
}

fn parse_resume_request(
    args: &[VmValue],
) -> Result<(BackendScope, WriterIdentity, ConflictPolicy), VmError> {
    let root = required_arg_string(args, 0, "__agent_state_resume", "root")?;
    let session_id = required_arg_string(args, 1, "__agent_state_resume", "session_id")?;
    let options = args.get(2).and_then(VmValue::as_dict).cloned();
    let writer = writer_identity(options.as_ref(), Some(&session_id));
    let conflict_policy = conflict_policy(options.as_ref())?;
    Ok((
        BackendScope {
            root: resolve_root(&root),
            namespace: session_id,
        },
        writer,
        conflict_policy,
    ))
}

fn resolve_root(root: &str) -> PathBuf {
    crate::stdlib::process::resolve_source_relative_path(root)
}

fn conflict_policy(options: Option<&BTreeMap<String, VmValue>>) -> Result<ConflictPolicy, VmError> {
    let Some(options) = options else {
        return Ok(ConflictPolicy::Ignore);
    };
    let raw = options
        .get("conflict_policy")
        .or_else(|| options.get("two_writer"))
        .map(VmValue::display)
        .unwrap_or_else(|| "ignore".to_string());
    ConflictPolicy::parse(&raw)
}

fn option_string(options: Option<&BTreeMap<String, VmValue>>, key: &str) -> Option<String> {
    options
        .and_then(|options| options.get(key))
        .map(VmValue::display)
        .filter(|value| !value.trim().is_empty())
}

fn writer_identity(
    options: Option<&BTreeMap<String, VmValue>>,
    session_id: Option<&str>,
) -> WriterIdentity {
    let mutation = crate::orchestration::current_mutation_session();
    let current_session = crate::agent_sessions::current_session_id();
    let session_id = session_id
        .map(|value| value.to_string())
        .or_else(|| current_session.clone())
        .or_else(|| mutation.as_ref().map(|session| session.session_id.clone()))
        .filter(|value| !value.is_empty());

    let worker_id = option_string(options, "worker_id").or_else(|| {
        mutation
            .as_ref()
            .and_then(|session| session.worker_id.clone())
    });
    let stage_id = option_string(options, "stage_id")
        .or_else(|| worker_id.clone())
        .or_else(|| mutation.as_ref().and_then(|session| session.run_id.clone()))
        .or_else(|| {
            mutation
                .as_ref()
                .and_then(|session| session.execution_kind.clone())
        });
    let writer_id = option_string(options, "writer_id")
        .or_else(|| worker_id.clone())
        .or_else(|| stage_id.clone())
        .or_else(|| session_id.clone());

    WriterIdentity {
        writer_id,
        stage_id,
        session_id,
        worker_id,
    }
}

fn default_session_id() -> String {
    crate::agent_sessions::current_session_id()
        .or_else(|| {
            crate::orchestration::current_mutation_session()
                .map(|session| session.session_id)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string())
}

fn handle_value(
    backend: &impl DurableStateBackend,
    scope: &BackendScope,
    writer: &WriterIdentity,
    conflict_policy: ConflictPolicy,
) -> VmValue {
    let mut handle = BTreeMap::new();
    handle.insert("_type".to_string(), VmValue::String(Rc::from(HANDLE_TYPE)));
    handle.insert(
        "backend".to_string(),
        VmValue::String(Rc::from(backend.backend_name())),
    );
    handle.insert(
        "root".to_string(),
        VmValue::String(Rc::from(scope.root.to_string_lossy().into_owned())),
    );
    handle.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(scope.namespace.clone())),
    );
    handle.insert(
        "handoff_key".to_string(),
        VmValue::String(Rc::from(HANDOFF_KEY)),
    );
    handle.insert(
        "conflict_policy".to_string(),
        VmValue::String(Rc::from(conflict_policy.as_str())),
    );
    handle.insert("writer".to_string(), writer_vm_value(writer));
    VmValue::Dict(Rc::new(handle))
}

fn writer_vm_value(writer: &WriterIdentity) -> VmValue {
    let mut value = BTreeMap::new();
    value.insert(
        "writer_id".to_string(),
        writer
            .writer_id
            .as_ref()
            .map(|item| VmValue::String(Rc::from(item.clone())))
            .unwrap_or(VmValue::Nil),
    );
    value.insert(
        "stage_id".to_string(),
        writer
            .stage_id
            .as_ref()
            .map(|item| VmValue::String(Rc::from(item.clone())))
            .unwrap_or(VmValue::Nil),
    );
    value.insert(
        "session_id".to_string(),
        writer
            .session_id
            .as_ref()
            .map(|item| VmValue::String(Rc::from(item.clone())))
            .unwrap_or(VmValue::Nil),
    );
    value.insert(
        "worker_id".to_string(),
        writer
            .worker_id
            .as_ref()
            .map(|item| VmValue::String(Rc::from(item.clone())))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(value))
}

fn scope_from_handle(handle: &BTreeMap<String, VmValue>) -> Result<BackendScope, VmError> {
    let root = handle
        .get("root")
        .map(VmValue::display)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime("state_handle is missing `root`".to_string()))?;
    let session_id = handle
        .get("session_id")
        .map(VmValue::display)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime("state_handle is missing `session_id`".to_string()))?;
    Ok(BackendScope {
        root: PathBuf::from(root),
        namespace: session_id,
    })
}

fn writer_from_handle(handle: &BTreeMap<String, VmValue>) -> WriterIdentity {
    let writer = handle.get("writer").and_then(VmValue::as_dict);
    WriterIdentity {
        writer_id: option_string(writer, "writer_id"),
        stage_id: option_string(writer, "stage_id"),
        session_id: option_string(writer, "session_id"),
        worker_id: option_string(writer, "worker_id"),
    }
}

fn write_options_from_handle(
    handle: &BTreeMap<String, VmValue>,
) -> Result<BackendWriteOptions, VmError> {
    let policy = handle
        .get("conflict_policy")
        .map(VmValue::display)
        .unwrap_or_else(|| "ignore".to_string());
    Ok(BackendWriteOptions {
        writer: writer_from_handle(handle),
        conflict_policy: ConflictPolicy::parse(&policy)?,
    })
}

fn enforce_conflict_policy(
    handle: &BTreeMap<String, VmValue>,
    outcome: &backend::BackendWriteOutcome,
) -> Result<(), VmError> {
    let Some(conflict) = &outcome.conflict else {
        return Ok(());
    };
    let options = write_options_from_handle(handle)?;
    let message = format!(
        "agent_state.write: key '{}' was previously written by '{}' and is now being written by '{}'",
        conflict.key,
        conflict.previous.display_name(),
        conflict.current.display_name()
    );
    match options.conflict_policy {
        ConflictPolicy::Ignore => Ok(()),
        ConflictPolicy::Warn => {
            let mut metadata = BTreeMap::new();
            metadata.insert("key".to_string(), serde_json::json!(conflict.key));
            metadata.insert(
                "previous_writer".to_string(),
                writer_json(&conflict.previous),
            );
            metadata.insert("current_writer".to_string(), writer_json(&conflict.current));
            crate::events::log_warn_meta("agent_state.write", &message, metadata);
            Ok(())
        }
        ConflictPolicy::Error => Err(VmError::Runtime(message)),
    }
}

fn writer_json(writer: &WriterIdentity) -> serde_json::Value {
    serde_json::json!({
        "writer_id": writer.writer_id,
        "stage_id": writer.stage_id,
        "session_id": writer.session_id,
        "worker_id": writer.worker_id,
    })
}

fn now_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::process::Command;

    use crate::agent_sessions;

    #[test]
    fn default_session_id_prefers_active_agent_session() {
        agent_sessions::push_current_session("session_test".to_string());
        assert_eq!(default_session_id(), "session_test");
        agent_sessions::pop_current_session();
    }

    #[test]
    fn writer_identity_defaults_to_current_session() {
        agent_sessions::push_current_session("session_writer".to_string());
        let writer = writer_identity(None, None);
        assert_eq!(writer.writer_id.as_deref(), Some("session_writer"));
        assert_eq!(writer.session_id.as_deref(), Some("session_writer"));
        agent_sessions::pop_current_session();
    }

    #[test]
    fn filesystem_round_trip_delete_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FilesystemBackend::new();
        let scope = BackendScope {
            root: dir.path().to_path_buf(),
            namespace: "session-a".to_string(),
        };
        backend.ensure_scope(&scope).unwrap();
        let options = BackendWriteOptions {
            writer: WriterIdentity {
                writer_id: Some("writer-a".to_string()),
                stage_id: None,
                session_id: Some("session-a".to_string()),
                worker_id: None,
            },
            conflict_policy: ConflictPolicy::Ignore,
        };
        backend.write(&scope, "plan.md", "plan", &options).unwrap();
        backend
            .write(&scope, "evidence/a.json", "{\"ok\":true}", &options)
            .unwrap();

        assert_eq!(
            backend.read(&scope, "plan.md").unwrap().as_deref(),
            Some("plan")
        );
        assert_eq!(
            backend.list(&scope).unwrap(),
            vec!["evidence/a.json".to_string(), "plan.md".to_string()]
        );

        backend.delete(&scope, "plan.md").unwrap();
        assert_eq!(backend.read(&scope, "plan.md").unwrap(), None);
        assert_eq!(
            backend.list(&scope).unwrap(),
            vec!["evidence/a.json".to_string()]
        );
    }

    #[test]
    fn filesystem_rejects_parent_escape_keys() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FilesystemBackend::new();
        let scope = BackendScope {
            root: dir.path().to_path_buf(),
            namespace: "session-b".to_string(),
        };
        backend.ensure_scope(&scope).unwrap();
        let error = backend
            .write(&scope, "../oops", "bad", &BackendWriteOptions::default())
            .unwrap_err();
        assert!(error.to_string().contains("must not escape"));
    }

    #[test]
    fn filesystem_detects_two_writer_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FilesystemBackend::new();
        let scope = BackendScope {
            root: dir.path().to_path_buf(),
            namespace: "session-c".to_string(),
        };
        backend.ensure_scope(&scope).unwrap();
        let first = BackendWriteOptions {
            writer: WriterIdentity {
                writer_id: Some("writer-a".to_string()),
                stage_id: Some("stage-a".to_string()),
                session_id: Some("session-c".to_string()),
                worker_id: None,
            },
            conflict_policy: ConflictPolicy::Ignore,
        };
        let second = BackendWriteOptions {
            writer: WriterIdentity {
                writer_id: Some("writer-b".to_string()),
                stage_id: Some("stage-b".to_string()),
                session_id: Some("session-c".to_string()),
                worker_id: None,
            },
            conflict_policy: ConflictPolicy::Error,
        };

        assert!(backend
            .write(&scope, "ledger.md", "one", &first)
            .unwrap()
            .conflict
            .is_none());
        let error = backend
            .write(&scope, "ledger.md", "two", &second)
            .unwrap_err();
        assert!(error.to_string().contains("writer-a"));
        assert!(error.to_string().contains("writer-b"));
        assert_eq!(
            backend.read(&scope, "ledger.md").unwrap().as_deref(),
            Some("one")
        );
    }

    #[test]
    fn crash_helper_aborts_after_temp_write() {
        let helper = std::env::var("HARN_AGENT_STATE_CRASH_HELPER").ok();
        let Some(target) = helper else {
            return;
        };
        let root = std::env::var("HARN_AGENT_STATE_CRASH_ROOT").unwrap();
        let scope = BackendScope {
            root: PathBuf::from(root),
            namespace: "session-crash".to_string(),
        };
        let backend = FilesystemBackend::new();
        backend.ensure_scope(&scope).unwrap();
        let options = BackendWriteOptions {
            writer: WriterIdentity {
                writer_id: Some("writer-crash".to_string()),
                stage_id: None,
                session_id: Some("session-crash".to_string()),
                worker_id: None,
            },
            conflict_policy: ConflictPolicy::Ignore,
        };
        if target == "abort" {
            let _ = backend.write(&scope, "plan.md", "after", &options);
        }
    }

    #[test]
    fn atomic_write_survives_abort_without_partial_content() {
        let exe = std::env::current_exe().unwrap();
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("session-crash");
        std::fs::create_dir_all(&session_dir).unwrap();
        let target_file = session_dir.join("plan.md");
        std::fs::write(&target_file, "before").unwrap();

        let status = Command::new(exe)
            .arg("--exact")
            .arg("stdlib::agent_state::tests::crash_helper_aborts_after_temp_write")
            .arg("--nocapture")
            .env("HARN_AGENT_STATE_CRASH_HELPER", "abort")
            .env(
                "HARN_AGENT_STATE_CRASH_ROOT",
                root.path().to_string_lossy().into_owned(),
            )
            .env("HARN_AGENT_STATE_ABORT_AFTER_TMP_WRITE", "1")
            .status()
            .unwrap();

        assert!(!status.success());
        assert_eq!(std::fs::read_to_string(&target_file).unwrap(), "before");
    }
}
