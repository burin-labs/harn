//! ACP session state and cancellation routing helpers.
use super::*;

mod budget_rearm;
mod cancellation;
pub(super) use cancellation::SessionCancellation;

#[derive(Clone)]
pub(super) struct ConcurrentSessionControl {
    pub(super) inputs: harn_vm::session_inputs::Store,
    pub(super) inject_state: harn_vm::bridge::HostBridgeInjectionState,
    pub(super) tool_call_cancellations: Arc<harn_vm::tool_call_cancellations::CancellationRegistry>,
    /// Shared with the transport router, which records `session/set_budget`
    /// re-arms here while a prompt may be running.
    pub(super) llm_spend: Arc<std::sync::Mutex<SessionLlmSpend>>,
    cancellation: SessionCancellation,
    prompt_active: Arc<AtomicBool>,
    /// The live prompt's client event transport. Controls are answered by
    /// dispatch handlers and router preemption, outside the prompt's execution
    /// scope, so they reach the turn's event stream only through this binding.
    /// Empty between turns, as the per-turn client sink always was: an idle
    /// control is answered by its JSON-RPC reply and its durable record.
    prompt_transport: Arc<std::sync::Mutex<harn_vm::agent_events::AgentEventTransport>>,
    pub(super) prompt_owner: Arc<std::sync::Mutex<Option<prompt_owner::PromptExecutionOwner>>>,
}

/// What one session's spend ceiling is measured against across its turns.
///
/// Each prompt installs a fresh cost scope, so without this every turn (and
/// every resumed process) started at $0 and a session cap was really a
/// per-prompt cap.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SessionLlmSpend {
    /// USD this session has spent. `None` until the first prompt in this
    /// process reads the persisted total.
    pub(super) spent_usd: Option<f64>,
    /// Ceilings a `session/set_budget` re-armed. They outlive the turn they
    /// arrived in.
    pub(super) llm_cost_usd: Option<CeilingRearm>,
    pub(super) llm_tokens: Option<CeilingRearm>,
    active_cost: Option<harn_vm::LlmCostBudgetHandle>,
    active_tokens: Option<harn_vm::LlmTokenBudgetHandle>,
}

/// What one `session/set_budget` field asks of its ceiling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum CeilingRearm {
    Clear,
    Set(f64),
}

impl CeilingRearm {
    fn cap(self) -> Option<f64> {
        match self {
            Self::Clear => None,
            Self::Set(cap) => Some(cap),
        }
    }

    fn token_cap(self) -> Option<u64> {
        self.cap().map(|tokens| tokens.max(0.0) as u64)
    }
}

impl SessionLlmSpend {
    /// The budget a turn runs under: the session's configured spec with any
    /// re-armed ceilings applied over it.
    pub(super) fn turn_budget(&self, configured: Option<BudgetSpec>) -> BudgetSpec {
        let mut budget = configured.unwrap_or_default();
        if let Some(cost) = self.llm_cost_usd {
            budget.llm_cost_usd = cost.cap();
        }
        if let Some(tokens) = self.llm_tokens {
            budget.llm_tokens = tokens.token_cap();
        }
        budget
    }
}

/// Writes the turn's cost-scope total back to the session when the turn's
/// future is dropped, whether it finished, failed, or was cancelled. Declare it
/// after the budget guard so it drops first. Cancellation can drop it outside
/// the task's poll, so it reads the owned ledger rather than ambient state.
pub(super) struct SessionSpendRecorder<'a> {
    session: Arc<std::sync::Mutex<SessionLlmSpend>>,
    cost: &'a harn_vm::LlmBudgetGuard,
    record_spend: bool,
}

impl<'a> SessionSpendRecorder<'a> {
    pub(super) fn new(
        session: Arc<std::sync::Mutex<SessionLlmSpend>>,
        budget: &'a crate::limits::BudgetGuard,
        record_spend: bool,
    ) -> Self {
        let cost = budget
            .llm_cost_guard()
            .expect("session turns install cost accounting");
        {
            let mut spend = session.lock().unwrap_or_else(|error| error.into_inner());
            spend.active_cost = Some(cost.handle());
            spend.active_tokens = budget.llm_token_guard().map(|guard| guard.handle());
            if let Some(ceiling) = spend.llm_cost_usd {
                cost.handle().set_ceiling(ceiling.cap());
            }
            if let (Some(ceiling), Some(tokens)) = (spend.llm_tokens, &spend.active_tokens) {
                tokens.set_ceiling(ceiling.token_cap());
            }
        }
        Self {
            session,
            cost,
            record_spend,
        }
    }
}

impl Drop for SessionSpendRecorder<'_> {
    fn drop(&mut self) {
        let spent = self.cost.total_cost();
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if session.active_cost.as_ref() == Some(&self.cost.handle()) {
            if self.record_spend {
                session.spent_usd = Some(spent);
            }
            session.active_cost = None;
            session.active_tokens = None;
        }
    }
}

impl ConcurrentSessionControl {
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self::for_session(SessionCancellation::default())
    }

    pub(super) fn for_session(cancellation: SessionCancellation) -> Self {
        Self {
            inputs: Arc::default(),
            inject_state: harn_vm::bridge::HostBridgeInjectionState::default(),
            tool_call_cancellations: Arc::new(
                harn_vm::tool_call_cancellations::CancellationRegistry::default(),
            ),
            llm_spend: Arc::default(),
            cancellation,
            prompt_active: Arc::new(AtomicBool::new(false)),
            prompt_transport: Arc::default(),
            prompt_owner: Arc::default(),
        }
    }

    pub(super) fn set_prompt_active(&self, active: bool) {
        self.prompt_active.store(active, Ordering::SeqCst);
        if !active {
            *self
                .prompt_owner
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
        }
    }

    pub(super) fn prompt_is_active(&self) -> bool {
        self.prompt_active.load(Ordering::SeqCst)
    }

    /// Bind (or, with the empty transport, release) the live turn's client
    /// event stream for controls answered outside its execution scope.
    pub(super) fn set_prompt_transport(
        &self,
        transport: harn_vm::agent_events::AgentEventTransport,
    ) {
        *self
            .prompt_transport
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = transport;
    }

    fn emit_control_event(&self, event: &harn_vm::agent_events::AgentEvent) {
        let transport = self
            .prompt_transport
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        transport.with(|| harn_vm::agent_events::emit_event(event));
    }
}

#[derive(Clone)]
pub(super) struct ConcurrentSessionControls {
    pub(super) sessions: Arc<std::sync::Mutex<HashMap<String, ConcurrentSessionControl>>>,
    pub(super) pending_prompts: super::delivery::PendingPrompts,
    pub(super) authenticated: Arc<AtomicBool>,
    auth_required_data: Arc<serde_json::Value>,
    pub(super) task_admission: Option<Arc<dyn SessionTaskAdmission>>,
    pub(super) machine_spend_quota: Option<harn_vm::llm::MachineSpendQuota>,
}

impl ConcurrentSessionControls {
    pub(super) fn new(authenticated: bool, auth_required_data: serde_json::Value) -> Self {
        Self {
            sessions: Arc::default(),
            pending_prompts: Default::default(),
            authenticated: Arc::new(AtomicBool::new(authenticated)),
            auth_required_data: Arc::new(auth_required_data),
            task_admission: None,
            machine_spend_quota: None,
        }
    }

    pub(super) fn with_task_admission(
        mut self,
        admission: Option<Arc<dyn SessionTaskAdmission>>,
    ) -> Self {
        self.task_admission = admission;
        self
    }

    pub(super) fn mark_authenticated(&self) {
        self.authenticated.store(true, Ordering::SeqCst);
    }

    pub(super) fn register(&self, session_id: &str, control: ConcurrentSessionControl) {
        harn_vm::session_inputs::bind(session_id, control.inputs.clone());
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session_id.to_string(), control);
    }

    pub(super) fn preempt_active_session_inputs(
        &self,
        message: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        if message.get("method").and_then(serde_json::Value::as_str) != Some("session/inputs") {
            return false;
        }
        let id = message
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if !self.authenticated.load(Ordering::SeqCst) {
            send_routed_error(
                output,
                &id,
                ACP_AUTH_REQUIRED_CODE,
                "Authentication is required",
            );
            return true;
        }
        let session_id = message
            .pointer("/params/sessionId")
            .and_then(serde_json::Value::as_str);
        let Some(control) = self.active_control(session_id) else {
            return false;
        };
        let result = super::session_inputs::execute(
            message.get("params").cloned().unwrap_or_default(),
            &control.inputs,
        );
        match result {
            Ok(result) => output.write_line(
                &serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result}).to_string(),
            ),
            Err(message) => send_routed_error(output, &id, -32602, &message),
        }
        true
    }

    pub(super) fn active_control(
        &self,
        session_id: Option<&str>,
    ) -> Option<ConcurrentSessionControl> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(session_id) = session_id {
            return sessions
                .get(session_id)
                .filter(|control| control.prompt_is_active())
                .cloned();
        }
        let mut active = sessions
            .values()
            .filter(|control| control.prompt_is_active());
        let control = active.next()?.clone();
        active.next().is_none().then_some(control)
    }

    /// Publish a control outcome to the session's durable subscribers and,
    /// while a turn is live, to that turn's client event stream.
    pub(super) fn emit_control_event(&self, event: &harn_vm::agent_events::AgentEvent) {
        let control = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(event.session_id())
            .cloned();
        match control {
            Some(control) => control.emit_control_event(event),
            None => harn_vm::agent_events::emit_event(event),
        }
    }

    pub(super) fn remove(&self, session_id: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session_id);
    }

    #[cfg(test)]
    fn apply_budget_rearm(&self, msg: &serde_json::Value) -> bool {
        self.try_apply_budget_rearm(msg).expect("budget rearm")
    }

    pub(super) fn preempt_active_live_client_operation(
        &self,
        msg: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        let Some(method) = msg.get("method").and_then(serde_json::Value::as_str) else {
            return false;
        };
        if !is_live_client_method(method) {
            return false;
        }
        let Some(id) = msg.get("id") else {
            return false;
        };
        if self.reject_unauthenticated(id, output) {
            return true;
        }
        let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
        let Some(session_id) = session_id_param(params) else {
            return false;
        };
        let control = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&session_id)
            .cloned();
        let Some(control) = control else {
            return false;
        };
        if !control
            .cancellation
            .can_preempt_live_control(control.prompt_is_active())
        {
            return false;
        }
        match apply_live_client_operation(method, &session_id, params) {
            Ok(operation) => write_live_client_operation(output, id, &session_id, operation),
            Err(message) => send_routed_error(output, id, -32602, &message),
        }
        true
    }

    pub(super) fn preempt_active_tool_call_cancel(
        &self,
        msg: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        if msg.get("method").and_then(serde_json::Value::as_str) != Some("session/cancel_tool_call")
        {
            return false;
        }
        let null_id = serde_json::Value::Null;
        let id = msg.get("id").unwrap_or(&null_id);
        if self.reject_unauthenticated(id, output) {
            return true;
        }
        let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
        let request = match ToolCallCancelRequest::parse(params) {
            Ok(request) => request,
            Err(message) => {
                if !id.is_null() {
                    send_routed_error(output, id, -32602, &message);
                }
                return true;
            }
        };
        let control = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&request.session_id)
            .cloned();
        let Some(control) = control else {
            return false;
        };
        if !control.prompt_is_active() {
            return false;
        }

        let result = request.cancel(&control.tool_call_cancellations);
        if !id.is_null() {
            let response = harn_vm::jsonrpc::response(id.clone(), result.into_value());
            let line = serde_json::to_string(&response)
                .expect("tool-call cancellation response must serialize");
            output.write_line(&line);
        }
        true
    }

    pub(super) fn reject_unauthenticated(
        &self,
        id: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        if self.authenticated.load(Ordering::SeqCst) {
            return false;
        }
        if !id.is_null() {
            let response = harn_vm::jsonrpc::error_response_with_data(
                id.clone(),
                ACP_AUTH_REQUIRED_CODE,
                "auth_required",
                (*self.auth_required_data).clone(),
            );
            if let Ok(line) = serde_json::to_string(&response) {
                output.write_line(&line);
            }
        }
        true
    }
}

pub(super) struct ToolCallCancelRequest {
    pub(super) session_id: String,
    pub(super) call_id: String,
    pub(super) reason: String,
    pub(super) inject_reminder: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ToolCallCancelResult {
    status: &'static str,
    call_id: String,
    tool: Option<String>,
}

impl ToolCallCancelResult {
    pub(super) fn not_found(call_id: String) -> Self {
        Self {
            status: harn_vm::tool_call_cancellations::CancelStatus::NotFound.as_str(),
            call_id,
            tool: None,
        }
    }

    pub(super) fn into_value(self) -> serde_json::Value {
        serde_json::to_value(self).expect("typed tool-call cancellation result must serialize")
    }
}

impl ToolCallCancelRequest {
    pub(super) fn parse(params: &serde_json::Value) -> Result<Self, String> {
        let session_id = session_id_param(params)
            .ok_or_else(|| "session/cancel_tool_call requires sessionId".to_string())?;
        let call_id = string_param(params, "toolCallId", "tool_call_id")
            .or_else(|| string_param(params, "callId", "call_id"))
            .filter(|call_id| !call_id.is_empty())
            .ok_or_else(|| "session/cancel_tool_call requires toolCallId".to_string())?;
        let reason = params
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("host cancelled in-flight tool call")
            .to_string();
        let inject_reminder = params
            .get("injectReminder")
            .or_else(|| params.get("inject_reminder"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        Ok(Self {
            session_id,
            call_id,
            reason,
            inject_reminder,
        })
    }

    pub(super) fn cancel(
        self,
        registry: &harn_vm::tool_call_cancellations::CancellationRegistry,
    ) -> ToolCallCancelResult {
        let outcome = registry.cancel(
            &self.session_id,
            &self.call_id,
            self.reason,
            self.inject_reminder,
        );
        ToolCallCancelResult {
            status: outcome.status.as_str(),
            call_id: self.call_id,
            tool: outcome.tool_name,
        }
    }
}

impl Default for ConcurrentSessionControls {
    fn default() -> Self {
        Self::new(true, serde_json::json!({ "authMethods": [] }))
    }
}

pub(super) fn send_routed_error(
    output: &AcpOutput,
    id: &serde_json::Value,
    code: i64,
    message: &str,
) {
    let response = harn_vm::jsonrpc::error_response(id.clone(), code, message);
    if let Ok(line) = serde_json::to_string(&response) {
        output.write_line(&line);
    }
}

#[derive(Clone, Default)]
pub(super) struct SessionInfo {
    pub(super) title: Option<String>,
    pub(super) meta: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum SessionBudget {
    #[default]
    Inherit,
    Unlimited,
    Custom(BudgetSpec),
}

#[derive(Default)]
pub(super) struct PromptCancellation {
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) notify: Arc<Notify>,
}

/// State owned by one admitted request, never shared acknowledgement state.
#[derive(Default)]
pub(super) enum PreparedSessionRequest {
    #[default]
    Unprepared,
    Prompt(Arc<PromptCancellation>),
    Close {
        _admission: Arc<()>,
    },
    Cancel {
        result: Result<bool, TaskAdmissionError>,
    },
    /// Start the turns a session's pending delivery prompts are owed, once
    /// the dispatcher reaches this point. Never decoded from the wire.
    DrainPendingPrompts {
        session_id: String,
    },
}

pub(super) struct Session {
    pub(super) cwd: PathBuf,
    pub(super) project_root: PathBuf,
    pub(super) store_scope: harn_vm::SessionStoreScope,
    /// Whether admitted prompts capture workspace boundaries for this session.
    pub(super) workspace_checkpoint: bool,
    /// Caller identities already reserved for this checkpoint lineage.
    pub(super) checkpoint_prompt_ids: std::collections::HashSet<String>,
    /// Failed automatic captures, so an older boundary with the same ID can't
    /// be mistaken for the current prompt's snapshot.
    pub(super) checkpoint_capture_failures: std::collections::HashMap<String, String>,
    /// If a cancel was requested for the current prompt execution.
    pub(super) cancellation: SessionCancellation,
    /// Host bridge for the active prompt, if one is running.
    pub(super) host_bridge: Option<Arc<harn_vm::bridge::HostBridge>>,
    /// Pending user-message inject state that survives prompt cancellation
    /// and active bridge replacement until delivery or explicit revoke.
    pub(super) inject_state: harn_vm::bridge::HostBridgeInjectionState,
    /// Shared state used by the transport router to steer an in-flight prompt.
    pub(super) concurrent_control: ConcurrentSessionControl,
    pub(super) info: SessionInfo,
    /// Snapshot of slash-commands most recently advertised over
    /// `available_commands_update` for this session, used to skip re-emits
    /// when the underlying pipeline source hasn't changed.
    pub(super) advertised_commands: Vec<DiscoveredCommand>,
    /// Active session mode id (one of [`modes::MODE_CATALOG`]). Drives
    /// the capability ceiling pushed for the next `session/prompt`.
    pub(super) current_mode_id: String,
    /// Session-level budget override applied to subsequent prompt turns.
    pub(super) budget: SessionBudget,
    /// Live execution allowance. Forks share it and later prompt turns retain it.
    pub(super) admission: Option<harn_vm::llm::ConservativeLlmBudget>,
    /// A cold restore or a prompt without admission cannot later acquire a
    /// fresh allowance that would omit earlier provider work.
    pub(super) admission_unavailable: bool,
    /// Prompt executions emitted to profile output for this ACP session.
    pub(super) profile_turn: u64,
    /// The environment policy this session launched under, resolved once at
    /// `session/new` and reused for every prompt turn.
    pub(super) environment_policy: harn_vm::security::SessionEnvironment,
    /// The root the runtime configurator was asked to warm for this session.
    /// It is released on close or re-anchor, even if `project_root` moved.
    pub(super) workspace_warm_root: Option<PathBuf>,
}

pub(super) fn session_project_root_for_cwd(cwd: &Path) -> PathBuf {
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    harn_vm::stdlib::process::find_project_root(&cwd).unwrap_or(cwd)
}

/// Why a persisted-session request cannot be scoped to one project.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AcpSessionProjectRootError {
    Missing,
    Invalid { cwd: String, detail: String },
}

impl std::fmt::Display for AcpSessionProjectRootError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => formatter.write_str("a cwd is required to select a project store"),
            Self::Invalid { cwd, detail } => {
                write!(
                    formatter,
                    "cwd `{cwd}` does not select a project directory: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for AcpSessionProjectRootError {}

/// Resolve the execution and capability root for a declared workspace.
///
/// The caller must name an existing directory; Harn resolves its nearest
/// manifest. Persistent sessions bind the selected cwd separately.
pub fn resolve_acp_session_project_root(
    cwd: Option<&str>,
) -> Result<PathBuf, AcpSessionProjectRootError> {
    let canonical = resolve_acp_session_cwd(cwd)?;
    Ok(harn_vm::stdlib::process::find_project_root(&canonical).unwrap_or(canonical))
}

pub(super) fn resolve_acp_session_cwd(
    cwd: Option<&str>,
) -> Result<PathBuf, AcpSessionProjectRootError> {
    let cwd = cwd
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .ok_or(AcpSessionProjectRootError::Missing)?;
    let canonical =
        std::fs::canonicalize(cwd).map_err(|error| AcpSessionProjectRootError::Invalid {
            cwd: cwd.to_string(),
            detail: error.to_string(),
        })?;
    if !canonical.is_dir() {
        return Err(AcpSessionProjectRootError::Invalid {
            cwd: cwd.to_string(),
            detail: "path is not a directory".to_string(),
        });
    }
    Ok(canonical)
}

/// Project one canonical store row into ACP's persisted-session shape.
pub fn acp_persisted_session_item(session: harn_session_store::SessionMeta) -> serde_json::Value {
    let mut item = serde_json::json!({
        "sessionId": session.id,
        "liveState": "persisted",
        "activePrompt": false,
        "attachableRoles": [],
        "createdAt": session.created_at,
        "updatedAt": session.updated_at,
        "eventCount": session.event_count,
        "usage": {
            "inputTokens": session.usage_input,
            "outputTokens": session.usage_output,
            "costUsdMicros": session.usage_cost_usd_micros,
        },
        "_meta": {
            "harn": {
                "liveState": "persisted",
                "activePrompt": false,
                "eventCount": session.event_count,
                "sessionType": session.session_type,
                "parentSessionId": session.parent_session_id,
                "projectScope": session.project_scope,
            }
        }
    });
    for (key, value) in [
        ("title", session.title.map(serde_json::Value::String)),
        ("cwd", session.cwd.map(serde_json::Value::String)),
        ("model", session.model.map(serde_json::Value::String)),
        (
            "lastEventId",
            session.last_event_id.map(serde_json::Value::from),
        ),
    ] {
        if let Some(value) = value {
            item[key] = value;
        }
    }
    item
}

/// Land a stop on a registered session.
///
/// `None` means the cancel named no session this server has registered,
/// so nothing was cancelled. `Some(true)` means this call is what
/// stopped the run; `Some(false)` means it was already stopped. The
/// caller needs the distinction to record an accurate control status,
/// and a bare `false` used to conflate "already stopped" with "missed".
pub(super) fn mark_cancelled_session(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    params: &serde_json::Value,
) -> Option<bool> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|value| value.as_str())?;
    let cancellation = lookup_session_cancellation(cancellations, session_id)?;
    let newly_cancelled = cancellation.cancel();
    if newly_cancelled {
        cancel_session_command_handles(session_id);
    }
    Some(newly_cancelled)
}

/// Terminate the long-running command handles this session owns, after a cancel
/// has been accepted for it.
///
/// The long-running command tool lets a backgrounded child outlive the tool
/// call that started it, which is the point of a background handle — but it also
/// means unwinding the agent loop does not reach those children. Foreground
/// children die with the dispatch future through the cancel-on-drop path;
/// backgrounded ones are exempt from it, so without this a stop leaves the
/// session's own processes running and their survival depends on the model
/// choosing to call the kill tool.
///
/// Scoped to handles whose `session_id` matches: one session's stop must never
/// reach into a sibling's children.
pub(super) fn cancel_session_command_handles(session_id: &str) {
    #[cfg(feature = "hostlib")]
    harn_hostlib::tools::long_running::cancel_session_handles(session_id);
    #[cfg(not(feature = "hostlib"))]
    let _ = session_id;
}

pub(super) fn lookup_session_cancellation(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    session_id: &str,
) -> Option<SessionCancellation> {
    cancellations
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(session_id)
        .cloned()
}

pub(super) fn preempt_session_interruption(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    controls: &ConcurrentSessionControls,
    msg: &serde_json::Value,
) -> bool {
    if !controls.authenticated.load(Ordering::SeqCst) {
        return false;
    }
    let method = msg.get("method").and_then(|value| value.as_str());
    let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
    match method {
        Some("session/cancel") => {
            if msg.get("id").is_some() {
                false
            } else {
                // Consume the frame ONLY when the cancel actually landed on a
                // registered session. A cancel naming an unknown or missing
                // sessionId used to be swallowed here regardless, which made a
                // miss indistinguishable on the wire from a working stop: the
                // agent loop ran on and nothing was logged, refused, or
                // answered. Falling through on a miss hands the frame to the
                // normal dispatch, which records the miss as a rejected
                // control outcome instead of dropping it.
                let Some(result) =
                    prompt_owner::cancel_session_prompt(cancellations, controls, params)
                else {
                    return false;
                };
                let newly_cancelled = match result {
                    Ok(cancelled) => cancelled,
                    Err(error) => {
                        if let Some(session_id) = session_id_param(params) {
                            prompt_owner::record_cancel_refusal(
                                controls,
                                &session_id,
                                params,
                                error,
                            );
                        }
                        return true;
                    }
                };
                // A stop accepted here never reaches the dispatch
                // handler, so this is the only place the mid-turn stop
                // can become a typed control event.
                if let Some(session_id) = params
                    .get("sessionId")
                    .or_else(|| params.get("session_id"))
                    .and_then(|value| value.as_str())
                {
                    super::core::record_and_emit_control(
                        controls,
                        session_id,
                        harn_session_store::ControlEvent::stop(
                            "session/cancel",
                            control_id(),
                            if newly_cancelled {
                                "cancelled"
                            } else {
                                "already_cancelled"
                            },
                        )
                        .with_actor(control_actor_from_params(params)),
                    );
                }
                true
            }
        }
        Some("session/truncate" | "session/close" | "session/stop") => {
            mark_cancelled_session(cancellations, params);
            false
        }
        _ => false,
    }
}

/// One `session/set_budget` control frame.
///
/// Shape: `params = { sessionId?, llm_cost_usd?: number|null, llm_tokens?:
/// number|null }`. An absent or malformed field leaves that ceiling unchanged
/// rather than guessing; an explicit `null` clears the cap; a finite number
/// re-arms it.
///
/// The router updates the named session's owned ledgers out-of-band, so a
/// suspended turn observes the new cap on its next LLM dispatch. The live
/// re-arm preserves accumulated spend. Frames without a session ID retain
/// the legacy ambient-budget behavior.
struct BudgetRearm {
    llm_cost_usd: Option<CeilingRearm>,
    llm_tokens: Option<CeilingRearm>,
}

impl BudgetRearm {
    fn parse(msg: &serde_json::Value) -> Option<Self> {
        if msg.get("method").and_then(|value| value.as_str()) != Some("session/set_budget") {
            return None;
        }
        let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
        Some(Self {
            llm_cost_usd: rearm_dimension(params.get("llm_cost_usd")),
            llm_tokens: rearm_dimension(params.get("llm_tokens")),
        })
    }

    fn apply_live(&self) {
        if let Some(cost) = self.llm_cost_usd {
            harn_vm::set_llm_cost_budget(cost.cap());
        }
        if let Some(tokens) = self.llm_tokens {
            harn_vm::set_llm_token_budget(tokens.token_cap());
        }
    }
}

/// `None` leaves the dimension untouched.
fn rearm_dimension(value: Option<&serde_json::Value>) -> Option<CeilingRearm> {
    match value {
        Some(serde_json::Value::Null) => Some(CeilingRearm::Clear),
        Some(serde_json::Value::Number(number)) => number
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(CeilingRearm::Set),
        _ => None,
    }
}

pub(super) fn prepare_session_request(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    controls: &ConcurrentSessionControls,
    msg: &serde_json::Value,
) -> PreparedSessionRequest {
    if !controls.authenticated.load(Ordering::SeqCst) {
        return PreparedSessionRequest::Unprepared;
    }
    if msg.get("method").and_then(|value| value.as_str()) == Some("session/cancel")
        && msg.get("id").is_some()
    {
        return prompt_owner::cancel_session_prompt(
            cancellations,
            controls,
            msg.get("params").unwrap_or(&serde_json::Value::Null),
        )
        .map(|result| PreparedSessionRequest::Cancel { result })
        .unwrap_or_default();
    }
    let Some(session_id) = msg
        .get("params")
        .and_then(|params| params.get("sessionId"))
        .and_then(|value| value.as_str())
    else {
        return PreparedSessionRequest::Unprepared;
    };
    let Some(cancellation) = lookup_session_cancellation(cancellations, session_id) else {
        return PreparedSessionRequest::Unprepared;
    };
    match msg.get("method").and_then(|value| value.as_str()) {
        Some("session/prompt") => PreparedSessionRequest::Prompt(cancellation.prepare_prompt()),
        Some("session/close" | "session/stop") => PreparedSessionRequest::Close {
            _admission: cancellation.prepare_close(),
        },
        _ => PreparedSessionRequest::Unprepared,
    }
}

#[cfg(test)]
mod budget_rearm_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(flavor = "current_thread")]
    async fn named_rearm_updates_suspended_turn_without_touching_the_caller() {
        use std::future::Future;
        use std::task::{Context, Waker};

        let controls = ConcurrentSessionControls::default();
        let control = ConcurrentSessionControl::new();
        controls.register("target", control.clone());
        let _caller_cost = harn_vm::install_llm_cost_budget_seeded(Some(3.0), 0.5);
        let _caller_tokens = harn_vm::install_llm_token_budget(30);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut turn = Box::pin(harn_vm::orchestration::scope_ambient_context(async {
            // Even an initially uncapped turn must own token accounting for
            // a later out-of-band ceiling to reach that turn.
            let guard = BudgetSpec::default().install_session_turn(0.25);
            let _recorder = SessionSpendRecorder::new(control.llm_spend.clone(), &guard, true);
            rx.await.unwrap();
            assert_eq!(harn_vm::peek_llm_cost_budget(), Some(1.0));
            assert_eq!(harn_vm::peek_llm_token_budget(), Some(10));
            assert_eq!(harn_vm::llm::peek_total_cost(), 0.25);
        }));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(turn.as_mut().poll(&mut cx).is_pending());
        assert!(controls.apply_budget_rearm(&set_budget_frame(json!({
            "sessionId": "missing", "llm_cost_usd": 9.0, "llm_tokens": 90
        }))));
        for invalid_id in [json!(null), json!(42), json!({"id": "target"})] {
            assert!(controls.apply_budget_rearm(&set_budget_frame(json!({
                "sessionId": invalid_id, "llm_cost_usd": 9.0, "llm_tokens": 90
            }))));
        }
        assert!(controls.apply_budget_rearm(&set_budget_frame(json!({
            "sessionId": "target", "llm_cost_usd": 1.0, "llm_tokens": 10
        }))));
        assert_eq!(harn_vm::peek_llm_cost_budget(), Some(3.0));
        assert_eq!(harn_vm::peek_llm_token_budget(), Some(30));
        assert_eq!(harn_vm::llm::peek_total_cost(), 0.5);
        tx.send(()).unwrap();
        assert!(turn.as_mut().poll(&mut cx).is_ready());
        let spend = control.llm_spend.lock().unwrap();
        assert_eq!(spend.spent_usd, Some(0.25));
        assert!(spend.active_cost.is_none());
        assert!(spend.active_tokens.is_none());
        assert_eq!(harn_vm::peek_llm_cost_budget(), Some(3.0));
        assert_eq!(harn_vm::peek_llm_token_budget(), Some(30));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_prompt_records_its_own_spend_outside_the_poll_scope() {
        use std::future::Future;
        use std::task::{Context, Waker};

        let session = Arc::new(std::sync::Mutex::new(SessionLlmSpend::default()));
        let _caller = harn_vm::install_llm_cost_budget_seeded(Some(3.0), 0.5);
        let mut turn = Box::pin(harn_vm::orchestration::scope_ambient_context(async {
            let budget = BudgetSpec {
                llm_cost_usd: Some(1.0),
                ..Default::default()
            };
            let guard = budget.install_session_turn(0.25);
            let _recorder = SessionSpendRecorder::new(session.clone(), &guard, true);
            std::future::pending::<()>().await;
        }));
        assert!(turn
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending());
        assert_eq!(harn_vm::llm::peek_total_cost(), 0.5);
        drop(turn);
        assert_eq!(session.lock().unwrap().spent_usd, Some(0.25));
        assert_eq!(harn_vm::llm::peek_total_cost(), 0.5);
        assert_eq!(harn_vm::peek_llm_cost_budget(), Some(3.0));
    }

    fn apply_session_budget_rearm(msg: &serde_json::Value) -> bool {
        ConcurrentSessionControls::new(true, serde_json::Value::Null).apply_budget_rearm(msg)
    }

    fn set_budget_frame(params: serde_json::Value) -> serde_json::Value {
        json!({ "jsonrpc": "2.0", "method": "session/set_budget", "params": params })
    }

    #[test]
    fn rearms_cost_and_token_ceilings_and_clears_with_null() {
        assert!(apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": 1.5, "llm_tokens": 50_000 })
        )));
        assert_eq!(harn_vm::peek_llm_cost_budget(), Some(1.5));
        assert_eq!(harn_vm::peek_llm_token_budget(), Some(50_000));

        // An explicit null clears the cap on that dimension.
        assert!(apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": null, "llm_tokens": null })
        )));
        assert_eq!(harn_vm::peek_llm_cost_budget(), None);
        assert_eq!(harn_vm::peek_llm_token_budget(), None);
    }

    #[test]
    fn absent_field_leaves_that_dimension_untouched() {
        apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": 2.0, "llm_tokens": 100 }),
        ));
        // Re-arm cost only; the token ceiling must survive.
        apply_session_budget_rearm(&set_budget_frame(json!({ "llm_cost_usd": 3.0 })));
        assert_eq!(harn_vm::peek_llm_cost_budget(), Some(3.0));
        assert_eq!(harn_vm::peek_llm_token_budget(), Some(100));
        // Leave the thread-local clean for any test reusing this worker thread.
        apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": null, "llm_tokens": null }),
        ));
    }

    #[test]
    fn ignores_non_budget_frames_and_malformed_values() {
        harn_vm::set_llm_cost_budget(Some(5.0));
        // A malformed (non-number, non-null) value leaves the ceiling untouched.
        assert!(apply_session_budget_rearm(&set_budget_frame(
            json!({ "llm_cost_usd": "lots" })
        )));
        assert_eq!(harn_vm::peek_llm_cost_budget(), Some(5.0));
        // A different method is not our control frame.
        assert!(!apply_session_budget_rearm(&json!({
            "method": "session/prompt", "params": {}
        })));
        harn_vm::set_llm_cost_budget(None);
    }

    #[test]
    fn active_tool_call_cancel_is_handled_on_the_preemptive_control_lane() {
        let controls = ConcurrentSessionControls::default();
        let control = ConcurrentSessionControl::new();
        control.set_prompt_active(true);
        let (handle, _guard) =
            control
                .tool_call_cancellations
                .register("session-1", "call-1", "shell");
        controls.register("session-1", control);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let output = AcpOutput::Channel(tx);

        assert!(controls.preempt_active_tool_call_cancel(
            &json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "session/cancel_tool_call",
                "params": {
                    "sessionId": "session-1",
                    "toolCallId": "call-1",
                    "reason": "host stop",
                    "injectReminder": false,
                },
            }),
            &output,
        ));

        assert!(handle.is_cancelled());
        assert_eq!(handle.reason().as_deref(), Some("host stop"));
        let response: serde_json::Value =
            serde_json::from_str(&rx.try_recv().expect("cancellation response")).expect("json");
        assert_eq!(response["result"]["status"], "cancelled");
        assert_eq!(response["result"]["tool"], "shell");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_prompt_takeover_is_handled_on_the_preemptive_control_lane() {
        let session_id = format!("session-{}", uuid::Uuid::now_v7());
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()))
            .expect("open fixture session");
        harn_vm::agent_sessions::attach_live_client(
            &session_id,
            harn_vm::agent_sessions::AttachLiveClient {
                client_id: "desktop".to_string(),
                mode: harn_vm::agent_sessions::LiveClientMode::Controller,
                takeover: false,
                prompt_injection: true,
                permission_routing: true,
                metadata: json!({"surface": "desktop"}),
            },
        )
        .expect("desktop attach");
        harn_vm::agent_sessions::attach_live_client(
            &session_id,
            harn_vm::agent_sessions::AttachLiveClient {
                client_id: "mobile".to_string(),
                mode: harn_vm::agent_sessions::LiveClientMode::Observer,
                takeover: false,
                prompt_injection: false,
                permission_routing: false,
                metadata: json!({"surface": "mobile-web"}),
            },
        )
        .expect("mobile attach");

        let controls = ConcurrentSessionControls::default();
        let control = ConcurrentSessionControl::new();
        let _prompt = control.cancellation.prepare_prompt();
        control.set_prompt_active(true);
        controls.register(&session_id, control);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let output = AcpOutput::Channel(tx);

        assert!(controls.preempt_active_live_client_operation(
            &json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "session/takeover",
                "params": {"sessionId": session_id, "clientId": "mobile"},
            }),
            &output,
        ));
        let notification: serde_json::Value =
            serde_json::from_str(&rx.recv().await.expect("takeover notification")).expect("json");
        let response: serde_json::Value =
            serde_json::from_str(&rx.recv().await.expect("takeover response")).expect("json");
        assert_eq!(
            notification["params"]["update"]["_meta"]["harn"]["action"],
            "takeover"
        );
        assert_eq!(response["result"]["previous_controller_id"], "desktop");
        assert_eq!(response["result"]["active_controller_id"], "mobile");
        harn_vm::agent_sessions::close(&session_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preemptive_control_lane_rejects_unauthenticated_takeover() {
        let session_id = format!("session-{}", uuid::Uuid::now_v7());
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()))
            .expect("open fixture session");
        for (client_id, mode) in [
            (
                "desktop",
                harn_vm::agent_sessions::LiveClientMode::Controller,
            ),
            ("mobile", harn_vm::agent_sessions::LiveClientMode::Observer),
        ] {
            let controller = mode == harn_vm::agent_sessions::LiveClientMode::Controller;
            harn_vm::agent_sessions::attach_live_client(
                &session_id,
                harn_vm::agent_sessions::AttachLiveClient {
                    client_id: client_id.to_string(),
                    mode,
                    takeover: false,
                    prompt_injection: controller,
                    permission_routing: controller,
                    metadata: json!({}),
                },
            )
            .expect("client attach");
        }

        let controls =
            ConcurrentSessionControls::new(false, json!({"authMethods": [{"id": "api-key"}]}));
        let control = ConcurrentSessionControl::new();
        control.set_prompt_active(true);
        controls.register(&session_id, control);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let output = AcpOutput::Channel(tx);

        assert!(controls.preempt_active_live_client_operation(
            &json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "session/takeover",
                "params": {"sessionId": session_id, "clientId": "mobile"},
            }),
            &output,
        ));
        let response: serde_json::Value =
            serde_json::from_str(&rx.recv().await.expect("auth response")).expect("json");
        assert_eq!(response["error"]["code"], ACP_AUTH_REQUIRED_CODE);
        assert_eq!(response["error"]["message"], "auth_required");
        assert!(
            rx.try_recv().is_err(),
            "takeover must not emit a notification"
        );
        let clients = harn_vm::agent_sessions::live_clients(&session_id).expect("session");
        assert_eq!(
            clients
                .iter()
                .find(|client| client.mode == harn_vm::agent_sessions::LiveClientMode::Controller)
                .map(|client| client.client_id.as_str()),
            Some("desktop")
        );
        harn_vm::agent_sessions::close(&session_id);
    }
}
