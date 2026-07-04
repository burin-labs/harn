use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{AnyEventLog, EventLog, LogError, LogEvent, Topic};
use crate::orchestration::CapabilityPolicy;

pub const CORRECTION_SCHEMA_V0: &str = "harn-correction/v0";
pub const CORRECTIONS_TOPIC: &str = "corrections.records";
pub const CORRECTION_EVENT_KIND: &str = "correction_recorded";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionScope {
    #[default]
    ThisRun,
    ThisPersona,
    All,
}

impl CorrectionScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThisRun => "this_run",
            Self::ThisPersona => "this_persona",
            Self::All => "all",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "this_run" => Ok(Self::ThisRun),
            "this_persona" => Ok(Self::ThisPersona),
            "all" => Ok(Self::All),
            other => Err(format!(
                "unsupported correction scope '{other}', expected this_run|this_persona|all"
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionRecord {
    pub schema: String,
    pub correction_id: String,
    pub from_decision: serde_json::Value,
    pub to_decision: serde_json::Value,
    pub reason: String,
    pub applied_by: String,
    pub scope: CorrectionScope,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl CorrectionRecord {
    pub fn new(
        from_decision: serde_json::Value,
        to_decision: serde_json::Value,
        reason: impl Into<String>,
        applied_by: impl Into<String>,
        scope: CorrectionScope,
    ) -> Self {
        let actor_id = extract_string_anywhere(
            &from_decision,
            &to_decision,
            &["actor_id", "actor", "agent", "trigger_id", "binding_id"],
        );
        let action =
            extract_string_anywhere(&from_decision, &to_decision, &["action", "event_kind"]);
        let trace_id = extract_string_anywhere(&from_decision, &to_decision, &["trace_id"]);
        Self {
            schema: CORRECTION_SCHEMA_V0.to_string(),
            correction_id: Uuid::now_v7().to_string(),
            from_decision,
            to_decision,
            reason: reason.into(),
            applied_by: applied_by.into(),
            scope,
            timestamp: OffsetDateTime::now_utc(),
            actor_id,
            action,
            trace_id,
            step: None,
            evidence_refs: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorrectionQueryFilters {
    pub actor_id: Option<String>,
    pub action: Option<String>,
    pub scope: Option<CorrectionScope>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub since: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub until: Option<OffsetDateTime>,
    pub limit: Option<usize>,
}

pub fn corrections_topic() -> Result<Topic, LogError> {
    Topic::new(CORRECTIONS_TOPIC)
}

pub async fn append_correction_record(
    log: &Arc<AnyEventLog>,
    record: &CorrectionRecord,
) -> Result<CorrectionRecord, LogError> {
    let payload = serde_json::to_value(record)
        .map_err(|error| LogError::Serde(format!("correction record encode error: {error}")))?;
    let mut headers = BTreeMap::new();
    headers.insert("correction_id".to_string(), record.correction_id.clone());
    headers.insert("scope".to_string(), record.scope.as_str().to_string());
    if let Some(actor_id) = record.actor_id.as_ref() {
        headers.insert("actor_id".to_string(), actor_id.clone());
    }
    if let Some(action) = record.action.as_ref() {
        headers.insert("action".to_string(), action.clone());
    }
    if let Some(trace_id) = record.trace_id.as_ref() {
        headers.insert("trace_id".to_string(), trace_id.clone());
    }
    log.append(
        &corrections_topic()?,
        LogEvent::new(CORRECTION_EVENT_KIND, payload).with_headers(headers),
    )
    .await?;
    Ok(record.clone())
}

pub async fn query_correction_records(
    log: &Arc<AnyEventLog>,
    filters: &CorrectionQueryFilters,
) -> Result<Vec<CorrectionRecord>, LogError> {
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for (_, event) in log
        .read_range(&corrections_topic()?, None, usize::MAX)
        .await?
    {
        if event.kind != CORRECTION_EVENT_KIND {
            continue;
        }
        let Ok(record) = serde_json::from_value::<CorrectionRecord>(event.payload) else {
            continue;
        };
        if !matches_filters(&record, filters) {
            continue;
        }
        if seen.insert(record.correction_id.clone()) {
            records.push(record);
        }
    }
    records.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then(left.correction_id.cmp(&right.correction_id))
    });
    apply_limit(&mut records, filters.limit);
    Ok(records)
}

pub async fn apply_corrections_to_policy(
    log: &Arc<AnyEventLog>,
    actor_id: &str,
    policy: CapabilityPolicy,
) -> Result<CapabilityPolicy, LogError> {
    let corrections = query_correction_records(
        log,
        &CorrectionQueryFilters {
            actor_id: Some(actor_id.to_string()),
            ..CorrectionQueryFilters::default()
        },
    )
    .await?;
    Ok(policy_with_corrections(actor_id, policy, &corrections))
}

pub fn policy_with_corrections(
    actor_id: &str,
    mut policy: CapabilityPolicy,
    corrections: &[CorrectionRecord],
) -> CapabilityPolicy {
    let should_tighten = corrections.iter().any(|record| {
        correction_applies_to_actor(record, actor_id)
            && matches!(
                record.scope,
                CorrectionScope::ThisPersona | CorrectionScope::All
            )
    });
    if should_tighten {
        cap_side_effect_level(&mut policy, "read_only");
    }
    policy
}

pub fn correction_record_from_json(value: serde_json::Value) -> Result<CorrectionRecord, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "correction record must be a dict".to_string())?;
    let from_decision = required_json(object, "from_decision")?;
    let to_decision = required_json(object, "to_decision")?;
    let reason = required_string(object, "reason")?;
    let applied_by = required_string(object, "applied_by")?;
    let scope = object
        .get("scope")
        .and_then(serde_json::Value::as_str)
        .map(CorrectionScope::parse)
        .transpose()?
        .unwrap_or_default();

    let mut record = CorrectionRecord::new(from_decision, to_decision, reason, applied_by, scope);
    if let Some(actor_id) = optional_string(object, "actor_id")
        .or_else(|| optional_string(object, "actor"))
        .or_else(|| optional_string(object, "agent"))
    {
        record.actor_id = Some(actor_id);
    }
    if let Some(action) = optional_string(object, "action") {
        record.action = Some(action);
    }
    if let Some(trace_id) = optional_string(object, "trace_id") {
        record.trace_id = Some(trace_id);
    }
    record.step = optional_string(object, "step");
    record.evidence_refs = match object.get("evidence_refs") {
        Some(value) => value
            .as_array()
            .cloned()
            .ok_or_else(|| "correction record field `evidence_refs` must be a list".to_string())?,
        None => Vec::new(),
    };
    if let Some(metadata) = object.get("metadata") {
        let Some(metadata_object) = metadata.as_object() else {
            return Err("correction record field `metadata` must be a dict".to_string());
        };
        record.metadata.extend(
            metadata_object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    Ok(record)
}

pub fn correction_query_filters_from_json(
    value: serde_json::Value,
) -> Result<CorrectionQueryFilters, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "correction query filters must be a dict".to_string())?;
    let scope = object
        .get("scope")
        .and_then(serde_json::Value::as_str)
        .map(CorrectionScope::parse)
        .transpose()?;
    Ok(CorrectionQueryFilters {
        actor_id: optional_string(object, "actor_id")
            .or_else(|| optional_string(object, "actor"))
            .or_else(|| optional_string(object, "agent")),
        action: optional_string(object, "action"),
        scope,
        since: object
            .get("since")
            .and_then(serde_json::Value::as_str)
            .map(parse_rfc3339)
            .transpose()?,
        until: object
            .get("until")
            .and_then(serde_json::Value::as_str)
            .map(parse_rfc3339)
            .transpose()?,
        limit: optional_limit(object, "limit")?,
    })
}

fn matches_filters(record: &CorrectionRecord, filters: &CorrectionQueryFilters) -> bool {
    if let Some(actor_id) = filters.actor_id.as_deref() {
        if !correction_applies_to_actor(record, actor_id) {
            return false;
        }
    }
    if let Some(action) = filters.action.as_deref() {
        if record.action.as_deref() != Some(action) {
            return false;
        }
    }
    if let Some(scope) = filters.scope {
        if record.scope != scope {
            return false;
        }
    }
    if let Some(since) = filters.since {
        if record.timestamp < since {
            return false;
        }
    }
    if let Some(until) = filters.until {
        if record.timestamp > until {
            return false;
        }
    }
    true
}

fn correction_applies_to_actor(record: &CorrectionRecord, actor_id: &str) -> bool {
    record.scope == CorrectionScope::All || record.actor_id.as_deref() == Some(actor_id)
}

fn cap_side_effect_level(policy: &mut CapabilityPolicy, ceiling: &str) {
    use crate::tool_annotations::SideEffectLevel;
    let current = policy.side_effect_level.as_deref().unwrap_or("network");
    if SideEffectLevel::rank_str(current) > SideEffectLevel::rank_str(ceiling) {
        policy.side_effect_level = Some(ceiling.to_string());
    }
}

fn apply_limit(records: &mut Vec<CorrectionRecord>, limit: Option<usize>) {
    let Some(limit) = limit else {
        return;
    };
    if records.len() <= limit {
        return;
    }
    let keep_from = records.len() - limit;
    records.drain(0..keep_from);
}

fn required_json(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<serde_json::Value, String> {
    object
        .get(field)
        .cloned()
        .ok_or_else(|| format!("correction record missing field `{field}`"))
}

fn required_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String, String> {
    optional_string(object, field)
        .ok_or_else(|| format!("correction record missing string field `{field}`"))
}

fn optional_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Option<String> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn optional_limit(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<usize>, String> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if let Some(limit) = value.as_u64() {
        return usize::try_from(limit)
            .map(Some)
            .map_err(|_| format!("correction query field `{field}` is too large"));
    }
    if value.as_i64().is_some() {
        return Err(format!(
            "correction query field `{field}` must be non-negative"
        ));
    }
    Err(format!(
        "correction query field `{field}` must be an integer"
    ))
}

fn extract_string_anywhere(
    first: &serde_json::Value,
    second: &serde_json::Value,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| extract_string(first, key).or_else(|| extract_string(second, key)))
}

fn extract_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|metadata| metadata.get(key))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

fn parse_rfc3339(value: &str) -> Result<OffsetDateTime, String> {
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|error| format!("invalid RFC3339 timestamp '{value}': {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::MemoryEventLog;

    #[tokio::test]
    async fn append_query_and_policy_round_trip() {
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        let mut record = CorrectionRecord::new(
            serde_json::json!({
                "actor_id": "bot",
                "action": "github.issue.opened",
                "outcome": "success"
            }),
            serde_json::json!({"outcome": "denied"}),
            "operator corrected unsafe issue triage",
            "alice",
            CorrectionScope::ThisPersona,
        );
        record.trace_id = Some("trace-correction".to_string());
        append_correction_record(&log, &record).await.unwrap();

        let records = query_correction_records(
            &log,
            &CorrectionQueryFilters {
                actor_id: Some("bot".to_string()),
                action: Some("github.issue.opened".to_string()),
                ..CorrectionQueryFilters::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].applied_by, "alice");
        assert_eq!(records[0].scope, CorrectionScope::ThisPersona);

        let policy = CapabilityPolicy {
            side_effect_level: Some("network".to_string()),
            ..CapabilityPolicy::default()
        };
        let adapted = apply_corrections_to_policy(&log, "bot", policy)
            .await
            .unwrap();
        assert_eq!(adapted.side_effect_level.as_deref(), Some("read_only"));
    }

    #[test]
    fn this_run_scope_does_not_change_persona_policy() {
        let record = CorrectionRecord::new(
            serde_json::json!({"actor_id": "bot", "action": "deploy"}),
            serde_json::json!({"outcome": "denied"}),
            "single-run correction",
            "alice",
            CorrectionScope::ThisRun,
        );
        let policy = CapabilityPolicy {
            side_effect_level: Some("network".to_string()),
            ..CapabilityPolicy::default()
        };

        let adapted = policy_with_corrections("bot", policy, &[record]);

        assert_eq!(adapted.side_effect_level.as_deref(), Some("network"));
    }

    #[test]
    fn correction_parsers_reject_malformed_optional_fields() {
        let record_error = correction_record_from_json(serde_json::json!({
            "from_decision": {"actor_id": "bot"},
            "to_decision": {"outcome": "denied"},
            "reason": "operator corrected routing",
            "applied_by": "alice",
            "evidence_refs": "not-a-list"
        }))
        .expect_err("invalid evidence_refs");
        assert!(record_error.contains("evidence_refs"));

        let limit_error = correction_query_filters_from_json(serde_json::json!({
            "limit": -1
        }))
        .expect_err("invalid limit");
        assert!(limit_error.contains("non-negative"));
    }
}
