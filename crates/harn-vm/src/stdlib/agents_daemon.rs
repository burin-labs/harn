use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use crate::bridge::HostBridge;
use crate::llm::daemon::{load_snapshot, DaemonSnapshot};
use crate::orchestration::DaemonEventKindRecord;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const SNAPSHOT_FILE: &str = "daemon.json";
const META_FILE: &str = "daemon.meta.json";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PersistedDaemonMeta {
    #[serde(rename = "_type")]
    type_name: String,
    id: String,
    name: String,
    prompt: String,
    system: Option<String>,
    session_id: String,
    options: serde_json::Value,
}

#[derive(Clone)]
struct DaemonSpawnSpec {
    id: String,
    name: String,
    prompt: String,
    system: Option<String>,
    session_id: String,
    persist_root: String,
    snapshot_path: String,
    meta_path: String,
    options: BTreeMap<String, VmValue>,
}

struct DaemonState {
    id: String,
    name: String,
    prompt: String,
    system: Option<String>,
    session_id: String,
    persist_root: String,
    snapshot_path: String,
    options: BTreeMap<String, VmValue>,
    bridge: Rc<HostBridge>,
    handle: Option<tokio::task::JoinHandle<Result<VmValue, VmError>>>,
    status: String,
    last_error: Option<String>,
    last_result: Option<serde_json::Value>,
    last_snapshot: Option<DaemonSnapshot>,
}

thread_local! {
    static DAEMON_REGISTRY: RefCell<BTreeMap<String, Rc<RefCell<DaemonState>>>> =
        const { RefCell::new(BTreeMap::new()) };
    static DAEMON_COUNTER: Cell<u64> = const { Cell::new(0) };
}

pub fn register_daemon_builtins(vm: &mut Vm) {
    vm.register_async_builtin("daemon_spawn", |args| async move {
        let child_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
            VmError::Runtime("daemon_spawn requires an async builtin VM context".to_string())
        })?;
        let config = require_dict_arg(&args, 0, "daemon_spawn")?;
        let spec = parse_spawn_spec(config, None, None)?;
        if find_daemon_by_root(&spec.persist_root)
            .is_some_and(|state| state.borrow().status == "running")
        {
            return Err(VmError::Runtime(format!(
                "daemon_spawn: a daemon is already running for '{}'",
                spec.persist_root
            )));
        }
        write_meta(&spec)?;
        let state = Rc::new(RefCell::new(DaemonState {
            id: spec.id.clone(),
            name: spec.name.clone(),
            prompt: spec.prompt.clone(),
            system: spec.system.clone(),
            session_id: spec.session_id.clone(),
            persist_root: spec.persist_root.clone(),
            snapshot_path: spec.snapshot_path.clone(),
            options: spec.options.clone(),
            bridge: new_daemon_bridge().await?,
            handle: None,
            status: "running".to_string(),
            last_error: None,
            last_result: None,
            last_snapshot: None,
        }));
        register_daemon(state.clone());
        spawn_daemon_task(state.clone(), child_vm);
        wait_for_snapshot(state.clone(), None, 500).await;
        record_daemon_event(
            &spec.id,
            &spec.name,
            DaemonEventKindRecord::Spawned,
            &spec.persist_root,
            summarize_text(&spec.prompt),
        );
        let summary = {
            let daemon = state.borrow();
            daemon_summary(&daemon)?
        };
        Ok(summary)
    });

    vm.register_async_builtin("daemon_trigger", |args| async move {
        let target = args
            .first()
            .ok_or_else(|| VmError::Runtime("daemon_trigger: missing daemon handle".to_string()))?;
        let payload = args
            .get(1)
            .ok_or_else(|| VmError::Runtime("daemon_trigger: missing event payload".to_string()))?;
        let daemon_id = daemon_id_from_value(target)?;
        let state = with_daemon_state(&daemon_id, |state| Ok(state.clone()))?;
        {
            let daemon = state.borrow();
            if daemon.status != "running" {
                return Err(VmError::Runtime(format!(
                    "daemon_trigger: daemon {} is not running",
                    daemon.id
                )));
            }
        }
        let message = trigger_payload_text(payload);
        let payload_summary = summarize_text(&message);
        let bridge = state.borrow().bridge.clone();
        bridge
            .push_queued_user_message(message, "finish_step")
            .await;
        bridge.signal_resume();
        {
            let daemon = state.borrow();
            record_daemon_event(
                &daemon.id,
                &daemon.name,
                DaemonEventKindRecord::Triggered,
                &daemon.persist_root,
                payload_summary.clone(),
            );
        }
        let summary = {
            let daemon = state.borrow();
            daemon_summary(&daemon)?
        };
        Ok(summary)
    });

    vm.register_builtin("daemon_snapshot", |args, _out| {
        let target = args.first().ok_or_else(|| {
            VmError::Runtime("daemon_snapshot: missing daemon handle".to_string())
        })?;
        let daemon_id = daemon_id_from_value(target)?;
        with_daemon_state(&daemon_id, |state| {
            let mut daemon = state.borrow_mut();
            let snapshot = refresh_snapshot(&mut daemon)?;
            record_daemon_event(
                &daemon.id,
                &daemon.name,
                DaemonEventKindRecord::Snapshotted,
                &daemon.persist_root,
                summarize_snapshot(snapshot.as_ref()),
            );
            Ok(snapshot_to_vm(&snapshot.unwrap_or_default()))
        })
    });

    vm.register_builtin("daemon_stop", |args, _out| {
        let target = args
            .first()
            .ok_or_else(|| VmError::Runtime("daemon_stop: missing daemon handle".to_string()))?;
        let daemon_id = daemon_id_from_value(target)?;
        with_daemon_state(&daemon_id, |state| {
            let mut daemon = state.borrow_mut();
            if daemon.status == "stopped" {
                return daemon_summary(&daemon);
            }
            refresh_snapshot(&mut daemon)?;
            if let Some(handle) = daemon.handle.take() {
                handle.abort();
            }
            daemon.status = "stopped".to_string();
            daemon.last_error = None;
            record_daemon_event(
                &daemon.id,
                &daemon.name,
                DaemonEventKindRecord::Stopped,
                &daemon.persist_root,
                summarize_snapshot(daemon.last_snapshot.as_ref()),
            );
            daemon_summary(&daemon)
        })
    });

    vm.register_async_builtin("daemon_resume", |args| async move {
        let child_vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
            VmError::Runtime("daemon_resume requires an async builtin VM context".to_string())
        })?;
        let persist_root = required_string_arg(&args, 0, "daemon_resume", "path")?;
        let paths = daemon_paths(&persist_root);
        let snapshot = load_snapshot(&paths.snapshot_path)?;

        if let Some(state) = find_daemon_by_root(&persist_root) {
            if state.borrow().status == "running" {
                return Err(VmError::Runtime(format!(
                    "daemon_resume: daemon '{}' is already running",
                    persist_root
                )));
            }
            {
                let mut daemon = state.borrow_mut();
                daemon.last_snapshot = Some(snapshot.clone());
                daemon.status = "running".to_string();
                daemon.last_error = None;
                daemon.last_result = None;
                daemon.options.insert(
                    "resume_path".to_string(),
                    VmValue::String(Rc::from(paths.snapshot_path.clone())),
                );
                daemon.options.insert(
                    "persist_path".to_string(),
                    VmValue::String(Rc::from(paths.snapshot_path.clone())),
                );
            }
            spawn_daemon_task(state.clone(), child_vm);
            wait_for_snapshot(state.clone(), Some(snapshot.saved_at.clone()), 500).await;
            {
                let daemon = state.borrow();
                record_daemon_event(
                    &daemon.id,
                    &daemon.name,
                    DaemonEventKindRecord::Resumed,
                    &daemon.persist_root,
                    summarize_snapshot(Some(&snapshot)),
                );
            }
            return daemon_summary(&state.borrow());
        }

        let meta = read_meta(&paths.meta_path)?;
        let options = match crate::stdlib::json_to_vm_value(&meta.options) {
            VmValue::Dict(dict) => (*dict).clone(),
            _ => {
                return Err(VmError::Runtime(format!(
                    "daemon_resume: metadata at '{}' is not a dict",
                    paths.meta_path
                )))
            }
        };
        let spec = parse_spawn_spec(
            &BTreeMap::from([
                (
                    "name".to_string(),
                    VmValue::String(Rc::from(meta.name.clone())),
                ),
                (
                    "task".to_string(),
                    VmValue::String(Rc::from(meta.prompt.clone())),
                ),
                (
                    "persist_path".to_string(),
                    VmValue::String(Rc::from(persist_root.clone())),
                ),
                (
                    "session_id".to_string(),
                    VmValue::String(Rc::from(meta.session_id.clone())),
                ),
                (
                    "options".to_string(),
                    VmValue::Dict(Rc::new(options.clone())),
                ),
            ]),
            Some(meta.id),
            meta.system,
        )?;
        let mut spec = spec;
        spec.options = options;
        spec.options
            .insert("daemon".to_string(), VmValue::Bool(true));
        spec.options
            .insert("persistent".to_string(), VmValue::Bool(false));
        spec.options.insert(
            "session_id".to_string(),
            VmValue::String(Rc::from(spec.session_id.clone())),
        );
        spec.options.insert(
            "persist_path".to_string(),
            VmValue::String(Rc::from(spec.snapshot_path.clone())),
        );
        spec.options.insert(
            "resume_path".to_string(),
            VmValue::String(Rc::from(spec.snapshot_path.clone())),
        );

        let state = Rc::new(RefCell::new(DaemonState {
            id: spec.id.clone(),
            name: spec.name.clone(),
            prompt: spec.prompt.clone(),
            system: spec.system.clone(),
            session_id: spec.session_id.clone(),
            persist_root: spec.persist_root.clone(),
            snapshot_path: spec.snapshot_path.clone(),
            options: spec.options.clone(),
            bridge: new_daemon_bridge().await?,
            handle: None,
            status: "running".to_string(),
            last_error: None,
            last_result: None,
            last_snapshot: Some(snapshot.clone()),
        }));
        register_daemon(state.clone());
        spawn_daemon_task(state.clone(), child_vm);
        wait_for_snapshot(state.clone(), Some(snapshot.saved_at.clone()), 500).await;
        record_daemon_event(
            &spec.id,
            &spec.name,
            DaemonEventKindRecord::Resumed,
            &spec.persist_root,
            summarize_snapshot(Some(&snapshot)),
        );
        let summary = {
            let daemon = state.borrow();
            daemon_summary(&daemon)?
        };
        Ok(summary)
    });
}

fn require_dict_arg<'a>(
    args: &'a [VmValue],
    idx: usize,
    fn_name: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    match args.get(idx) {
        Some(VmValue::Dict(dict)) => Ok(dict),
        _ => Err(VmError::Runtime(format!(
            "{fn_name}: expected a config dict"
        ))),
    }
}

fn required_string_arg(
    args: &[VmValue],
    idx: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<String, VmError> {
    match args.get(idx) {
        Some(VmValue::String(text)) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(VmError::Runtime(format!(
            "{fn_name}: `{arg_name}` must be a non-empty string"
        ))),
    }
}

fn optional_string(dict: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    dict.get(key)
        .map(VmValue::display)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn daemon_paths(root: &str) -> DaemonPaths {
    let root_path = PathBuf::from(root);
    DaemonPaths {
        persist_root: root.to_string(),
        snapshot_path: root_path.join(SNAPSHOT_FILE).to_string_lossy().into_owned(),
        meta_path: root_path.join(META_FILE).to_string_lossy().into_owned(),
    }
}

struct DaemonPaths {
    persist_root: String,
    snapshot_path: String,
    meta_path: String,
}

fn parse_spawn_spec(
    config: &BTreeMap<String, VmValue>,
    explicit_id: Option<String>,
    explicit_system: Option<String>,
) -> Result<DaemonSpawnSpec, VmError> {
    let mut options = if let Some(VmValue::Dict(dict)) = config.get("options") {
        (**dict).clone()
    } else {
        config.clone()
    };
    options.remove("name");
    options.remove("prompt");
    options.remove("task");
    options.remove("system");
    options.remove("options");

    let prompt = optional_string(config, "task")
        .or_else(|| optional_string(config, "prompt"))
        .ok_or_else(|| {
            VmError::Runtime("daemon_spawn: config must include `task` or `prompt`".to_string())
        })?;
    let persist_root = optional_string(config, "persist_path").ok_or_else(|| {
        VmError::Runtime("daemon_spawn: config must include `persist_path`".to_string())
    })?;
    let paths = daemon_paths(&persist_root);
    let id = explicit_id.unwrap_or_else(next_daemon_id);
    let name = optional_string(config, "name").unwrap_or_else(|| id.clone());
    let session_id = optional_string(config, "session_id")
        .unwrap_or_else(|| format!("daemon_session_{}", uuid::Uuid::now_v7()));
    let system = explicit_system.or_else(|| optional_string(config, "system"));

    options.insert("daemon".to_string(), VmValue::Bool(true));
    options.insert("persistent".to_string(), VmValue::Bool(false));
    options.insert(
        "session_id".to_string(),
        VmValue::String(Rc::from(session_id.clone())),
    );
    options.insert(
        "persist_path".to_string(),
        VmValue::String(Rc::from(paths.snapshot_path.clone())),
    );
    options.remove("resume_path");

    Ok(DaemonSpawnSpec {
        id,
        name,
        prompt,
        system,
        session_id,
        persist_root: paths.persist_root,
        snapshot_path: paths.snapshot_path,
        meta_path: paths.meta_path,
        options,
    })
}

fn next_daemon_id() -> String {
    DAEMON_COUNTER.with(|counter| {
        let next = counter.get() + 1;
        counter.set(next);
        format!("daemon_{}", uuid::Uuid::now_v7())
    })
}

fn register_daemon(state: Rc<RefCell<DaemonState>>) {
    let daemon_id = state.borrow().id.clone();
    DAEMON_REGISTRY.with(|registry| {
        registry.borrow_mut().insert(daemon_id, state);
    });
}

fn find_daemon_by_root(persist_root: &str) -> Option<Rc<RefCell<DaemonState>>> {
    DAEMON_REGISTRY.with(|registry| {
        registry
            .borrow()
            .values()
            .find(|state| state.borrow().persist_root == persist_root)
            .cloned()
    })
}

fn daemon_id_from_value(value: &VmValue) -> Result<String, VmError> {
    match value {
        VmValue::String(text) => Ok(text.to_string()),
        VmValue::Dict(dict) => match dict.get("id") {
            Some(VmValue::String(id)) => Ok(id.to_string()),
            Some(other) => Ok(other.display()),
            None => Err(VmError::Runtime(
                "daemon handle dict is missing an id field".to_string(),
            )),
        },
        _ => Err(VmError::Runtime(
            "expected daemon handle or daemon id".to_string(),
        )),
    }
}

fn with_daemon_state<T>(
    daemon_id: &str,
    f: impl FnOnce(&Rc<RefCell<DaemonState>>) -> Result<T, VmError>,
) -> Result<T, VmError> {
    let state = DAEMON_REGISTRY.with(|registry| registry.borrow().get(daemon_id).cloned());
    let state = state.ok_or_else(|| VmError::Runtime(format!("unknown daemon '{daemon_id}'")))?;
    f(&state)
}

fn refresh_snapshot(daemon: &mut DaemonState) -> Result<Option<DaemonSnapshot>, VmError> {
    let path = PathBuf::from(&daemon.snapshot_path);
    if !path.exists() {
        return Ok(daemon.last_snapshot.clone());
    }
    let snapshot = load_snapshot(&daemon.snapshot_path)?;
    daemon.last_snapshot = Some(snapshot.clone());
    Ok(Some(snapshot))
}

fn snapshot_to_vm(snapshot: &DaemonSnapshot) -> VmValue {
    let json = serde_json::to_value(snapshot).unwrap_or_default();
    crate::stdlib::json_to_vm_value(&json)
}

fn daemon_summary(daemon: &DaemonState) -> Result<VmValue, VmError> {
    let mut summary = serde_json::json!({
        "id": daemon.id,
        "name": daemon.name,
        "status": daemon.status,
        "session_id": daemon.session_id,
        "persist_path": daemon.persist_root,
        "snapshot_path": daemon.snapshot_path,
    });
    if let Some(error) = &daemon.last_error {
        summary["error"] = serde_json::json!(error);
    }
    if let Some(result) = &daemon.last_result {
        summary["result"] = result.clone();
    }
    if let Some(snapshot) = &daemon.last_snapshot {
        summary["daemon_state"] = serde_json::json!(snapshot.daemon_state);
        summary["saved_at"] = serde_json::json!(snapshot.saved_at);
    }
    Ok(crate::stdlib::json_to_vm_value(&summary))
}

async fn new_daemon_bridge() -> Result<Rc<HostBridge>, VmError> {
    let Some(vm) = crate::vm::clone_async_builtin_child_vm() else {
        return Ok(Rc::new(HostBridge::from_parts(
            Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
            Arc::new(std::sync::Mutex::new(())),
            1,
        )));
    };
    let module_path = daemon_bridge_module_path()?;
    HostBridge::from_harn_module(vm, &module_path)
        .await
        .map(Rc::new)
}

fn daemon_bridge_module_path() -> Result<PathBuf, VmError> {
    let dir = std::env::temp_dir().join("harn-daemon-bridge");
    std::fs::create_dir_all(&dir)
        .map_err(|error| VmError::Runtime(format!("daemon bridge mkdir error: {error}")))?;
    let path = dir.join("noop_host.harn");
    if !Path::new(&path).exists() {
        std::fs::write(&path, "pub fn request_permission() {\n  return true\n}\n")
            .map_err(|error| VmError::Runtime(format!("daemon bridge write error: {error}")))?;
    }
    Ok(path)
}

fn trigger_payload_text(payload: &VmValue) -> String {
    match payload {
        VmValue::String(text) => text.to_string(),
        _ => serde_json::to_string(&crate::llm::vm_value_to_json(payload))
            .unwrap_or_else(|_| payload.display()),
    }
}

fn summarize_text(text: &str) -> Option<String> {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    const MAX_LEN: usize = 160;
    if compact.len() <= MAX_LEN {
        Some(compact)
    } else {
        Some(format!("{}...", &compact[..MAX_LEN]))
    }
}

fn summarize_snapshot(snapshot: Option<&DaemonSnapshot>) -> Option<String> {
    snapshot.map(|snapshot| {
        format!(
            "state={} saved_at={}",
            snapshot.daemon_state, snapshot.saved_at
        )
    })
}

fn record_daemon_event(
    daemon_id: &str,
    name: &str,
    kind: DaemonEventKindRecord,
    persist_path: &str,
    payload_summary: Option<String>,
) {
    let mut fields = serde_json::Map::new();
    fields.insert("daemon_id".to_string(), serde_json::json!(daemon_id));
    fields.insert("name".to_string(), serde_json::json!(name));
    fields.insert("kind".to_string(), serde_json::json!(kind));
    fields.insert("persist_path".to_string(), serde_json::json!(persist_path));
    fields.insert(
        "payload_summary".to_string(),
        payload_summary.map_or(serde_json::Value::Null, serde_json::Value::String),
    );
    crate::llm::append_observability_sidecar_entry("daemon_event", fields);
}

fn write_meta(spec: &DaemonSpawnSpec) -> Result<(), VmError> {
    let meta = PersistedDaemonMeta {
        type_name: "daemon_meta".to_string(),
        id: spec.id.clone(),
        name: spec.name.clone(),
        prompt: spec.prompt.clone(),
        system: spec.system.clone(),
        session_id: spec.session_id.clone(),
        options: crate::llm::vm_value_to_json(&VmValue::Dict(Rc::new(spec.options.clone()))),
    };
    let meta_path = PathBuf::from(&spec.meta_path);
    if let Some(parent) = meta_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| VmError::Runtime(format!("daemon meta mkdir error: {error}")))?;
    }
    let encoded = serde_json::to_string_pretty(&meta)
        .map_err(|error| VmError::Runtime(format!("daemon meta encode error: {error}")))?;
    std::fs::write(&meta_path, encoded)
        .map_err(|error| VmError::Runtime(format!("daemon meta write error: {error}")))?;
    Ok(())
}

fn read_meta(meta_path: &str) -> Result<PersistedDaemonMeta, VmError> {
    let content = std::fs::read_to_string(meta_path)
        .map_err(|error| VmError::Runtime(format!("daemon meta read error: {error}")))?;
    serde_json::from_str(&content)
        .map_err(|error| VmError::Runtime(format!("daemon meta parse error: {error}")))
}

fn spawn_daemon_task(state: Rc<RefCell<DaemonState>>, mut vm: crate::vm::Vm) {
    let (prompt, system, options, bridge) = {
        let daemon = state.borrow();
        (
            daemon.prompt.clone(),
            daemon.system.clone(),
            daemon.options.clone(),
            daemon.bridge.clone(),
        )
    };
    vm.set_bridge(bridge.clone());
    let task_state = state.clone();
    let handle = tokio::task::spawn_local(async move {
        let args = vec![
            VmValue::String(Rc::from(prompt)),
            match system {
                Some(text) => VmValue::String(Rc::from(text)),
                None => VmValue::Nil,
            },
            VmValue::Dict(Rc::new(options)),
        ];

        crate::llm::install_current_host_bridge(bridge);
        let mut bridge_cleared = false;
        let mut future = Pin::from(Box::new(vm.call_named_builtin("agent_loop", args)));
        let result = std::future::poll_fn(|cx| {
            let polled = Future::poll(future.as_mut(), cx);
            if !bridge_cleared {
                crate::llm::clear_current_host_bridge();
                bridge_cleared = true;
            }
            polled
        })
        .await;
        if !bridge_cleared {
            crate::llm::clear_current_host_bridge();
        }

        {
            let mut daemon = task_state.borrow_mut();
            match &result {
                Ok(value) => {
                    daemon.status = "stopped".to_string();
                    daemon.last_error = None;
                    daemon.last_result = Some(crate::llm::vm_value_to_json(value));
                    let _ = refresh_snapshot(&mut daemon);
                }
                Err(error) => {
                    daemon.status = "failed".to_string();
                    daemon.last_error = Some(error.to_string());
                    let _ = refresh_snapshot(&mut daemon);
                }
            }
            daemon.handle = None;
        }

        result
    });

    state.borrow_mut().handle = Some(handle);
}

async fn wait_for_snapshot(
    state: Rc<RefCell<DaemonState>>,
    baseline_saved_at: Option<String>,
    timeout_ms: u64,
) {
    let start = std::time::Instant::now();
    loop {
        let maybe_saved_at = {
            let mut daemon = state.borrow_mut();
            match refresh_snapshot(&mut daemon) {
                Ok(snapshot) => snapshot.map(|snapshot| snapshot.saved_at),
                Err(_) => None,
            }
        };
        if let Some(saved_at) = maybe_saved_at {
            if baseline_saved_at
                .as_ref()
                .is_none_or(|baseline| baseline != &saved_at)
            {
                break;
            }
        }
        if start.elapsed().as_millis() as u64 >= timeout_ms {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}
