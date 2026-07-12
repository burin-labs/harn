//! Default-off typed terminal-session host capability.
//!
//! The PTY and VT implementation lives in `harn-terminal`; this module owns
//! Harn boundary policy, bounded session ids, schema-shaped requests, secret
//! environment custody, and VM value conversion.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harn_terminal::{
    CellRegion, InputEvent, ProcessStatus, SessionOptions, TerminalError, TerminalSession,
    DEFAULT_RAW_CAPACITY,
};
use harn_vm::orchestration::{current_execution_policy, SandboxProfile};
use harn_vm::VmValue;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::HostlibError;
use crate::json::vm_dict_to_json;
use crate::process::handle::is_sensitive_env_name;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};
use crate::tools::args::{dict_arg, resolve_host_path};
use crate::tools::permissions::{gated_handler_for, FEATURE_TERMINAL_SESSION};

const MODULE: &str = "terminal_session";
const START: &str = "hostlib_terminal_session_start";
const SEND_KEYS: &str = "hostlib_terminal_session_send_keys";
const CAPTURE: &str = "hostlib_terminal_session_capture";
const RESIZE: &str = "hostlib_terminal_session_resize";
const WAIT_IDLE: &str = "hostlib_terminal_session_wait_idle";
const END: &str = "hostlib_terminal_session_end";
const MAX_SESSIONS: usize = 8;
const MAX_ENVIRONMENT_ENTRIES: usize = 128;
const MAX_WAIT_MS: u64 = 30_000;

#[derive(Default)]
struct SessionManager {
    sessions: Mutex<BTreeMap<String, Arc<TerminalSession>>>,
}

impl SessionManager {
    fn start(&self, request: StartRequest) -> Result<(String, Arc<TerminalSession>), HostlibError> {
        ensure_unrestricted(START)?;
        validate_start_request(&request)?;
        let cwd = request.cwd.map(resolve_host_path);
        let workspace_roots = current_execution_policy()
            .map(|policy| policy.workspace_roots)
            .unwrap_or_else(|| {
                cwd.as_ref()
                    .map(|path| vec![path.display().to_string()])
                    .unwrap_or_default()
            });
        if let Some(reason) = harn_vm::orchestration::universal_catastrophic_reason(
            &request.argv[0],
            &request.argv[1..],
            &workspace_roots,
        ) {
            return Err(HostlibError::CatastrophicFloor {
                builtin: START,
                message: reason,
            });
        }

        if let Some(path) = cwd.as_ref() {
            if !path.is_dir() {
                return Err(HostlibError::InvalidParameter {
                    builtin: START,
                    param: "cwd",
                    message: format!("working directory does not exist: {}", path.display()),
                });
            }
        }

        let mut sessions = self.sessions.lock().map_err(|_| poisoned(START))?;
        if sessions.len() >= MAX_SESSIONS {
            return Err(HostlibError::Backend {
                builtin: START,
                message: format!("terminal session limit reached ({MAX_SESSIONS})"),
            });
        }

        let mut env = request.env;
        env.entry("TERM".to_string())
            .or_insert_with(|| "xterm-256color".to_string());
        let env_remove = std::env::vars_os()
            .filter_map(|(key, _)| key.into_string().ok())
            .filter(|name| is_sensitive_env_name(name))
            .collect();
        let terminal = Arc::new(
            TerminalSession::spawn(SessionOptions {
                argv: request.argv,
                rows: request.rows,
                cols: request.columns,
                cwd,
                env,
                env_remove,
                raw_capacity: DEFAULT_RAW_CAPACITY,
            })
            .map_err(|error| terminal_error(START, "request", error))?,
        );
        let session_id = format!("terminal-{}", uuid::Uuid::now_v7());
        sessions.insert(session_id.clone(), Arc::clone(&terminal));
        Ok((session_id, terminal))
    }

    fn get(
        &self,
        builtin: &'static str,
        session_id: &str,
    ) -> Result<Arc<TerminalSession>, HostlibError> {
        self.sessions
            .lock()
            .map_err(|_| poisoned(builtin))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| HostlibError::InvalidParameter {
                builtin,
                param: "session_id",
                message: format!("unknown terminal session `{session_id}`"),
            })
    }

    fn remove(&self, session_id: &str) -> Result<Option<Arc<TerminalSession>>, HostlibError> {
        Ok(self
            .sessions
            .lock()
            .map_err(|_| poisoned(END))?
            .remove(session_id))
    }
}

/// Capability handle for typed terminal sessions.
#[derive(Clone, Default)]
pub struct TerminalSessionCapability {
    manager: Arc<SessionManager>,
}

impl TerminalSessionCapability {
    /// Construct an isolated session manager.
    pub fn new() -> Self {
        Self::default()
    }
}

impl HostlibCapability for TerminalSessionCapability {
    fn module_name(&self) -> &'static str {
        MODULE
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        register(registry, START, "start", self.manager.clone(), start);
        register(
            registry,
            SEND_KEYS,
            "send_keys",
            self.manager.clone(),
            send_keys,
        );
        register(registry, CAPTURE, "capture", self.manager.clone(), capture);
        register(registry, RESIZE, "resize", self.manager.clone(), resize);
        register(
            registry,
            WAIT_IDLE,
            "wait_idle",
            self.manager.clone(),
            wait_idle,
        );
        register(registry, END, "end", self.manager.clone(), end);
    }
}

fn register(
    registry: &mut BuiltinRegistry,
    name: &'static str,
    method: &'static str,
    manager: Arc<SessionManager>,
    runner: fn(&SessionManager, &[VmValue]) -> Result<VmValue, HostlibError>,
) {
    let handler: SyncHandler = Arc::new(move |args| runner(&manager, args));
    registry.register(RegisteredBuiltin {
        name,
        module: MODULE,
        method,
        handler: gated_handler_for(FEATURE_TERMINAL_SESSION, name, handler),
    });
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    argv: Vec<String>,
    #[serde(default = "default_rows")]
    rows: u16,
    #[serde(default = "default_columns")]
    columns: u16,
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendRequest {
    session_id: String,
    events: Vec<InputEvent>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRequest {
    session_id: String,
    region: Option<CellRegion>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResizeRequest {
    session_id: String,
    rows: u16,
    columns: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitIdleRequest {
    session_id: String,
    after_revision: Option<u64>,
    #[serde(default = "default_quiet_ms")]
    quiet_ms: u64,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndRequest {
    session_id: String,
    #[serde(default = "default_end_timeout_ms")]
    timeout_ms: u64,
}

fn start(manager: &SessionManager, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request: StartRequest = request(START, args)?;
    let rows = request.rows;
    let columns = request.columns;
    let (session_id, _) = manager.start(request)?;
    Ok(VmValue::dict([
        ("session_id", VmValue::string(session_id)),
        ("rows", VmValue::Int(i64::from(rows))),
        ("columns", VmValue::Int(i64::from(columns))),
    ]))
}

fn send_keys(manager: &SessionManager, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request: SendRequest = request(SEND_KEYS, args)?;
    let session = manager.get(SEND_KEYS, &request.session_id)?;
    let bytes_sent = session
        .send(&request.events)
        .map_err(|error| terminal_error(SEND_KEYS, "events", error))?;
    let revision = session
        .capture(None)
        .map_err(|error| terminal_error(SEND_KEYS, "session_id", error))?
        .revision;
    Ok(VmValue::dict([
        ("session_id", VmValue::string(request.session_id)),
        (
            "bytes_sent",
            VmValue::Int(i64::try_from(bytes_sent).unwrap_or(i64::MAX)),
        ),
        (
            "revision",
            VmValue::Int(i64::try_from(revision).unwrap_or(i64::MAX)),
        ),
    ]))
}

fn capture(manager: &SessionManager, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request: CaptureRequest = request(CAPTURE, args)?;
    let session = manager.get(CAPTURE, &request.session_id)?;
    let capture = session
        .capture(request.region)
        .map_err(|error| terminal_error(CAPTURE, "region", error))?;
    encode_response(CAPTURE, request.session_id, &capture)
}

fn resize(manager: &SessionManager, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request: ResizeRequest = request(RESIZE, args)?;
    let session = manager.get(RESIZE, &request.session_id)?;
    session
        .resize(request.rows, request.columns)
        .map_err(|error| terminal_error(RESIZE, "rows", error))?;
    let revision = session
        .capture(None)
        .map_err(|error| terminal_error(RESIZE, "session_id", error))?
        .revision;
    Ok(VmValue::dict([
        ("session_id", VmValue::string(request.session_id)),
        ("rows", VmValue::Int(i64::from(request.rows))),
        ("columns", VmValue::Int(i64::from(request.columns))),
        (
            "revision",
            VmValue::Int(i64::try_from(revision).unwrap_or(i64::MAX)),
        ),
    ]))
}

fn wait_idle(manager: &SessionManager, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request: WaitIdleRequest = request(WAIT_IDLE, args)?;
    validate_wait(request.quiet_ms, request.timeout_ms)?;
    let session = manager.get(WAIT_IDLE, &request.session_id)?;
    let quiet = Duration::from_millis(request.quiet_ms);
    let timeout = Duration::from_millis(request.timeout_ms);
    let result = match request.after_revision {
        Some(revision) => session.wait_idle_after(revision, quiet, timeout),
        None => session.wait_idle(quiet, timeout),
    }
    .map_err(|error| terminal_error(WAIT_IDLE, "timeout_ms", error))?;
    encode_response(WAIT_IDLE, request.session_id, &result)
}

fn end(manager: &SessionManager, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request: EndRequest = request(END, args)?;
    if request.timeout_ms == 0 || request.timeout_ms > MAX_WAIT_MS {
        return Err(HostlibError::InvalidParameter {
            builtin: END,
            param: "timeout_ms",
            message: format!("must be between 1 and {MAX_WAIT_MS}"),
        });
    }
    let session =
        manager
            .remove(&request.session_id)?
            .ok_or_else(|| HostlibError::InvalidParameter {
                builtin: END,
                param: "session_id",
                message: format!("unknown terminal session `{}`", request.session_id),
            })?;
    let status = session
        .end(Duration::from_millis(request.timeout_ms))
        .map_err(|error| terminal_error(END, "timeout_ms", error))?;
    status_response(request.session_id, status)
}

fn request<T: DeserializeOwned>(
    builtin: &'static str,
    args: &[VmValue],
) -> Result<T, HostlibError> {
    let dict = dict_arg(builtin, args)?;
    serde_json::from_value(vm_dict_to_json(&dict)).map_err(|error| HostlibError::InvalidParameter {
        builtin,
        param: "request",
        message: error.to_string(),
    })
}

fn status_response(session_id: String, status: ProcessStatus) -> Result<VmValue, HostlibError> {
    encode_response(END, session_id, &status)
}

fn encode_response(
    builtin: &'static str,
    session_id: String,
    value: &impl Serialize,
) -> Result<VmValue, HostlibError> {
    let mut json = serde_json::to_value(value).map_err(|error| HostlibError::Backend {
        builtin,
        message: format!("failed to encode terminal response: {error}"),
    })?;
    let object = json.as_object_mut().ok_or_else(|| HostlibError::Backend {
        builtin,
        message: "terminal response did not serialize as an object".to_string(),
    })?;
    object.insert(
        "session_id".to_string(),
        serde_json::Value::String(session_id),
    );
    Ok(harn_vm::json_to_vm_value(&json))
}

fn validate_start_request(request: &StartRequest) -> Result<(), HostlibError> {
    if request.argv.is_empty() || request.argv[0].is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin: START,
            param: "argv",
            message: "must start with a non-empty executable".to_string(),
        });
    }
    if request.env.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(HostlibError::InvalidParameter {
            builtin: START,
            param: "env",
            message: format!("must contain at most {MAX_ENVIRONMENT_ENTRIES} entries"),
        });
    }
    let mut normalized = BTreeSet::new();
    for key in request.env.keys() {
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(HostlibError::InvalidParameter {
                builtin: START,
                param: "env",
                message: format!("invalid environment key `{key}`"),
            });
        }
        if is_sensitive_env_name(key) {
            return Err(HostlibError::InvalidParameter {
                builtin: START,
                param: "env",
                message: format!("secret-bearing environment key `{key}` is not allowed"),
            });
        }
        let folded = key.to_ascii_uppercase();
        if !normalized.insert(folded) {
            return Err(HostlibError::InvalidParameter {
                builtin: START,
                param: "env",
                message: format!("environment key `{key}` is duplicated case-insensitively"),
            });
        }
    }
    if let Some((key, _)) = request.env.iter().find(|(_, value)| value.contains('\0')) {
        return Err(HostlibError::InvalidParameter {
            builtin: START,
            param: "env",
            message: format!("environment value for `{key}` contains a NUL byte"),
        });
    }
    Ok(())
}

fn validate_wait(quiet_ms: u64, timeout_ms: u64) -> Result<(), HostlibError> {
    if quiet_ms > timeout_ms {
        return Err(HostlibError::InvalidParameter {
            builtin: WAIT_IDLE,
            param: "quiet_ms",
            message: "must not exceed timeout_ms".to_string(),
        });
    }
    if timeout_ms == 0 || timeout_ms > MAX_WAIT_MS {
        return Err(HostlibError::InvalidParameter {
            builtin: WAIT_IDLE,
            param: "timeout_ms",
            message: format!("must be between 1 and {MAX_WAIT_MS}"),
        });
    }
    Ok(())
}

fn ensure_unrestricted(builtin: &'static str) -> Result<(), HostlibError> {
    let Some(policy) = current_execution_policy() else {
        return Ok(());
    };
    if policy.sandbox_profile == SandboxProfile::Unrestricted {
        return Ok(());
    }
    let profile = policy.sandbox_profile.as_str().to_string();
    Err(HostlibError::SandboxUnsupported {
        builtin,
        profile: profile.clone(),
        message: format!(
            "hostlib: {builtin}: terminal PTY spawning cannot preserve the active `{profile}` \
             sandbox; use an explicitly unrestricted trusted harness"
        ),
    })
}

fn terminal_error(
    builtin: &'static str,
    param: &'static str,
    error: TerminalError,
) -> HostlibError {
    match error {
        TerminalError::InvalidArgument(message) => HostlibError::InvalidParameter {
            builtin,
            param,
            message,
        },
        other => HostlibError::Backend {
            builtin,
            message: other.to_string(),
        },
    }
}

fn poisoned(builtin: &'static str) -> HostlibError {
    HostlibError::Backend {
        builtin,
        message: "terminal session manager was poisoned".to_string(),
    }
}

const fn default_rows() -> u16 {
    24
}

const fn default_columns() -> u16 {
    80
}

const fn default_quiet_ms() -> u64 {
    50
}

const fn default_timeout_ms() -> u64 {
    10_000
}

const fn default_end_timeout_ms() -> u64 {
    2_000
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PolicyGuard;

    impl Drop for PolicyGuard {
        fn drop(&mut self) {
            harn_vm::orchestration::pop_execution_policy();
        }
    }

    #[test]
    fn rejects_secret_environment_keys() {
        let request = StartRequest {
            argv: vec!["sh".into()],
            rows: 24,
            columns: 80,
            cwd: None,
            env: BTreeMap::from([("OPENAI_API_KEY".into(), "secret".into())]),
        };
        let error = validate_start_request(&request).expect_err("secret must be rejected");
        assert!(error.to_string().contains("secret-bearing"));
    }

    #[test]
    fn idle_timeout_must_cover_quiet_window() {
        let error = validate_wait(100, 50).expect_err("invalid wait");
        assert!(error.to_string().contains("must not exceed"));
    }

    #[test]
    fn restricted_sandbox_fails_closed_with_typed_error() {
        harn_vm::orchestration::push_execution_policy(
            harn_vm::orchestration::CapabilityPolicy::default(),
        );
        let _guard = PolicyGuard;
        let error = ensure_unrestricted(START).expect_err("worktree sandbox must be rejected");
        assert!(matches!(
            error,
            HostlibError::SandboxUnsupported { ref profile, .. } if profile == "worktree"
        ));
        let vm_error = harn_vm::VmError::from(error);
        let harn_vm::VmError::Thrown(VmValue::Dict(payload)) = vm_error else {
            panic!("expected structured thrown error");
        };
        assert_eq!(
            payload.get("kind").map(VmValue::display),
            Some("sandbox_unsupported".to_string())
        );
        assert_eq!(
            payload.get("profile").map(VmValue::display),
            Some("worktree".to_string())
        );
    }

    #[test]
    fn catastrophic_floor_runs_before_pty_allocation() {
        let request = StartRequest {
            argv: vec!["rm".into(), "-rf".into(), "/".into()],
            rows: 24,
            columns: 80,
            cwd: None,
            env: BTreeMap::new(),
        };
        let error = match SessionManager::default().start(request) {
            Err(error) => error,
            Ok(_) => panic!("catastrophic command must not spawn"),
        };
        assert!(matches!(error, HostlibError::CatastrophicFloor { .. }));
    }
}
