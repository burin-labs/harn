use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
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
use crate::runtime_limits::RuntimeLimits;
use crate::schema::schema_expect_value;
use crate::stdlib::host::dispatch_mock_host_call;
use crate::stdlib::macros::{harn_builtin, BuiltinSignature, Param, VmBuiltinDef, TY_ANY, TY_DICT};
use crate::stdlib::options::{duration_from_value, ErrorKind};
use crate::stdlib::waitpoint::{
    cancel_waitpoint_on, complete_waitpoint_on, create_waitpoint_on, inspect_waitpoint_on,
    wait_on_waitpoints, WaitpointRecord, WaitpointStatus, WaitpointWaitFailure,
    WaitpointWaitOptions,
};
use crate::triggers::dispatcher::current_dispatch_context;
use crate::value::{categorized_error, ErrorCategory, VmError, VmValue};
use crate::vm::{AsyncBuiltinCtx, Vm};

const HITL_EVENT_LOG_QUEUE_DEPTH: usize = RuntimeLimits::DEFAULT.default_event_log_queue_depth;
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub id: String,
    pub action: String,
    #[serde(default)]
    pub args: JsonValue,
    pub principal: String,
    pub requested_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    pub approvers_required: u32,
    #[serde(default)]
    pub evidence_refs: Vec<JsonValue>,
    #[serde(default)]
    pub undo_metadata: JsonValue,
    #[serde(default)]
    pub capabilities_requested: Vec<String>,
}

impl ApprovalRequest {
    pub fn new(
        id: impl Into<String>,
        action: impl Into<String>,
        args: JsonValue,
        principal: impl Into<String>,
        requested_at: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            action: action.into(),
            args,
            principal: principal.into(),
            requested_at: requested_at.into(),
            deadline: None,
            approvers_required: 1,
            evidence_refs: Vec::new(),
            undo_metadata: JsonValue::Null,
            capabilities_requested: Vec::new(),
        }
    }
}

pub(crate) fn approval_request_for_host_permission(
    id: impl Into<String>,
    action: impl Into<String>,
    args: JsonValue,
    principal: impl Into<String>,
    evidence_refs: Vec<JsonValue>,
    undo_metadata: JsonValue,
    capabilities_requested: Vec<String>,
) -> ApprovalRequest {
    let mut request = ApprovalRequest::new(id, action, args, principal, now_rfc3339());
    request.evidence_refs = evidence_refs;
    request.undo_metadata = undo_metadata;
    request.capabilities_requested = capabilities_requested;
    request
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
    args: Option<VmValue>,
    quorum: u32,
    reviewers: Vec<String>,
    deadline: StdDuration,
    principal: Option<String>,
    evidence_refs: Vec<JsonValue>,
    undo_metadata: Option<JsonValue>,
    capabilities_requested: Vec<String>,
}

#[derive(Clone, Debug)]
struct ApprovalProgress {
    request_id: String,
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

// `Completed` carries the full `WaitpointRecord`, which dominates the
// enum's size — boxing it would force every match arm to indirect even
// though the enum is dropped within nanoseconds of being constructed
// (it's a local return type for the waitpoint poll loop, never stored).
// Surfaced by the host-target compile of `harn-vm` introduced when
// `harn-cli`'s build script gained `harn-vm` as a build-dep for the
// AOT bytecode embedding pass (G7 / harn#2300).
#[allow(clippy::large_enum_variant)]
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
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &ASK_USER_BUILTIN_DEF,
    &REQUEST_APPROVAL_BUILTIN_DEF,
    &DUAL_CONTROL_BUILTIN_DEF,
    &ESCALATE_TO_BUILTIN_DEF,
];

#[harn_builtin(
    sig = "ask_user(prompt: string, options?: dict) -> any",
    kind = "async",
    category = "hitl"
)]
async fn ask_user_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    ask_user_impl(Some(&ctx), &args).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("request_approval", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "hitl"
)]
async fn request_approval_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    request_approval_impl(Some(&ctx), &args).await
}

#[harn_builtin(
    sig = "dual_control(n: int, m: int, action: closure, approvers?: list) -> dict",
    kind = "async",
    category = "hitl"
)]
async fn dual_control_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    dual_control_impl(&ctx, &args).await
}

#[harn_builtin(
    sig = "escalate_to(role: string, reason: string) -> dict",
    kind = "async",
    category = "hitl"
)]
async fn escalate_to_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    escalate_to_impl(Some(&ctx), &args).await
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
    let agent = agent.into();
    let requested_at_time = OffsetDateTime::now_utc();
    let requested_at = format_rfc3339(requested_at_time);
    let mut approval_request = ApprovalRequest::new(
        request_id.clone(),
        action.into(),
        detail.clone(),
        agent.clone(),
        requested_at.clone(),
    );
    approval_request.deadline = deadline_after(
        requested_at_time,
        StdDuration::from_millis(HITL_APPROVAL_TIMEOUT_MS),
    );
    approval_request.approvers_required = 1;
    let approval_request_json = serde_json::to_value(&approval_request)
        .map_err(|error| VmError::Runtime(error.to_string()))?;
    let request = HitlRequestEnvelope {
        request_id: request_id.clone(),
        kind: HitlRequestKind::Approval,
        agent,
        trace_id: trace_id.clone(),
        run_id: None,
        requested_at: requested_at.clone(),
        payload: json!({
            "approval_request": approval_request_json,
            "id": approval_request.id,
            "action": approval_request.action,
            "args": approval_request.args,
            "principal": approval_request.principal,
            "requested_at": requested_at,
            "deadline": approval_request.deadline,
            "approvers_required": approval_request.approvers_required,
            "evidence_refs": approval_request.evidence_refs,
            "undo_metadata": approval_request.undo_metadata,
            "capabilities_requested": approval_request.capabilities_requested,
            "detail": detail,
            "quorum": 1,
            "reviewers": reviewers,
            "deadline_ms": HITL_APPROVAL_TIMEOUT_MS,
        }),
    };
    create_request_waitpoint(log, &request).await?;
    append_request(log, &request).await?;
    maybe_notify_host(None, &request);
    Ok(request_id)
}

async fn ask_user_impl(
    ctx: Option<&AsyncBuiltinCtx>,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
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
    maybe_notify_host(ctx, &request);
    emit_hitl_requested(&request);
    maybe_apply_mock_response(HitlRequestKind::Question, &request_id, &request.payload).await?;

    match wait_for_request_waitpoint_with_events(
        &request_id,
        HitlRequestKind::Question,
        options.timeout,
    )
    .await?
    {
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

async fn request_approval_impl(
    ctx: Option<&AsyncBuiltinCtx>,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    let action = required_string_arg(args, 0, "request_approval")?;
    let options = parse_approval_options(args.get(1), "request_approval")?;
    let keys = current_dispatch_keys();
    let request_id = next_request_id(HitlRequestKind::Approval, keys.as_ref());
    let trace_id = keys
        .as_ref()
        .map(|keys| keys.trace_id.clone())
        .unwrap_or_else(new_trace_id);
    let agent = keys
        .as_ref()
        .map(|keys| keys.agent.clone())
        .unwrap_or_default();
    let requested_at_time = OffsetDateTime::now_utc();
    let requested_at = format_rfc3339(requested_at_time);
    let principal = options.principal.clone().unwrap_or_else(|| agent.clone());
    let approval_args = options
        .args
        .as_ref()
        .or(options.detail.as_ref())
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(JsonValue::Null);
    let mut approval_request = ApprovalRequest::new(
        request_id.clone(),
        action.clone(),
        approval_args,
        principal,
        requested_at.clone(),
    );
    approval_request.deadline = deadline_after(requested_at_time, options.deadline);
    approval_request.approvers_required = options.quorum;
    approval_request.evidence_refs = options.evidence_refs.clone();
    approval_request.undo_metadata = options
        .undo_metadata
        .clone()
        .or_else(|| {
            crate::orchestration::current_mutation_session()
                .and_then(|session| serde_json::to_value(session).ok())
        })
        .unwrap_or(JsonValue::Null);
    approval_request.capabilities_requested = options.capabilities_requested.clone();
    let approval_request_json = serde_json::to_value(&approval_request)
        .map_err(|error| VmError::Runtime(error.to_string()))?;
    let log = ensure_hitl_event_log();
    let request = HitlRequestEnvelope {
        request_id: request_id.clone(),
        kind: HitlRequestKind::Approval,
        agent,
        trace_id: trace_id.clone(),
        run_id: crate::orchestration::current_mutation_session().and_then(|session| session.run_id),
        requested_at: requested_at.clone(),
        payload: json!({
            "approval_request": approval_request_json,
            "id": approval_request.id,
            "action": action,
            "args": approval_request.args,
            "principal": approval_request.principal,
            "requested_at": requested_at,
            "deadline": approval_request.deadline,
            "approvers_required": approval_request.approvers_required,
            "evidence_refs": approval_request.evidence_refs,
            "undo_metadata": approval_request.undo_metadata,
            "capabilities_requested": approval_request.capabilities_requested,
            "detail": options.detail.as_ref().map(crate::llm::vm_value_to_json),
            "quorum": options.quorum,
            "reviewers": options.reviewers,
            "deadline_ms": options.deadline.as_millis() as u64,
        }),
    };
    create_request_waitpoint(&log, &request).await?;
    append_request(&log, &request).await?;
    maybe_notify_host(ctx, &request);
    emit_hitl_requested(&request);
    maybe_apply_mock_response(HitlRequestKind::Approval, &request_id, &request.payload).await?;

    match wait_for_request_waitpoint_with_events(
        &request_id,
        HitlRequestKind::Approval,
        Some(options.deadline),
    )
    .await?
    {
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

pub(crate) async fn request_approval_for_side_effect(
    action: &str,
    detail: JsonValue,
    principal: String,
    reviewers: Vec<String>,
    capabilities_requested: Vec<String>,
) -> Result<VmValue, VmError> {
    let mut options = crate::value::DictMap::new();
    options.insert(
        crate::value::intern_key("args"),
        crate::stdlib::json_to_vm_value(&detail),
    );
    options.insert(
        crate::value::intern_key("detail"),
        crate::stdlib::json_to_vm_value(&detail),
    );
    options.put_str("principal", principal);
    options.insert(
        crate::value::intern_key("reviewers"),
        VmValue::List(std::sync::Arc::new(
            reviewers
                .into_iter()
                .map(|reviewer| VmValue::String(arcstr::ArcStr::from(reviewer)))
                .collect(),
        )),
    );
    options.insert(
        crate::value::intern_key("capabilities_requested"),
        VmValue::List(std::sync::Arc::new(
            capabilities_requested
                .into_iter()
                .map(|capability| VmValue::String(arcstr::ArcStr::from(capability)))
                .collect(),
        )),
    );
    let args = vec![
        VmValue::String(arcstr::ArcStr::from(action.to_string())),
        VmValue::dict(options),
    ];
    request_approval_impl(None, &args).await
}

async fn dual_control_impl(ctx: &AsyncBuiltinCtx, args: &[VmValue]) -> Result<VmValue, VmError> {
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
    let agent = keys
        .as_ref()
        .map(|keys| keys.agent.clone())
        .unwrap_or_default();
    let requested_at_time = OffsetDateTime::now_utc();
    let requested_at = format_rfc3339(requested_at_time);
    let mut approval_request = ApprovalRequest::new(
        request_id.clone(),
        action_name.clone(),
        json!({
            "n": n,
            "m": m,
            "approvers": approvers.clone(),
        }),
        agent.clone(),
        requested_at.clone(),
    );
    approval_request.deadline = deadline_after(
        requested_at_time,
        StdDuration::from_millis(HITL_APPROVAL_TIMEOUT_MS),
    );
    approval_request.approvers_required = n as u32;
    approval_request.undo_metadata = crate::orchestration::current_mutation_session()
        .and_then(|session| serde_json::to_value(session).ok())
        .unwrap_or(JsonValue::Null);
    let approval_request_json = serde_json::to_value(&approval_request)
        .map_err(|error| VmError::Runtime(error.to_string()))?;
    let log = ensure_hitl_event_log();
    let request = HitlRequestEnvelope {
        request_id: request_id.clone(),
        kind: HitlRequestKind::DualControl,
        agent,
        trace_id: trace_id.clone(),
        run_id: crate::orchestration::current_mutation_session().and_then(|session| session.run_id),
        requested_at: requested_at.clone(),
        payload: json!({
            "approval_request": approval_request_json,
            "id": approval_request.id,
            "args": approval_request.args,
            "principal": approval_request.principal,
            "requested_at": requested_at,
            "deadline": approval_request.deadline,
            "approvers_required": approval_request.approvers_required,
            "evidence_refs": approval_request.evidence_refs,
            "undo_metadata": approval_request.undo_metadata,
            "capabilities_requested": approval_request.capabilities_requested,
            "n": n,
            "m": m,
            "action": action_name,
            "approvers": approvers,
            "deadline_ms": HITL_APPROVAL_TIMEOUT_MS,
        }),
    };
    create_request_waitpoint(&log, &request).await?;
    append_request(&log, &request).await?;
    maybe_notify_host(Some(ctx), &request);
    emit_hitl_requested(&request);
    maybe_apply_mock_response(HitlRequestKind::DualControl, &request_id, &request.payload).await?;

    match wait_for_request_waitpoint_with_events(
        &request_id,
        HitlRequestKind::DualControl,
        Some(StdDuration::from_millis(HITL_APPROVAL_TIMEOUT_MS)),
    )
    .await?
    {
        WaitpointOutcome::Completed(record) => {
            let _ = approval_record_from_waitpoint(&record, "dual_control")?;
            let mut vm = ctx.child_vm();
            let result = vm.call_closure_pub(&action, &[]).await?;
            ctx.forward_output(&vm.take_output());

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

async fn escalate_to_impl(
    ctx: Option<&AsyncBuiltinCtx>,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
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
    maybe_notify_host(ctx, &request);
    emit_hitl_requested(&request);
    maybe_apply_mock_response(HitlRequestKind::Escalation, &request_id, &request.payload).await?;

    match wait_for_request_waitpoint_with_events(&request_id, HitlRequestKind::Escalation, None)
        .await?
    {
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
        request_id: request.request_id.clone(),
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
        .or_else(|| request.payload.get("approvers_required"))
        .or_else(|| {
            request
                .payload
                .get("approval_request")
                .and_then(|approval| approval.get("approvers_required"))
        })
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
        .or_else(|| {
            request
                .payload
                .get("approval_request")
                .and_then(|approval| approval.get("reviewers"))
        })
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
        "request_id": progress.request_id.clone(),
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
                    responded_at: record.cancelled_at,
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
                record.reason,
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
        .map(|(key, value)| {
            (
                crate::value::intern_key(&key),
                crate::stdlib::json_to_vm_value(&value),
            )
        })
        .collect::<crate::value::DictMap>();
    params.put_str("request_id", request_id);
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
    response_dict: &crate::value::DictMap,
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

fn maybe_notify_host(ctx: Option<&AsyncBuiltinCtx>, request: &HitlRequestEnvelope) {
    let Some(bridge) = ctx.and_then(|ctx| ctx.child_vm().bridge.clone()) else {
        return;
    };
    bridge.notify(
        "harn.hitl.requested",
        serde_json::to_value(request).unwrap_or(JsonValue::Null),
    );
}

/// Emit a `HitlRequested` `AgentEvent` so transport adapters
/// (currently the A2A `A2aWorkerSink`) can flip a task into
/// `input-required` while the script is suspended on the waitpoint.
/// No-op when there is no current agent session — the bridge-level
/// `harn.hitl.requested` notification still fires for hosts that drive
/// HITL UX through the bridge.
fn emit_hitl_requested(request: &HitlRequestEnvelope) {
    let Some(session_id) = crate::agent_sessions::current_session_id() else {
        return;
    };
    crate::agent_events::emit_event(&crate::agent_events::AgentEvent::HitlRequested {
        session_id,
        request_id: request.request_id.clone(),
        kind: request.kind.as_str().to_string(),
        payload: request.payload.clone(),
    });
}

/// Companion to `emit_hitl_requested`: notifies sinks that the
/// suspended waitpoint has resolved so a paused task can flip back
/// out of `input-required`. `outcome` is one of `"answered"`,
/// `"timeout"`, `"cancelled"`, or `"error"`.
fn emit_hitl_resolved(request_id: &str, kind: HitlRequestKind, outcome: &str) {
    let Some(session_id) = crate::agent_sessions::current_session_id() else {
        return;
    };
    crate::agent_events::emit_event(&crate::agent_events::AgentEvent::HitlResolved {
        session_id,
        request_id: request_id.to_string(),
        kind: kind.as_str().to_string(),
        outcome: outcome.to_string(),
    });
}

/// Wrapper around `wait_for_request_waitpoint` that emits the
/// canonical `HitlResolved` `AgentEvent` regardless of which terminal
/// branch the waitpoint takes (response / timeout / cancellation /
/// error). Pair-emitted with `emit_hitl_requested` so transport
/// adapters can bracket the `input-required` pause cleanly without
/// each `*_impl` having to duplicate the emission at every match arm.
async fn wait_for_request_waitpoint_with_events(
    request_id: &str,
    kind: HitlRequestKind,
    timeout: Option<StdDuration>,
) -> Result<WaitpointOutcome, VmError> {
    let outcome = wait_for_request_waitpoint(request_id, timeout).await;
    let label = match &outcome {
        Ok(WaitpointOutcome::Completed(_)) => "answered",
        Ok(WaitpointOutcome::Timeout) => "timeout",
        Ok(WaitpointOutcome::Cancelled { .. }) => "cancelled",
        Err(_) => "error",
    };
    emit_hitl_resolved(request_id, kind, label);
    outcome
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
    let capabilities_requested = optional_string_list(
        dict.and_then(|dict| dict.get("capabilities_requested")),
        builtin,
    )?;
    let evidence_refs = dict
        .and_then(|dict| dict.get("evidence_refs"))
        .map(|value| match value {
            VmValue::List(items) => Ok(items
                .iter()
                .map(crate::llm::vm_value_to_json)
                .collect::<Vec<_>>()),
            _ => Err(VmError::Runtime(format!(
                "{builtin}: evidence_refs must be a list"
            ))),
        })
        .transpose()?
        .unwrap_or_default();
    let deadline = dict
        .and_then(|dict| dict.get("deadline"))
        .map(parse_duration_value)
        .transpose()?
        .unwrap_or_else(|| StdDuration::from_millis(HITL_APPROVAL_TIMEOUT_MS));
    Ok(ApprovalOptions {
        detail: dict.and_then(|dict| dict.get("detail")).cloned(),
        args: dict.and_then(|dict| dict.get("args")).cloned(),
        quorum: quorum as u32,
        reviewers,
        deadline,
        principal: dict
            .and_then(|dict| dict.get("principal"))
            .map(VmValue::display)
            .filter(|value| !value.is_empty()),
        evidence_refs,
        undo_metadata: dict
            .and_then(|dict| dict.get("undo_metadata"))
            .map(crate::llm::vm_value_to_json),
        capabilities_requested,
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
    duration_from_value(value, "hitl", "timeout", ErrorKind::Runtime)
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
    let mut headers = std::collections::BTreeMap::new();
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
        VmValue::String(_) => VmValue::String(arcstr::ArcStr::from(value.display())),
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
    format_rfc3339(OffsetDateTime::now_utc())
}

fn format_rfc3339(timestamp: OffsetDateTime) -> String {
    timestamp
        .format(&Rfc3339)
        .unwrap_or_else(|_| timestamp.to_string())
}

fn deadline_after(requested_at: OffsetDateTime, duration: StdDuration) -> Option<String> {
    time::Duration::try_from(duration)
        .ok()
        .map(|duration| format_rfc3339(requested_at + duration))
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
    use std::sync::OnceLock;

    use tokio::sync::Mutex;

    use super::{
        HITL_APPROVALS_TOPIC, HITL_DUAL_CONTROL_TOPIC, HITL_ESCALATIONS_TOPIC, HITL_QUESTIONS_TOPIC,
    };
    use crate::event_log::{install_default_for_base_dir, EventLog, Topic};
    use crate::{compile_source, register_vm_stdlib, reset_thread_local_state, Vm, VmError};

    /// Serialize tests that exercise the request-approval path. Those tests
    /// drive the Harn VM through its full HITL state machine and rely on
    /// thread-local event-log handles that are set up by
    /// `execute_hitl_script` → `reset_thread_local_state()`. Under heavy
    /// parallel load the OS thread that a `current_thread` tokio runtime
    /// runs on can be reused between tests; if the outgoing test's async
    /// drop runs concurrently with the incoming test's reset the thread-
    /// local event log is in a transitional state and events can be double-
    /// counted or missed. Holding this mutex for the duration of each test
    /// turns the hazard into a hard serialize.
    fn hitl_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

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

    async fn event_payloads(
        log: std::sync::Arc<crate::event_log::AnyEventLog>,
        topic: &str,
    ) -> Vec<serde_json::Value> {
        log.read_range(&Topic::new(topic).expect("valid topic"), None, usize::MAX)
            .await
            .expect("read topic")
            .into_iter()
            .map(|(_, event)| event.payload)
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
  const answer: int = ask_user("Pick a number", {default: 0})
  __io_println(answer)
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
        let _guard = hitl_lock().lock().await;
        tokio::task::LocalSet::new()
            .run_until(async {
                reset_thread_local_state();
                let dir = tempfile::tempdir().expect("tempdir");
                let source = r#"
pipeline test(task) {
  host_mock("hitl", "approval", [
    {approved: true, reviewer: "alice", reason: "ok"},
    {approved: true, reviewer: "bob", reason: "ship it"},
  ])
  const record = request_approval(
    "deploy production",
    {quorum: 2, reviewers: ["alice", "bob", "carol"]},
  )
  __io_println(record.approved)
  __io_println(len(record.reviewers))
  __io_println(record.reviewers[0])
  __io_println(record.reviewers[1])
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
    async fn request_approval_emits_canonical_approval_request_payload() {
        tokio::task::LocalSet::new()
            .run_until(async {
                reset_thread_local_state();
                let dir = tempfile::tempdir().expect("tempdir");
                let log = install_default_for_base_dir(dir.path()).expect("install event log");
                let source = r#"
pipeline test(task) {
  host_mock("hitl", "approval", {approved: true, reviewer: "alice", reason: "ok"})
  request_approval("deploy production", {
    args: {environment: "prod"},
    quorum: 1,
    reviewers: ["alice"],
    evidence_refs: [{kind: "run", uri: "run_123"}],
    undo_metadata: {strategy: "rollback"},
    capabilities_requested: ["deploy.production"],
  })
}
"#;
                let chunk = compile_source(source).expect("compile source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_source_dir(dir.path());
                vm.execute(&chunk).await.expect("script succeeds");

                let payloads = event_payloads(log, HITL_APPROVALS_TOPIC).await;
                let request_payload = &payloads[0]["payload"];
                let approval_request = &request_payload["approval_request"];
                assert_eq!(approval_request["id"], request_payload["id"]);
                assert_eq!(approval_request["action"], "deploy production");
                assert_eq!(approval_request["args"]["environment"], "prod");
                assert_eq!(approval_request["approvers_required"], 1);
                assert_eq!(approval_request["evidence_refs"][0]["uri"], "run_123");
                assert_eq!(approval_request["undo_metadata"]["strategy"], "rollback");
                assert_eq!(
                    approval_request["capabilities_requested"][0],
                    "deploy.production"
                );
                assert!(approval_request["requested_at"].as_str().is_some());
                assert!(approval_request["deadline"].as_str().is_some());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_approval_surfaces_denials_as_typed_errors() {
        let _guard = hitl_lock().lock().await;
        tokio::task::LocalSet::new()
            .run_until(async {
                reset_thread_local_state();
                let dir = tempfile::tempdir().expect("tempdir");
                let source = r#"
pipeline test(task) {
  host_mock("hitl", "approval", {approved: false, reviewer: "alice", reason: "unsafe"})
  const denied = try {
    request_approval("drop table", {reviewers: ["alice"]})
  }
  __io_println(is_err(denied))
  __io_println(unwrap_err(denied).name)
  __io_println(unwrap_err(denied).reason)
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
  const result = dual_control(2, 3, { -> "launched" }, ["alice", "bob", "carol"])
  __io_println(result)
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
  const handle = escalate_to("admin", "need override")
  __io_println(handle.status)
  __io_println(handle.reviewer)
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

    /// `harn-serve` adapters (A2A `input-required`, ACP `hitl_request`)
    /// rely on the canonical `AgentEvent::HitlRequested` /
    /// `AgentEvent::HitlResolved` pair to bracket every HITL pause.
    /// Pin the contract here so future HITL primitives keep emitting
    /// the event around their waitpoint blocks.
    #[tokio::test(flavor = "current_thread")]
    async fn ask_user_emits_hitl_request_and_resolution_to_agent_event_sinks() {
        use std::sync::Mutex as StdMutex;

        tokio::task::LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let session_id = "hitl-session".to_string();
                let captured: std::sync::Arc<StdMutex<Vec<crate::agent_events::AgentEvent>>> =
                    std::sync::Arc::new(StdMutex::new(Vec::new()));

                struct CaptureSink(std::sync::Arc<StdMutex<Vec<crate::agent_events::AgentEvent>>>);
                impl crate::agent_events::AgentEventSink for CaptureSink {
                    fn handle_event(&self, event: &crate::agent_events::AgentEvent) {
                        self.0.lock().expect("captured").push(event.clone());
                    }
                }

                // Inline the script setup rather than using the
                // `execute_hitl_script` helper: that helper calls
                // `reset_thread_local_state` (which wipes the session
                // store), so any session pushed before it would be
                // gone by the time `ask_user` runs.
                crate::reset_thread_local_state();
                crate::event_log::install_default_for_base_dir(dir.path())
                    .expect("install event log");

                crate::agent_events::reset_all_sinks();
                let sink: std::sync::Arc<dyn crate::agent_events::AgentEventSink> =
                    std::sync::Arc::new(CaptureSink(captured.clone()));
                crate::agent_events::register_sink(session_id.clone(), sink);
                crate::agent_sessions::open_or_create(Some(session_id.clone()));
                let _guard = crate::agent_sessions::enter_current_session(session_id.clone());

                let source = r#"
pipeline test(task) {
  host_mock("hitl", "question", {answer: "ok"})
  const answer: string = ask_user("Are you sure?", {default: "no"})
  __io_println(answer)
}
"#;
                let chunk = crate::compile_source(source).expect("compile source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_source_dir(dir.path());
                vm.execute(&chunk).await.expect("script runs");
                assert_eq!(vm.output().trim_end(), "ok");

                let events = captured.lock().expect("captured");
                let mut iter = events.iter().filter(|event| {
                    matches!(
                        event,
                        crate::agent_events::AgentEvent::HitlRequested { .. }
                            | crate::agent_events::AgentEvent::HitlResolved { .. }
                    )
                });
                let requested = iter.next().expect("HitlRequested emitted");
                let resolved = iter.next().expect("HitlResolved emitted");
                assert!(iter.next().is_none(), "exactly one pair: {events:?}");

                let crate::agent_events::AgentEvent::HitlRequested {
                    session_id: req_session,
                    request_id: req_id,
                    kind: req_kind,
                    payload,
                } = requested
                else {
                    panic!("expected HitlRequested, got: {requested:?}");
                };
                assert_eq!(req_session, &session_id);
                assert_eq!(req_kind, "question");
                assert!(req_id.starts_with("hitl_question_"));
                assert_eq!(payload["prompt"], "Are you sure?");

                let crate::agent_events::AgentEvent::HitlResolved {
                    request_id: res_id,
                    kind: res_kind,
                    outcome,
                    ..
                } = resolved
                else {
                    panic!("expected HitlResolved, got: {resolved:?}");
                };
                assert_eq!(res_id, req_id);
                assert_eq!(res_kind, "question");
                assert_eq!(outcome, "answered");

                drop(_guard);
                crate::agent_events::reset_all_sinks();
            })
            .await;
    }
}
