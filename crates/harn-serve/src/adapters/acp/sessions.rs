//! ACP session state and cancellation routing helpers.
use super::*;

#[derive(Clone)]
pub(super) struct ConcurrentSessionControl {
    pub(super) inject_state: harn_vm::bridge::HostBridgeInjectionState,
    pub(super) tool_call_cancellations: Arc<harn_vm::tool_call_cancellations::CancellationRegistry>,
    prompt_active: Arc<AtomicBool>,
}

impl ConcurrentSessionControl {
    pub(super) fn new() -> Self {
        Self {
            inject_state: harn_vm::bridge::HostBridgeInjectionState::default(),
            tool_call_cancellations: Arc::new(
                harn_vm::tool_call_cancellations::CancellationRegistry::default(),
            ),
            prompt_active: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn set_prompt_active(&self, active: bool) {
        self.prompt_active.store(active, Ordering::SeqCst);
    }

    pub(super) fn prompt_is_active(&self) -> bool {
        self.prompt_active.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
pub(super) struct ConcurrentSessionControls {
    sessions: Arc<std::sync::Mutex<HashMap<String, ConcurrentSessionControl>>>,
    inject_owners: Arc<std::sync::Mutex<HashMap<String, BTreeMap<String, InjectControlRecord>>>>,
    authenticated: Arc<AtomicBool>,
    auth_required_data: Arc<serde_json::Value>,
}

impl ConcurrentSessionControls {
    pub(super) fn new(authenticated: bool, auth_required_data: serde_json::Value) -> Self {
        Self {
            sessions: Arc::default(),
            inject_owners: Arc::default(),
            authenticated: Arc::new(AtomicBool::new(authenticated)),
            auth_required_data: Arc::new(auth_required_data),
        }
    }

    pub(super) fn mark_authenticated(&self) {
        self.authenticated.store(true, Ordering::SeqCst);
    }

    pub(super) fn register(&self, session_id: &str, control: ConcurrentSessionControl) {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(session_id.to_string(), control);
    }

    pub(super) fn remove(&self, session_id: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session_id);
        self.inject_owners
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session_id);
    }

    pub(super) fn inject_owner(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Option<serde_json::Value> {
        self.inject_owners
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(session_id)
            .and_then(|records| records.get(message_id))
            .map(|record| record.owner.clone())
    }

    pub(super) fn record_inject_owner(
        &self,
        session_id: &str,
        message_id: String,
        actor: serde_json::Value,
    ) {
        self.inject_owners
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(session_id.to_string())
            .or_default()
            .insert(message_id, InjectControlRecord { owner: actor });
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
        let active = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&session_id)
            .is_some_and(ConcurrentSessionControl::prompt_is_active);
        if !active {
            return false;
        }
        match apply_live_client_operation(method, &session_id, params) {
            Ok(operation) => write_live_client_operation(output, id, &session_id, operation),
            Err(message) => send_routed_error(output, id, -32602, &message),
        }
        true
    }

    pub(super) async fn preempt_active_prompt_inject(
        &self,
        msg: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        if msg.get("method").and_then(serde_json::Value::as_str) != Some("session/inject") {
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
            send_routed_error(output, id, -32602, "session/inject requires sessionId");
            return true;
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
        if !control.prompt_is_active() {
            return false;
        }

        let actor = control_actor_from_params(params);
        let mode = match bridge_mode_for_session_inject(params) {
            Ok(mode) => mode,
            Err(message) => {
                send_routed_error(output, id, -32602, &message);
                emit_routed_control_outcome(
                    &session_id,
                    "rejected",
                    actor,
                    serde_json::json!({"sessionId": session_id}),
                    Some("invalid_mode"),
                );
                return true;
            }
        };
        let (content, transcript_content) =
            match normalize_session_inject_content("session/inject", params) {
                Ok(content) => content,
                Err(message) => {
                    send_routed_error(output, id, -32602, &message);
                    emit_routed_control_outcome(
                        &session_id,
                        "rejected",
                        actor,
                        serde_json::json!({"sessionId": session_id}),
                        Some("invalid_content"),
                    );
                    return true;
                }
            };
        let message_id = control
            .inject_state
            .push_pending_user_message(content, transcript_content, mode)
            .await;
        self.record_inject_owner(&session_id, message_id.clone(), actor.clone());
        emit_routed_control_outcome(
            &session_id,
            "accepted",
            actor.clone(),
            serde_json::json!({"sessionId": session_id, "messageId": message_id}),
            None,
        );
        let response = harn_vm::jsonrpc::response(
            id.clone(),
            serde_json::json!({
                "messageId": message_id,
                "status": "accepted",
                "_meta": { "harn": { "actor": actor } }
            }),
        );
        if let Ok(line) = serde_json::to_string(&response) {
            output.write_line(&line);
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

    fn reject_unauthenticated(&self, id: &serde_json::Value, output: &AcpOutput) -> bool {
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

fn send_routed_error(output: &AcpOutput, id: &serde_json::Value, code: i64, message: &str) {
    let response = harn_vm::jsonrpc::error_response(id.clone(), code, message);
    if let Ok(line) = serde_json::to_string(&response) {
        output.write_line(&line);
    }
}

fn emit_routed_control_outcome(
    session_id: &str,
    outcome: &str,
    actor: serde_json::Value,
    target: serde_json::Value,
    reason: Option<&str>,
) {
    harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::ControlOutcome {
        session_id: session_id.to_string(),
        control_id: control_id(),
        method: "session/inject".to_string(),
        outcome: outcome.to_string(),
        status: if outcome == "accepted" {
            "accepted".to_string()
        } else {
            "rejected".to_string()
        },
        actor,
        target,
        reason: reason.map(str::to_string),
        metadata: serde_json::Value::Null,
    });
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

#[derive(Clone)]
pub(super) struct SessionCancellation {
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) notify: Arc<Notify>,
    routed_cancel_ack_pending: Arc<AtomicBool>,
    /// Set by the transport reader after it resets cancellation for a
    /// prompt, so the prompt handler does not erase a cancel notification
    /// that arrived while the prompt was queued.
    prepared_prompt: Arc<AtomicBool>,
}

impl Default for SessionCancellation {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
            routed_cancel_ack_pending: Arc::new(AtomicBool::new(false)),
            prepared_prompt: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl SessionCancellation {
    pub(super) fn cancel(&self) -> bool {
        let already_cancelled = self.cancelled.swap(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        !already_cancelled
    }

    pub(super) fn cancel_for_routed_request(&self) {
        if self.cancel() {
            self.routed_cancel_ack_pending.store(true, Ordering::SeqCst);
        }
    }

    pub(super) fn take_routed_cancel_ack(&self) -> bool {
        self.routed_cancel_ack_pending.swap(false, Ordering::SeqCst)
    }

    pub(super) fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
        self.routed_cancel_ack_pending
            .store(false, Ordering::SeqCst);
    }

    pub(super) fn prepare_prompt(&self) {
        self.reset();
        self.prepared_prompt.store(true, Ordering::SeqCst);
    }

    pub(super) fn begin_prompt(&self) {
        if !self.prepared_prompt.swap(false, Ordering::SeqCst) {
            self.reset();
        }
    }
}

pub(super) struct Session {
    pub(super) cwd: PathBuf,
    pub(super) project_root: PathBuf,
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
    /// Prompt executions emitted to profile output for this ACP session.
    pub(super) profile_turn: u64,
    /// The environment policy this session launched under, resolved once at
    /// `session/new` and reused for every prompt turn.
    pub(super) environment_policy: harn_vm::security::SessionEnvironment,
}

pub(super) fn session_project_root_for_cwd(cwd: &Path) -> PathBuf {
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    harn_vm::stdlib::process::find_project_root(&cwd).unwrap_or(cwd)
}

/// Why a persisted-session request cannot be scoped to one project.
#[derive(Clone, Debug, Eq, PartialEq)]
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

/// Resolve the only project store a persisted-session request may inspect.
///
/// Cold session lookup never searches sibling projects or falls back to the
/// listener's process directory. The caller must name an existing directory;
/// Harn then resolves it to its nearest project root.
pub fn resolve_acp_session_project_root(
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
    Ok(harn_vm::stdlib::process::find_project_root(&canonical).unwrap_or(canonical))
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

pub(super) fn mark_cancelled_session(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    params: &serde_json::Value,
) -> bool {
    let Some(session_id) = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|value| value.as_str())
    else {
        return false;
    };
    let Some(cancellation) = lookup_session_cancellation(cancellations, session_id) else {
        return false;
    };
    cancellation.cancel();
    true
}

pub(super) fn mark_cancelled_session_for_routed_request(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    params: &serde_json::Value,
) -> bool {
    let Some(session_id) = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|value| value.as_str())
    else {
        return false;
    };
    let Some(cancellation) = lookup_session_cancellation(cancellations, session_id) else {
        return false;
    };
    cancellation.cancel_for_routed_request();
    true
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
    msg: &serde_json::Value,
) -> bool {
    let method = msg.get("method").and_then(|value| value.as_str());
    let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
    match method {
        Some("session/cancel") => {
            if msg.get("id").is_some() {
                mark_cancelled_session_for_routed_request(cancellations, params);
                false
            } else {
                mark_cancelled_session(cancellations, params);
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

/// Re-arm the live LLM `call_budget` ceilings on the engine thread out-of-band,
/// so a prompt turn already in flight observes the new cap on its next LLM
/// dispatch (burin-labs/burin-code#1561). Returns `true` when `msg` was a
/// `session/set_budget` control frame (so the router drops it instead of
/// queueing it behind the active turn).
///
/// This runs on the same router task / engine thread that drives the prompt
/// turn. The cost/token ceilings are per-thread thread-locals
/// (`harn_vm::set_llm_*_budget`), which is exactly why this reaches an in-flight
/// turn: the blocked message loop would only process the frame after the turn
/// unwinds. It mirrors how `session/cancel` preempts a running turn from the
/// router.
///
/// Shape: `params = { llm_cost_usd?: number|null, llm_tokens?: number|null }`.
/// An absent field leaves that ceiling unchanged; an explicit `null` clears the
/// cap; a number re-arms it. The control re-arms the live ceiling **in place**,
/// preserving accumulated spend — it deliberately does not touch
/// `session.budget` (the per-turn `@budget` source), which would reset
/// accumulation at the next turn.
pub(super) fn apply_session_budget_rearm(msg: &serde_json::Value) -> bool {
    if msg.get("method").and_then(|value| value.as_str()) != Some("session/set_budget") {
        return false;
    }
    let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
    rearm_dimension(params.get("llm_cost_usd"), harn_vm::set_llm_cost_budget);
    rearm_dimension(params.get("llm_tokens"), |cap| {
        harn_vm::set_llm_token_budget(cap.map(|tokens| tokens.max(0.0) as u64));
    });
    true
}

/// Re-arm one `session/set_budget` ceiling dimension. An absent field
/// (`None`) or a malformed value leaves the live ceiling untouched rather than
/// guessing; an explicit JSON `null` clears the cap (`set(None)`); a finite
/// number re-arms it (`set(Some(cap))`). The `set` closure adapts the `f64`
/// ceiling to the dimension's thread-local setter.
fn rearm_dimension(value: Option<&serde_json::Value>, set: impl FnOnce(Option<f64>)) {
    match value {
        Some(serde_json::Value::Null) => set(None),
        Some(serde_json::Value::Number(number)) => {
            if let Some(cap) = number.as_f64().filter(|n| n.is_finite()) {
                set(Some(cap));
            }
        }
        _ => {}
    }
}

pub(super) fn prepare_session_prompt(
    cancellations: &Arc<std::sync::Mutex<HashMap<String, SessionCancellation>>>,
    msg: &serde_json::Value,
) {
    if msg.get("method").and_then(|value| value.as_str()) != Some("session/prompt") {
        return;
    }
    let Some(session_id) = msg
        .get("params")
        .and_then(|params| params.get("sessionId"))
        .and_then(|value| value.as_str())
    else {
        return;
    };
    if let Some(cancellation) = lookup_session_cancellation(cancellations, session_id) {
        cancellation.prepare_prompt();
    }
}

#[cfg(test)]
mod budget_rearm_tests {
    use super::*;
    use serde_json::json;

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
    async fn active_prompt_inject_is_handled_on_the_preemptive_control_lane() {
        let controls = ConcurrentSessionControls::default();
        let control = ConcurrentSessionControl::new();
        control.set_prompt_active(true);
        controls.register("session-1", control.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let output = AcpOutput::Channel(tx);
        let actor = json!({
            "clientId": "mobile",
            "role": "host_owner",
            "source": "agents_api",
        });

        let handled = controls
            .preempt_active_prompt_inject(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 7,
                    "method": "session/inject",
                    "params": {
                        "sessionId": "session-1",
                        "mode": "steer",
                        "content": "continue with the mobile correction",
                        "_meta": { "harn": { "actor": actor } },
                    },
                }),
                &output,
            )
            .await;

        assert!(handled);
        let response: serde_json::Value =
            serde_json::from_str(&rx.recv().await.expect("injection response")).expect("json");
        let message_id = response["result"]["messageId"]
            .as_str()
            .expect("message id");
        assert_eq!(response["result"]["status"], "accepted");
        assert_eq!(controls.inject_owner("session-1", message_id), Some(actor));

        let pending = control.inject_state.pending_injections_json().await;
        assert_eq!(pending["pendingCount"], 1);
        assert_eq!(pending["injections"][0]["messageId"], message_id);
        assert_eq!(
            pending["injections"][0]["content"],
            "continue with the mobile correction"
        );
        assert_eq!(pending["injections"][0]["mode"], "finish_step");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_prompt_interrupt_inject_reaches_the_immediate_checkpoint() {
        let controls = ConcurrentSessionControls::default();
        let control = ConcurrentSessionControl::new();
        control.set_prompt_active(true);
        controls.register("session-1", control.clone());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let output = AcpOutput::Channel(tx);

        let handled = controls
            .preempt_active_prompt_inject(
                &json!({
                    "jsonrpc": "2.0",
                    "id": 8,
                    "method": "session/inject",
                    "params": {
                        "sessionId": "session-1",
                        "mode": "interrupt_immediate",
                        "content": "stop before the next tool",
                    },
                }),
                &output,
            )
            .await;

        assert!(handled);
        let response: serde_json::Value =
            serde_json::from_str(&rx.recv().await.expect("injection response")).expect("json");
        assert_eq!(response["result"]["status"], "accepted");

        let pending = control.inject_state.pending_injections_json().await;
        assert_eq!(pending["pendingCount"], 1);
        assert_eq!(pending["injections"][0]["kind"], "user");
        assert_eq!(pending["injections"][0]["mode"], "interrupt_immediate");
        assert_eq!(
            pending["injections"][0]["content"],
            "stop before the next tool"
        );

        let bridge = harn_vm::bridge::HostBridge::from_parts_with_writer_and_control(
            Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            Arc::new(|_: &str| Ok(())),
            1,
            harn_vm::bridge::HostBridgeControlState::new(
                Arc::new(AtomicBool::new(false)),
                Arc::new(tokio::sync::Notify::new()),
                control.inject_state.clone(),
                control.tool_call_cancellations.clone(),
            ),
        );
        assert!(
            bridge
                .take_queued_user_messages_for(
                    harn_vm::bridge::DeliveryCheckpoint::AfterCurrentOperation
                )
                .await
                .is_empty(),
            "interrupt_immediate must not be downgraded to finish_step"
        );
        let delivered = bridge
            .take_queued_user_messages_for(harn_vm::bridge::DeliveryCheckpoint::InterruptImmediate)
            .await;
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].content, "stop before the next tool");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_prompt_takeover_is_handled_on_the_preemptive_control_lane() {
        let session_id = format!("session-{}", uuid::Uuid::now_v7());
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
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
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
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
