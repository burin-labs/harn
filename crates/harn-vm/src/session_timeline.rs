//! Session timeline projection for client-facing observability.
//!
//! This module does not own persistence. It projects the existing run record
//! spans and event-log topics into one stable, redacted shape that clients can
//! query or subscribe to without learning Harn's storage internals.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::stream::{self, BoxStream};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::event_log::{AnyEventLog, EventId, EventLog, LogError, LogEvent, Topic};
use crate::orchestration::{load_run_record, RunRecord, RunTraceSpanRecord};
use crate::redact::{current_policy, RedactionPolicy};

pub const SESSION_TIMELINE_SCHEMA_VERSION: u32 = 1;
pub const SESSION_TIMELINE_QUERY_METHOD: &str = "harn.session_timeline.query";
pub const SESSION_TIMELINE_SUBSCRIBE_METHOD: &str = "harn.session_timeline.subscribe";
pub const SESSION_TIMELINE_UNSUBSCRIBE_METHOD: &str = "harn.session_timeline.unsubscribe";
pub const SESSION_TIMELINE_UPDATE_METHOD: &str = "harn.session_timeline.update";

const DEFAULT_QUERY_LIMIT: usize = 1024;
const READ_BATCH_SIZE: usize = 256;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionTimelineQuery {
    #[serde(alias = "session_id")]
    pub session_id: Option<String>,
    #[serde(alias = "run_id")]
    pub run_id: Option<String>,
    #[serde(alias = "run_path")]
    pub run_path: Option<String>,
    #[serde(alias = "project_id")]
    pub project_id: Option<String>,
    #[serde(alias = "from_cursor")]
    pub from_cursor: SessionTimelineCursor,
    pub limit: Option<usize>,
}

impl SessionTimelineQuery {
    pub fn for_session(session_id: impl Into<String>) -> Self {
        Self {
            session_id: Some(session_id.into()),
            ..Self::default()
        }
    }

    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_QUERY_LIMIT).max(1)
    }

    fn topics(&self) -> Vec<Topic> {
        let mut topics = Vec::new();
        if let Some(session_id) = self.session_id.as_deref() {
            topics.push(agent_events_topic(session_id));
        }
        topics.push(static_topic(crate::channels::CHANNEL_TRANSCRIPT_TOPIC));
        topics.push(static_topic(crate::channels::CHANNEL_AUDIT_TOPIC));
        topics
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionTimelineCursor {
    pub topics: BTreeMap<String, EventId>,
}

impl SessionTimelineCursor {
    pub fn event_id_for(&self, topic: &Topic) -> Option<EventId> {
        self.topics.get(topic.as_str()).copied()
    }

    fn bump(&mut self, topic: &str, event_id: EventId) {
        self.topics
            .entry(topic.to_string())
            .and_modify(|cursor| *cursor = (*cursor).max(event_id))
            .or_insert(event_id);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionTimelineSnapshot {
    pub schema_version: u32,
    pub query: SessionTimelineQuery,
    pub cursor: SessionTimelineCursor,
    pub nodes: Vec<SessionTimelineNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionTimelineUpdate {
    pub schema_version: u32,
    pub cursor: SessionTimelineCursor,
    pub node: SessionTimelineNode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionTimelineNode {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<String>,
    pub category: String,
    pub kind: String,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurred_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub attributes: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<SessionTimelineReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<SessionTimelineLink>,
    pub order: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionTimelineReference {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionTimelineLink {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

#[derive(Debug)]
pub enum SessionTimelineError {
    EventLog(LogError),
    RunRecord(String),
}

impl std::fmt::Display for SessionTimelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLog(error) => error.fmt(f),
            Self::RunRecord(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SessionTimelineError {}

impl From<LogError> for SessionTimelineError {
    fn from(error: LogError) -> Self {
        Self::EventLog(error)
    }
}

#[derive(Clone)]
struct TimelineDraft {
    sort_ms: i128,
    node: SessionTimelineNode,
}

pub fn agent_events_topic(session_id: &str) -> Topic {
    Topic::new(format!(
        "observability.agent_events.{}",
        crate::event_log::sanitize_topic_component(session_id)
    ))
    .expect("sanitized session id should produce a valid topic")
}

pub fn timeline_from_run_record(
    run: &RunRecord,
    query: SessionTimelineQuery,
) -> SessionTimelineSnapshot {
    let policy = current_policy();
    let mut builder = TimelineBuilder::new(query.clone());
    if run_matches_query(run, &query) {
        builder.add_run_spans(run, &policy);
    }
    builder.finish()
}

pub async fn query_session_timeline(
    log: Option<&AnyEventLog>,
    run: Option<&RunRecord>,
    query: SessionTimelineQuery,
) -> Result<SessionTimelineSnapshot, SessionTimelineError> {
    let policy = current_policy();
    let mut builder = TimelineBuilder::new(query.clone());
    if let Some(run) = run.filter(|run| run_matches_query(run, &query)) {
        builder.add_run_spans(run, &policy);
    } else if run.is_none() {
        if let Some(run) = load_run_for_timeline(&query)? {
            if run_matches_query(&run, &query) {
                builder.add_run_spans(&run, &policy);
            }
        }
    }
    if let Some(log) = log {
        builder.add_event_log(log, &policy).await?;
    }
    Ok(builder.finish())
}

pub async fn subscribe_session_timeline(
    log: Arc<AnyEventLog>,
    query: SessionTimelineQuery,
) -> Result<
    BoxStream<'static, Result<SessionTimelineUpdate, SessionTimelineError>>,
    SessionTimelineError,
> {
    let policy = current_policy();
    let mut streams = Vec::new();
    for topic in query.topics() {
        let topic_name = topic.as_str().to_string();
        let from_cursor = query.from_cursor.event_id_for(&topic);
        let events = log.clone().subscribe(&topic, from_cursor).await?;
        let query = query.clone();
        let policy = policy.clone();
        streams.push(Box::pin(events.filter_map(move |item| {
            let topic_name = topic_name.clone();
            let query = query.clone();
            let policy = policy.clone();
            async move {
                match item {
                    Ok((event_id, event)) => {
                        event_update(&query, &policy, &topic_name, event_id, event).map(Ok)
                    }
                    Err(error) => Some(Err(SessionTimelineError::EventLog(error))),
                }
            }
        }))
            as BoxStream<
                'static,
                Result<SessionTimelineUpdate, SessionTimelineError>,
            >);
    }
    Ok(Box::pin(stream::select_all(streams)))
}

struct TimelineBuilder {
    query: SessionTimelineQuery,
    cursor: SessionTimelineCursor,
    nodes: Vec<TimelineDraft>,
}

impl TimelineBuilder {
    fn new(query: SessionTimelineQuery) -> Self {
        Self {
            cursor: query.from_cursor.clone(),
            query,
            nodes: Vec::new(),
        }
    }

    fn add_run_spans(&mut self, run: &RunRecord, policy: &RedactionPolicy) {
        for span in &run.trace_spans {
            if !span_matches_query(span, &self.query) {
                continue;
            }
            let node = span_node(span, policy);
            self.nodes.push(TimelineDraft {
                sort_ms: i128::from(span.start_ms),
                node,
            });
        }
    }

    async fn add_event_log(
        &mut self,
        log: &AnyEventLog,
        policy: &RedactionPolicy,
    ) -> Result<(), SessionTimelineError> {
        for topic in self.query.topics() {
            let topic_name = topic.as_str().to_string();
            let mut from = self.query.from_cursor.event_id_for(&topic);
            loop {
                let batch = log.read_range(&topic, from, READ_BATCH_SIZE).await?;
                let batch_len = batch.len();
                for (event_id, event) in batch {
                    from = Some(event_id);
                    self.cursor.bump(&topic_name, event_id);
                    if let Some(node) =
                        event_node(&self.query, policy, &topic_name, event_id, event)
                    {
                        let sort_ms = node
                            .occurred_at_ms
                            .map(i128::from)
                            .or_else(|| node.start_ms.map(i128::from))
                            .unwrap_or(i128::from(event_id));
                        self.nodes.push(TimelineDraft { sort_ms, node });
                    }
                }
                if batch_len < READ_BATCH_SIZE || self.nodes.len() >= self.query.limit() {
                    break;
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> SessionTimelineSnapshot {
        self.nodes.sort_by(|left, right| {
            left.sort_ms
                .cmp(&right.sort_ms)
                .then_with(|| left.node.id.cmp(&right.node.id))
        });
        self.nodes.truncate(self.query.limit());

        let visible_ids: BTreeSet<String> = self
            .nodes
            .iter()
            .map(|draft| draft.node.id.clone())
            .collect();
        let mut children_by_parent: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for draft in &self.nodes {
            let Some(parent_id) = draft.node.parent_id.as_ref() else {
                continue;
            };
            if visible_ids.contains(parent_id) {
                children_by_parent
                    .entry(parent_id.clone())
                    .or_default()
                    .push(draft.node.id.clone());
            }
        }

        let nodes = self
            .nodes
            .into_iter()
            .enumerate()
            .map(|(index, mut draft)| {
                draft.node.order = index as u64;
                draft.node.children = children_by_parent
                    .remove(&draft.node.id)
                    .unwrap_or_default();
                draft.node
            })
            .collect();

        SessionTimelineSnapshot {
            schema_version: SESSION_TIMELINE_SCHEMA_VERSION,
            query: self.query,
            cursor: self.cursor,
            nodes,
        }
    }
}

fn event_update(
    query: &SessionTimelineQuery,
    policy: &RedactionPolicy,
    topic: &str,
    event_id: EventId,
    event: LogEvent,
) -> Option<SessionTimelineUpdate> {
    let mut node = event_node(query, policy, topic, event_id, event)?;
    node.order = 0;
    let mut cursor = SessionTimelineCursor::default();
    cursor.bump(topic, event_id);
    Some(SessionTimelineUpdate {
        schema_version: SESSION_TIMELINE_SCHEMA_VERSION,
        cursor,
        node,
    })
}

fn event_node(
    query: &SessionTimelineQuery,
    policy: &RedactionPolicy,
    topic: &str,
    event_id: EventId,
    mut event: LogEvent,
) -> Option<SessionTimelineNode> {
    event.redact_in_place(policy);
    if topic.starts_with("observability.agent_events.") {
        return agent_event_node(query, topic, event_id, event);
    }
    if topic == crate::channels::CHANNEL_TRANSCRIPT_TOPIC {
        return channel_lifecycle_node(query, topic, event_id, event);
    }
    if topic == crate::channels::CHANNEL_AUDIT_TOPIC {
        return channel_audit_node(query, topic, event_id, event);
    }
    None
}

fn span_node(span: &RunTraceSpanRecord, policy: &RedactionPolicy) -> SessionTimelineNode {
    let mut attributes = serde_json::json!(span.metadata);
    policy.redact_json_in_place(&mut attributes);
    let status = attributes
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("completed")
        .to_string();
    SessionTimelineNode {
        id: span_node_id(&span.trace_id, span.span_id),
        parent_id: span
            .parent_id
            .map(|parent| span_node_id(&span.trace_id, parent)),
        children: Vec::new(),
        category: "span".to_string(),
        kind: span.kind.clone(),
        name: span.name.clone(),
        status,
        trace_id: Some(span.trace_id.clone()),
        span_id: Some(span.span_id.to_string()),
        occurred_at_ms: None,
        start_ms: Some(span.start_ms),
        duration_ms: Some(span.duration_ms),
        attributes,
        references: vec![SessionTimelineReference {
            kind: "run_trace_span".to_string(),
            id: Some(span.span_id.to_string()),
            topic: None,
            event_id: None,
        }],
        links: span
            .links
            .iter()
            .map(|link| SessionTimelineLink {
                kind: link
                    .attributes
                    .get("harn.link.kind")
                    .cloned()
                    .unwrap_or_else(|| "span_link".to_string()),
                target_id: Some(format!("span:{}:{}", link.trace_id, link.span_id)),
                trace_id: Some(link.trace_id.clone()),
                span_id: Some(link.span_id.clone()),
                event_id: None,
            })
            .collect(),
        order: 0,
    }
}

fn agent_event_node(
    query: &SessionTimelineQuery,
    topic: &str,
    event_id: EventId,
    event: LogEvent,
) -> Option<SessionTimelineNode> {
    if !event_matches_query(
        query,
        &event.payload,
        Some(&event.headers),
        &["session_id"],
        &[],
    ) {
        return None;
    }
    let event_value = event.payload.get("event").unwrap_or(&event.payload);
    let event_type = event_value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(event.kind.as_str());
    let status = event_status(event_value).unwrap_or("observed").to_string();
    Some(SessionTimelineNode {
        id: format!("event:{topic}:{event_id}"),
        parent_id: None,
        children: Vec::new(),
        category: "agent_event".to_string(),
        kind: event.kind.clone(),
        name: event_type.to_string(),
        status,
        trace_id: None,
        span_id: None,
        occurred_at_ms: Some(event.occurred_at_ms),
        start_ms: None,
        duration_ms: duration_ms(event_value),
        attributes: event.payload,
        references: vec![event_ref(topic, event_id)],
        links: Vec::new(),
        order: 0,
    })
}

fn channel_lifecycle_node(
    query: &SessionTimelineQuery,
    topic: &str,
    event_id: EventId,
    event: LogEvent,
) -> Option<SessionTimelineNode> {
    if !event_matches_query(
        query,
        &event.payload,
        Some(&event.headers),
        &["session_id", "matched_in_session_id"],
        &["pipeline_id"],
    ) {
        return None;
    }
    let channel_event_id = string_field(&event.payload, "event_id");
    let trigger_id = string_field(&event.payload, "trigger_id");
    let is_match = event.kind == crate::channels::CHANNEL_MATCH_TRANSCRIPT_KIND;
    let id = if is_match {
        format!(
            "channel:{}:match:{}",
            channel_event_id.as_deref().unwrap_or("unknown"),
            trigger_id.as_deref().unwrap_or("unknown")
        )
    } else {
        format!(
            "channel:{}:emit",
            channel_event_id.as_deref().unwrap_or("unknown")
        )
    };
    let mut links: Vec<SessionTimelineLink> = if is_match {
        channel_event_id
            .as_ref()
            .map(|event_id| SessionTimelineLink {
                kind: "channel_emit".to_string(),
                target_id: Some(format!("channel:{event_id}:emit")),
                trace_id: None,
                span_id: None,
                event_id: Some(event_id.clone()),
            })
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    if is_match {
        links.extend(channel_batch_links(&event.payload));
    }
    Some(SessionTimelineNode {
        id,
        parent_id: None,
        children: Vec::new(),
        category: "channel".to_string(),
        kind: event.kind.clone(),
        name: string_field(&event.payload, "name_resolved")
            .or_else(|| string_field(&event.payload, "name"))
            .unwrap_or_else(|| event.kind.clone()),
        status: if event
            .payload
            .get("duplicate")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            "duplicate".to_string()
        } else {
            "observed".to_string()
        },
        trace_id: None,
        span_id: string_field(&event.payload, "span_id"),
        occurred_at_ms: Some(event.occurred_at_ms),
        start_ms: None,
        duration_ms: None,
        attributes: event.payload,
        references: vec![event_ref(topic, event_id)],
        links,
        order: 0,
    })
}

fn channel_audit_node(
    query: &SessionTimelineQuery,
    topic: &str,
    event_id: EventId,
    event: LogEvent,
) -> Option<SessionTimelineNode> {
    if !event_matches_query(
        query,
        &event.payload,
        Some(&event.headers),
        &["session_id", "matched_in_session_id"],
        &["pipeline_id", "run_id"],
    ) {
        return None;
    }
    let channel_event_id = string_field(&event.payload, "event_id");
    let trigger_id = string_field(&event.payload, "trigger_id");
    let is_match = event.kind == crate::channels::CHANNEL_MATCH_RECEIPT_KIND;
    let id = if is_match {
        format!(
            "channel_receipt:{}:match:{}",
            channel_event_id.as_deref().unwrap_or("unknown"),
            trigger_id.as_deref().unwrap_or("unknown")
        )
    } else {
        format!(
            "channel_receipt:{}:emit",
            channel_event_id.as_deref().unwrap_or("unknown")
        )
    };
    let mut links: Vec<SessionTimelineLink> = if is_match {
        channel_event_id
            .as_ref()
            .map(|event_id| SessionTimelineLink {
                kind: "channel_emit".to_string(),
                target_id: Some(format!("channel_receipt:{event_id}:emit")),
                trace_id: None,
                span_id: None,
                event_id: Some(event_id.clone()),
            })
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };
    if is_match {
        links.extend(channel_batch_links(&event.payload));
    }
    Some(SessionTimelineNode {
        id,
        parent_id: None,
        children: Vec::new(),
        category: "channel_audit".to_string(),
        kind: event.kind.clone(),
        name: string_field(&event.payload, "name_resolved").unwrap_or_else(|| event.kind.clone()),
        status: event
            .payload
            .get("handler_result")
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                event.payload.get("inserted").and_then(|inserted| {
                    if inserted.as_bool() == Some(false) {
                        Some("duplicate")
                    } else {
                        None
                    }
                })
            })
            .unwrap_or("recorded")
            .to_string(),
        trace_id: None,
        span_id: string_field(&event.payload, "span_id"),
        occurred_at_ms: Some(event.occurred_at_ms),
        start_ms: None,
        duration_ms: None,
        attributes: event.payload,
        references: vec![event_ref(topic, event_id)],
        links,
        order: 0,
    })
}

fn event_matches_query(
    query: &SessionTimelineQuery,
    payload: &serde_json::Value,
    headers: Option<&BTreeMap<String, String>>,
    session_keys: &[&str],
    run_keys: &[&str],
) -> bool {
    field_query_matches(query.session_id.as_deref(), payload, headers, session_keys)
        && field_query_matches(query.run_id.as_deref(), payload, headers, run_keys)
        && field_query_matches(
            query.project_id.as_deref(),
            payload,
            headers,
            &["project_id", "projectId", "workspace_id", "workspaceId"],
        )
}

fn field_query_matches(
    expected: Option<&str>,
    payload: &serde_json::Value,
    headers: Option<&BTreeMap<String, String>>,
    keys: &[&str],
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    if expected.is_empty() {
        return true;
    }
    if keys.is_empty() {
        return true;
    }
    keys.iter().any(|key| {
        payload
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == expected)
            || payload
                .get("event")
                .and_then(|event| event.get(*key))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| value == expected)
            || headers
                .and_then(|headers| headers.get(*key))
                .is_some_and(|value| value == expected)
    })
}

fn span_matches_query(span: &RunTraceSpanRecord, query: &SessionTimelineQuery) -> bool {
    if let Some(session_id) = query.session_id.as_deref() {
        let has_session_attr = span.metadata.contains_key("session_id")
            || span.metadata.contains_key("agent_session_id");
        if has_session_attr
            && !metadata_matches(
                &span.metadata,
                &["session_id", "agent_session_id"],
                session_id,
            )
        {
            return false;
        }
    }
    true
}

fn run_matches_query(run: &RunRecord, query: &SessionTimelineQuery) -> bool {
    if let Some(run_id) = query.run_id.as_deref() {
        if run.id != run_id {
            return false;
        }
    }
    if let Some(project_id) = query.project_id.as_deref() {
        if !metadata_matches(&run.metadata, &["project_id", "projectId"], project_id) {
            return false;
        }
    }
    true
}

fn metadata_matches(
    metadata: &BTreeMap<String, serde_json::Value>,
    keys: &[&str],
    expected: &str,
) -> bool {
    keys.iter().any(|key| {
        metadata
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| value == expected)
    })
}

fn event_status(value: &serde_json::Value) -> Option<&str> {
    value
        .get("status")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("verdict").and_then(serde_json::Value::as_str))
}

fn duration_ms(value: &serde_json::Value) -> Option<u64> {
    value
        .get("duration_ms")
        .or_else(|| value.get("judge_duration_ms"))
        .and_then(serde_json::Value::as_u64)
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    let value = value.get(key)?;
    if let Some(text) = value.as_str() {
        if !text.is_empty() {
            return Some(text.to_string());
        }
        return None;
    }
    value.as_u64().map(|number| number.to_string())
}

fn channel_batch_links(payload: &serde_json::Value) -> Vec<SessionTimelineLink> {
    payload
        .get("batch")
        .and_then(|batch| batch.get("constituent_event_ids"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .map(|event_id| SessionTimelineLink {
            kind: "channel_batch_member".to_string(),
            target_id: None,
            trace_id: None,
            span_id: None,
            event_id: Some(event_id.to_string()),
        })
        .collect()
}

fn span_node_id(trace_id: &str, span_id: u64) -> String {
    format!("span:{trace_id}:{span_id}")
}

fn event_ref(topic: &str, event_id: EventId) -> SessionTimelineReference {
    SessionTimelineReference {
        kind: "event_log".to_string(),
        id: None,
        topic: Some(topic.to_string()),
        event_id: Some(event_id),
    }
}

fn static_topic(topic: &str) -> Topic {
    Topic::new(topic).expect("static session timeline topic should be valid")
}

fn load_run_for_timeline(
    query: &SessionTimelineQuery,
) -> Result<Option<RunRecord>, SessionTimelineError> {
    if let Some(path) = query
        .run_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        return load_run_record_for_timeline(Path::new(path), true);
    }

    let Some(run_id) = query
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
    else {
        return Ok(None);
    };
    let path = default_run_record_path(run_id)?;
    load_run_record_for_timeline(&path, false)
}

fn load_run_record_for_timeline(
    path: &Path,
    explicit: bool,
) -> Result<Option<RunRecord>, SessionTimelineError> {
    if !path.exists() {
        if explicit {
            return Err(SessionTimelineError::RunRecord(format!(
                "session timeline run record not found: {}",
                path.display()
            )));
        }
        return Ok(None);
    }
    load_run_record(path).map(Some).map_err(|error| {
        SessionTimelineError::RunRecord(format!(
            "failed to load session timeline run record {}: {error}",
            path.display()
        ))
    })
}

fn default_run_record_path(run_id: &str) -> Result<PathBuf, SessionTimelineError> {
    if run_id == "." || run_id == ".." || run_id.contains('/') || run_id.contains('\\') {
        return Err(SessionTimelineError::RunRecord(format!(
            "session timeline runId is not a valid default run-record filename: {run_id}"
        )));
    }
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Ok(crate::runtime_paths::run_root(&base).join(format!("{run_id}.json")))
}

#[cfg(test)]
#[path = "session_timeline_tests.rs"]
mod session_timeline_tests;
