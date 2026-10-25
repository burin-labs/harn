//! `session/prompt` with `delivery`: `steer` joins the running turn at its
//! next safe point, `queue` starts a new turn once the current one ends.
//!
//! Every delivery prompt enters the session input queue at once, whether or
//! not a turn is running. The request stays pending until its message is
//! inserted and is then answered with the message's `messageId`. Before
//! insertion it can be withdrawn with `$/cancel_request`, and `session/close`
//! or a closed connection fail it with `-32800`. `session/cancel` stops only
//! the running turn; pending prompts then open the next turns.
use super::*;
use harn_vm::bridge::PendingUserMessageMutationResult;

struct PendingPrompt {
    session_id: String,
    request_id: serde_json::Value,
    message_id: String,
    delivery: AcpPromptDelivery,
    /// Rides on the receipt.
    meta: serde_json::Value,
    /// Its message left the session queue and a turn is being prepared for
    /// it. The request stays here, cancellable, until the turn claims
    /// admission; the turn is then the only thing that answers it.
    admitting: bool,
    /// A cancel or close reached the prompt before its turn claimed it.
    withdrawn: Option<String>,
    /// The turn claimed admission: a cancel no longer withdraws the prompt,
    /// and its receipt waits for the message's insertion.
    claimed: bool,
    /// Set when the receipt is sent, so the turn knows its request is answered.
    inserted: Arc<std::sync::atomic::AtomicBool>,
}

/// How a turn's claim on its delivery prompt ended.
pub(super) enum AdmissionClaim {
    Admitted,
    /// Withdrawn before admission; the turn must not run.
    Withdrawn(String),
}

/// Every unanswered delivery prompt this connection sent, in arrival order.
#[derive(Clone, Default)]
pub(super) struct PendingPrompts(Arc<std::sync::Mutex<PendingState>>);

/// The pending list and the sessions that no longer admit delivery prompts,
/// under one lock so closing and registering cannot interleave.
#[derive(Default)]
struct PendingState {
    prompts: Vec<PendingPrompt>,
    closed: std::collections::BTreeSet<String>,
}

impl std::ops::Deref for PendingState {
    type Target = Vec<PendingPrompt>;
    fn deref(&self) -> &Self::Target {
        &self.prompts
    }
}

impl std::ops::DerefMut for PendingState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.prompts
    }
}

/// Keeps a session's delivery admission closed until dropped.
pub(super) struct DeliveryClosure {
    pending: PendingPrompts,
    session_id: String,
}

impl Drop for DeliveryClosure {
    fn drop(&mut self) {
        self.pending.lock().closed.remove(&self.session_id);
    }
}

/// What the router did with a `session/prompt`.
pub(super) enum DeliveryRoute {
    /// Not a delivery prompt to a known session; route it as usual.
    NotDelivery,
    /// Answered with an error; nothing was queued.
    Answered,
    /// Queued. The dispatcher must look again once it is free, in case no
    /// running turn takes the message.
    Enqueued { session_id: String },
}

/// Answers a pending prompt when a loop checkpoint inserts its message.
#[derive(Clone)]
pub(super) struct PromptReceipts {
    pending: PendingPrompts,
    output: AcpOutput,
}

impl PromptReceipts {
    pub(super) fn new(pending: PendingPrompts, output: AcpOutput) -> Self {
        Self { pending, output }
    }

    pub(super) fn inserted(&self, session_id: &str, message_id: &str) {
        if let Some((request_id, meta, inserted)) =
            self.pending.take_inserted(session_id, message_id)
        {
            inserted.store(true, std::sync::atomic::Ordering::SeqCst);
            send_receipt(&self.output, &request_id, message_id, Some(meta));
        }
    }
}

impl PendingPrompts {
    fn lock(&self) -> std::sync::MutexGuard<'_, PendingState> {
        self.0.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Queue a request's pending entry. Refused once the session no longer
    /// admits delivery prompts.
    pub(super) fn register(
        &self,
        session_id: &str,
        request_id: serde_json::Value,
        message_id: &str,
        delivery: AcpPromptDelivery,
        meta: serde_json::Value,
    ) -> Result<(), ()> {
        let mut state = self.lock();
        if state.closed.contains(session_id) {
            return Err(());
        }
        state.prompts.push(PendingPrompt {
            session_id: session_id.to_string(),
            request_id,
            message_id: message_id.to_string(),
            delivery,
            meta,
            admitting: false,
            withdrawn: None,
            claimed: false,
            inserted: Arc::default(),
        });
        Ok(())
    }

    /// Stop admitting delivery prompts for a session that is being retired.
    /// Call before sweeping its pending prompts: a prompt registering after
    /// this is refused and answered with an error, never left unanswered.
    pub(super) fn close_delivery(&self, session_id: &str) -> DeliveryClosure {
        self.lock().closed.insert(session_id.to_string());
        DeliveryClosure {
            pending: self.clone(),
            session_id: session_id.to_string(),
        }
    }

    /// Remove an entry a checkpoint or turn inserted, with what answers it.
    fn take_inserted(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Option<(
        serde_json::Value,
        serde_json::Value,
        Arc<std::sync::atomic::AtomicBool>,
    )> {
        let mut pending = self.lock();
        let position = pending.iter().position(|prompt| {
            prompt.session_id == session_id && prompt.message_id == message_id
        })?;
        let prompt = pending.remove(position);
        Some((prompt.request_id, prompt.meta, prompt.inserted))
    }

    fn contains(&self, session_id: &str, message_id: &str) -> bool {
        self.lock()
            .iter()
            .any(|prompt| prompt.session_id == session_id && prompt.message_id == message_id)
    }

    fn take(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Option<(serde_json::Value, serde_json::Value)> {
        let mut pending = self.lock();
        let position = pending.iter().position(|prompt| {
            prompt.session_id == session_id && prompt.message_id == message_id
        })?;
        let prompt = pending.remove(position);
        Some((prompt.request_id, prompt.meta))
    }

    /// Move a prompt from the session queue into turn preparation. It stays
    /// listed, so a cancel or close can still reach it.
    fn begin_admission(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Option<(
        serde_json::Value,
        serde_json::Value,
        Arc<std::sync::atomic::AtomicBool>,
    )> {
        let mut pending = self.lock();
        let prompt = pending.iter_mut().find(|prompt| {
            prompt.session_id == session_id && prompt.message_id == message_id && !prompt.admitting
        })?;
        prompt.admitting = true;
        Some((
            prompt.request_id.clone(),
            prompt.meta.clone(),
            prompt.inserted.clone(),
        ))
    }

    /// The turn's one decision point: past it, a cancel no longer withdraws
    /// the prompt. Atomic with every withdrawal under the list's lock.
    fn claim_admission(&self, session_id: &str, message_id: &str) -> AdmissionClaim {
        let mut pending = self.lock();
        let Some(position) = pending
            .iter()
            .position(|prompt| prompt.session_id == session_id && prompt.message_id == message_id)
        else {
            return AdmissionClaim::Admitted;
        };
        if let Some(reason) = pending[position].withdrawn.clone() {
            pending.remove(position);
            return AdmissionClaim::Withdrawn(reason);
        }
        pending[position].claimed = true;
        AdmissionClaim::Admitted
    }

    /// Flag the prompts `select` picks as withdrawn and return the ones still
    /// queued, which the caller revokes and answers. A prompt whose turn is
    /// being prepared is not returned: the turn claims admission once, sees
    /// the flag, and is then the only thing that answers it.
    fn select_for_withdrawal(
        &self,
        select: impl Fn(&PendingPrompt) -> bool,
        reason: &str,
    ) -> Vec<(String, String)> {
        let mut pending = self.lock();
        let mut queued = Vec::new();
        for prompt in pending
            .iter_mut()
            .filter(|prompt| !prompt.claimed && select(prompt))
        {
            // Flag every selected prompt at selection: the revocation that
            // follows awaits, and a drain may admit the prompt meanwhile.
            prompt.withdrawn.get_or_insert_with(|| reason.to_string());
            if !prompt.admitting {
                queued.push((prompt.session_id.clone(), prompt.message_id.clone()));
            }
        }
        queued
    }

    /// Forget an admitting prompt whose turn ended without claiming it.
    fn finish_admission(&self, session_id: &str, message_id: &str) {
        self.lock()
            .retain(|prompt| !(prompt.session_id == session_id && prompt.message_id == message_id));
    }

    /// The message the next owed turn starts with: the earliest steer no turn
    /// took, else the earliest queued prompt.
    fn next_owed(&self, session_id: &str) -> Option<String> {
        let pending = self.lock();
        let first = |wanted: AcpPromptDelivery| {
            pending
                .iter()
                .find(|prompt| {
                    prompt.session_id == session_id
                        && prompt.delivery == wanted
                        && !prompt.admitting
                })
                .map(|prompt| prompt.message_id.clone())
        };
        first(AcpPromptDelivery::Steer).or_else(|| first(AcpPromptDelivery::Queue))
    }
}

impl ConcurrentSessionControls {
    /// Router lane for `session/prompt`: a delivery prompt is queued here,
    /// without waiting behind a running turn.
    pub(super) async fn route_delivery_prompt(
        &self,
        msg: &serde_json::Value,
        output: &AcpOutput,
    ) -> DeliveryRoute {
        if msg.get("method").and_then(serde_json::Value::as_str) != Some(ACP_METHOD_SESSION_PROMPT)
        {
            return DeliveryRoute::NotDelivery;
        }
        let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
        let Some(id) = msg.get("id").filter(|_| has_delivery(params)) else {
            return DeliveryRoute::NotDelivery;
        };
        if self.reject_unauthenticated(id, output) {
            return DeliveryRoute::Answered;
        }
        let Some(session_id) = session_id_param(params) else {
            return DeliveryRoute::NotDelivery;
        };
        if self.registered_control(&session_id).is_none() {
            return DeliveryRoute::NotDelivery;
        }
        if self
            .enqueue_delivery_prompt(&session_id, id, params, output)
            .await
        {
            DeliveryRoute::Enqueued { session_id }
        } else {
            DeliveryRoute::Answered
        }
    }

    /// Validate and queue one delivery prompt for a registered session.
    /// Returns whether it was queued; otherwise its request has been answered
    /// with the error.
    pub(super) async fn enqueue_delivery_prompt(
        &self,
        session_id: &str,
        id: &serde_json::Value,
        params: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        let Some(control) = self.registered_control(session_id) else {
            send_routed_error(
                output,
                id,
                -32004,
                &format!("Session not found: {session_id}"),
            );
            return false;
        };
        let delivery = match parse_delivery(params) {
            Ok(delivery) => delivery,
            Err(message) => {
                send_routed_error(output, id, -32602, &message);
                return false;
            }
        };
        let actor = control_actor_from_params(params);
        let checked = normalize_delivery_content(params)
            .and_then(|prompt| match &prompt.correlation.message_id {
                Some(message_id) if self.pending_prompts.contains(session_id, message_id) => Err((
                    "duplicate_message_id",
                    format!("session/prompt: messageId {message_id} is already pending"),
                )),
                _ => Ok(prompt),
            })
            .and_then(|prompt| delivery_goal(params, delivery).map(|goal| (prompt, goal)));
        let (prompt, goal) = match checked {
            Ok(checked) => checked,
            Err((reason, message)) => {
                send_routed_error(output, id, -32602, &message);
                self.emit_control_event(&harn_vm::agent_events::AgentEvent::ControlOutcome {
                    session_id: session_id.to_string(),
                    control_id: control_id(),
                    method: ACP_METHOD_SESSION_PROMPT.to_string(),
                    outcome: "rejected".to_string(),
                    status: "rejected".to_string(),
                    actor,
                    target: serde_json::json!({"sessionId": session_id}),
                    reason: Some(reason.to_string()),
                    metadata: serde_json::Value::Null,
                });
                return false;
            }
        };
        let command = task_admission::InjectionCommand {
            request_id: id.clone(),
            message_id: prompt.correlation.message_id,
            content: prompt.text,
            transcript_content: serde_json::Value::Array(prompt.content),
            delivery,
            goal,
            actor,
        };
        match self
            .enqueue_injection(session_id, &control.inject_state, params, command)
            .await
        {
            Ok(_) => true,
            Err(error) => {
                send_routed_error(output, id, error.rpc_code(), error.message());
                false
            }
        }
    }

    /// `$/cancel_request` withdraws a pending delivery prompt. Other requests
    /// ignore it, as ACP allows.
    pub(super) async fn preempt_cancel_request(
        &self,
        msg: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        if msg.get("method").and_then(serde_json::Value::as_str) != Some(ACP_METHOD_CANCEL_REQUEST)
        {
            return false;
        }
        if let Some(request_id) = msg.pointer("/params/requestId") {
            if self.authenticated.load(Ordering::SeqCst) {
                self.withdraw(
                    |prompt| prompt.request_id == *request_id,
                    "request_cancelled",
                    Some(output),
                )
                .await;
            }
        }
        true
    }

    pub(super) async fn cancel_pending_prompts(
        &self,
        session_id: &str,
        reason: &str,
        output: &AcpOutput,
    ) {
        self.withdraw(
            |prompt| prompt.session_id == session_id,
            reason,
            Some(output),
        )
        .await;
    }

    /// The connection that sent these prompts is gone: nothing can answer or
    /// withdraw them, so none is inserted.
    pub(super) async fn drop_pending_prompts(&self) {
        self.withdraw(|_| true, "connection_closed", None).await;
    }

    /// Fail a prompt whose message the session already revoked.
    pub(super) fn fail_pending_prompt(
        &self,
        session_id: &str,
        message_id: &str,
        reason: &str,
        output: &AcpOutput,
    ) {
        if let Some((request_id, _)) = self.pending_prompts.take(session_id, message_id) {
            send_cancelled(output, &request_id, Some(message_id), reason);
        }
    }

    async fn withdraw(
        &self,
        select: impl Fn(&PendingPrompt) -> bool,
        reason: &str,
        output: Option<&AcpOutput>,
    ) {
        let selected = self.pending_prompts.select_for_withdrawal(select, reason);
        for (session_id, message_id) in selected {
            let revoked = match self.registered_control(&session_id) {
                Some(control) => {
                    control
                        .inject_state
                        .revoke_pending_user_message(&message_id)
                        .await
                }
                None => PendingUserMessageMutationResult::UnknownMessageId,
            };
            // A message a checkpoint already took is inserted; its receipt
            // still answers the request.
            if revoked == PendingUserMessageMutationResult::AlreadyDelivered {
                continue;
            }
            if let Some((request_id, _)) = self.pending_prompts.take(&session_id, &message_id) {
                if let Some(output) = output {
                    send_cancelled(output, &request_id, Some(&message_id), reason);
                }
                if revoked == PendingUserMessageMutationResult::Mutated {
                    emit_withdrawn(self, &session_id, &message_id, reason);
                }
            }
        }
    }
}

impl AcpServer {
    /// A delivery prompt the dispatcher reached directly: queue it, and start
    /// its turn when no turn is running to take it.
    pub(super) async fn handle_delivery_prompt(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(session_id) = session_id_param(params) else {
            self.send_prompt_protocol_error(id, "Missing sessionId");
            return;
        };
        let queued = self
            .concurrent_controls
            .enqueue_delivery_prompt(&session_id, id, params, &self.output)
            .await;
        let running = self
            .concurrent_controls
            .registered_control(&session_id)
            .is_some_and(|control| control.prompt_is_active());
        if queued && !running {
            self.drain_pending_prompts(&session_id).await;
        }
    }

    /// Start the turns pending prompts are owed once no turn is running: the
    /// earliest steer opens one turn and the rest of the waiting steers join
    /// it at its first checkpoint, then each queued prompt gets a turn of its
    /// own, in arrival order.
    pub(super) async fn drain_pending_prompts(&mut self, session_id: &str) {
        while let Some(message_id) = self
            .concurrent_controls
            .pending_prompts
            .next_owed(session_id)
        {
            let Some(inject_state) = self
                .sessions
                .get(session_id)
                .map(|session| session.inject_state.clone())
            else {
                // The session is gone: answer what is still pending for it.
                self.concurrent_controls
                    .cancel_pending_prompts(session_id, "session_closed", &self.output)
                    .await;
                return;
            };
            let Some((request_id, _meta, inserted)) = self
                .concurrent_controls
                .pending_prompts
                .begin_admission(session_id, &message_id)
            else {
                continue;
            };
            let Some(message) = inject_state.take_pending_user_message(&message_id).await else {
                self.concurrent_controls
                    .pending_prompts
                    .finish_admission(session_id, &message_id);
                send_cancelled(&self.output, &request_id, Some(&message_id), "not_pending");
                continue;
            };
            let params = serde_json::json!({
                "sessionId": session_id,
                "messageId": message.message_id,
                "prompt": message.transcript_content,
            });
            self.handle_session_prompt(
                &request_id,
                &params,
                None,
                TurnReply::receipt(message.message_id, inserted),
            )
            .await;
            self.concurrent_controls
                .pending_prompts
                .finish_admission(session_id, &message_id);
        }
    }

    /// Decide, once, whether a delivery prompt's turn is admitted. A cancel
    /// or close that reached it first has withdrawn it: its request is
    /// answered here and the turn must not run. A plain prompt always is.
    pub(super) fn claim_turn_admission(
        &self,
        id: &serde_json::Value,
        session_id: &str,
        reply: &TurnReply,
    ) -> bool {
        let TurnReply::Receipt { message_id, .. } = reply else {
            return true;
        };
        match self
            .concurrent_controls
            .pending_prompts
            .claim_admission(session_id, message_id)
        {
            AdmissionClaim::Admitted => true,
            AdmissionClaim::Withdrawn(reason) => {
                send_cancelled(&self.output, id, Some(message_id), &reason);
                emit_withdrawn(&self.concurrent_controls, session_id, message_id, &reason);
                false
            }
        }
    }

    pub(super) async fn handle_session_pending_prompts(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        if let Some(inject_state) =
            self.session_inject_state(id, params, ACP_METHOD_SESSION_PENDING_PROMPTS)
        {
            self.send_response(id, pending_prompts_listing(&inject_state).await);
        }
    }
}

impl ConcurrentSessionControls {
    /// Answer `session/pending_prompts` without waiting for the running turn.
    pub(super) async fn preempt_pending_prompts(
        &self,
        msg: &serde_json::Value,
        output: &AcpOutput,
    ) -> bool {
        if msg.get("method").and_then(serde_json::Value::as_str)
            != Some(ACP_METHOD_SESSION_PENDING_PROMPTS)
        {
            return false;
        }
        let Some(id) = msg.get("id") else {
            return false;
        };
        if self.reject_unauthenticated(id, output) {
            return true;
        }
        let params = msg.get("params").unwrap_or(&serde_json::Value::Null);
        let Some(control) = session_id_param(params).and_then(|id| self.registered_control(&id))
        else {
            return false;
        };
        send_json_response(
            output,
            id,
            pending_prompts_listing(&control.inject_state).await,
        );
        true
    }
}

/// Pending delivery prompts and reminders, in the order the session will
/// take them.
async fn pending_prompts_listing(
    inject_state: &harn_vm::bridge::HostBridgeInjectionState,
) -> serde_json::Value {
    let listing = inject_state.pending_injections_json().await;
    let mut prompts = Vec::new();
    let mut reminders = Vec::new();
    for entry in listing["injections"].as_array().into_iter().flatten() {
        let delivery = match (entry["kind"].as_str(), entry["mode"].as_str()) {
            (Some("reminder"), _) => {
                reminders.push(entry.clone());
                continue;
            }
            (Some("user"), Some("finish_step")) => AcpPromptDelivery::Steer,
            (Some("user"), Some("next_turn")) => AcpPromptDelivery::Queue,
            _ => continue,
        };
        prompts.push(serde_json::json!({
            "messageId": entry["messageId"],
            "delivery": delivery,
            "prompt": entry["content"],
            "position": entry["position"],
        }));
    }
    serde_json::json!({"prompts": prompts, "reminders": reminders})
}

/// How a turn's start and end reach the client.
pub(super) enum TurnReply {
    /// A plain prompt: its response reports how the turn ended.
    Response,
    /// A delivery prompt: its response is the insertion receipt, and
    /// `state_update` notifications report the turn, since no request spans it.
    Receipt {
        message_id: String,
        /// Set when the receipt goes out with the message's insertion.
        inserted: Arc<std::sync::atomic::AtomicBool>,
    },
}

impl TurnReply {
    fn receipt(message_id: String, inserted: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self::Receipt {
            message_id,
            inserted,
        }
    }

    pub(super) fn is_inserted(&self) -> bool {
        matches!(self, Self::Receipt { inserted, .. } if inserted.load(std::sync::atomic::Ordering::SeqCst))
    }
}

impl AcpServer {
    /// The turn was admitted. The receipt is not sent here: it goes out with
    /// the message's insertion into the transcript (`PromptReceipts::inserted`).
    pub(super) fn turn_started(&self, session_id: &str, reply: &TurnReply) {
        if matches!(reply, TurnReply::Receipt { .. }) {
            self.send_state_update(session_id, serde_json::json!({"state": "running"}));
        }
    }

    /// Report a turn's result: the prompt response, or the idle update.
    pub(super) fn turn_ended(
        &self,
        id: &serde_json::Value,
        session_id: &str,
        reply: &TurnReply,
        result: serde_json::Value,
    ) {
        if !reply.is_inserted() {
            self.send_response(id, result);
            return;
        }
        let mut update = serde_json::json!({"state": "idle", "stopReason": result["stopReason"]});
        if let Some(meta) = result.get("_meta") {
            update["_meta"] = meta.clone();
        }
        self.send_state_update(session_id, update);
    }

    /// Report a turn that failed: the prompt's error response, or, once its
    /// message is inserted, the idle update with the same error object.
    pub(super) fn turn_failed(
        &self,
        id: &serde_json::Value,
        session_id: &str,
        reply: &TurnReply,
        message: &str,
        terminal_class: harn_vm::llm::AgentTerminalClass,
        facts: super::types::AcpPromptFailureFacts,
    ) {
        if !reply.is_inserted() {
            self.send_prompt_failure(id, message, terminal_class, facts);
            return;
        }
        let data = super::types::AcpPromptErrorData::with_facts(terminal_class, facts);
        self.send_state_update(
            session_id,
            serde_json::json!({
                "state": "idle",
                "stopReason": "error",
                "error": {"code": -32000, "message": message, "data": data},
            }),
        );
        eprintln!("{message}");
    }

    fn send_state_update(&self, session_id: &str, mut update: serde_json::Value) {
        update["sessionUpdate"] = serde_json::json!("state_update");
        self.send_notification(
            "session/update",
            serde_json::json!({"sessionId": session_id, "update": update}),
        );
    }
}

fn has_delivery(params: &serde_json::Value) -> bool {
    params
        .get("delivery")
        .is_some_and(|delivery| !delivery.is_null())
}

/// The prompt's `delivery`, rejected with `-32602` when it is not one Harn
/// advertises.
fn parse_delivery(params: &serde_json::Value) -> Result<AcpPromptDelivery, String> {
    let raw = params.get("delivery").cloned().unwrap_or_default();
    serde_json::from_value(raw.clone()).map_err(|_| {
        format!("session/prompt: unsupported delivery {raw}; expected \"steer\" or \"queue\"")
    })
}

fn normalize_delivery_content(
    params: &serde_json::Value,
) -> Result<schema::NormalizedAcpPrompt, (&'static str, String)> {
    let prompt = normalize_acp_prompt(params).map_err(|message| ("invalid_content", message))?;
    if prompt.content.is_empty() {
        return Err((
            "invalid_content",
            "session/prompt: a delivery prompt must not be empty".to_string(),
        ));
    }
    Ok(prompt)
}

/// `_meta.harn.goal` retargets the run as a steer lands. A queued prompt
/// opens its own turn and cannot carry one.
fn delivery_goal(
    params: &serde_json::Value,
    delivery: AcpPromptDelivery,
) -> Result<Option<harn_session_store::ControlGoal>, (&'static str, String)> {
    let Some(raw) = params
        .pointer("/_meta/harn/goal")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    if delivery != AcpPromptDelivery::Steer {
        return Err((
            "invalid_goal",
            "session/prompt: `_meta.harn.goal` retargets the running turn and needs delivery `steer`"
                .to_string(),
        ));
    }
    harn_session_store::ControlGoal::parse(raw)
        .map(Some)
        .map_err(|message| ("invalid_goal", format!("session/prompt: {message}")))
}

fn send_receipt(
    output: &AcpOutput,
    request_id: &serde_json::Value,
    message_id: &str,
    meta: Option<serde_json::Value>,
) {
    let receipt = AcpSessionPromptReceipt {
        message_id: message_id.to_string(),
        meta,
    };
    send_json_response(
        output,
        request_id,
        serde_json::to_value(receipt).expect("prompt receipt serializes"),
    );
}

fn send_cancelled(
    output: &AcpOutput,
    request_id: &serde_json::Value,
    message_id: Option<&str>,
    reason: &str,
) {
    let response = harn_vm::jsonrpc::error_response_with_data(
        request_id.clone(),
        ACP_REQUEST_CANCELLED_CODE,
        "Request cancelled before the message was inserted",
        serde_json::json!({"reason": reason, "messageId": message_id}),
    );
    if let Ok(line) = serde_json::to_string(&response) {
        output.write_line(&line);
    }
}

fn emit_withdrawn(
    controls: &ConcurrentSessionControls,
    session_id: &str,
    message_id: &str,
    reason: &str,
) {
    controls.emit_control_event(&harn_vm::agent_events::AgentEvent::ControlOutcome {
        session_id: session_id.to_string(),
        control_id: control_id(),
        method: ACP_METHOD_SESSION_PROMPT.to_string(),
        outcome: "withdrawn".to_string(),
        status: "accepted".to_string(),
        actor: serde_json::Value::Null,
        target: serde_json::json!({"sessionId": session_id, "messageId": message_id}),
        reason: Some(reason.to_string()),
        metadata: serde_json::Value::Null,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered() -> PendingPrompts {
        let pending = PendingPrompts::default();
        pending
            .register(
                "s",
                serde_json::json!(7),
                "m7",
                AcpPromptDelivery::Queue,
                serde_json::Value::Null,
            )
            .expect("session admits prompts");
        pending
    }

    /// A cancel that lands after the drain took the prompt off the session
    /// queue but before its turn is admitted must still stop it.
    #[test]
    fn a_cancel_during_admission_withdraws_the_prompt_at_the_claim() {
        let pending = registered();
        assert!(pending.begin_admission("s", "m7").is_some());
        assert_eq!(pending.next_owed("s"), None);
        let queued = pending.select_for_withdrawal(|_| true, "request_cancelled");
        assert!(
            queued.is_empty(),
            "an admitting prompt is the turn's to answer"
        );
        assert!(matches!(
            pending.claim_admission("s", "m7"),
            AdmissionClaim::Withdrawn(reason) if reason == "request_cancelled"
        ));
        assert!(!pending.contains("s", "m7"));
    }

    /// The cancel selects the prompt while it is still queued, then waits on
    /// revocation; the drain admits it in that interval.
    #[test]
    fn a_cancel_that_selected_a_queued_prompt_still_withdraws_it_if_a_drain_admits_it() {
        let pending = registered();
        let queued = pending.select_for_withdrawal(|_| true, "request_cancelled");
        assert_eq!(queued, vec![("s".to_string(), "m7".to_string())]);
        assert!(pending.begin_admission("s", "m7").is_some());
        assert!(matches!(
            pending.claim_admission("s", "m7"),
            AdmissionClaim::Withdrawn(reason) if reason == "request_cancelled"
        ));
    }

    /// A prompt racing session close is refused, so its request is answered
    /// with an error instead of waiting on a session that is going away.
    #[test]
    fn a_prompt_registering_after_delivery_closes_is_refused() {
        let pending = PendingPrompts::default();
        let closure = pending.close_delivery("s");
        let refused = pending.register(
            "s",
            serde_json::json!(8),
            "m8",
            AcpPromptDelivery::Steer,
            serde_json::Value::Null,
        );
        assert!(refused.is_err());
        assert!(!pending.contains("s", "m8"));
        // Other sessions still admit, and the closure's end reopens this one.
        assert!(registered().contains("s", "m7"));
        drop(closure);
        assert!(pending
            .register(
                "s",
                serde_json::json!(9),
                "m9",
                AcpPromptDelivery::Steer,
                serde_json::Value::Null,
            )
            .is_ok());
    }

    /// Past the claim the prompt is the turn's: a cancel cannot withdraw it,
    /// and its receipt waits for the insertion.
    #[test]
    fn a_claimed_prompt_keeps_its_entry_until_its_message_is_inserted() {
        let pending = registered();
        let (_, _, inserted) = pending.begin_admission("s", "m7").expect("admitting");
        assert!(matches!(
            pending.claim_admission("s", "m7"),
            AdmissionClaim::Admitted
        ));
        assert!(pending.contains("s", "m7"));
        let queued = pending.select_for_withdrawal(|_| true, "request_cancelled");
        assert!(queued.is_empty());
        assert!(matches!(
            pending.claim_admission("s", "m7"),
            AdmissionClaim::Admitted
        ));
        assert!(!inserted.load(std::sync::atomic::Ordering::SeqCst));
        let (request_id, _, flag) = pending.take_inserted("s", "m7").expect("entry");
        assert_eq!(request_id, serde_json::json!(7));
        assert!(Arc::ptr_eq(&flag, &inserted));
    }

    #[test]
    fn a_claim_with_no_cancel_admits_and_closes_the_window() {
        let pending = registered();
        pending.begin_admission("s", "m7").expect("admitting");
        assert!(matches!(
            pending.claim_admission("s", "m7"),
            AdmissionClaim::Admitted
        ));
        let queued = pending.select_for_withdrawal(|_| true, "request_cancelled");
        assert!(
            queued.is_empty(),
            "a claimed prompt is no longer withdrawable"
        );
    }
}
