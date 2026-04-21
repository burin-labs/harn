use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{
    active_event_log, sanitize_topic_component, AnyEventLog, EventLog, LogError, LogEvent, Topic,
};

pub const OPENTRUSTGRAPH_SCHEMA_V0: &str = "opentrustgraph/v0";
pub const TRUST_GRAPH_GLOBAL_TOPIC: &str = "trust.graph";
pub const TRUST_GRAPH_TOPIC_PREFIX: &str = "trust.graph.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyTier {
    Shadow,
    Suggest,
    ActWithApproval,
    #[default]
    ActAuto,
}

impl AutonomyTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Suggest => "suggest",
            Self::ActWithApproval => "act_with_approval",
            Self::ActAuto => "act_auto",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustOutcome {
    Success,
    Failure,
    Denied,
    Timeout,
}

impl TrustOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Denied => "denied",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrustRecord {
    pub schema: String,
    pub record_id: String,
    pub agent: String,
    pub action: String,
    pub approver: Option<String>,
    pub outcome: TrustOutcome,
    pub trace_id: String,
    pub autonomy_tier: AutonomyTier,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl TrustRecord {
    pub fn new(
        agent: impl Into<String>,
        action: impl Into<String>,
        approver: Option<String>,
        outcome: TrustOutcome,
        trace_id: impl Into<String>,
        autonomy_tier: AutonomyTier,
    ) -> Self {
        Self {
            schema: OPENTRUSTGRAPH_SCHEMA_V0.to_string(),
            record_id: Uuid::now_v7().to_string(),
            agent: agent.into(),
            action: action.into(),
            approver,
            outcome,
            trace_id: trace_id.into(),
            autonomy_tier,
            timestamp: OffsetDateTime::now_utc(),
            cost_usd: None,
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustQueryFilters {
    pub agent: Option<String>,
    pub action: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub since: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub until: Option<OffsetDateTime>,
    pub tier: Option<AutonomyTier>,
    pub outcome: Option<TrustOutcome>,
    pub limit: Option<usize>,
    pub grouped_by_trace: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustTraceGroup {
    pub trace_id: String,
    pub records: Vec<TrustRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustAgentSummary {
    pub agent: String,
    pub total: u64,
    pub success_rate: f64,
    pub mean_cost_usd: Option<f64>,
    pub tier_distribution: BTreeMap<String, u64>,
    pub outcome_distribution: BTreeMap<String, u64>,
}

fn global_topic() -> Result<Topic, LogError> {
    Topic::new(TRUST_GRAPH_GLOBAL_TOPIC)
}

pub fn topic_for_agent(agent: &str) -> Result<Topic, LogError> {
    Topic::new(format!(
        "{TRUST_GRAPH_TOPIC_PREFIX}{}",
        sanitize_topic_component(agent)
    ))
}

pub async fn append_trust_record(
    log: &Arc<AnyEventLog>,
    record: &TrustRecord,
) -> Result<(), LogError> {
    let payload = serde_json::to_value(record)
        .map_err(|error| LogError::Serde(format!("trust record encode error: {error}")))?;
    let mut headers = BTreeMap::new();
    headers.insert("trace_id".to_string(), record.trace_id.clone());
    headers.insert("agent".to_string(), record.agent.clone());
    headers.insert(
        "autonomy_tier".to_string(),
        record.autonomy_tier.as_str().to_string(),
    );
    headers.insert("outcome".to_string(), record.outcome.as_str().to_string());
    let event = LogEvent::new("trust_recorded", payload).with_headers(headers);
    let per_agent = topic_for_agent(&record.agent)?;
    log.append(&global_topic()?, event.clone()).await?;
    log.append(&per_agent, event).await?;
    Ok(())
}

pub async fn append_active_trust_record(record: &TrustRecord) -> Result<(), LogError> {
    let log = active_event_log()
        .ok_or_else(|| LogError::Config("trust graph requires an active event log".to_string()))?;
    append_trust_record(&log, record).await
}

pub async fn query_trust_records(
    log: &Arc<AnyEventLog>,
    filters: &TrustQueryFilters,
) -> Result<Vec<TrustRecord>, LogError> {
    let topic = query_topic(filters)?;
    let events = log.read_range(&topic, None, usize::MAX).await?;
    let mut records = Vec::new();
    for (_, event) in events {
        if event.kind != "trust_recorded" {
            continue;
        }
        let Ok(record) = serde_json::from_value::<TrustRecord>(event.payload) else {
            continue;
        };
        if !matches_filters(&record, filters) {
            continue;
        }
        records.push(record);
    }
    records.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then(left.agent.cmp(&right.agent))
            .then(left.record_id.cmp(&right.record_id))
    });
    apply_record_limit(&mut records, filters.limit);
    Ok(records)
}

pub fn group_trust_records_by_trace(records: &[TrustRecord]) -> Vec<TrustTraceGroup> {
    let mut groups: Vec<TrustTraceGroup> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for record in records {
        if let Some(index) = positions.get(record.trace_id.as_str()).copied() {
            groups[index].records.push(record.clone());
            continue;
        }
        positions.insert(record.trace_id.clone(), groups.len());
        groups.push(TrustTraceGroup {
            trace_id: record.trace_id.clone(),
            records: vec![record.clone()],
        });
    }
    groups
}

pub fn summarize_trust_records(records: &[TrustRecord]) -> Vec<TrustAgentSummary> {
    #[derive(Default)]
    struct RunningSummary {
        total: u64,
        successes: u64,
        cost_sum: f64,
        cost_count: u64,
        tier_distribution: BTreeMap<String, u64>,
        outcome_distribution: BTreeMap<String, u64>,
    }

    let mut by_agent: BTreeMap<String, RunningSummary> = BTreeMap::new();
    for record in records {
        let entry = by_agent.entry(record.agent.clone()).or_default();
        entry.total += 1;
        if record.outcome == TrustOutcome::Success {
            entry.successes += 1;
        }
        if let Some(cost_usd) = record.cost_usd {
            entry.cost_sum += cost_usd;
            entry.cost_count += 1;
        }
        *entry
            .tier_distribution
            .entry(record.autonomy_tier.as_str().to_string())
            .or_default() += 1;
        *entry
            .outcome_distribution
            .entry(record.outcome.as_str().to_string())
            .or_default() += 1;
    }

    by_agent
        .into_iter()
        .map(|(agent, summary)| TrustAgentSummary {
            agent,
            total: summary.total,
            success_rate: if summary.total == 0 {
                0.0
            } else {
                summary.successes as f64 / summary.total as f64
            },
            mean_cost_usd: (summary.cost_count > 0)
                .then_some(summary.cost_sum / summary.cost_count as f64),
            tier_distribution: summary.tier_distribution,
            outcome_distribution: summary.outcome_distribution,
        })
        .collect()
}

pub async fn resolve_agent_autonomy_tier(
    log: &Arc<AnyEventLog>,
    agent: &str,
    default: AutonomyTier,
) -> Result<AutonomyTier, LogError> {
    let records = query_trust_records(
        log,
        &TrustQueryFilters {
            agent: Some(agent.to_string()),
            ..TrustQueryFilters::default()
        },
    )
    .await?;
    let mut current = default;
    for record in records {
        if matches!(record.action.as_str(), "trust.promote" | "trust.demote")
            && record.outcome == TrustOutcome::Success
        {
            current = record.autonomy_tier;
        }
    }
    Ok(current)
}

fn matches_filters(record: &TrustRecord, filters: &TrustQueryFilters) -> bool {
    if let Some(agent) = filters.agent.as_deref() {
        if record.agent != agent {
            return false;
        }
    }
    if let Some(action) = filters.action.as_deref() {
        if record.action != action {
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
    if let Some(tier) = filters.tier {
        if record.autonomy_tier != tier {
            return false;
        }
    }
    if let Some(outcome) = filters.outcome {
        if record.outcome != outcome {
            return false;
        }
    }
    true
}

fn query_topic(filters: &TrustQueryFilters) -> Result<Topic, LogError> {
    match filters.agent.as_deref() {
        Some(agent) => topic_for_agent(agent),
        None => global_topic(),
    }
}

fn apply_record_limit(records: &mut Vec<TrustRecord>, limit: Option<usize>) {
    let Some(limit) = limit else {
        return;
    };
    if records.len() <= limit {
        return;
    }
    let keep_from = records.len() - limit;
    records.drain(0..keep_from);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::MemoryEventLog;
    use time::Duration;

    #[tokio::test]
    async fn append_and_query_round_trip() {
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        let mut record = TrustRecord::new(
            "github-triage-bot",
            "github.issue.opened",
            Some("reviewer".to_string()),
            TrustOutcome::Success,
            "trace-1",
            AutonomyTier::ActWithApproval,
        );
        record.cost_usd = Some(1.25);
        append_trust_record(&log, &record).await.unwrap();

        let records = query_trust_records(
            &log,
            &TrustQueryFilters {
                agent: Some("github-triage-bot".to_string()),
                ..TrustQueryFilters::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].agent, "github-triage-bot");
        assert_eq!(records[0].cost_usd, Some(1.25));
    }

    #[tokio::test]
    async fn resolve_autonomy_tier_prefers_latest_control_record() {
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        append_trust_record(
            &log,
            &TrustRecord::new(
                "bot",
                "trust.promote",
                None,
                TrustOutcome::Success,
                "trace-1",
                AutonomyTier::ActWithApproval,
            ),
        )
        .await
        .unwrap();
        append_trust_record(
            &log,
            &TrustRecord::new(
                "bot",
                "trust.demote",
                None,
                TrustOutcome::Success,
                "trace-2",
                AutonomyTier::Shadow,
            ),
        )
        .await
        .unwrap();

        let tier = resolve_agent_autonomy_tier(&log, "bot", AutonomyTier::ActAuto)
            .await
            .unwrap();
        assert_eq!(tier, AutonomyTier::Shadow);
    }

    #[tokio::test]
    async fn query_limit_keeps_newest_matching_records() {
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        let base = OffsetDateTime::now_utc();
        for (offset, action) in ["first", "second", "third"].into_iter().enumerate() {
            let mut record = TrustRecord::new(
                "bot",
                action,
                None,
                TrustOutcome::Success,
                format!("trace-{action}"),
                AutonomyTier::ActAuto,
            );
            record.timestamp = base + Duration::seconds(offset as i64);
            append_trust_record(&log, &record).await.unwrap();
        }

        let records = query_trust_records(
            &log,
            &TrustQueryFilters {
                agent: Some("bot".to_string()),
                limit: Some(2),
                ..TrustQueryFilters::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].action, "second");
        assert_eq!(records[1].action, "third");
    }

    #[test]
    fn group_by_trace_preserves_chronological_group_order() {
        let make_record = |trace_id: &str, action: &str| TrustRecord {
            trace_id: trace_id.to_string(),
            action: action.to_string(),
            ..TrustRecord::new(
                "bot",
                action,
                None,
                TrustOutcome::Success,
                trace_id,
                AutonomyTier::ActAuto,
            )
        };
        let grouped = group_trust_records_by_trace(&[
            make_record("trace-1", "first"),
            make_record("trace-2", "second"),
            make_record("trace-1", "third"),
        ]);

        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].trace_id, "trace-1");
        assert_eq!(grouped[0].records.len(), 2);
        assert_eq!(grouped[0].records[1].action, "third");
        assert_eq!(grouped[1].trace_id, "trace-2");
    }
}
