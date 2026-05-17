//! Agent Client Protocol (ACP) server implementation.
//!
//! Implements the ACP specification (<https://agentclientprotocol.com>) so that
//! harn can act as an agent runtime accessible from any host application
//! (IDEs, CLI tools, web apps, etc.).  Communication is JSON-RPC 2.0 over stdin/stdout, following the same
//! structural pattern as the existing `--bridge` mode.
//!
//! Module map: `transport` owns stdio/channel routing, `sessions` owns session
//! state and cancellation, `schema` owns capability and prompt normalization,
//! `auth` owns ACP credential normalization, and `bridge` owns outbound host
//! calls and update/log/progress emission.

mod auth;
mod bridge;
mod builtins;
mod commands;
mod events;
mod execute;
mod io;
mod modes;
mod schema;
mod sessions;
#[cfg(test)]
mod tests;
mod transport;

use auth::acp_auth_request_for_method;
use bridge::{AcpBridge, AcpOutput};
#[cfg(test)]
use schema::configured_llm_route_for_capabilities;
use schema::{acp_agent_capabilities, normalize_acp_prompt, retarget_prompt_text};
pub use schema::{
    ACP_SCHEMA_COMPATIBILITY, ACP_SESSION_UPDATE_VARIANTS, HARN_AGENT_EVENT_KINDS,
    HARN_AGENT_EVENT_METHOD, HARN_CONTENT_EXTENSION_FIELDS, HARN_SESSION_UPDATE_EXTENSIONS,
    HARN_TOOL_LIFECYCLE_EXTENSION_FIELDS,
};
use sessions::{
    mark_cancelled_session, preempt_session_cancel_or_truncate, prepare_session_prompt, Session,
    SessionCancellation, SessionInfo,
};
pub use transport::{run_acp_channel_server, run_acp_server};

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use harn_vm::agent_events::{clear_session_sinks, register_sink, AgentEventSink};
use harn_vm::visible_text::{sanitize_visible_assistant_text, VisibleTextState};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, oneshot, Mutex, Notify};

use crate::{
    AdapterDescriptor, AuthMethodConfig, AuthPolicy, AuthRequest, AuthenticatedPrincipal,
    AuthorizationDecision,
};
use commands::{
    discover_commands, parse_slash_invocation, render_available_commands, DiscoveredCommand,
};
use events::AcpAgentEventSink;
use io::send_json_response;

const ACP_AUTH_REQUIRED_CODE: i64 = -32000;

fn session_id_param(params: &serde_json::Value) -> Option<String> {
    params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn nonnegative_usize_param(
    params: &serde_json::Value,
    names: &[&str],
    label: &str,
) -> Result<Option<usize>, String> {
    for name in names {
        let Some(value) = params.get(*name) else {
            continue;
        };
        return match value.as_i64() {
            Some(value) if value >= 0 => Ok(Some(value as usize)),
            _ => Err(format!("Invalid {label}: must be >= 0")),
        };
    }
    Ok(None)
}

fn append_profile_json_line(
    path: &std::path::Path,
    session_id: &str,
    turn: u64,
    rollup: &harn_vm::profile::RunProfile,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create profile directory {}: {error}",
                    parent.display()
                )
            })?;
        }
    }
    let line = serde_json::to_string(&serde_json::json!({
        "turn": turn,
        "session_id": session_id,
        "rollup": rollup,
    }))
    .map_err(|error| format!("failed to serialize profile: {error}"))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    writeln!(file, "{line}")
        .map_err(|error| format!("failed to append {}: {error}", path.display()))
}

#[async_trait(?Send)]
pub trait AcpRuntimeConfigurator: Send + Sync {
    async fn configure(
        &self,
        _vm: &mut harn_vm::Vm,
        _source_path: Option<&std::path::Path>,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct NoopAcpRuntimeConfigurator;

#[async_trait(?Send)]
impl AcpRuntimeConfigurator for NoopAcpRuntimeConfigurator {}

#[derive(Clone, Default)]
pub struct AcpProfileConfig {
    pub text: bool,
    pub json_path: Option<PathBuf>,
}

impl AcpProfileConfig {
    pub fn is_enabled(&self) -> bool {
        self.text || self.json_path.is_some()
    }
}

#[derive(Clone)]
pub struct AcpServerConfig {
    pub pipeline: Option<String>,
    pub auth_policy: AuthPolicy,
    pub runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
    pub llm_config_overrides: Option<harn_vm::llm_config::ProvidersConfig>,
    pub llm_capability_overrides: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    pub profile: AcpProfileConfig,
}

impl AcpServerConfig {
    pub fn new(pipeline: Option<String>) -> Self {
        Self {
            pipeline,
            auth_policy: AuthPolicy::allow_all(),
            runtime_configurator: Arc::new(NoopAcpRuntimeConfigurator),
            llm_config_overrides: None,
            llm_capability_overrides: None,
            profile: AcpProfileConfig::default(),
        }
    }

    pub fn for_pipeline(path: impl Into<String>) -> Self {
        Self::new(Some(path.into()))
    }

    pub fn with_runtime_configurator(
        mut self,
        runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
    ) -> Self {
        self.runtime_configurator = runtime_configurator;
        self
    }

    pub fn with_auth_policy(mut self, auth_policy: AuthPolicy) -> Self {
        self.auth_policy = auth_policy;
        self
    }

    pub fn with_llm_overrides(
        mut self,
        llm_config: Option<harn_vm::llm_config::ProvidersConfig>,
        llm_capabilities: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    ) -> Self {
        self.llm_config_overrides = llm_config;
        self.llm_capability_overrides = llm_capabilities;
        self
    }

    pub fn with_profile(mut self, profile: AcpProfileConfig) -> Self {
        self.profile = profile;
        self
    }
}

/// Cached compiled pipeline. The ACP server re-uses the same pipeline file
/// across every `session/prompt`; recompiling on each turn was the dominant
/// per-turn overhead inside the VM (~80ms on auto.harn) before this cache.
/// Keyed by (path, mtime, target_pipeline_name) — when the file's mtime moves
/// forward we discard the cache and re-read/re-compile.
struct CompileCacheEntry {
    path: PathBuf,
    mtime: SystemTime,
    target_pipeline: Option<String>,
    source: String,
    chunk: harn_vm::Chunk,
}

/// Cached VM baseline for a file-backed ACP pipeline. This is intentionally
/// separate from bytecode caching: source can compile from cache while VM
/// setup is invalidated by cwd, project-root discovery, target pipeline, or
/// ACP mode changes.
struct VmBaselineCacheEntry {
    path: PathBuf,
    mtime: SystemTime,
    target_pipeline: Option<String>,
    source: String,
    cwd: PathBuf,
    project_root: Option<PathBuf>,
    mode_id: String,
    baseline: harn_vm::VmBaseline,
}

/// ACP server that reads JSON-RPC requests from a transport and writes
/// responses / notifications back to that same transport.
pub struct AcpServer {
    descriptor: AdapterDescriptor,
    /// Optional pipeline file to execute on each `session/prompt`.
    pipeline: Option<String>,
    /// Shared harn-serve auth policy for adapter entrypoints.
    auth_policy: AuthPolicy,
    /// Principal authenticated through ACP's connection-level `authenticate` method.
    authenticated_principal: Option<AuthenticatedPrincipal>,
    /// CLI/project hook used to install package-provided runtime extensions.
    runtime_configurator: Arc<dyn AcpRuntimeConfigurator>,
    /// Active sessions keyed by session ID.
    sessions: HashMap<String, Session>,
    /// Monotonically increasing JSON-RPC request ID for outgoing requests.
    next_id: AtomicU64,
    /// Pending outgoing request waiters, keyed by JSON-RPC id.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    /// Cancel flags are shared with the transport reader so
    /// `session/cancel` can interrupt a blocked `session/prompt` turn.
    session_cancellations: Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    /// Transport output sink.
    output: AcpOutput,
    /// Compiled-pipeline cache. One slot — the ACP server runs a single
    /// pipeline file for its lifetime, so we only ever cache one chunk.
    compile_cache: Option<CompileCacheEntry>,
    /// Prepared VM baseline cache for the same file-backed pipeline.
    vm_baseline_cache: Option<VmBaselineCacheEntry>,
    /// Per-turn profile emission settings.
    profile: AcpProfileConfig,
}

impl AcpServer {
    pub fn new(config: AcpServerConfig) -> Self {
        Self::new_with_output(config, AcpOutput::stdout())
    }

    fn new_with_output(config: AcpServerConfig, output: AcpOutput) -> Self {
        harn_vm::llm_config::set_user_overrides(config.llm_config_overrides.clone());
        harn_vm::llm::capabilities::set_user_overrides(config.llm_capability_overrides.clone());

        Self {
            descriptor: AdapterDescriptor {
                id: "acp".to_string(),
                caller_shape: "agent-session".to_string(),
                supports_streaming: true,
                supports_cancel: true,
            },
            pipeline: config.pipeline,
            auth_policy: config.auth_policy,
            authenticated_principal: None,
            runtime_configurator: config.runtime_configurator,
            sessions: HashMap::new(),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            session_cancellations: Arc::new(std::sync::Mutex::new(HashMap::new())),
            output,
            compile_cache: None,
            vm_baseline_cache: None,
            profile: config.profile,
        }
    }

    /// Compile `source` for `target_pipeline` (or the default entry point
    /// when `target_pipeline` is None), reusing the cached chunk when the
    /// file at `source_path` has the same mtime as the last cache fill and
    /// the target hasn't changed.
    ///
    /// Returns `(chunk, hit)` so the caller can keep its existing compile-
    /// time telemetry meaningful (hits report ~0 ms).
    ///
    /// Inline-mode prompts pass `source_path: None` and never hit cache —
    /// the source is freshly generated per turn so there's nothing to reuse.
    fn compile_pipeline_cached(
        &mut self,
        source: &str,
        source_path: Option<&Path>,
        target_pipeline: Option<&str>,
    ) -> Result<(harn_vm::Chunk, bool), String> {
        let target_owned = target_pipeline.map(|s| s.to_string());
        let cache_key = source_path.and_then(|path| {
            std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .map(|mtime| (path.to_path_buf(), mtime))
        });
        if let Some((ref path, mtime)) = cache_key {
            if let Some(entry) = self.compile_cache.as_ref() {
                if entry.path == *path
                    && entry.mtime == mtime
                    && entry.target_pipeline == target_owned
                    && entry.source == source
                {
                    return Ok((entry.chunk.clone(), true));
                }
            }
        }
        let chunk = match target_pipeline {
            Some(name) => harn_vm::compile_source_named(source, name),
            None => harn_vm::compile_source(source),
        }
        .map_err(|e| format!("Compilation error: {e}"))?;
        if let Some((path, mtime)) = cache_key {
            self.compile_cache = Some(CompileCacheEntry {
                path,
                mtime,
                target_pipeline: target_owned,
                source: source.to_string(),
                chunk: chunk.clone(),
            });
        }
        Ok((chunk, false))
    }

    async fn prepare_vm_baseline_cached(
        &mut self,
        source: &str,
        source_path: Option<&Path>,
        target_pipeline: Option<&str>,
        cwd: &Path,
        mode_id: &str,
    ) -> Result<(Option<harn_vm::VmBaseline>, Option<bool>, u64), String> {
        let Some(source_path) = source_path else {
            return Ok((None, None, 0));
        };

        let prepare_started = Instant::now();
        let target_owned = target_pipeline.map(str::to_string);
        let cache_key = std::fs::metadata(source_path)
            .and_then(|m| m.modified())
            .ok()
            .map(|mtime| (source_path.to_path_buf(), mtime));
        let source_parent = source_path.parent().unwrap_or(cwd);
        let project_root = harn_vm::stdlib::process::find_project_root(source_parent)
            .or_else(|| harn_vm::stdlib::process::find_project_root(cwd));

        if let Some((ref path, mtime)) = cache_key {
            if let Some(entry) = self.vm_baseline_cache.as_ref() {
                if entry.path == *path
                    && entry.mtime == mtime
                    && entry.target_pipeline == target_owned
                    && entry.source == source
                    && entry.cwd == cwd
                    && entry.project_root == project_root
                    && entry.mode_id == mode_id
                {
                    return Ok((
                        Some(entry.baseline.clone()),
                        Some(true),
                        prepare_started.elapsed().as_millis() as u64,
                    ));
                }
            }
        }

        let baseline = execute::prepare_vm_baseline(
            source,
            source_path,
            cwd,
            self.runtime_configurator.clone(),
        )
        .await?;
        if let Some((path, mtime)) = cache_key {
            self.vm_baseline_cache = Some(VmBaselineCacheEntry {
                path,
                mtime,
                target_pipeline: target_owned,
                source: source.to_string(),
                cwd: cwd.to_path_buf(),
                project_root,
                mode_id: mode_id.to_string(),
                baseline: baseline.clone(),
            });
        } else {
            self.vm_baseline_cache = None;
        }

        Ok((
            Some(baseline),
            Some(false),
            prepare_started.elapsed().as_millis() as u64,
        ))
    }

    /// Write a complete JSON-RPC message to the current transport.
    fn write_line(&self, line: &str) {
        self.output.write_line(line);
    }

    /// Send a JSON-RPC success response.
    fn send_response(&self, id: &serde_json::Value, result: serde_json::Value) {
        let response = harn_vm::jsonrpc::response(id.clone(), result);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    /// Send a JSON-RPC error response.
    fn send_error(&self, id: &serde_json::Value, code: i64, message: &str) {
        let response = harn_vm::jsonrpc::error_response(id.clone(), code, message);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    fn send_error_with_data(
        &self,
        id: &serde_json::Value,
        code: i64,
        message: &str,
        data: serde_json::Value,
    ) {
        let response = harn_vm::jsonrpc::error_response_with_data(id.clone(), code, message, data);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    #[allow(dead_code)]
    fn send_notification(&self, method: &str, params: serde_json::Value) {
        let notification = harn_vm::jsonrpc::notification(method, params);
        if let Ok(line) = serde_json::to_string(&notification) {
            self.write_line(&line);
        }
    }

    /// Send a `session/update` notification with an agent message chunk.
    #[allow(dead_code)]
    fn send_update(&self, session_id: &str, text: &str) {
        let visible_text = sanitize_visible_assistant_text(text, true);
        let mut content = serde_json::json!({
            "type": "text",
            "text": text,
        });
        let mut content_meta = serde_json::Map::new();
        content_meta.insert(
            "visible_text".to_string(),
            serde_json::Value::String(visible_text.clone()),
        );
        content_meta.insert(
            "visible_delta".to_string(),
            serde_json::Value::String(visible_text),
        );
        events::merge_harn_meta(&mut content, content_meta);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": content,
                },
            }),
        );
    }

    fn send_prompt_error(&self, session_id: &str, id: &serde_json::Value, message: &str) {
        self.send_update(session_id, &format!("Error: {message}\n"));
        self.send_error(id, -32000, message);
        eprintln!("{message}");
    }

    /// Generate a unique session ID.
    fn next_session_id(&mut self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn register_session_cancellation(&mut self, session_id: &str) -> SessionCancellation {
        let cancellation = SessionCancellation::default();
        self.session_cancellations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), cancellation.clone());
        cancellation
    }

    fn handle_initialize(&self, id: &serde_json::Value) {
        self.send_response(
            id,
            serde_json::json!({
                "protocolVersion": 1,
                "agentCapabilities": acp_agent_capabilities(),
                "agentInfo": {
                    "name": "harn",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "authMethods": self.auth_policy.acp_auth_methods(),
            }),
        );
    }

    fn auth_required_data(&self) -> serde_json::Value {
        serde_json::json!({
            "authMethods": self.auth_policy.acp_auth_methods(),
        })
    }

    fn send_auth_required(&self, id: &serde_json::Value) {
        self.send_error_with_data(
            id,
            ACP_AUTH_REQUIRED_CODE,
            "auth_required",
            self.auth_required_data(),
        );
    }

    fn requires_authentication(&self) -> bool {
        !self.auth_policy.methods.is_empty() && self.authenticated_principal.is_none()
    }

    fn reject_unauthenticated(&self, id: &serde_json::Value) -> bool {
        if !self.requires_authentication() {
            return false;
        }
        if !id.is_null() {
            self.send_auth_required(id);
        }
        true
    }

    async fn handle_authenticate(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let method_id = match params.get("methodId").and_then(|value| value.as_str()) {
            Some(method_id) => method_id,
            None => {
                self.send_error(id, -32602, "authenticate requires methodId");
                return;
            }
        };
        let Some(method) = self.auth_policy.method_by_acp_id(method_id) else {
            self.send_error_with_data(
                id,
                -32602,
                "authenticate methodId was not advertised",
                self.auth_required_data(),
            );
            return;
        };
        let auth = match acp_auth_request_for_method(method, params) {
            Ok(auth) => auth,
            Err(message) => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &message,
                    self.auth_required_data(),
                );
                return;
            }
        };
        match self.auth_policy.authorize(&auth).await {
            AuthorizationDecision::Authorized(principal) => {
                self.authenticated_principal = Some(principal.clone());
                self.send_response(
                    id,
                    serde_json::json!({
                        "_meta": {
                            "harn": {
                                "authenticated": true,
                                "principal": {
                                    "subject": principal.subject,
                                    "scheme": principal.scheme,
                                }
                            }
                        }
                    }),
                );
            }
            AuthorizationDecision::Rejected(message) => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &message,
                    self.auth_required_data(),
                );
            }
        }
    }

    pub fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }

    fn insert_session(&mut self, session_id: String, cwd: PathBuf, info: SessionInfo) {
        let cancellation = self.register_session_cancellation(&session_id);
        self.sessions.insert(
            session_id.clone(),
            Session {
                cwd,
                cancellation,
                host_bridge: None,
                info,
                advertised_commands: Vec::new(),
                current_mode_id: modes::DEFAULT_MODE_ID.to_string(),
                profile_turn: 0,
            },
        );
        harn_vm::agent_sessions::open_or_create(Some(session_id));
    }

    fn handle_session_new(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let session_id = self.next_session_id();
        self.insert_session(session_id.clone(), cwd, SessionInfo::default());

        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "modes": modes::session_mode_state(modes::DEFAULT_MODE_ID),
                "configOptions": modes::config_options_state(
                    modes::DEFAULT_MODE_ID,
                    self.pinned_model(&session_id).as_deref(),
                ),
            }),
        );

        self.emit_available_commands(&session_id);
    }

    /// Read the configured pipeline source for `session_id`. Returns
    /// `None` for inline-prompt sessions (no `--pipeline`) and on read
    /// error — the regular prompt path will surface the error to the
    /// client at execution time.
    fn read_pipeline_source(&self, session_id: &str) -> Option<String> {
        let pipeline_path = self.pipeline.as_deref()?;
        let cwd = &self.sessions.get(session_id)?.cwd;
        let full_path = if std::path::Path::new(pipeline_path).is_absolute() {
            PathBuf::from(pipeline_path)
        } else {
            cwd.join(pipeline_path)
        };
        std::fs::read_to_string(&full_path).ok()
    }

    /// Discover and emit `available_commands_update` if the command set
    /// has changed since the last emission for this session.
    fn emit_available_commands(&mut self, session_id: &str) {
        let Some(source) = self.read_pipeline_source(session_id) else {
            return;
        };
        self.refresh_advertised_commands(session_id, &source);
    }

    /// Hot-reload variant of [`Self::emit_available_commands`] that uses
    /// pre-loaded source instead of re-reading from disk. Driven from
    /// `handle_session_prompt` on every prompt so editor changes between
    /// prompts propagate to the client without a restart.
    fn refresh_advertised_commands(&mut self, session_id: &str, source: &str) {
        let commands = discover_commands(source);
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };
        if session.advertised_commands == commands {
            return;
        }
        session.advertised_commands = commands.clone();
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": render_available_commands(&commands),
                },
            }),
        );
    }

    fn emit_session_info_update(&self, session_id: &str, info: &SessionInfo) {
        let mut update = serde_json::json!({
            "sessionUpdate": "session_info_update",
        });
        if let Some(title) = &info.title {
            update["title"] = serde_json::json!(title);
        }
        if !info.meta.is_empty() {
            update["_meta"] = serde_json::Value::Object(info.meta.clone());
        }
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }),
        );
    }

    fn begin_profile_turn(&mut self, session_id: &str) -> u64 {
        if !self.profile.is_enabled() {
            return 0;
        }
        let Some(session) = self.sessions.get_mut(session_id) else {
            return 0;
        };
        session.profile_turn += 1;
        harn_vm::tracing::set_tracing_enabled(true);
        session.profile_turn
    }

    fn finish_profile_turn(&self, session_id: &str, turn: u64) {
        if turn == 0 || !self.profile.is_enabled() {
            return;
        }
        let spans = harn_vm::tracing::take_spans();
        let rollup = harn_vm::profile::build(&spans);
        if self.profile.text {
            eprintln!("[harn] ACP profile session={session_id} turn={turn}");
            eprint!("{}", harn_vm::profile::render(&rollup));
        }
        if let Some(path) = self.profile.json_path.as_ref() {
            if let Err(error) = append_profile_json_line(path, session_id, turn, &rollup) {
                eprintln!("warning: failed to write ACP profile: {error}");
            }
        }
    }

    fn handle_session_fork(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let src_id = session_id_param(params);
        let Some(src_id) = src_id else {
            self.send_error(id, -32602, "Missing session_id");
            return;
        };
        let Some(src_cwd) = self
            .sessions
            .get(&src_id)
            .map(|session| session.cwd.clone())
        else {
            self.send_error(id, -32602, &format!("Unknown session: {src_id}"));
            return;
        };

        if !harn_vm::agent_sessions::exists(&src_id) {
            harn_vm::agent_sessions::open_or_create(Some(src_id.clone()));
        }

        let keep_first =
            match nonnegative_usize_param(params, &["keep_first", "keepFirst"], "keep_first") {
                Ok(value) => value,
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    return;
                }
            };
        let dst_id = params
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        if let Some(dst_id) = dst_id.as_deref() {
            if self.sessions.contains_key(dst_id) {
                self.send_error(id, -32602, &format!("Session already exists: {dst_id}"));
                return;
            }
            if harn_vm::agent_sessions::exists(dst_id) {
                self.send_error(id, -32602, &format!("Session already exists: {dst_id}"));
                return;
            }
        }
        let branch_name = params
            .get("branch_name")
            .and_then(|value| value.as_str())
            .map(str::to_string);

        let new_session_id = match keep_first {
            Some(keep_first) => harn_vm::agent_sessions::fork_at(&src_id, keep_first, dst_id),
            None => harn_vm::agent_sessions::fork(&src_id, dst_id),
        };
        let Some(new_session_id) = new_session_id else {
            self.send_error(id, -32000, &format!("Failed to fork session: {src_id}"));
            return;
        };

        let snapshot = harn_vm::agent_sessions::snapshot(&new_session_id)
            .and_then(|value| serde_json::to_value(harn_vm::llm::vm_value_to_json(&value)).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let branched_at = snapshot
            .get("branched_at_event_index")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let mut meta = serde_json::Map::new();
        meta.insert("state".to_string(), serde_json::json!("forked"));
        meta.insert("parent_id".to_string(), serde_json::json!(src_id.clone()));
        meta.insert("branched_at".to_string(), branched_at.clone());
        if let Some(branch_name) = &branch_name {
            meta.insert("branch_name".to_string(), serde_json::json!(branch_name));
        }
        let info = SessionInfo {
            title: branch_name,
            meta,
        };

        let parent_mode_id = self
            .sessions
            .get(&src_id)
            .map(|session| session.current_mode_id.clone())
            .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
        let cancellation = self.register_session_cancellation(&new_session_id);
        self.sessions.insert(
            new_session_id.clone(),
            Session {
                cwd: src_cwd,
                cancellation,
                host_bridge: None,
                info: info.clone(),
                advertised_commands: Vec::new(),
                current_mode_id: parent_mode_id.clone(),
                profile_turn: 0,
            },
        );
        self.emit_session_info_update(&new_session_id, &info);
        self.emit_available_commands(&new_session_id);
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": new_session_id,
                "state": "forked",
                "parent_id": src_id,
                "branched_at": branched_at,
                "modes": modes::session_mode_state(&parent_mode_id),
                "configOptions": modes::config_options_state(
                    &parent_mode_id,
                    self.pinned_model(&new_session_id).as_deref(),
                ),
            }),
        );
    }

    fn handle_session_truncate(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = session_id_param(params) else {
            self.send_error(id, -32602, "Missing sessionId");
            return;
        };
        let keep_first =
            match nonnegative_usize_param(params, &["keepFirst", "keep_first"], "keepFirst") {
                Ok(Some(value)) => value,
                Ok(None) => {
                    self.send_error(id, -32602, "Missing keepFirst");
                    return;
                }
                Err(message) => {
                    self.send_error(id, -32602, &message);
                    return;
                }
            };
        let Some(cancellation) = self
            .sessions
            .get(&session_id)
            .map(|session| session.cancellation.clone())
        else {
            self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
            return;
        };

        cancellation.cancel();
        if !harn_vm::agent_sessions::exists(&session_id) {
            harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        }
        let Some(result) = harn_vm::agent_sessions::truncate(&session_id, keep_first) else {
            self.send_error(
                id,
                -32000,
                &format!("Failed to truncate session: {session_id}"),
            );
            return;
        };

        let mut update = serde_json::json!({
            "sessionUpdate": "session_truncated",
            "keptTurnCount": result.kept_turn_count,
            "removedTurnCount": result.removed_turn_count,
            "newTipTurnId": result.new_tip_turn_id.clone(),
        });
        if let Some(reason) = params.get("reason").and_then(|value| value.as_str()) {
            update["reason"] = serde_json::json!(reason);
        }
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }),
        );
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "keptTurnCount": result.kept_turn_count,
                "removedTurnCount": result.removed_turn_count,
                "newTipTurnId": result.new_tip_turn_id,
            }),
        );
    }

    async fn handle_session_prompt(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                self.send_error(id, -32602, "Missing sessionId");
                return;
            }
        };

        let prompt = match normalize_acp_prompt(params) {
            Ok(prompt) => prompt,
            Err(message) => {
                self.send_prompt_error(&session_id, id, &message);
                return;
            }
        };
        let prompt_text = prompt.text.clone();

        let (cwd, cancellation, current_mode_id) = match self.sessions.get_mut(&session_id) {
            Some(s) => {
                s.cancellation.begin_prompt();
                s.host_bridge = None;
                (
                    s.cwd.clone(),
                    s.cancellation.clone(),
                    s.current_mode_id.clone(),
                )
            }
            None => {
                self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
                return;
            }
        };
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());

        let (source, source_path) = if let Some(ref pipeline_path) = self.pipeline {
            let full_path = if Path::new(pipeline_path).is_absolute() {
                PathBuf::from(pipeline_path)
            } else {
                cwd.join(pipeline_path)
            };
            match std::fs::read_to_string(&full_path) {
                Ok(src) => (src, Some(full_path)),
                Err(e) => {
                    let message = format!("Failed to read pipeline {}: {e}", full_path.display());
                    self.send_prompt_error(&session_id, id, &message);
                    return;
                }
            }
        } else {
            // Inline-prompt mode has no persistent pipeline source to host
            // `@command`-tagged decls, so a leading slash invocation can
            // only be the user expecting to invoke an advertised command
            // that doesn't exist. Surface a friendly error instead of
            // wrapping `/foo args` into `pipeline main() { /foo args }`,
            // which would fail with a generic "Compilation error" later.
            if parse_slash_invocation(&prompt_text).is_some() {
                self.send_prompt_error(
                    &session_id,
                    id,
                    "Slash commands require `--pipeline <file>`; the agent is running in inline mode.",
                );
                return;
            }
            // Wrap inline prompt source in a pipeline so the compiler has
            // an entry point.
            let wrapped = format!("pipeline main() {{\n{prompt_text}\n}}");
            (wrapped, None)
        };

        // Hot-reload: re-discover slash-commands from the just-loaded
        // source and emit `available_commands_update` if the set changed
        // since the last advertise. Only meaningful when a pipeline file
        // is configured; inline prompts have no persistent surface to
        // attach commands to.
        if source_path.is_some() {
            self.refresh_advertised_commands(&session_id, &source);
        }

        // Slash-command dispatch: if the prompt begins with `/<name>` and
        // `<name>` matches an advertised command, route to the named
        // pipeline with the post-name text as the new `prompt`. Unknown
        // slashes fall through unmodified — the default pipeline can
        // choose to treat them as text or surface its own diagnostic.
        let (effective_prompt, target_pipeline) = match parse_slash_invocation(&prompt_text) {
            Some((cmd_name, args)) => {
                let pipeline_name = self.sessions.get(&session_id).and_then(|session| {
                    session
                        .advertised_commands
                        .iter()
                        .find(|c| c.name == cmd_name)
                        .map(|c| c.pipeline_name.clone())
                });
                match pipeline_name {
                    Some(name) => (args.to_string(), Some(name)),
                    None => (prompt_text.clone(), None),
                }
            }
            None => (prompt_text.clone(), None),
        };
        let prompt_text = effective_prompt;
        let mut prompt = prompt;
        if prompt_text != prompt.text {
            retarget_prompt_text(&mut prompt, prompt_text.clone());
        }

        let output = self.output.clone();
        let pending = self.pending.clone();
        let next_id = &self.next_id;
        let sid = session_id.clone();

        // Translate AgentEvents into ACP session/update notifications so
        // the client observes tool lifecycle on the wire. The event-log
        // sink is reinstalled here because prompt teardown clears all
        // per-session transport sinks after each turn.
        clear_session_sinks(&session_id);
        harn_vm::agent_sessions::register_event_log_sink(&session_id);
        register_sink(
            session_id.clone(),
            Arc::new(AcpAgentEventSink::new(output.clone())),
        );

        let bridge = Rc::new(AcpBridge {
            session_id: sid.clone(),
            output: output.clone(),
            pending: pending.clone(),
            next_id_counter: AtomicU64::new(next_id.fetch_add(1000, Ordering::SeqCst)),
            cancellation: cancellation.clone(),
            script_name: std::sync::Mutex::new(String::new()),
            assistant_state: std::sync::Mutex::new(VisibleTextState::default()),
        });
        let bridge_output = output.clone();
        let host_bridge = Rc::new(harn_vm::bridge::HostBridge::from_parts_with_writer(
            bridge.pending.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |line| {
                bridge_output.write_line(line);
                Ok(())
            }),
            bridge.next_id_counter.fetch_add(10_000, Ordering::SeqCst),
        ));
        host_bridge.set_session_id(&bridge.session_id);

        let compile_started = Instant::now();
        let (chunk, cache_hit) = match self.compile_pipeline_cached(
            &source,
            source_path.as_deref(),
            target_pipeline.as_deref(),
        ) {
            Ok(value) => value,
            Err(message) => {
                // Drop the error's "Compilation error: " prefix added inside
                // the helper — the caller used to format it identically.
                let formatted = message
                    .strip_prefix("Compilation error: ")
                    .map(|rest| format!("Compilation error: {rest}"))
                    .unwrap_or(message);
                self.send_prompt_error(&session_id, id, &formatted);
                return;
            }
        };
        let compile_ms = compile_started.elapsed().as_millis() as u64;
        bridge.send_log(
            "info",
            &format!(
                "ACP_BOOT: compile_ms={compile_ms} cache={}",
                if cache_hit { "hit" } else { "miss" }
            ),
            Some(serde_json::json!({
                "compile_ms": compile_ms,
                "compile_cache": if cache_hit { "hit" } else { "miss" },
                "pipeline": source_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<inline>".to_string()),
            })),
        );
        let profile_turn = self.begin_profile_turn(&session_id);
        let _mode_guard = modes::ModePolicyGuard::enter(&current_mode_id);
        let (vm_baseline, vm_baseline_cache_hit, vm_baseline_prepare_ms) = match self
            .prepare_vm_baseline_cached(
                &source,
                source_path.as_deref(),
                target_pipeline.as_deref(),
                &cwd,
                &current_mode_id,
            )
            .await
        {
            Ok(value) => value,
            Err(message) => {
                self.finish_profile_turn(&session_id, profile_turn);
                self.send_prompt_error(&session_id, id, &message);
                return;
            }
        };
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.host_bridge = Some(host_bridge.clone());
        }

        let id_owned = id.clone();
        let send_output = self.output.clone();
        let host_bridge_for_response = host_bridge.clone();
        let result = execute::execute_chunk(
            chunk,
            bridge.clone(),
            host_bridge,
            execute::PromptGlobals {
                text: &prompt_text,
                content: &prompt.content,
                messages: &prompt.messages,
            },
            execute::VmSetup {
                source: &source,
                baseline: vm_baseline.as_ref(),
                baseline_cache_hit: vm_baseline_cache_hit,
                baseline_prepare_ms: vm_baseline_prepare_ms,
                source_path: source_path.as_deref(),
                cwd: &cwd,
                runtime_configurator: self.runtime_configurator.clone(),
            },
        )
        .await;
        self.finish_profile_turn(&session_id, profile_turn);
        drop(_mode_guard);
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.host_bridge = None;
        }

        // Unregister so a session reusing this id can't receive stale
        // events routed to a dropped stdout lock.
        clear_session_sinks(&session_id);

        match result {
            Ok(output) => {
                if !output.is_empty() {
                    bridge.send_update(&output);
                }
                let stop_reason = if cancellation.cancelled.load(Ordering::SeqCst) {
                    "cancelled".to_string()
                } else {
                    host_bridge_for_response
                        .take_prompt_stop_reason()
                        .unwrap_or_else(|| "end_turn".to_string())
                };
                send_json_response(
                    &send_output,
                    &id_owned,
                    serde_json::json!({"stopReason": stop_reason}),
                );
            }
            Err(e) => {
                if cancellation.cancelled.load(Ordering::SeqCst) {
                    send_json_response(
                        &send_output,
                        &id_owned,
                        serde_json::json!({"stopReason": "cancelled"}),
                    );
                } else {
                    self.send_prompt_error(&sid, &id_owned, &e);
                }
            }
        }
    }

    fn handle_session_cancel(&mut self, params: &serde_json::Value) {
        mark_cancelled_session(&self.session_cancellations, params);
    }

    async fn handle_session_input(&self, params: &serde_json::Value) {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| self.sessions.keys().next().map(|s| s.as_str()));
        let Some(session_id) = session_id else {
            return;
        };
        let Some(content) = params.get("content").and_then(|v| v.as_str()) else {
            return;
        };
        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("wait_for_completion");
        if let Some(bridge) = self
            .sessions
            .get(session_id)
            .and_then(|session| session.host_bridge.clone())
        {
            bridge
                .push_queued_user_message(content.to_string(), mode)
                .await;
        }
    }

    fn handle_agent_resume(&self, params: &serde_json::Value) {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| self.sessions.keys().next().map(|s| s.as_str()));
        let Some(session_id) = session_id else {
            return;
        };
        if let Some(bridge) = self
            .sessions
            .get(session_id)
            .and_then(|session| session.host_bridge.clone())
        {
            bridge.signal_resume();
        }
    }

    fn handle_session_list(&self, id: &serde_json::Value) {
        let sessions: Vec<serde_json::Value> = self
            .sessions
            .iter()
            .map(|(sid, session)| {
                let mut item = serde_json::json!({
                    "sessionId": sid,
                    "cwd": session.cwd,
                });
                if let Some(title) = &session.info.title {
                    item["title"] = serde_json::json!(title);
                }
                if !session.info.meta.is_empty() {
                    item["_meta"] = serde_json::Value::Object(session.info.meta.clone());
                }
                item
            })
            .collect();
        self.send_response(id, serde_json::json!({"sessions": sessions}));
    }

    async fn handle_hitl_respond(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let session_cwd = params
            .get("sessionId")
            .and_then(|value| value.as_str())
            .and_then(|session_id| self.sessions.get(session_id))
            .map(|session| session.cwd.as_path());
        let fallback_cwd = self
            .sessions
            .values()
            .next()
            .map(|session| session.cwd.as_path());
        let cwd = session_cwd.or(fallback_cwd);
        let response: harn_vm::HitlHostResponse = match serde_json::from_value(params.clone()) {
            Ok(response) => response,
            Err(error) => {
                self.send_error(
                    id,
                    -32602,
                    &format!("Invalid harn.hitl.respond params: {error}"),
                );
                return;
            }
        };
        match harn_vm::append_hitl_response(cwd, response).await {
            Ok(_) => self.send_response(id, serde_json::json!({"ok": true})),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn workflow_base_dir_for<'a>(&'a self, params: &'a serde_json::Value) -> Option<&'a PathBuf> {
        params
            .get("sessionId")
            .and_then(|value| value.as_str())
            .and_then(|session_id| self.sessions.get(session_id))
            .map(|session| &session.cwd)
            .or_else(|| self.sessions.values().next().map(|session| &session.cwd))
    }

    async fn handle_workflow_signal(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/signal: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/signal: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/signal: no session cwd available");
            return;
        };
        let payload = params
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match harn_vm::workflow_signal_for_base(base_dir, workflow_id, name, payload) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_workflow_query(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/query: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/query: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/query: no session cwd available");
            return;
        };
        match harn_vm::workflow_query_for_base(base_dir, workflow_id, name) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    async fn handle_workflow_update(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/update: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/update: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/update: no session cwd available");
            return;
        };
        let payload = params
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let timeout_ms = params
            .get("timeoutMs")
            .and_then(|value| value.as_u64())
            .unwrap_or(30_000);
        match harn_vm::workflow_update_for_base(
            base_dir,
            workflow_id,
            name,
            payload,
            std::time::Duration::from_millis(timeout_ms),
        )
        .await
        {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_workflow_pause(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/pause: missing workflowId");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/pause: no session cwd available");
            return;
        };
        match harn_vm::workflow_pause_for_base(base_dir, workflow_id) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_workflow_resume(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/resume: missing workflowId");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/resume: no session cwd available");
            return;
        };
        match harn_vm::workflow_resume_for_base(base_dir, workflow_id) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn session_restore_result(&self, session_id: &str) -> Option<serde_json::Value> {
        let session = self.sessions.get(session_id)?;
        let mut session_value = serde_json::json!({
            "sessionId": session_id,
            "cwd": session.cwd.display().to_string(),
        });
        if let Some(title) = session.info.title.as_ref() {
            session_value["title"] = serde_json::json!(title);
        }
        if !session.info.meta.is_empty() {
            session_value["_meta"] = serde_json::Value::Object(session.info.meta.clone());
        }

        Some(serde_json::json!({
            "session": session_value,
            "modes": modes::session_mode_state(&session.current_mode_id),
            "configOptions": modes::config_options_state(
                &session.current_mode_id,
                self.pinned_model(session_id).as_deref(),
            ),
        }))
    }

    fn restored_session_id<'a>(
        &self,
        id: &serde_json::Value,
        params: &'a serde_json::Value,
        method: &str,
    ) -> Option<&'a str> {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, &format!("{method} requires sessionId"));
            return None;
        };

        if !self.sessions.contains_key(session_id) {
            self.send_error(id, -32602, &format!("unknown session: {session_id}"));
            return None;
        };

        harn_vm::agent_sessions::open_or_create(Some(session_id.to_string()));
        Some(session_id)
    }

    async fn handle_session_load(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = self.restored_session_id(id, params, "session/load") else {
            return;
        };

        let replay_events =
            match harn_vm::orchestration::load_agent_session_replay_events(session_id).await {
                Ok(events) => events,
                Err(error) => {
                    self.send_error(
                        id,
                        -32000,
                        &format!("Failed to replay session {session_id}: {error}"),
                    );
                    return;
                }
            };
        let replay_sink = AcpAgentEventSink::for_replay(self.output.clone());
        for replay_event in &replay_events {
            replay_sink.handle_event(&replay_event.event);
        }
        let replayed: Vec<serde_json::Value> = replay_events
            .iter()
            .map(|event| {
                serde_json::json!({
                    "eventId": event.event_id,
                    "type": serde_json::to_value(&event.event)
                        .ok()
                        .and_then(|value| value.get("type").cloned())
                        .unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();

        let mut result = self
            .session_restore_result(session_id)
            .expect("validated session should still exist");
        result["replayed"] = serde_json::json!(replayed);
        self.send_response(id, result);
    }

    fn handle_session_resume(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = self.restored_session_id(id, params, "session/resume") else {
            return;
        };
        let result = self
            .session_restore_result(session_id)
            .expect("validated session should still exist");
        self.send_response(id, result);
    }

    fn set_session_mode(&mut self, session_id: &str, mode_id: &str) -> Result<bool, String> {
        if !modes::is_known(mode_id) {
            return Err(format!(
                "Unknown mode '{mode_id}'. Available: {}",
                modes::known_mode_ids().join(", ")
            ));
        }
        let Some(session) = self.sessions.get_mut(session_id) else {
            return Err(format!("Unknown session: {session_id}"));
        };
        if session.current_mode_id == mode_id {
            return Ok(false);
        }
        session.current_mode_id = mode_id.to_string();
        Ok(true)
    }

    /// Pin (or clear, with `None`) the LLM model selector for `session_id`.
    /// Returns `Ok(true)` when the value changed so callers can decide
    /// whether to broadcast a `config_option_update` notification.
    ///
    /// The harn-vm session is auto-created if it doesn't exist yet (e.g.
    /// when a client pins a model before its first prompt), keeping
    /// the wire surface order-independent.
    fn set_session_model(
        &mut self,
        session_id: &str,
        model: Option<String>,
    ) -> Result<bool, String> {
        if !self.sessions.contains_key(session_id) {
            return Err(format!("Unknown session: {session_id}"));
        }
        if !harn_vm::agent_sessions::exists(session_id) {
            harn_vm::agent_sessions::open_or_create(Some(session_id.to_string()));
        }
        harn_vm::agent_sessions::set_pinned_model(session_id, model)
    }

    /// Read the currently pinned model for `session_id`, if any. Returns
    /// `None` for unknown sessions or sessions running on the ambient
    /// default — both are indistinguishable on the wire.
    fn pinned_model(&self, session_id: &str) -> Option<String> {
        harn_vm::agent_sessions::pinned_model(session_id)
    }

    fn emit_current_mode_update(&self, session_id: &str, mode_id: &str) {
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "current_mode_update",
                    "modeId": mode_id,
                },
            }),
        );
    }

    fn emit_config_option_update(&self, session_id: &str, mode_id: &str) {
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "config_option_update",
                    "configOptions": modes::config_options_state(
                        mode_id,
                        self.pinned_model(session_id).as_deref(),
                    ),
                },
            }),
        );
    }

    fn handle_session_set_mode(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/set_mode requires sessionId");
            return;
        };
        let Some(mode_id) = params
            .get("modeId")
            .or_else(|| params.get("mode_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/set_mode requires modeId");
            return;
        };

        match self.set_session_mode(session_id, mode_id) {
            Ok(changed) => {
                self.send_response(id, serde_json::json!({}));
                if changed {
                    self.emit_current_mode_update(session_id, mode_id);
                    self.emit_config_option_update(session_id, mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    fn handle_session_set_config_option(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = params.get("sessionId").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires sessionId");
            return;
        };
        let Some(config_id) = params.get("configId").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires configId");
            return;
        };
        let Some(value) = params.get("value").and_then(serde_json::Value::as_str) else {
            self.send_error(id, -32602, "session/set_config_option requires value");
            return;
        };

        let session_id = session_id.to_string();
        match config_id {
            "mode" => self.apply_set_mode_config_option(id, &session_id, value),
            "model" => self.apply_set_model_config_option(id, &session_id, value),
            other => self.send_error(
                id,
                -32602,
                &format!("Unknown config option '{other}'. Available: mode, model"),
            ),
        }
    }

    fn apply_set_mode_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        mode_id: &str,
    ) {
        match self.set_session_mode(session_id, mode_id) {
            Ok(changed) => {
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": modes::config_options_state(
                            mode_id,
                            self.pinned_model(session_id).as_deref(),
                        ),
                    }),
                );
                if changed {
                    self.emit_current_mode_update(session_id, mode_id);
                    self.emit_config_option_update(session_id, mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    fn apply_set_model_config_option(
        &mut self,
        id: &serde_json::Value,
        session_id: &str,
        raw_value: &str,
    ) {
        let normalized = match modes::validate_model_selector(raw_value) {
            Ok(value) => value,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        match self.set_session_model(session_id, normalized.clone()) {
            Ok(changed) => {
                let current_mode_id = self
                    .sessions
                    .get(session_id)
                    .map(|session| session.current_mode_id.clone())
                    .unwrap_or_else(|| modes::DEFAULT_MODE_ID.to_string());
                self.send_response(
                    id,
                    serde_json::json!({
                        "configOptions": modes::config_options_state(
                            &current_mode_id,
                            normalized.as_deref(),
                        ),
                    }),
                );
                if changed {
                    self.emit_config_option_update(session_id, &current_mode_id);
                }
            }
            Err(message) => self.send_error(id, -32602, &message),
        }
    }

    async fn handle_incoming_message(&mut self, msg: serde_json::Value) {
        if msg.get("method").is_none() && msg.get("id").is_some() {
            if let Some(id) = msg["id"].as_u64() {
                let mut pending = self.pending.lock().await;
                if let Some(sender) = pending.remove(&id) {
                    let _ = sender.send(msg);
                }
            }
            return;
        }

        let method = match msg.get("method").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return,
        };
        let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));

        match method.as_str() {
            "initialize" => {
                self.handle_initialize(&id);
            }
            "authenticate" => {
                self.handle_authenticate(&id, &params).await;
            }
            "session/new" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_new(&id, &params);
            }
            "session/load" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_load(&id, &params).await;
            }
            "session/resume" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_resume(&id, &params);
            }
            "session/fork" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_fork(&id, &params);
            }
            "session/truncate" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_truncate(&id, &params);
            }
            "session/set_mode" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_set_mode(&id, &params);
            }
            "session/set_config_option" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_set_config_option(&id, &params);
            }
            "session/prompt" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_prompt(&id, &params).await;
            }
            "session/cancel" => {
                self.handle_session_cancel(&params);
            }
            "session/input" | "user_message" | "agent/user_message" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_input(&params).await;
            }
            "agent/resume" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_agent_resume(&params);
            }
            "harn.hitl.respond" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_hitl_respond(&id, &params).await;
            }
            "workflow/signal" | "harn.workflow.signal" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_signal(&id, &params).await;
            }
            "workflow/query" | "harn.workflow.query" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_query(&id, &params);
            }
            "workflow/update" | "harn.workflow.update" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_update(&id, &params).await;
            }
            "workflow/pause" | "harn.workflow.pause" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_pause(&id, &params);
            }
            "workflow/resume" | "harn.workflow.resume" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_workflow_resume(&id, &params);
            }
            "session/list" => {
                if self.reject_unauthenticated(&id) {
                    return;
                }
                self.handle_session_list(&id);
            }
            _ => {
                if !id.is_null() {
                    self.send_error(&id, -32601, &format!("Method not found: {method}"));
                }
            }
        }
    }
}
