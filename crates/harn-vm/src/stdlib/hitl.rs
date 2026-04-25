use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::Digest;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{
    active_event_log, install_default_for_base_dir, install_memory_for_current_thread, AnyEventLog,
    EventLog, LogEvent, Topic,
};
use crate::schema::schema_expect_value;
use crate::stdlib::host::dispatch_mock_host_call;
use crate::stdlib::waitpoint::{
    cancel_waitpoint_on, complete_waitpoint_on, create_waitpoint_on, inspect_waitpoint_on,
    wait_on_waitpoints, WaitpointRecord, WaitpointStatus, WaitpointWaitFailure,
    WaitpointWaitOptions,
};
use crate::triggers::dispatcher::current_dispatch_context;
use crate::value::{categorized_error, ErrorCategory, VmError, VmValue};
use crate::vm::{clone_async_builtin_child_vm, Vm};

const HITL_EVENT_LOG_QUEUE_DEPTH: usize = 128;
const HITL_APPROVAL_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;
const HITL_QUESTION_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

pub const HITL_QUESTIONS_TOPIC: &str = "hitl.questions";
pub const HITL_APPROVALS_TOPIC: &str = "hitl.approvals";
pub const HITL_DUAL_CONTROL_TOPIC: &str = "hitl.dual_control";
pub const HITL_ESCALATIONS_TOPIC: &str = "hitl.escalations";

thread_local! {
    static REQUEST_SEQUENCE: RefCell<RequestSequenceState> = RefCell::new(RequestSequenceState::default());
}

#[derive(Default)]
pub(crate) struct RequestSequenceState {
    pub(crate) instance_key: String,
    pub(crate) next_seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HitlRequestKind {
    Question,
    Approval,
    DualControl,
    Escalation,
}

impl HitlRequestKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Question => "question",
            Self::Approval => "approval",
            Self::DualControl => "dual_control",
            Self::Escalation => "escalation",
        }
    }

    fn topic(self) -> &'static str {
        match self {
            Self::Question => HITL_QUESTIONS_TOPIC,
            Self::Approval => HITL_APPROVALS_TOPIC,
            Self::DualControl => HITL_DUAL_CONTROL_TOPIC,
            Self::Escalation => HITL_ESCALATIONS_TOPIC,
        }
    }

    fn request_event_kind(self) -> &'static str {
        match self {
            Self::Question => "hitl.question_asked",
            Self::Approval => "hitl.approval_requested",
            Self::DualControl => "hitl.dual_control_requested",
            Self::Escalation => "hitl.escalation_issued",
        }
    }

    pub(crate) fn from_request_id(request_id: &str) -> Option<Self> {
        if request_id.starts_with("hitl_question_") {
            Some(Self::Question)
        } else if request_id.starts_with("hitl_approval_") {
            Some(Self::Approval)
        } else if request_id.starts_with("hitl_dual_control_") {
            Some(Self::DualControl)
        } else if request_id.starts_with("hitl_escalation_") {
            Some(Self::Escalation)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HitlHostResponse {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HitlRequestEnvelope {
    request_id: String,
    kind: HitlRequestKind,
    #[serde(default)]
    agent: String,
    trace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    requested_at: String,
    payload: JsonValue,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HitlTimeoutRecord {
    request_id: String,
    kind: HitlRequestKind,
    trace_id: String,
    timed_out_at: String,
}

#[derive(Clone, Debug)]
struct DispatchKeys {
    instance_key: String,
    stable_base: String,
    agent: String,
    trace_id: String,
}

#[derive(Clone, Debug)]
struct AskUserOptions {
    schema: Option<VmValue>,
    timeout: Option<StdDuration>,
    default: Option<VmValue>,
}

#[derive(Clone, Debug)]
struct ApprovalOptions {
    detail: Option<VmValue>,
    quorum: u32,
    reviewers: Vec<String>,
    deadline: StdDuration,
}

#[derive(Clone, Debug)]
struct ApprovalProgress {
    reviewers: BTreeSet<String>,
    signatures: Vec<ApprovalSignature>,
    reason: Option<String>,
    approved_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ApprovalSignature {
    reviewer: String,
    signed_at: String,
    signature: String,
}

#[derive(Clone, Debug)]
enum ApprovalResolution {
    Pending,
    Approved(ApprovalProgress),
    Denied(HitlHostResponse),
}

#[derive(Clone, Debug)]
enum WaitpointOutcome {
    Completed(WaitpointRecord),
    Timeout,
    Cancelled {
        wait_id: String,
        waitpoint_ids: Vec<String>,
        reason: Option<String>,
    },
}

pub(crate) fn register_hitl_builtins(vm: &mut Vm) {
    vm.register_async_builtin("ask_user", |args| {
        Box::pin(async move { ask_user_impl(&args).await })
    });

    vm.register_async_builtin("request_approval", |args| {
        Box::pin(async move { request_approval_impl(&args).await })
    });

    vm.register_async_builtin("dual_control", |args| {
        Box::pin(async move { dual_control_impl(&args).await })
    });

    vm.register_async_builtin("escalate_to", |args| {
        Box::pin(async move { escalate_to_impl(&args).await })
    });
}

pub(crate) fn reset_hitl_state() {
    REQUEST_SEQUENCE.with(|slot| {
        *slot.borrow_mut() = RequestSequenceState::default();
    });
}

pub(crate) fn take_hitl_state() -> RequestSequenceState {
    REQUEST_SEQUENCE.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
}

pub(crate) fn restore_hitl_state(state: RequestSequenceState) {
    REQUEST_SEQUENCE.with(|slot| {
        *slot.borrow_mut() = state;
    });
}

pub async fn append_hitl_response(
    base_dir: Option<&Path>,
    mut response: HitlHostResponse,
) -> Result<u64, String> {
    let kind = HitlRequestKind::from_request_id(&response.request_id)
        .ok_or_else(|| format!("unknown HITL request id '{}'", response.request_id))?;
    if response.responded_at.is_none() {
        response.responded_at = Some(now_rfc3339());
    }
    let log = ensure_hitl_event_log_for(base_dir)?;
    let headers = response_headers(&response.request_id);
    let topic = Topic::new(kind.topic()).map_err(|error| error.to_string())?;
    let event_id = log
        .append(
            &topic,
            LogEvent::new(
                match kind {
                    HitlRequestKind::Escalation => "hitl.escalation_accepted",
                    _ => "hitl.response_received",
                },
                serde_json::to_value(&response).map_err(|error| error.to_string())?,
            )
            .with_headers(headers),
        )
        .await
        .map_err(|error| error.to_string())?;
    finalize_hitl_response(&log, kind, &response).await?;
    Ok(event_id)
}

pub async fn append_approval_request_on(
    log: &Arc<AnyEventLog>,
    agent: impl Into<String>,
    trace_id: impl Into<String>,
    action: impl Into<String>,
    detail: JsonValue,
    reviewers: Vec<String>,
) -> Result<String, VmError> {
    let request_id = next_request_id(HitlRequestKind::Approval, current_dispatch_keys().as_ref());
    let trace_id = trace_id.into();
    let request = HitlRequestEnvelope {
        request_id: request_id.clone(),
        kind: HitlRequestKind::Approval,
        agent: agent.into(),
        trace_id: trace_id.clone(),
        run_id: None,
        requested_at: now_rfc3339(),
        payload: json!({
            "action": action.into(),
            "detail": detail,
            "quorum": 1,
            "reviewers": reviewers,
            "deadline_ms": HITL_APPROVAL_TIMEOUT_MS,
        }),
    };
    create_request_waitpoint(log, &request).await?;
    append_request(log, &request).await?;
    maybe_notify_host(&request);
    Ok(request_id)
}

async fn ask_user_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let prompt = required_string_arg(args, 0, "ask_user")?;
    let options = parse_ask_user_options(args.get(1))?;
    let keys = current_dispatch_keys();
    let request_id = next_request_id(HitlRequestKind::Question, keys.as_ref());
    let trace_id = keys
        .as_ref()
        .map(|keys| keys.trace_id.clone())
        .unwrap_or_else(new_trace_id);
    let log = ensure_hitl_event_log();
    let request = HitlRequestEnvelope {
        request_id: request_id.clone(),
        kind: HitlRequestKind::Question,
        agent: keys
            .as_ref()
            .map(|keys| keys.agent.clone())
            .unwrap_or_default(),
        trace_id: trace_id.clone(),
        run_id: crate::orchestration::current_mutation_session().and_then(|session| session.run_id),
        requested_at: now_rfc3339(),
        payload: json!({
            "prompt": prompt,
            "schema": options.schema.as_ref().map(crate::llm::vm_value_to_json),
            "default": options.default.as_ref().map(crate::llm::vm_value_to_json),
            "timeout_ms": options.timeout.map(|timeout| timeout.as_millis() as u64),
        }),
    };
    create_request_waitpoint(&log, &request).await?;
    append_request(&log, &request).await?;
    maybe_notify_host(&request);
    maybe_apply_mock_response(HitlRequestKind::Question, &request_id, &request.payload).await?;

    match wait_for_request_waitpoint(&request_id, options.timeout).await? {
        WaitpointOutcome::Completed(record) => {
            let answer = record
                .value
                .as_ref()
                .map(crate::stdlib::json_to_vm_value)
                .unwrap_or(VmValue::Nil);
            if let Some(schema) = options.schema.as_ref() {
                return schema_expect_value(&answer, schema, true);
            }
            if let Some(default) = options.default.as_ref() {
                return Ok(coerce_like_default(&answer, default));
            }
            Ok(answer)
        }
        WaitpointOutcome::Timeout => {
            append_timeout_once(&log, HitlRequestKind::Question, &request_id, &trace_id).await?;
            if let Some(default) = options.default {
                return Ok(default);
            }
            Err(timeout_error(&request_id, HitlRequestKind::Question))
        }
        WaitpointOutcome::Cancelled {
            wait_id,
            waitpoint_ids,
            reason,
        } => Err(hitl_cancelled_error(
            &request_id,
            HitlRequestKind::Question,
            &wait_id,
            &waitpoint_ids,
            reason,
        )),
    }
}

async fn request_approval_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let action = required_string_arg(args, 0, "request_approval")?;
    let options = parse_approval_options(args.get(1), "request_approval")?;
    let keys = current_dispatch_keys();
    let request_id = next_request_id(HitlRequestKind::Approval, keys.as_ref());
    let trace_id = keys
        .as_ref()
        .map(|keys| keys.trace_id.clone())
        .unwrap_or_else(new_trace_id);
    let log = ensure_hitl_event_log();
    let request = HitlRequestEnvelope {
        request_id: request_id.clone(),
        kind: HitlRequestKind::Approval,
        agent: keys
            .as_ref()
            .map(|keys| keys.agent.clone())
            .unwrap_or_default(),
        trace_id: trace_id.clone(),
        run_id: crate::orchestration::current_mutation_session().and_then(|session| session.run_id),
        requested_at: now_rfc3339(),
        payload: json!({
            "action": action,
            "detail": options.detail.as_ref().map(crate::llm::vm_value_to_json),
            "quorum": options.quorum,
            "reviewers": options.reviewers,
            "deadline_ms": options.deadline.as_millis() as u64,
        }),
    };
    create_request_waitpoint(&log, &request).await?;
    append_request(&log, &request).await?;
    maybe_notify_host(&request);
    maybe_apply_mock_response(HitlRequestKind::Approval, &request_id, &request.payload).await?;

    match wait_for_request_waitpoint(&request_id, Some(options.deadline)).await? {
        WaitpointOutcome::Completed(record) => {
            approval_record_from_waitpoint(&record, "request_approval")
        }
        WaitpointOutcome::Timeout => {
            append_timeout_once(&log, HitlRequestKind::Approval, &request_id, &trace_id).await?;
            Err(timeout_error(&request_id, HitlRequestKind::Approval))
        }
        WaitpointOutcome::Cancelled { .. } => {
            Err(approval_wait_error(&log, HitlRequestKind::Approval, &request_id).await)
        }
    }
}

async fn dual_control_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let n = required_positive_int_arg(args, 0, "dual_control")?;
    let m = required_positive_int_arg(args, 1, "dual_control")?;
    if n > m {
        return Err(VmError::Runtime(
            "dual_control: n must be less than or equal to m".to_string(),
        ));
    }
    let action = args
        .get(2)
        .and_then(|value| match value {
            VmValue::Closure(closure) => Some(closure.clone()),
            _ => None,
        })
        .ok_or_else(|| VmError::Runtime("dual_control: action must be a closure".to_string()))?;
    let approvers = optional_string_list(args.get(3), "dual_control")?;
    if !approvers.is_empty() && approvers.len() < m as usize {
        return Err(VmError::Runtime(format!(
            "dual_control: expected at least {m} approvers, got {}",
            approvers.len()
        )));
    }

    let keys = current_dispatch_keys();
    let request_id = next_request_id(HitlRequestKind::DualControl, keys.as_ref());
    let trace_id = keys
        .as_ref()
        .map(|keys| keys.trace_id.clone())
        .unwrap_or_else(new_trace_id);
    let action_name = if action.func.name.is_empty() {
        "anonymous".to_string()
    } else {
        action.func.name.clone()
    };
    let log = ensure_hitl_event_log();
    let request = HitlRequestEnvelope {
        request_id: request_id.clone(),
        kind: HitlRequestKind::DualControl,
        agent: keys
            .as_ref()
            .map(|keys| keys.agent.clone())
            .unwrap_or_default(),
        trace_id: trace_id.clone(),
        run_id: crate::orchestration::current_mutation_session().and_then(|session| session.run_id),
        requested_at: now_rfc3339(),
        payload: json!({
            "n": n,
            "m": m,
            "action": action_name,
            "approvers": approvers,
            "deadline_ms": HITL_APPROVAL_TIMEOUT_MS,
        }),
    };
    create_request_waitpoint(&log, &request).await?;
    append_request(&log, &request).await?;
    maybe_notify_host(&request);
    maybe_apply_mock_response(HitlRequestKind::DualControl, &request_id, &request.payload).await?;

    match wait_for_request_waitpoint(
        &request_id,
        Some(StdDuration::from_millis(HITL_APPROVAL_TIMEOUT_MS)),
    )
    .await?
    {
        WaitpointOutcome::Completed(record) => {
            let _ = approval_record_from_waitpoint(&record, "dual_control")?;
            let mut vm = clone_async_builtin_child_vm().ok_or_else(|| {
                VmError::Runtime("dual_control requires an async builtin VM context".to_string())
            })?;
            let result = vm.call_closure_pub(&action, &[]).await?;

            append_named_event(
                &log,
                HitlRequestKind::DualControl,
                "hitl.dual_control_executed",
                &request_id,
                &trace_id,
                json!({
                    "request_id": request_id,
                    "result": crate::llm::vm_value_to_json(&result),
                }),
            )
            .await?;

            Ok(result)
        }
        WaitpointOutcome::Timeout => {
            append_timeout_once(&log, HitlRequestKind::DualControl, &request_id, &trace_id).await?;
            Err(timeout_error(&request_id, HitlRequestKind::DualControl))
        }
        WaitpointOutcome::Cancelled { .. } => {
            Err(approval_wait_error(&log, HitlRequestKind::DualControl, &request_id).await)
        }
    }
}

async fn escalate_to_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let role = required_string_arg(args, 0, "escalate_to")?;
    let reason = required_string_arg(args, 1, "escalate_to")?;
    let keys = current_dispatch_keys();
    let request_id = next_request_id(HitlRequestKind::Escalation, keys.as_ref());
    let trace_id = keys
        .as_ref()
        .map(|keys| keys.trace_id.clone())
        .unwrap_or_else(new_trace_id);
    let log = ensure_hitl_event_log();
    let request = HitlRequestEnvelope {
        request_id: request_id.clone(),
        kind: HitlRequestKind::Escalation,
        agent: keys
            .as_ref()
            .map(|keys| keys.agent.clone())
            .unwrap_or_default(),
        trace_id: trace_id.clone(),
        run_id: crate::orchestration::current_mutation_session().and_then(|session| session.run_id),
        requested_at: now_rfc3339(),
        payload: json!({
            "role": role,
            "reason": reason,
            "capability_policy": escalation_capability_policy(),
        }),
    };
    create_request_waitpoint(&log, &request).await?;
    append_request(&log, &request).await?;
    maybe_notify_host(&request);
    maybe_apply_mock_response(HitlRequestKind::Escalation, &request_id, &request.payload).await?;

    match wait_for_request_waitpoint(&request_id, None).await? {
        WaitpointOutcome::Completed(record) => {
            let accepted_at = record.completed_at.clone();
            let reviewer = record.completed_by.clone();
            let accepted = record
                .value
                .as_ref()
                .and_then(|value| value.get("accepted"))
                .and_then(JsonValue::as_bool)
                .unwrap_or(true);
            Ok(crate::stdlib::json_to_vm_value(&json!({
                "request_id": request_id,
                "role": role,
                "reason": reason,
                "trace_id": trace_id,
                "status": if accepted { "accepted" } else { "pending" },
                "accepted_at": accepted_at,
                "reviewer": reviewer,
            })))
        }
        WaitpointOutcome::Timeout => Err(timeout_error(&request_id, HitlRequestKind::Escalation)),
        WaitpointOutcome::Cancelled {
            wait_id,
            waitpoint_ids,
            reason,
        } => Err(hitl_cancelled_error(
            &request_id,
            HitlRequestKind::Escalation,
            &wait_id,
            &waitpoint_ids,
            reason,
        )),
    }
}

async fn create_request_waitpoint(
    log: &Arc<AnyEventLog>,
    request: &HitlRequestEnvelope,
) -> Result<(), VmError> {
    create_waitpoint_on(
        log,
        Some(request.request_id.clone()),
        Some(json!({
            "kind": request.kind.as_str(),
            "agent": request.agent.clone(),
            "trace_id": request.trace_id.clone(),
            "requested_at": request.requested_at.clone(),
            "payload": request.payload.clone(),
        })),
    )
    .await?;
    Ok(())
}

async fn wait_for_request_waitpoint(
    request_id: &str,
    timeout: Option<StdDuration>,
) -> Result<WaitpointOutcome, VmError> {
    match wait_on_waitpoints(
        vec![request_id.to_string()],
        WaitpointWaitOptions { timeout },
    )
    .await
    {
        Ok(records) => Ok(WaitpointOutcome::Completed(
            records
                .into_iter()
                .next()
                .expect("single waitpoint wait result"),
        )),
        Err(WaitpointWaitFailure::Timeout { .. }) => Ok(WaitpointOutcome::Timeout),
        Err(WaitpointWaitFailure::Cancelled {
            wait_id,
            waitpoint_ids,
            reason,
        }) => Ok(WaitpointOutcome::Cancelled {
            wait_id,
            waitpoint_ids,
            reason,
        }),
        Err(WaitpointWaitFailure::Vm(error)) => {
            if let Some(outcome) = waitpoint_outcome_from_vm_error(&error) {
                return Ok(outcome);
            }
            Err(error)
        }
    }
}

fn waitpoint_outcome_from_vm_error(error: &VmError) -> Option<WaitpointOutcome> {
    let VmError::Thrown(VmValue::Dict(dict)) = error else {
        return None;
    };
    let name = dict.get("name").and_then(vm_string)?;
    match name {
        "WaitpointTimeoutError" => Some(WaitpointOutcome::Timeout),
        "WaitpointCancelledError" => Some(WaitpointOutcome::Cancelled {
            wait_id: dict
                .get("wait_id")
                .and_then(vm_string)
                .unwrap_or_default()
                .to_string(),
            waitpoint_ids: dict
                .get("waitpoint_ids")
                .and_then(vm_string_list)
                .unwrap_or_default(),
            reason: dict
                .get("reason")
                .and_then(vm_string)
                .map(ToString::to_string),
        }),
        _ => None,
    }
}

async fn finalize_hitl_response(
    log: &Arc<AnyEventLog>,
    kind: HitlRequestKind,
    response: &HitlHostResponse,
) -> Result<(), String> {
    match kind {
        HitlRequestKind::Question => {
            if waitpoint_is_terminal(log, &response.request_id).await? {
                return Ok(());
            }
            complete_waitpoint_on(
                log,
                &response.request_id,
                response.answer.clone(),
                response.reviewer.clone(),
                response.reason.clone(),
                response.metadata.clone(),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
        }
        HitlRequestKind::Escalation => {
            if !response.accepted.unwrap_or(false)
                || waitpoint_is_terminal(log, &response.request_id).await?
            {
                return Ok(());
            }
            complete_waitpoint_on(
                log,
                &response.request_id,
                Some(json!({
                    "accepted": true,
                    "reviewer": response.reviewer,
                    "reason": response.reason,
                    "responded_at": response.responded_at,
                })),
                response.reviewer.clone(),
                response.reason.clone(),
                response.metadata.clone(),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
        }
        HitlRequestKind::Approval | HitlRequestKind::DualControl => {
            if waitpoint_is_terminal(log, &response.request_id).await? {
                return Ok(());
            }
            let request = load_request_envelope(log, kind, &response.request_id)
                .await
                .map_err(|error| error.to_string())?;
            match resolve_approval_state(log, kind, &request)
                .await
                .map_err(|error| error.to_string())?
            {
                ApprovalResolution::Pending => Ok(()),
                ApprovalResolution::Approved(progress) => {
                    let record = approval_record_json(&progress);
                    append_named_event(
                        log,
                        kind,
                        approved_event_kind(kind),
                        &request.request_id,
                        &request.trace_id,
                        json!({
                            "request_id": request.request_id.clone(),
                            "record": record.clone(),
                        }),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    complete_waitpoint_on(
                        log,
                        &request.request_id,
                        Some(record),
                        response.reviewer.clone(),
                        progress.reason.clone(),
                        response.metadata.clone(),
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                }
                ApprovalResolution::Denied(denied) => {
                    append_named_event(
                        log,
                        kind,
                        denied_event_kind(kind),
                        &request.request_id,
                        &request.trace_id,
                        json!({
                            "request_id": request.request_id.clone(),
                            "reviewer": denied.reviewer.clone(),
                            "reason": denied.reason.clone(),
                        }),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    cancel_waitpoint_on(
                        log,
                        &request.request_id,
                        denied.reviewer.clone(),
                        denied.reason.clone(),
                        denied.metadata.clone(),
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                }
            }
        }
    }
}

async fn waitpoint_is_terminal(log: &Arc<AnyEventLog>, request_id: &str) -> Result<bool, String> {
    Ok(inspect_waitpoint_on(log, request_id)
        .await
        .map_err(|error| error.to_string())?
        .is_some_and(|record| record.status != WaitpointStatus::Open))
}

async fn load_request_envelope(
    log: &Arc<AnyEventLog>,
    kind: HitlRequestKind,
    request_id: &str,
) -> Result<HitlRequestEnvelope, VmError> {
    let topic = topic(kind)?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(log_error)?;
    events
        .into_iter()
        .filter(|(_, event)| event.kind == kind.request_event_kind())
        .find_map(|(_, event)| {
            if !event_matches_request(&event, request_id) {
                return None;
            }
            serde_json::from_value::<HitlRequestEnvelope>(event.payload).ok()
        })
        .ok_or_else(|| {
            VmError::Runtime(format!("missing HITL request envelope for '{request_id}'"))
        })
}

async fn resolve_approval_state(
    log: &Arc<AnyEventLog>,
    kind: HitlRequestKind,
    request: &HitlRequestEnvelope,
) -> Result<ApprovalResolution, VmError> {
    let quorum = approval_quorum_from_request(kind, request)?;
    let allowed_reviewers = approval_reviewers_from_request(kind, request)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut progress = ApprovalProgress {
        reviewers: BTreeSet::new(),
        signatures: Vec::new(),
        reason: None,
        approved_at: None,
    };
    let topic = topic(kind)?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(log_error)?;
    for (_, event) in events {
        if !event_matches_request(&event, &request.request_id)
            || event.kind != "hitl.response_received"
        {
            continue;
        }
        let response: HitlHostResponse = serde_json::from_value(event.payload)
            .map_err(|error| VmError::Runtime(error.to_string()))?;
        if let Some(reviewer) = response.reviewer.as_deref() {
            if !allowed_reviewers.is_empty() && !allowed_reviewers.contains(reviewer) {
                continue;
            }
            if progress.reviewers.contains(reviewer) {
                continue;
            }
        }
        if response.approved.unwrap_or(false) {
            if let Some(reviewer) = response.reviewer.clone() {
                let signed_at = response.responded_at.clone().unwrap_or_else(now_rfc3339);
                progress.reviewers.insert(reviewer.clone());
                progress.signatures.push(ApprovalSignature {
                    reviewer: reviewer.clone(),
                    signed_at: signed_at.clone(),
                    signature: response.signature.clone().unwrap_or_else(|| {
                        approval_receipt_signature(
                            &request.request_id,
                            &reviewer,
                            &signed_at,
                            true,
                            response.reason.as_deref(),
                        )
                    }),
                });
            }
            progress.reason = response.reason.clone();
            progress.approved_at = response.responded_at.clone();
            if progress.reviewers.len() as u32 >= quorum {
                return Ok(ApprovalResolution::Approved(progress));
            }
            continue;
        }
        return Ok(ApprovalResolution::Denied(response));
    }
    Ok(ApprovalResolution::Pending)
}

fn approval_quorum_from_request(
    kind: HitlRequestKind,
    request: &HitlRequestEnvelope,
) -> Result<u32, VmError> {
    let key = match kind {
        HitlRequestKind::DualControl => "n",
        _ => "quorum",
    };
    let quorum = request
        .payload
        .get(key)
        .and_then(JsonValue::as_u64)
        .unwrap_or(1);
    u32::try_from(quorum).map_err(|_| {
        VmError::Runtime(format!(
            "invalid quorum in HITL request '{}'",
            request.request_id
        ))
    })
}

fn approval_reviewers_from_request(
    kind: HitlRequestKind,
    request: &HitlRequestEnvelope,
) -> Vec<String> {
    let key = match kind {
        HitlRequestKind::DualControl => "approvers",
        _ => "reviewers",
    };
    request
        .payload
        .get(key)
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn approval_record_json(progress: &ApprovalProgress) -> JsonValue {
    json!({
        "approved": true,
        "reviewers": progress.reviewers.iter().cloned().collect::<Vec<_>>(),
        "approved_at": progress.approved_at.clone().unwrap_or_else(now_rfc3339),
        "reason": progress.reason,
        "signatures": progress.signatures,
    })
}

fn approval_receipt_signature(
    request_id: &str,
    reviewer: &str,
    signed_at: &str,
    approved: bool,
    reason: Option<&str>,
) -> String {
    let material = format!(
        "harn-hitl-approval-v1\nrequest_id:{request_id}\nreviewer:{reviewer}\nsigned_at:{signed_at}\napproved:{approved}\nreason:{}\n",
        reason.unwrap_or("")
    );
    let hash = sha2::Sha256::digest(material.as_bytes());
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

fn approval_record_from_waitpoint(
    record: &WaitpointRecord,
    builtin: &str,
) -> Result<VmValue, VmError> {
    record
        .value
        .as_ref()
        .map(crate::stdlib::json_to_vm_value)
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing approval record")))
}

async fn approval_wait_error(
    log: &Arc<AnyEventLog>,
    kind: HitlRequestKind,
    request_id: &str,
) -> VmError {
    if let Ok(Some(record)) = inspect_waitpoint_on(log, request_id).await {
        if record.status == WaitpointStatus::Cancelled
            && record.reason.as_deref() != Some("upstream_cancelled")
        {
            return approval_denied_error(
                request_id,
                HitlHostResponse {
                    request_id: request_id.to_string(),
                    answer: None,
                    approved: Some(false),
                    accepted: None,
                    reviewer: record.cancelled_by.clone(),
                    reason: record.reason.clone(),
                    metadata: record.metadata.clone(),
                    responded_at: record.cancelled_at.clone(),
                    signature: None,
                },
            );
        }
        if record.status == WaitpointStatus::Cancelled {
            return hitl_cancelled_error(
                request_id,
                kind,
                "",
                &[request_id.to_string()],
                record.reason.clone(),
            );
        }
    }
    hitl_cancelled_error(
        request_id,
        kind,
        "",
        &[request_id.to_string()],
        Some("upstream_cancelled".to_string()),
    )
}

async fn append_timeout_once(
    log: &Arc<AnyEventLog>,
    kind: HitlRequestKind,
    request_id: &str,
    trace_id: &str,
) -> Result<(), VmError> {
    if hitl_event_exists(log, kind, request_id, "hitl.timeout").await? {
        return Ok(());
    }
    append_timeout(log, kind, request_id, trace_id).await
}

async fn hitl_event_exists(
    log: &Arc<AnyEventLog>,
    kind: HitlRequestKind,
    request_id: &str,
    event_kind: &str,
) -> Result<bool, VmError> {
    let topic = topic(kind)?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(log_error)?;
    Ok(events
        .into_iter()
        .any(|(_, event)| event.kind == event_kind && event_matches_request(&event, request_id)))
}

fn approved_event_kind(kind: HitlRequestKind) -> &'static str {
    match kind {
        HitlRequestKind::DualControl => "hitl.dual_control_approved",
        _ => "hitl.approval_approved",
    }
}

fn denied_event_kind(kind: HitlRequestKind) -> &'static str {
    match kind {
        HitlRequestKind::DualControl => "hitl.dual_control_denied",
        _ => "hitl.approval_denied",
    }
}

async fn append_request(
    log: &Arc<AnyEventLog>,
    request: &HitlRequestEnvelope,
) -> Result<(), VmError> {
    let topic = topic(request.kind)?;
    log.append(
        &topic,
        LogEvent::new(
            request.kind.request_event_kind(),
            serde_json::to_value(request).map_err(|error| VmError::Runtime(error.to_string()))?,
        )
        .with_headers(request_headers(request)),
    )
    .await
    .map(|_| ())
    .map_err(log_error)
}

async fn append_named_event(
    log: &Arc<AnyEventLog>,
    kind: HitlRequestKind,
    event_kind: &str,
    request_id: &str,
    trace_id: &str,
    payload: JsonValue,
) -> Result<(), VmError> {
    let topic = topic(kind)?;
    let headers = headers_with_trace(request_id, trace_id);
    log.append(
        &topic,
        LogEvent::new(event_kind, payload).with_headers(headers),
    )
    .await
    .map(|_| ())
    .map_err(log_error)
}

async fn append_timeout(
    log: &Arc<AnyEventLog>,
    kind: HitlRequestKind,
    request_id: &str,
    trace_id: &str,
) -> Result<(), VmError> {
    append_named_event(
        log,
        kind,
        "hitl.timeout",
        request_id,
        trace_id,
        serde_json::to_value(HitlTimeoutRecord {
            request_id: request_id.to_string(),
            kind,
            trace_id: trace_id.to_string(),
            timed_out_at: now_rfc3339(),
        })
        .map_err(|error| VmError::Runtime(error.to_string()))?,
    )
    .await
}

async fn maybe_apply_mock_response(
    kind: HitlRequestKind,
    request_id: &str,
    request_payload: &JsonValue,
) -> Result<(), VmError> {
    let mut params = request_payload
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(key, value)| (key, crate::stdlib::json_to_vm_value(&value)))
        .collect::<BTreeMap<_, _>>();
    params.insert(
        "request_id".to_string(),
        VmValue::String(Rc::from(request_id.to_string())),
    );
    let Some(result) = dispatch_mock_host_call("hitl", kind.as_str(), &params) else {
        return Ok(());
    };
    let value = result?;
    let responses = match value {
        VmValue::List(items) => items.iter().cloned().collect::<Vec<_>>(),
        other => vec![other],
    };
    for response in responses {
        let response_dict = response.as_dict().ok_or_else(|| {
            VmError::Runtime(format!(
                "mocked HITL {} response must be a dict or list<dict>",
                kind.as_str()
            ))
        })?;
        let hitl_response = parse_hitl_response_dict(request_id, response_dict)?;
        append_hitl_response(None, hitl_response)
            .await
            .map_err(VmError::Runtime)?;
    }
    Ok(())
}

fn parse_hitl_response_dict(
    request_id: &str,
    response_dict: &BTreeMap<String, VmValue>,
) -> Result<HitlHostResponse, VmError> {
    Ok(HitlHostResponse {
        request_id: request_id.to_string(),
        answer: response_dict
            .get("answer")
            .map(crate::llm::vm_value_to_json),
        approved: response_dict.get("approved").and_then(vm_bool),
        accepted: response_dict.get("accepted").and_then(vm_bool),
        reviewer: response_dict.get("reviewer").map(VmValue::display),
        reason: response_dict.get("reason").map(VmValue::display),
        metadata: response_dict
            .get("metadata")
            .map(crate::llm::vm_value_to_json),
        responded_at: response_dict.get("responded_at").map(VmValue::display),
        signature: response_dict.get("signature").map(VmValue::display),
    })
}

fn maybe_notify_host(request: &HitlRequestEnvelope) {
    let Some(bridge) = clone_async_builtin_child_vm().and_then(|vm| vm.bridge.clone()) else {
        return;
    };
    bridge.notify(
        "harn.hitl.requested",
        serde_json::to_value(request).unwrap_or(JsonValue::Null),
    );
}

fn parse_ask_user_options(value: Option<&VmValue>) -> Result<AskUserOptions, VmError> {
    let Some(value) = value else {
        return Ok(AskUserOptions {
            schema: None,
            timeout: Some(default_question_timeout()),
            default: None,
        });
    };
    let dict = value
        .as_dict()
        .ok_or_else(|| VmError::Runtime("ask_user: options must be a dict".to_string()))?;
    Ok(AskUserOptions {
        schema: dict
            .get("schema")
            .cloned()
            .filter(|value| !matches!(value, VmValue::Nil)),
        timeout: dict
            .get("timeout")
            .map(parse_duration_value)
            .transpose()?
            .or_else(|| Some(default_question_timeout())),
        default: dict
            .get("default")
            .cloned()
            .filter(|value| !matches!(value, VmValue::Nil)),
    })
}

fn default_question_timeout() -> StdDuration {
    StdDuration::from_millis(HITL_QUESTION_TIMEOUT_MS)
}

fn escalation_capability_policy() -> JsonValue {
    crate::orchestration::current_execution_policy()
        .and_then(|policy| serde_json::to_value(policy).ok())
        .unwrap_or(JsonValue::Null)
}

fn parse_approval_options(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<ApprovalOptions, VmError> {
    let dict = match value {
        None => None,
        Some(VmValue::Dict(dict)) => Some(dict),
        Some(_) => {
            return Err(VmError::Runtime(format!(
                "{builtin}: options must be a dict"
            )))
        }
    };
    let quorum = dict
        .and_then(|dict| dict.get("quorum"))
        .and_then(VmValue::as_int)
        .unwrap_or(1);
    if quorum <= 0 {
        return Err(VmError::Runtime(format!(
            "{builtin}: quorum must be positive"
        )));
    }
    let reviewers = optional_string_list(dict.and_then(|dict| dict.get("reviewers")), builtin)?;
    let deadline = dict
        .and_then(|dict| dict.get("deadline"))
        .map(parse_duration_value)
        .transpose()?
        .unwrap_or_else(|| StdDuration::from_millis(HITL_APPROVAL_TIMEOUT_MS));
    Ok(ApprovalOptions {
        detail: dict.and_then(|dict| dict.get("detail")).cloned(),
        quorum: quorum as u32,
        reviewers,
        deadline,
    })
}

fn required_string_arg(args: &[VmValue], idx: usize, builtin: &str) -> Result<String, VmError> {
    args.get(idx)
        .map(VmValue::display)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: expected string argument at {idx}")))
}

fn required_positive_int_arg(args: &[VmValue], idx: usize, builtin: &str) -> Result<i64, VmError> {
    let value = args
        .get(idx)
        .and_then(VmValue::as_int)
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: expected int argument at {idx}")))?;
    if value <= 0 {
        return Err(VmError::Runtime(format!(
            "{builtin}: expected a positive int at {idx}"
        )));
    }
    Ok(value)
}

fn optional_string_list(value: Option<&VmValue>, builtin: &str) -> Result<Vec<String>, VmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        VmValue::List(list) => Ok(list.iter().map(VmValue::display).collect()),
        _ => Err(VmError::Runtime(format!(
            "{builtin}: expected list<string>"
        ))),
    }
}

fn parse_duration_value(value: &VmValue) -> Result<StdDuration, VmError> {
    match value {
        VmValue::Duration(ms) if *ms >= 0 => Ok(StdDuration::from_millis(*ms as u64)),
        VmValue::Int(ms) if *ms >= 0 => Ok(StdDuration::from_millis(*ms as u64)),
        VmValue::Float(ms) if *ms >= 0.0 => Ok(StdDuration::from_millis(*ms as u64)),
        _ => Err(VmError::Runtime(
            "expected a duration or millisecond count".to_string(),
        )),
    }
}

fn ensure_hitl_event_log() -> Arc<AnyEventLog> {
    active_event_log()
        .unwrap_or_else(|| install_memory_for_current_thread(HITL_EVENT_LOG_QUEUE_DEPTH))
}

fn ensure_hitl_event_log_for(base_dir: Option<&Path>) -> Result<Arc<AnyEventLog>, String> {
    if let Some(log) = active_event_log() {
        return Ok(log);
    }
    let Some(base_dir) = base_dir else {
        return Ok(install_memory_for_current_thread(
            HITL_EVENT_LOG_QUEUE_DEPTH,
        ));
    };
    install_default_for_base_dir(base_dir).map_err(|error| error.to_string())
}

fn current_dispatch_keys() -> Option<DispatchKeys> {
    let context = current_dispatch_context()?;
    let stable_base = context
        .replay_of_event_id
        .clone()
        .unwrap_or_else(|| context.trigger_event.id.0.clone());
    let instance_key = format!(
        "{}::{}",
        context.trigger_event.id.0,
        context.replay_of_event_id.as_deref().unwrap_or("live")
    );
    Some(DispatchKeys {
        instance_key,
        stable_base,
        agent: context.agent_id,
        trace_id: context.trigger_event.trace_id.0,
    })
}

fn next_request_id(kind: HitlRequestKind, dispatch_keys: Option<&DispatchKeys>) -> String {
    if let Some(keys) = dispatch_keys {
        let seq = REQUEST_SEQUENCE.with(|slot| {
            let mut state = slot.borrow_mut();
            if state.instance_key != keys.instance_key {
                state.instance_key = keys.instance_key.clone();
                state.next_seq = 0;
            }
            state.next_seq += 1;
            state.next_seq
        });
        return format!("hitl_{}_{}_{}", kind.as_str(), keys.stable_base, seq);
    }
    format!("hitl_{}_{}", kind.as_str(), Uuid::now_v7())
}

fn request_headers(request: &HitlRequestEnvelope) -> BTreeMap<String, String> {
    let mut headers = headers_with_trace(&request.request_id, &request.trace_id);
    if let Some(run_id) = request.run_id.as_ref() {
        headers.insert("run_id".to_string(), run_id.clone());
    }
    headers
}

fn response_headers(request_id: &str) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("request_id".to_string(), request_id.to_string());
    headers
}

fn headers_with_trace(request_id: &str, trace_id: &str) -> BTreeMap<String, String> {
    let mut headers = response_headers(request_id);
    headers.insert("trace_id".to_string(), trace_id.to_string());
    headers
}

fn topic(kind: HitlRequestKind) -> Result<Topic, VmError> {
    Topic::new(kind.topic()).map_err(|error| VmError::Runtime(error.to_string()))
}

fn event_matches_request(event: &LogEvent, request_id: &str) -> bool {
    event
        .headers
        .get("request_id")
        .is_some_and(|value| value == request_id)
        || event
            .payload
            .get("request_id")
            .and_then(JsonValue::as_str)
            .is_some_and(|value| value == request_id)
}

fn approval_denied_error(request_id: &str, response: HitlHostResponse) -> VmError {
    VmError::Thrown(crate::stdlib::json_to_vm_value(&json!({
        "name": "ApprovalDeniedError",
        "category": "generic",
        "message": response.reason.clone().unwrap_or_else(|| "approval was denied".to_string()),
        "request_id": request_id,
        "reviewers": response.reviewer.into_iter().collect::<Vec<_>>(),
        "reason": response.reason,
    })))
}

fn hitl_cancelled_error(
    request_id: &str,
    kind: HitlRequestKind,
    wait_id: &str,
    waitpoint_ids: &[String],
    reason: Option<String>,
) -> VmError {
    let _ = categorized_error("HITL cancelled", ErrorCategory::Cancelled);
    let message = reason
        .clone()
        .unwrap_or_else(|| format!("{} cancelled", kind.as_str()));
    VmError::Thrown(crate::stdlib::json_to_vm_value(&json!({
        "name": "HumanCancelledError",
        "category": ErrorCategory::Cancelled.as_str(),
        "message": message,
        "request_id": request_id,
        "kind": kind.as_str(),
        "wait_id": wait_id,
        "waitpoint_ids": waitpoint_ids,
        "reason": reason,
    })))
}

fn timeout_error(request_id: &str, kind: HitlRequestKind) -> VmError {
    let _ = categorized_error("HITL timed out", ErrorCategory::Timeout);
    VmError::Thrown(crate::stdlib::json_to_vm_value(&json!({
        "name": "HumanTimeoutError",
        "category": ErrorCategory::Timeout.as_str(),
        "message": format!("{} timed out", kind.as_str()),
        "request_id": request_id,
        "kind": kind.as_str(),
    })))
}

fn coerce_like_default(value: &VmValue, default: &VmValue) -> VmValue {
    match default {
        VmValue::Int(_) => match value {
            VmValue::Int(_) => value.clone(),
            VmValue::Float(number) => VmValue::Int(*number as i64),
            VmValue::String(text) => text
                .parse::<i64>()
                .map(VmValue::Int)
                .unwrap_or_else(|_| default.clone()),
            _ => default.clone(),
        },
        VmValue::Float(_) => match value {
            VmValue::Float(_) => value.clone(),
            VmValue::Int(number) => VmValue::Float(*number as f64),
            VmValue::String(text) => text
                .parse::<f64>()
                .map(VmValue::Float)
                .unwrap_or_else(|_| default.clone()),
            _ => default.clone(),
        },
        VmValue::Bool(_) => match value {
            VmValue::Bool(_) => value.clone(),
            VmValue::String(text) if text.eq_ignore_ascii_case("true") => VmValue::Bool(true),
            VmValue::String(text) if text.eq_ignore_ascii_case("false") => VmValue::Bool(false),
            _ => default.clone(),
        },
        VmValue::String(_) => VmValue::String(Rc::from(value.display())),
        VmValue::Duration(_) => match value {
            VmValue::Duration(_) => value.clone(),
            VmValue::Int(ms) => VmValue::Duration(*ms),
            _ => default.clone(),
        },
        VmValue::Nil => value.clone(),
        _ => {
            if value.type_name() == default.type_name() {
                value.clone()
            } else {
                default.clone()
            }
        }
    }
}

fn log_error(error: impl std::fmt::Display) -> VmError {
    VmError::Runtime(error.to_string())
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().to_string())
}

fn new_trace_id() -> String {
    format!("trace_{}", Uuid::now_v7())
}

fn vm_bool(value: &VmValue) -> Option<bool> {
    match value {
        VmValue::Bool(flag) => Some(*flag),
        _ => None,
    }
}

fn vm_string(value: &VmValue) -> Option<&str> {
    match value {
        VmValue::String(text) => Some(text.as_ref()),
        _ => None,
    }
}

fn vm_string_list(value: &VmValue) -> Option<Vec<String>> {
    match value {
        VmValue::List(values) => Some(values.iter().map(VmValue::display).collect()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HITL_APPROVALS_TOPIC, HITL_DUAL_CONTROL_TOPIC, HITL_ESCALATIONS_TOPIC, HITL_QUESTIONS_TOPIC,
    };
    use crate::event_log::{install_default_for_base_dir, EventLog, Topic};
    use crate::{compile_source, register_vm_stdlib, reset_thread_local_state, Vm, VmError};

    async fn execute_hitl_script(
        base_dir: &std::path::Path,
        source: &str,
    ) -> Result<(String, Vec<String>, Vec<String>, Vec<String>, Vec<String>), VmError> {
        reset_thread_local_state();
        let log = install_default_for_base_dir(base_dir).expect("install event log");
        let chunk = compile_source(source).expect("compile source");
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.set_source_dir(base_dir);
        vm.execute(&chunk).await?;
        let output = vm.output().trim_end().to_string();
        let question_events = event_kinds(log.clone(), HITL_QUESTIONS_TOPIC).await;
        let approval_events = event_kinds(log.clone(), HITL_APPROVALS_TOPIC).await;
        let dual_control_events = event_kinds(log.clone(), HITL_DUAL_CONTROL_TOPIC).await;
        let escalation_events = event_kinds(log, HITL_ESCALATIONS_TOPIC).await;
        Ok((
            output,
            question_events,
            approval_events,
            dual_control_events,
            escalation_events,
        ))
    }

    async fn event_kinds(
        log: std::sync::Arc<crate::event_log::AnyEventLog>,
        topic: &str,
    ) -> Vec<String> {
        log.read_range(&Topic::new(topic).expect("valid topic"), None, usize::MAX)
            .await
            .expect("read topic")
            .into_iter()
            .map(|(_, event)| event.kind)
            .collect()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ask_user_coerces_to_default_type_and_logs_events() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let source = r#"
pipeline test(task) {
  host_mock("hitl", "question", {answer: "9"})
  let answer: int = ask_user("Pick a number", {default: 0})
  println(answer)
}
"#;
                let (
                    output,
                    question_events,
                    approval_events,
                    dual_control_events,
                    escalation_events,
                ) = execute_hitl_script(dir.path(), source)
                    .await
                    .expect("script succeeds");
                assert_eq!(output, "9");
                assert_eq!(
                    question_events,
                    vec![
                        "hitl.question_asked".to_string(),
                        "hitl.response_received".to_string()
                    ]
                );
                assert!(approval_events.is_empty());
                assert!(dual_control_events.is_empty());
                assert!(escalation_events.is_empty());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_approval_waits_for_quorum_and_emits_a_record() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let source = r#"
pipeline test(task) {
  host_mock("hitl", "approval", [
    {approved: true, reviewer: "alice", reason: "ok"},
    {approved: true, reviewer: "bob", reason: "ship it"},
  ])
  let record = request_approval(
    "deploy production",
    {quorum: 2, reviewers: ["alice", "bob", "carol"]},
  )
  println(record.approved)
  println(len(record.reviewers))
  println(record.reviewers[0])
  println(record.reviewers[1])
}
"#;
                let (_, _, approval_events, _, _) = execute_hitl_script(dir.path(), source)
                    .await
                    .expect("script succeeds");
                assert_eq!(
                    approval_events,
                    vec![
                        "hitl.approval_requested".to_string(),
                        "hitl.response_received".to_string(),
                        "hitl.response_received".to_string(),
                        "hitl.approval_approved".to_string(),
                    ]
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_approval_surfaces_denials_as_typed_errors() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let source = r#"
pipeline test(task) {
  host_mock("hitl", "approval", {approved: false, reviewer: "alice", reason: "unsafe"})
  let denied = try {
    request_approval("drop table", {reviewers: ["alice"]})
  }
  println(is_err(denied))
  println(unwrap_err(denied).name)
  println(unwrap_err(denied).reason)
}
"#;
                let (output, _, approval_events, _, _) = execute_hitl_script(dir.path(), source)
                    .await
                    .expect("script succeeds");
                assert_eq!(output, "true\nApprovalDeniedError\nunsafe");
                assert_eq!(
                    approval_events,
                    vec![
                        "hitl.approval_requested".to_string(),
                        "hitl.response_received".to_string(),
                        "hitl.approval_denied".to_string(),
                    ]
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dual_control_executes_action_after_quorum() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let source = r#"
pipeline test(task) {
  host_mock("hitl", "dual_control", [
    {approved: true, reviewer: "alice"},
    {approved: true, reviewer: "bob"},
  ])
  let result = dual_control(2, 3, { -> "launched" }, ["alice", "bob", "carol"])
  println(result)
}
"#;
                let (output, _, _, dual_control_events, _) =
                    execute_hitl_script(dir.path(), source)
                        .await
                        .expect("script succeeds");
                assert_eq!(output, "launched");
                assert_eq!(
                    dual_control_events,
                    vec![
                        "hitl.dual_control_requested".to_string(),
                        "hitl.response_received".to_string(),
                        "hitl.response_received".to_string(),
                        "hitl.dual_control_approved".to_string(),
                        "hitl.dual_control_executed".to_string(),
                    ]
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn escalate_to_waits_for_acceptance_event() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let source = r#"
pipeline test(task) {
  host_mock("hitl", "escalation", {accepted: true, reviewer: "lead", reason: "taking over"})
  let handle = escalate_to("admin", "need override")
  println(handle.status)
  println(handle.reviewer)
}
"#;
                let (output, _, _, _, escalation_events) = execute_hitl_script(dir.path(), source)
                    .await
                    .expect("script succeeds");
                assert_eq!(output, "accepted\nlead");
                assert_eq!(
                    escalation_events,
                    vec![
                        "hitl.escalation_issued".to_string(),
                        "hitl.escalation_accepted".to_string(),
                    ]
                );
            })
            .await;
    }
}
