use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value as JsonValue;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::event_log::{active_event_log, EventId, EventLog, LogEvent, Topic};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::hitl::{
    HitlRequestKind, HITL_APPROVALS_TOPIC, HITL_DUAL_CONTROL_TOPIC, HITL_ESCALATIONS_TOPIC,
    HITL_QUESTIONS_TOPIC,
};

const HITL_RESPONSES_TOPIC: &str = "hitl.responses";
const DEFAULT_LIMIT: usize = 500;
const MAX_LIMIT: usize = 5_000;

#[derive(Clone, Debug, Default)]
struct HitlPendingFilters {
    since: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
    kinds: Option<BTreeSet<String>>,
    agent: Option<String>,
    limit: usize,
}

#[derive(Clone, Debug)]
struct PendingHitlRow {
    request_id: String,
    request_kind: &'static str,
    agent: String,
    prompt: String,
    trace_id: String,
    timestamp: String,
    timestamp_value: OffsetDateTime,
    approvers: Vec<String>,
    metadata: JsonValue,
    event_id: EventId,
}

#[derive(Clone, Debug, Deserialize)]
struct HitlRequestEnvelope {
    request_id: String,
    kind: HitlRequestKind,
    #[serde(default)]
    agent: String,
    trace_id: String,
    requested_at: String,
    payload: JsonValue,
}

pub(crate) fn register_hitl_read_builtins(vm: &mut Vm) {
    vm.register_async_builtin("hitl_pending", |args| {
        Box::pin(async move { hitl_pending_impl(&args).await })
    });
}

async fn hitl_pending_impl(args: &[VmValue]) -> Result<VmValue, VmError> {
    let filters = parse_filters(args.first())?;
    let Some(log) = active_event_log() else {
        return Ok(VmValue::List(Rc::new(Vec::new())));
    };
    let rows = read_pending_rows(&log, &filters).await?;
    Ok(VmValue::List(Rc::new(
        rows.into_iter().map(pending_row_to_value).collect(),
    )))
}

async fn read_pending_rows(
    log: &Arc<crate::event_log::AnyEventLog>,
    filters: &HitlPendingFilters,
) -> Result<Vec<PendingHitlRow>, VmError> {
    let mut requests = BTreeMap::<String, PendingHitlRow>::new();
    let mut resolved = BTreeSet::<String>::new();

    for (kind, topic_name) in request_topics() {
        collect_topic_state(log, kind, topic_name, &mut requests, &mut resolved).await?;
    }
    collect_response_topic_state(log, &mut resolved).await?;

    let mut rows = requests
        .into_values()
        .filter(|row| !resolved.contains(&row.request_id))
        .filter(|row| matches_filters(row, filters))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .timestamp_value
            .cmp(&left.timestamp_value)
            .then_with(|| right.event_id.cmp(&left.event_id))
    });
    rows.truncate(filters.limit);
    Ok(rows)
}

async fn collect_topic_state(
    log: &Arc<crate::event_log::AnyEventLog>,
    kind: HitlRequestKind,
    topic_name: &'static str,
    requests: &mut BTreeMap<String, PendingHitlRow>,
    resolved: &mut BTreeSet<String>,
) -> Result<(), VmError> {
    let topic = Topic::new(topic_name).map_err(log_error)?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(log_error)?;
    for (event_id, event) in events {
        if event.kind == request_event_kind(kind) {
            let row = parse_request_row(event_id, topic_name, &event)?;
            requests.insert(row.request_id.clone(), row);
            continue;
        }
        if is_terminal_event(kind, &event) {
            if let Some(request_id) = request_id_for_event(&event) {
                resolved.insert(request_id);
            }
        }
    }
    Ok(())
}

async fn collect_response_topic_state(
    log: &Arc<crate::event_log::AnyEventLog>,
    resolved: &mut BTreeSet<String>,
) -> Result<(), VmError> {
    let topic = Topic::new(HITL_RESPONSES_TOPIC).map_err(log_error)?;
    let events = log
        .read_range(&topic, None, usize::MAX)
        .await
        .map_err(log_error)?;
    for (_, event) in events {
        let Some(request_id) = request_id_for_event(&event) else {
            continue;
        };
        let Some(kind) = HitlRequestKind::from_request_id(&request_id) else {
            continue;
        };
        if is_terminal_event(kind, &event) {
            resolved.insert(request_id);
        }
    }
    Ok(())
}

fn parse_request_row(
    event_id: EventId,
    _topic_name: &'static str,
    event: &LogEvent,
) -> Result<PendingHitlRow, VmError> {
    let envelope: HitlRequestEnvelope =
        serde_json::from_value(event.payload.clone()).map_err(log_error)?;
    let timestamp_value =
        OffsetDateTime::parse(&envelope.requested_at, &Rfc3339).map_err(log_error)?;
    let request_kind = HitlRequestKind::from_request_id(&envelope.request_id)
        .unwrap_or(envelope.kind)
        .as_str();
    let metadata = envelope.payload.clone();
    let prompt = prompt_from_payload(&envelope.payload);
    let approvers = approvers_from_payload(&envelope.payload);
    Ok(PendingHitlRow {
        request_id: envelope.request_id,
        request_kind,
        agent: envelope.agent,
        prompt,
        trace_id: envelope.trace_id,
        timestamp: envelope.requested_at,
        timestamp_value,
        approvers,
        metadata,
        event_id,
    })
}

fn pending_row_to_value(row: PendingHitlRow) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "request_id".to_string(),
        VmValue::String(Rc::from(row.request_id)),
    );
    dict.insert(
        "request_kind".to_string(),
        VmValue::String(Rc::from(row.request_kind)),
    );
    dict.insert("agent".to_string(), VmValue::String(Rc::from(row.agent)));
    dict.insert("prompt".to_string(), VmValue::String(Rc::from(row.prompt)));
    dict.insert(
        "trace_id".to_string(),
        VmValue::String(Rc::from(row.trace_id)),
    );
    dict.insert(
        "timestamp".to_string(),
        VmValue::String(Rc::from(row.timestamp)),
    );
    dict.insert(
        "approvers".to_string(),
        VmValue::List(Rc::new(
            row.approvers
                .into_iter()
                .map(|value| VmValue::String(Rc::from(value)))
                .collect(),
        )),
    );
    dict.insert(
        "metadata".to_string(),
        crate::stdlib::json_to_vm_value(&row.metadata),
    );
    VmValue::Dict(Rc::new(dict))
}

fn parse_filters(value: Option<&VmValue>) -> Result<HitlPendingFilters, VmError> {
    let mut filters = HitlPendingFilters {
        limit: DEFAULT_LIMIT,
        ..HitlPendingFilters::default()
    };
    let Some(value) = value else {
        return Ok(filters);
    };
    let (VmValue::Nil | VmValue::Dict(_)) = value else {
        return Err(VmError::Runtime(
            "hitl_pending: filters must be a dict or nil".to_string(),
        ));
    };
    let Some(dict) = value.as_dict() else {
        return Ok(filters);
    };

    if let Some(since) = dict.get("since") {
        filters.since = Some(parse_rfc3339_filter(since, "since")?);
    }
    if let Some(until) = dict.get("until") {
        filters.until = Some(parse_rfc3339_filter(until, "until")?);
    }
    if let Some(agent) = dict.get("agent") {
        filters.agent = Some(required_string_value(agent, "agent", "hitl_pending")?);
    }
    if let Some(limit) = dict.get("limit") {
        let limit = limit.as_int().ok_or_else(|| {
            VmError::Runtime("hitl_pending: limit must be a positive int".to_string())
        })?;
        if limit <= 0 {
            return Err(VmError::Runtime(
                "hitl_pending: limit must be a positive int".to_string(),
            ));
        }
        filters.limit = (limit as usize).min(MAX_LIMIT);
    }
    if let Some(kinds) = dict.get("kinds") {
        let VmValue::List(kinds) = kinds else {
            return Err(VmError::Runtime(
                "hitl_pending: kinds must be a list<string>".to_string(),
            ));
        };
        let mut parsed = BTreeSet::new();
        for kind in kinds.iter() {
            let value = kind.display();
            let normalized = match value.as_str() {
                "question" | "approval" | "dual_control" | "escalation" => value.to_string(),
                _ => {
                    return Err(VmError::Runtime(format!(
                        "hitl_pending: unsupported kind '{value}'"
                    )))
                }
            };
            parsed.insert(normalized);
        }
        filters.kinds = Some(parsed);
    }
    Ok(filters)
}

fn parse_rfc3339_filter(value: &VmValue, field: &str) -> Result<OffsetDateTime, VmError> {
    let text = required_string_value(value, field, "hitl_pending")?;
    OffsetDateTime::parse(&text, &Rfc3339)
        .map_err(|error| VmError::Runtime(format!("hitl_pending: invalid {field}: {error}")))
}

fn required_string_value(value: &VmValue, field: &str, builtin: &str) -> Result<String, VmError> {
    match value {
        VmValue::String(text) if !text.is_empty() => Ok(text.to_string()),
        _ => Err(VmError::Runtime(format!(
            "{builtin}: {field} must be a string"
        ))),
    }
}

fn matches_filters(row: &PendingHitlRow, filters: &HitlPendingFilters) -> bool {
    if let Some(since) = filters.since {
        if row.timestamp_value < since {
            return false;
        }
    }
    if let Some(until) = filters.until {
        if row.timestamp_value > until {
            return false;
        }
    }
    if let Some(kinds) = filters.kinds.as_ref() {
        if !kinds.contains(row.request_kind) {
            return false;
        }
    }
    if let Some(agent) = filters.agent.as_deref() {
        if row.agent != agent {
            return false;
        }
    }
    true
}

fn request_topics() -> [(HitlRequestKind, &'static str); 4] {
    [
        (HitlRequestKind::Question, HITL_QUESTIONS_TOPIC),
        (HitlRequestKind::Approval, HITL_APPROVALS_TOPIC),
        (HitlRequestKind::DualControl, HITL_DUAL_CONTROL_TOPIC),
        (HitlRequestKind::Escalation, HITL_ESCALATIONS_TOPIC),
    ]
}

fn request_event_kind(kind: HitlRequestKind) -> &'static str {
    match kind {
        HitlRequestKind::Question => "hitl.question_asked",
        HitlRequestKind::Approval => "hitl.approval_requested",
        HitlRequestKind::DualControl => "hitl.dual_control_requested",
        HitlRequestKind::Escalation => "hitl.escalation_issued",
    }
}

fn is_terminal_event(kind: HitlRequestKind, event: &LogEvent) -> bool {
    match kind {
        HitlRequestKind::Question => {
            matches!(
                event.kind.as_str(),
                "hitl.response_received" | "hitl.timeout"
            )
        }
        HitlRequestKind::Approval => match event.kind.as_str() {
            "hitl.approval_approved" | "hitl.approval_denied" | "hitl.timeout" => true,
            "hitl.response_received" => response_bool_field(event, "approved") == Some(false),
            _ => false,
        },
        HitlRequestKind::DualControl => match event.kind.as_str() {
            "hitl.dual_control_approved"
            | "hitl.dual_control_denied"
            | "hitl.dual_control_executed"
            | "hitl.timeout" => true,
            "hitl.response_received" => response_bool_field(event, "approved") == Some(false),
            _ => false,
        },
        HitlRequestKind::Escalation => match event.kind.as_str() {
            "hitl.escalation_accepted" | "hitl.timeout" => true,
            "hitl.response_received" => response_bool_field(event, "accepted") == Some(true),
            _ => false,
        },
    }
}

fn response_bool_field(event: &LogEvent, field: &str) -> Option<bool> {
    event.payload.get(field).and_then(JsonValue::as_bool)
}

fn request_id_for_event(event: &LogEvent) -> Option<String> {
    event.headers.get("request_id").cloned().or_else(|| {
        event
            .payload
            .get("request_id")
            .and_then(JsonValue::as_str)
            .map(str::to_string)
    })
}

fn prompt_from_payload(payload: &JsonValue) -> String {
    payload
        .get("prompt")
        .or_else(|| payload.get("action"))
        .or_else(|| payload.get("reason"))
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string()
}

fn approvers_from_payload(payload: &JsonValue) -> Vec<String> {
    payload
        .get("approvers")
        .or_else(|| payload.get("reviewers"))
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

fn log_error(error: impl std::fmt::Display) -> VmError {
    VmError::Runtime(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{install_default_for_base_dir, EventLog};
    use crate::stdlib::register_vm_stdlib;
    use crate::{compile_source, reset_thread_local_state, Vm};
    use serde_json::json;

    async fn append_hitl_request(
        log: &Arc<crate::event_log::AnyEventLog>,
        topic_name: &str,
        kind: &str,
        request_id: &str,
        agent: &str,
        timestamp: &str,
        payload: JsonValue,
    ) {
        let topic = Topic::new(topic_name).expect("valid topic");
        let event = LogEvent::new(
            kind,
            json!({
                "request_id": request_id,
                "kind": HitlRequestKind::from_request_id(request_id).expect("kind"),
                "agent": agent,
                "trace_id": format!("trace-{request_id}"),
                "requested_at": timestamp,
                "payload": payload,
            }),
        )
        .with_headers(BTreeMap::from([(
            "request_id".to_string(),
            request_id.to_string(),
        )]));
        log.append(&topic, event).await.expect("append request");
    }

    async fn append_terminal_event(
        log: &Arc<crate::event_log::AnyEventLog>,
        topic_name: &str,
        kind: &str,
        request_id: &str,
        payload: JsonValue,
    ) {
        let topic = Topic::new(topic_name).expect("valid topic");
        let event = LogEvent::new(kind, payload).with_headers(BTreeMap::from([(
            "request_id".to_string(),
            request_id.to_string(),
        )]));
        log.append(&topic, event).await.expect("append terminal");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hitl_pending_reads_filters_limits_and_hides_terminal_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = install_default_for_base_dir(dir.path()).expect("install event log");
        append_hitl_request(
            &log,
            HITL_QUESTIONS_TOPIC,
            "hitl.question_asked",
            "hitl_question_1",
            "alpha",
            "2026-01-01T00:00:00Z",
            json!({"prompt": "q1"}),
        )
        .await;
        append_hitl_request(
            &log,
            HITL_QUESTIONS_TOPIC,
            "hitl.question_asked",
            "hitl_question_2",
            "beta",
            "2026-01-01T00:01:00Z",
            json!({"prompt": "q2"}),
        )
        .await;
        append_hitl_request(
            &log,
            HITL_QUESTIONS_TOPIC,
            "hitl.question_asked",
            "hitl_question_3",
            "beta",
            "2026-01-01T00:02:00Z",
            json!({"prompt": "q3"}),
        )
        .await;
        append_hitl_request(
            &log,
            HITL_APPROVALS_TOPIC,
            "hitl.approval_requested",
            "hitl_approval_1",
            "approver",
            "2026-01-01T00:03:00Z",
            json!({"action": "ship", "reviewers": ["alice", "bob"]}),
        )
        .await;
        append_hitl_request(
            &log,
            HITL_APPROVALS_TOPIC,
            "hitl.approval_requested",
            "hitl_approval_2",
            "approver",
            "2026-01-01T00:04:00Z",
            json!({"action": "deploy", "reviewers": ["carol"]}),
        )
        .await;
        append_hitl_request(
            &log,
            HITL_DUAL_CONTROL_TOPIC,
            "hitl.dual_control_requested",
            "hitl_dual_control_1",
            "ops",
            "2026-01-01T00:05:00Z",
            json!({"action": "rotate keys", "approvers": ["alice", "bob", "carol"]}),
        )
        .await;
        append_terminal_event(
            &log,
            HITL_QUESTIONS_TOPIC,
            "hitl.response_received",
            "hitl_question_2",
            json!({"request_id": "hitl_question_2", "answer": "done"}),
        )
        .await;

        let all_rows = read_pending_rows(
            &log,
            &HitlPendingFilters {
                limit: DEFAULT_LIMIT,
                ..HitlPendingFilters::default()
            },
        )
        .await
        .expect("read pending rows");
        assert_eq!(all_rows.len(), 5);
        assert_eq!(all_rows[0].request_kind, "dual_control");
        assert_eq!(all_rows[1].request_kind, "approval");
        assert_eq!(all_rows[2].request_kind, "approval");
        assert_eq!(all_rows[3].request_kind, "question");
        assert_eq!(all_rows[4].request_kind, "question");

        let questions = read_pending_rows(
            &log,
            &HitlPendingFilters {
                kinds: Some(BTreeSet::from([String::from("question")])),
                limit: DEFAULT_LIMIT,
                ..HitlPendingFilters::default()
            },
        )
        .await
        .expect("filter questions");
        assert_eq!(questions.len(), 2);

        let since_rows = read_pending_rows(
            &log,
            &HitlPendingFilters {
                since: Some(OffsetDateTime::parse("2026-01-01T00:03:30Z", &Rfc3339).unwrap()),
                limit: DEFAULT_LIMIT,
                ..HitlPendingFilters::default()
            },
        )
        .await
        .expect("filter since");
        assert_eq!(since_rows.len(), 2);
        assert_eq!(since_rows[0].request_id, "hitl_dual_control_1");
        assert_eq!(since_rows[1].request_id, "hitl_approval_2");

        let limited = read_pending_rows(
            &log,
            &HitlPendingFilters {
                limit: 2,
                ..HitlPendingFilters::default()
            },
        )
        .await
        .expect("limit rows");
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].request_id, "hitl_dual_control_1");
        assert_eq!(limited[1].request_id, "hitl_approval_2");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hitl_pending_returns_empty_list_without_attached_event_log() {
        reset_thread_local_state();
        let chunk = compile_source(
            r#"
pipeline test(task) {
  let rows = hitl_pending({})
  println(len(rows))
}
"#,
        )
        .expect("compile source");
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm.execute(&chunk).await.expect("execute");
        assert_eq!(vm.output().trim_end(), "0");
    }
}
