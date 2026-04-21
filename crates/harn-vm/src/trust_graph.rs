use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::event_log::{
    active_event_log, sanitize_topic_component, AnyEventLog, EventId, EventLog, LogError, LogEvent,
    Topic,
};
use crate::orchestration::CapabilityPolicy;

pub const OPENTRUSTGRAPH_SCHEMA_V0: &str = "opentrustgraph/v0";
pub const TRUST_GRAPH_GLOBAL_TOPIC: &str = "trust_graph";
pub const TRUST_GRAPH_LEGACY_GLOBAL_TOPIC: &str = "trust.graph";
pub const TRUST_GRAPH_TOPIC_PREFIX: &str = "trust_graph.";
pub const TRUST_GRAPH_LEGACY_TOPIC_PREFIX: &str = "trust.graph.";
pub const TRUST_GRAPH_EVENT_KIND: &str = "trust_recorded";

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
    pub chain_index: u64,
    #[serde(default)]
    pub previous_hash: Option<String>,
    #[serde(default)]
    pub entry_hash: String,
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
            chain_index: 0,
            previous_hash: None,
            entry_hash: String::new(),
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustScore {
    pub agent: String,
    pub action: Option<String>,
    pub total: u64,
    pub successes: u64,
    pub failures: u64,
    pub denied: u64,
    pub timeouts: u64,
    pub success_rate: f64,
    pub latest_outcome: Option<TrustOutcome>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub latest_timestamp: Option<OffsetDateTime>,
    pub effective_tier: AutonomyTier,
    pub policy: CapabilityPolicy,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustChainReport {
    pub topic: String,
    pub total: u64,
    pub verified: bool,
    pub root_hash: Option<String>,
    pub broken_at_event_id: Option<EventId>,
    pub errors: Vec<String>,
}

fn global_topic() -> Result<Topic, LogError> {
    Topic::new(TRUST_GRAPH_GLOBAL_TOPIC)
}

fn legacy_global_topic() -> Result<Topic, LogError> {
    Topic::new(TRUST_GRAPH_LEGACY_GLOBAL_TOPIC)
}

pub fn topic_for_agent(agent: &str) -> Result<Topic, LogError> {
    Topic::new(format!(
        "{TRUST_GRAPH_TOPIC_PREFIX}{}",
        sanitize_topic_component(agent)
    ))
}

pub fn legacy_topic_for_agent(agent: &str) -> Result<Topic, LogError> {
    Topic::new(format!(
        "{TRUST_GRAPH_LEGACY_TOPIC_PREFIX}{}",
        sanitize_topic_component(agent)
    ))
}

pub async fn append_trust_record(
    log: &Arc<AnyEventLog>,
    record: &TrustRecord,
) -> Result<TrustRecord, LogError> {
    let finalized = finalize_trust_record(log, record.clone()).await?;
    let payload = serde_json::to_value(&finalized)
        .map_err(|error| LogError::Serde(format!("trust record encode error: {error}")))?;
    let mut headers = BTreeMap::new();
    headers.insert("trace_id".to_string(), finalized.trace_id.clone());
    headers.insert("agent".to_string(), finalized.agent.clone());
    headers.insert(
        "autonomy_tier".to_string(),
        finalized.autonomy_tier.as_str().to_string(),
    );
    headers.insert(
        "outcome".to_string(),
        finalized.outcome.as_str().to_string(),
    );
    headers.insert("entry_hash".to_string(), finalized.entry_hash.clone());
    let event = LogEvent::new(TRUST_GRAPH_EVENT_KIND, payload).with_headers(headers);
    for topic in append_topics_for_record(&finalized)? {
        log.append(&topic, event.clone()).await?;
    }
    Ok(finalized)
}

pub async fn append_active_trust_record(record: &TrustRecord) -> Result<TrustRecord, LogError> {
    let log = active_event_log()
        .ok_or_else(|| LogError::Config("trust graph requires an active event log".to_string()))?;
    append_trust_record(&log, record).await
}

pub async fn query_trust_records(
    log: &Arc<AnyEventLog>,
    filters: &TrustQueryFilters,
) -> Result<Vec<TrustRecord>, LogError> {
    let topics = query_topics(filters)?;
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for topic in topics {
        for (_, event) in log.read_range(&topic, None, usize::MAX).await? {
            if event.kind != TRUST_GRAPH_EVENT_KIND {
                continue;
            }
            let Ok(record) = serde_json::from_value::<TrustRecord>(event.payload) else {
                continue;
            };
            if !matches_filters(&record, filters) {
                continue;
            }
            let dedupe_key = trust_record_dedupe_key(&record);
            if seen.insert(dedupe_key) {
                records.push(record);
            }
        }
    }
    records.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then(left.chain_index.cmp(&right.chain_index))
            .then(left.agent.cmp(&right.agent))
            .then(left.record_id.cmp(&right.record_id))
    });
    apply_record_limit(&mut records, filters.limit);
    Ok(records)
}

pub async fn trust_score_for(
    log: &Arc<AnyEventLog>,
    agent: &str,
    action: Option<&str>,
) -> Result<TrustScore, LogError> {
    let records = query_trust_records(
        log,
        &TrustQueryFilters {
            agent: Some(agent.to_string()),
            action: action.map(ToString::to_string),
            ..TrustQueryFilters::default()
        },
    )
    .await?;
    let effective_tier = resolve_agent_autonomy_tier(log, agent, AutonomyTier::ActAuto).await?;
    Ok(score_from_records(agent, action, effective_tier, &records))
}

pub async fn policy_for_agent(
    log: &Arc<AnyEventLog>,
    agent: &str,
) -> Result<CapabilityPolicy, LogError> {
    Ok(trust_score_for(log, agent, None).await?.policy)
}

pub async fn verify_trust_chain(log: &Arc<AnyEventLog>) -> Result<TrustChainReport, LogError> {
    let (topic, records) = preferred_chain_records(log).await?;
    let mut previous_hash: Option<String> = None;
    let mut errors = Vec::new();
    let mut broken_at_event_id = None;

    for (position, (event_id, record)) in records.iter().enumerate() {
        let expected_index = (position as u64) + 1;
        if record.chain_index != expected_index {
            errors.push(format!(
                "event {event_id}: expected chain_index {expected_index}, found {}",
                record.chain_index
            ));
        }
        if record.previous_hash != previous_hash {
            errors.push(format!(
                "event {event_id}: previous_hash mismatch; expected {:?}, found {:?}",
                previous_hash, record.previous_hash
            ));
        }
        match compute_trust_record_hash(record) {
            Ok(expected_hash) if expected_hash == record.entry_hash => {}
            Ok(expected_hash) => errors.push(format!(
                "event {event_id}: entry_hash mismatch; expected {expected_hash}, found {}",
                record.entry_hash
            )),
            Err(error) => errors.push(format!("event {event_id}: {error}")),
        }
        if !errors.is_empty() && broken_at_event_id.is_none() {
            broken_at_event_id = Some(*event_id);
        }
        previous_hash = Some(record.entry_hash.clone());
    }

    Ok(TrustChainReport {
        topic: topic.as_str().to_string(),
        total: records.len() as u64,
        verified: errors.is_empty(),
        root_hash: records.last().map(|(_, record)| record.entry_hash.clone()),
        broken_at_event_id,
        errors,
    })
}

pub fn compute_trust_record_hash(record: &TrustRecord) -> Result<String, LogError> {
    let mut value = serde_json::to_value(record)
        .map_err(|error| LogError::Serde(format!("trust record hash encode error: {error}")))?;
    if let Some(object) = value.as_object_mut() {
        object.remove("entry_hash");
    }
    let canonical = serde_json::to_string(&value)
        .map_err(|error| LogError::Serde(format!("trust record canonicalize error: {error}")))?;
    let digest = Sha256::digest(canonical.as_bytes());
    Ok(format!("sha256:{}", hex::encode(digest)))
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

fn query_topics(filters: &TrustQueryFilters) -> Result<Vec<Topic>, LogError> {
    match filters.agent.as_deref() {
        Some(agent) => Ok(vec![
            topic_for_agent(agent)?,
            legacy_topic_for_agent(agent)?,
        ]),
        None => Ok(vec![global_topic()?, legacy_global_topic()?]),
    }
}

fn append_topics_for_record(record: &TrustRecord) -> Result<Vec<Topic>, LogError> {
    Ok(vec![
        global_topic()?,
        legacy_global_topic()?,
        topic_for_agent(&record.agent)?,
        legacy_topic_for_agent(&record.agent)?,
    ])
}

async fn finalize_trust_record(
    log: &Arc<AnyEventLog>,
    mut record: TrustRecord,
) -> Result<TrustRecord, LogError> {
    let latest = latest_chain_record(log).await?;
    record.chain_index = latest
        .as_ref()
        .map(|(_, record)| record.chain_index.saturating_add(1).max(1))
        .unwrap_or(1);
    record.previous_hash = latest.and_then(|(_, record)| {
        if record.entry_hash.is_empty() {
            compute_trust_record_hash(&record).ok()
        } else {
            Some(record.entry_hash)
        }
    });
    record.entry_hash.clear();
    record.entry_hash = compute_trust_record_hash(&record)?;
    Ok(record)
}

async fn latest_chain_record(
    log: &Arc<AnyEventLog>,
) -> Result<Option<(EventId, TrustRecord)>, LogError> {
    let (_, records) = preferred_chain_records(log).await?;
    Ok(records.into_iter().last())
}

async fn preferred_chain_records(
    log: &Arc<AnyEventLog>,
) -> Result<(Topic, Vec<(EventId, TrustRecord)>), LogError> {
    let canonical = global_topic()?;
    let canonical_records = read_trust_records_from_topic(log, &canonical).await?;
    if !canonical_records.is_empty() {
        return Ok((canonical, canonical_records));
    }
    let legacy = legacy_global_topic()?;
    let legacy_records = read_trust_records_from_topic(log, &legacy).await?;
    if legacy_records.is_empty() {
        Ok((canonical, Vec::new()))
    } else {
        Ok((legacy, legacy_records))
    }
}

async fn read_trust_records_from_topic(
    log: &Arc<AnyEventLog>,
    topic: &Topic,
) -> Result<Vec<(EventId, TrustRecord)>, LogError> {
    let events = log.read_range(topic, None, usize::MAX).await?;
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for (event_id, event) in events {
        if event.kind != TRUST_GRAPH_EVENT_KIND {
            continue;
        }
        let Ok(record) = serde_json::from_value::<TrustRecord>(event.payload) else {
            continue;
        };
        if seen.insert(trust_record_dedupe_key(&record)) {
            records.push((event_id, record));
        }
    }
    Ok(records)
}

fn trust_record_dedupe_key(record: &TrustRecord) -> String {
    if !record.entry_hash.is_empty() {
        return record.entry_hash.clone();
    }
    record.record_id.clone()
}

fn score_from_records(
    agent: &str,
    action: Option<&str>,
    effective_tier: AutonomyTier,
    records: &[TrustRecord],
) -> TrustScore {
    let mut score = TrustScore {
        agent: agent.to_string(),
        action: action.map(ToString::to_string),
        effective_tier,
        ..TrustScore::default()
    };
    for record in records {
        score.total += 1;
        match record.outcome {
            TrustOutcome::Success => score.successes += 1,
            TrustOutcome::Failure => score.failures += 1,
            TrustOutcome::Denied => score.denied += 1,
            TrustOutcome::Timeout => score.timeouts += 1,
        }
        score.latest_outcome = Some(record.outcome);
        score.latest_timestamp = Some(record.timestamp);
    }
    score.success_rate = if score.total == 0 {
        0.0
    } else {
        score.successes as f64 / score.total as f64
    };
    score.policy = policy_from_score(&score);
    score
}

fn policy_from_score(score: &TrustScore) -> CapabilityPolicy {
    let mut policy = CapabilityPolicy {
        side_effect_level: Some(
            match score.effective_tier {
                AutonomyTier::Shadow => "none",
                AutonomyTier::Suggest => "read_only",
                AutonomyTier::ActWithApproval => "workspace_write",
                AutonomyTier::ActAuto => "network",
            }
            .to_string(),
        ),
        ..CapabilityPolicy::default()
    };
    let latest_bad = matches!(
        score.latest_outcome,
        Some(TrustOutcome::Denied | TrustOutcome::Failure | TrustOutcome::Timeout)
    );
    if latest_bad || (score.total >= 3 && score.success_rate < 0.5) {
        policy.side_effect_level = Some("read_only".to_string());
    }
    if matches!(score.effective_tier, AutonomyTier::Shadow) {
        policy.recursion_limit = Some(0);
    }
    policy
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
        assert_eq!(records[0].chain_index, 1);
        assert!(records[0].previous_hash.is_none());
        assert!(records[0].entry_hash.starts_with("sha256:"));
    }

    #[tokio::test]
    async fn verify_chain_detects_hash_tampering() {
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        let first = append_trust_record(
            &log,
            &TrustRecord::new(
                "bot",
                "first",
                None,
                TrustOutcome::Success,
                "trace-1",
                AutonomyTier::Suggest,
            ),
        )
        .await
        .unwrap();
        let mut second = append_trust_record(
            &log,
            &TrustRecord::new(
                "bot",
                "second",
                None,
                TrustOutcome::Success,
                "trace-2",
                AutonomyTier::Suggest,
            ),
        )
        .await
        .unwrap();

        let report = verify_trust_chain(&log).await.unwrap();
        assert!(report.verified);
        assert_eq!(
            report.root_hash.as_deref(),
            Some(second.entry_hash.as_str())
        );
        assert_eq!(
            second.previous_hash.as_deref(),
            Some(first.entry_hash.as_str())
        );

        second.previous_hash = Some(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        );
        second.entry_hash =
            "sha256:1111111111111111111111111111111111111111111111111111111111111111".to_string();
        log.append(
            &global_topic().unwrap(),
            LogEvent::new(
                TRUST_GRAPH_EVENT_KIND,
                serde_json::to_value(second).unwrap(),
            ),
        )
        .await
        .unwrap();
        let report = verify_trust_chain(&log).await.unwrap();
        assert!(!report.verified);
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("previous_hash mismatch")));
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
