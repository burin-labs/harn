use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::actor_chain::ActorChain;
use crate::event_log::{
    active_event_log, sanitize_topic_component, AnyEventLog, EventId, EventLog, LogError, LogEvent,
    Topic,
};
use crate::orchestration::{CapabilityPolicy, EffectRecord};

pub const OPENTRUSTGRAPH_SCHEMA_V0: &str = "opentrustgraph/v0";
/// OpenTrustGraph v0.1: additive metadata schema. Reserves lineage keys under
/// `TrustRecord.metadata` so chain validators can prove that child-agent
/// effects, actors, and actor-chain policy alerts stay inside the parent chain.
///
/// Backwards compatible: v0 records are still accepted (the new keys are
/// optional). One patch release window after this bump, v0 will be
/// dropped per `opentrustgraph-spec/CONFORMANCE.md` §5.
pub const OPENTRUSTGRAPH_SCHEMA_V0_1: &str = "opentrustgraph/v0.1";
/// Set of schema discriminators accepted by the v0.1 validator.
pub const OPENTRUSTGRAPH_ACCEPTED_SCHEMAS: &[&str] =
    &[OPENTRUSTGRAPH_SCHEMA_V0_1, OPENTRUSTGRAPH_SCHEMA_V0];
pub const OPENTRUSTGRAPH_CHAIN_SCHEMA_V0: &str = "opentrustgraph-chain/v0";

/// Reserved metadata key for the effect grant attached to a record by its
/// spawning parent.
pub const METADATA_KEY_EFFECTS_GRANT: &str = "effects_grant";
/// Reserved metadata key for the effects the recorded action actually
/// exercised. Must be a subset of the parent's `effects_grant`.
pub const METADATA_KEY_EFFECTS_USED: &str = "effects_used";
/// Reserved metadata key pointing at the parent record's `record_id`.
/// Lets verifiers reconstruct the agent chain without scanning the whole
/// stream.
pub const METADATA_KEY_PARENT_RECORD_ID: &str = "parent_record_id";
/// Reserved metadata key carrying the RFC 8693 actor chain for the record.
/// When paired with `parent_record_id`, the nested `act` chain must extend
/// the parent's actor chain by exactly one hop.
pub const METADATA_KEY_ACTOR_CHAIN: &str = "actor_chain";
/// Reserved metadata key for actor-chain policy alerts.
pub const METADATA_KEY_ACTOR_CHAIN_ALERT: &str = "actor_chain_alert";
pub const TRUST_GRAPH_RECORDS_TOPIC: &str = "trust_graph.records";
pub const TRUST_GRAPH_GLOBAL_TOPIC: &str = "trust_graph";
pub const TRUST_GRAPH_LEGACY_GLOBAL_TOPIC: &str = "trust.graph";
pub const TRUST_GRAPH_TOPIC_PREFIX: &str = "trust_graph.";
pub const TRUST_GRAPH_LEGACY_TOPIC_PREFIX: &str = "trust.graph.";
pub const TRUST_GRAPH_EVENT_KIND: &str = "trust_recorded";
pub const TRUST_ACTION_RELEASE: &str = "release";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrustRecordActionKind {
    Release {
        bundle_hash: String,
        harn_version: String,
        parent_trust_record_id: Option<String>,
    },
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
            schema: OPENTRUSTGRAPH_SCHEMA_V0_1.to_string(),
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

    pub fn release(
        agent: impl Into<String>,
        bundle_hash: impl Into<String>,
        harn_version: impl Into<String>,
        parent_trust_record_id: Option<String>,
        trace_id: impl Into<String>,
        autonomy_tier: AutonomyTier,
    ) -> Self {
        let bundle_hash = bundle_hash.into();
        let harn_version = harn_version.into();
        let action_kind = TrustRecordActionKind::Release {
            bundle_hash: bundle_hash.clone(),
            harn_version: harn_version.clone(),
            parent_trust_record_id: parent_trust_record_id.clone(),
        };
        let mut record = Self::new(
            agent,
            TRUST_ACTION_RELEASE,
            None,
            TrustOutcome::Success,
            trace_id,
            autonomy_tier,
        );
        record
            .metadata
            .insert("action_kind".to_string(), serde_json::json!(action_kind));
        record
            .metadata
            .insert("bundle_hash".to_string(), serde_json::json!(bundle_hash));
        record
            .metadata
            .insert("harn_version".to_string(), serde_json::json!(harn_version));
        record.metadata.insert(
            "parent_trust_record_id".to_string(),
            parent_trust_record_id
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null),
        );
        record
    }

    /// Attach the typed effect grant a parent extended to this record.
    /// Empty grants are skipped so records stay compact when there is
    /// nothing to prove.
    pub fn with_effects_grant(mut self, effects: Vec<EffectRecord>) -> Self {
        self.set_effects_grant(effects);
        self
    }

    pub fn set_effects_grant(&mut self, effects: Vec<EffectRecord>) {
        if effects.is_empty() {
            self.metadata.remove(METADATA_KEY_EFFECTS_GRANT);
            return;
        }
        self.metadata.insert(
            METADATA_KEY_EFFECTS_GRANT.to_string(),
            serde_json::to_value(effects).expect("EffectRecord is serializable"),
        );
    }

    pub fn effects_grant(&self) -> Vec<EffectRecord> {
        decode_effect_list(self.metadata.get(METADATA_KEY_EFFECTS_GRANT))
    }

    /// Attach the typed effect set the action actually exercised.
    /// Verifiers must check `effects_used ⊆ effects_grant` through the
    /// parent chain.
    pub fn with_effects_used(mut self, effects: Vec<EffectRecord>) -> Self {
        self.set_effects_used(effects);
        self
    }

    pub fn set_effects_used(&mut self, effects: Vec<EffectRecord>) {
        if effects.is_empty() {
            self.metadata.remove(METADATA_KEY_EFFECTS_USED);
            return;
        }
        self.metadata.insert(
            METADATA_KEY_EFFECTS_USED.to_string(),
            serde_json::to_value(effects).expect("EffectRecord is serializable"),
        );
    }

    pub fn effects_used(&self) -> Vec<EffectRecord> {
        decode_effect_list(self.metadata.get(METADATA_KEY_EFFECTS_USED))
    }

    /// Point this record at its parent's `record_id`. The existing
    /// release-record key (`parent_trust_record_id`) is retained for the
    /// release flow; this is the generic spawn-lineage pointer.
    pub fn with_parent_record_id(mut self, parent_record_id: impl Into<String>) -> Self {
        self.set_parent_record_id(Some(parent_record_id.into()));
        self
    }

    pub fn set_parent_record_id(&mut self, parent_record_id: Option<String>) {
        match parent_record_id {
            Some(id) if !id.is_empty() => {
                self.metadata.insert(
                    METADATA_KEY_PARENT_RECORD_ID.to_string(),
                    serde_json::Value::String(id),
                );
            }
            _ => {
                self.metadata.remove(METADATA_KEY_PARENT_RECORD_ID);
            }
        }
    }

    pub fn parent_record_id(&self) -> Option<String> {
        self.metadata
            .get(METADATA_KEY_PARENT_RECORD_ID)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    }

    /// Attach the RFC 8693 actor chain for the principal that caused this
    /// record.
    pub fn with_actor_chain(mut self, actor_chain: ActorChain) -> Self {
        self.set_actor_chain(Some(actor_chain));
        self
    }

    /// Set or clear the reserved `actor_chain` metadata entry.
    pub fn set_actor_chain(&mut self, actor_chain: Option<ActorChain>) {
        match actor_chain {
            Some(actor_chain) => {
                self.metadata.insert(
                    METADATA_KEY_ACTOR_CHAIN.to_string(),
                    actor_chain.to_json_value(),
                );
            }
            None => {
                self.metadata.remove(METADATA_KEY_ACTOR_CHAIN);
            }
        }
    }

    /// Decode the reserved actor-chain metadata entry, dropping malformed
    /// values for callers that only need best-effort display data.
    pub fn actor_chain(&self) -> Option<ActorChain> {
        self.try_actor_chain().ok().flatten()
    }

    /// Decode the reserved actor-chain metadata entry and report malformed
    /// RFC 8693 claim shapes to strict validators.
    pub fn try_actor_chain(&self) -> Result<Option<ActorChain>, crate::ActorChainError> {
        self.metadata
            .get(METADATA_KEY_ACTOR_CHAIN)
            .map(ActorChain::from_json_value)
            .transpose()
    }

    pub fn with_actor_chain_alert(mut self, alert: serde_json::Value) -> Self {
        self.set_actor_chain_alert(Some(alert));
        self
    }

    pub fn set_actor_chain_alert(&mut self, alert: Option<serde_json::Value>) {
        match alert {
            Some(alert) => {
                self.metadata
                    .insert(METADATA_KEY_ACTOR_CHAIN_ALERT.to_string(), alert);
            }
            None => {
                self.metadata.remove(METADATA_KEY_ACTOR_CHAIN_ALERT);
            }
        }
    }

    pub fn actor_chain_alert(&self) -> Option<&serde_json::Value> {
        self.metadata.get(METADATA_KEY_ACTOR_CHAIN_ALERT)
    }
}

fn decode_effect_list(value: Option<&serde_json::Value>) -> Vec<EffectRecord> {
    value
        .and_then(|value| serde_json::from_value::<Vec<EffectRecord>>(value.clone()).ok())
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustGraphRecord {
    pub actor_id: String,
    pub action: String,
    pub approver: Option<String>,
    pub outcome: TrustOutcome,
    #[serde(default)]
    pub evidence_refs: Vec<serde_json::Value>,
    pub trace_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub autonomy_tier_at_time: AutonomyTier,
}

impl TrustGraphRecord {
    pub fn from_trust_record(record: &TrustRecord) -> Self {
        Self {
            actor_id: record.agent.clone(),
            action: record.action.clone(),
            approver: record.approver.clone(),
            outcome: record.outcome,
            evidence_refs: evidence_refs_from_metadata(&record.metadata),
            trace_id: record.trace_id.clone(),
            timestamp: record.timestamp,
            autonomy_tier_at_time: record.autonomy_tier,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustChainReport {
    pub topic: String,
    pub total: u64,
    pub verified: bool,
    pub root_hash: Option<String>,
    pub broken_at_event_id: Option<EventId>,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustChainExportProducer {
    pub name: String,
    pub version: String,
}

impl Default for TrustChainExportProducer {
    fn default() -> Self {
        Self {
            name: "harn".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustChainExportMetadata {
    pub topic: String,
    pub total: u64,
    pub root_hash: Option<String>,
    pub verified: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub producer: TrustChainExportProducer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrustChainExport {
    pub schema: String,
    pub chain: TrustChainExportMetadata,
    pub records: Vec<TrustRecord>,
}

fn global_topic() -> Result<Topic, LogError> {
    Topic::new(TRUST_GRAPH_GLOBAL_TOPIC)
}

fn legacy_global_topic() -> Result<Topic, LogError> {
    Topic::new(TRUST_GRAPH_LEGACY_GLOBAL_TOPIC)
}

fn records_topic() -> Result<Topic, LogError> {
    Topic::new(TRUST_GRAPH_RECORDS_TOPIC)
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
    append_trust_graph_record_projection(log, &finalized).await?;
    Ok(finalized)
}

pub async fn append_active_trust_record(record: &TrustRecord) -> Result<TrustRecord, LogError> {
    let log = active_event_log()
        .ok_or_else(|| LogError::Config("trust graph requires an active event log".to_string()))?;
    append_trust_record(&log, record).await
}

pub async fn append_scope_attenuation_alert(
    log: &Arc<AnyEventLog>,
    actor_chain: &crate::ActorChain,
    violation: &crate::ScopeAttenuationViolation,
    trace_id: impl Into<String>,
) -> Result<TrustRecord, LogError> {
    let record = TrustRecord::new(
        violation.child_subject(),
        "identity.scope_attenuation",
        None,
        TrustOutcome::Denied,
        trace_id,
        AutonomyTier::ActAuto,
    )
    .with_actor_chain(actor_chain.clone())
    .with_actor_chain_alert(violation.to_json_value());
    append_trust_record(log, &record).await
}

pub async fn append_active_scope_attenuation_alert(
    actor_chain: &crate::ActorChain,
    violation: &crate::ScopeAttenuationViolation,
    trace_id: impl Into<String>,
) -> Result<TrustRecord, LogError> {
    let log = active_event_log()
        .ok_or_else(|| LogError::Config("trust graph requires an active event log".to_string()))?;
    append_scope_attenuation_alert(&log, actor_chain, violation, trace_id).await
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

pub async fn query_trust_graph_records(
    log: &Arc<AnyEventLog>,
    filters: &TrustQueryFilters,
) -> Result<Vec<TrustGraphRecord>, LogError> {
    let mut graph_records = Vec::new();
    let mut seen = HashSet::new();

    for record in query_trust_records(log, filters).await? {
        let graph_record = TrustGraphRecord::from_trust_record(&record);
        let dedupe_key = trust_graph_record_dedupe_key(&graph_record);
        if seen.insert(dedupe_key) {
            graph_records.push(graph_record);
        }
    }

    for (_, event) in log.read_range(&records_topic()?, None, usize::MAX).await? {
        if event.kind != TRUST_GRAPH_EVENT_KIND {
            continue;
        }
        let Ok(record) = serde_json::from_value::<TrustGraphRecord>(event.payload) else {
            continue;
        };
        if !matches_graph_filters(&record, filters) {
            continue;
        }
        let dedupe_key = trust_graph_record_dedupe_key(&record);
        if seen.insert(dedupe_key) {
            graph_records.push(record);
        }
    }

    graph_records.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then(left.actor_id.cmp(&right.actor_id))
            .then(left.action.cmp(&right.action))
            .then(left.trace_id.cmp(&right.trace_id))
    });
    apply_graph_record_limit(&mut graph_records, filters.limit);
    Ok(graph_records)
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
    let mut score = score_from_records(agent, action, effective_tier, &records);
    score.policy =
        crate::corrections::apply_corrections_to_policy(log, agent, score.policy).await?;
    Ok(score)
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
    let lineage_errors = validate_lineage_invariants(
        records
            .iter()
            .map(|(event_id, record)| (format!("event {event_id}"), Some(*event_id), record)),
    );
    if broken_at_event_id.is_none() {
        broken_at_event_id = lineage_errors.iter().find_map(|error| error.event_id);
    }
    errors.extend(lineage_errors.into_iter().map(|error| error.message));

    Ok(TrustChainReport {
        topic: topic.as_str().to_string(),
        total: records.len() as u64,
        verified: errors.is_empty(),
        root_hash: records.last().map(|(_, record)| record.entry_hash.clone()),
        broken_at_event_id,
        errors,
    })
}

pub async fn export_trust_chain(log: &Arc<AnyEventLog>) -> Result<TrustChainExport, LogError> {
    let (topic, records_with_ids) = preferred_chain_records(log).await?;
    let report = verify_trust_chain(log).await?;
    let records: Vec<TrustRecord> = records_with_ids.into_iter().map(|(_, r)| r).collect();
    Ok(TrustChainExport {
        schema: OPENTRUSTGRAPH_CHAIN_SCHEMA_V0.to_string(),
        chain: TrustChainExportMetadata {
            topic: topic.as_str().to_string(),
            total: records.len() as u64,
            root_hash: records.last().map(|record| record.entry_hash.clone()),
            verified: report.verified,
            generated_at: OffsetDateTime::now_utc(),
            producer: TrustChainExportProducer::default(),
        },
        records,
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

struct LineageInvariantError {
    event_id: Option<EventId>,
    message: String,
}

impl LineageInvariantError {
    fn new(event_id: Option<EventId>, message: String) -> Self {
        Self { event_id, message }
    }
}

fn validate_lineage_invariants<'a, I>(records: I) -> Vec<LineageInvariantError>
where
    I: IntoIterator<Item = (String, Option<EventId>, &'a TrustRecord)>,
{
    let mut errors = Vec::new();
    let mut by_id: HashMap<&'a str, &'a TrustRecord> = HashMap::new();

    for (label, event_id, record) in records {
        let actor_chain = match record.try_actor_chain() {
            Ok(actor_chain) => actor_chain,
            Err(error) => {
                errors.push(LineageInvariantError::new(
                    event_id,
                    format!("{label}: actor_chain invalid: {error}"),
                ));
                None
            }
        };
        let effects_used = record.effects_used();
        if let Some(parent_id) = record.parent_record_id() {
            let parent = by_id.get(parent_id.as_str()).copied();
            if parent.is_none() && (!effects_used.is_empty() || actor_chain.is_some()) {
                errors.push(LineageInvariantError::new(
                    event_id,
                    format!("{label}: parent_record_id {parent_id:?} not found in chain"),
                ));
            }
            if let Some(parent) = parent {
                validate_effect_lineage(
                    &mut errors,
                    &label,
                    event_id,
                    &parent_id,
                    parent,
                    &effects_used,
                );
                validate_actor_lineage(
                    &mut errors,
                    &label,
                    event_id,
                    &parent_id,
                    parent,
                    actor_chain,
                );
            }
        }

        if !record.record_id.is_empty() {
            by_id.insert(record.record_id.as_str(), record);
        }
    }

    errors
}

fn validate_effect_lineage(
    errors: &mut Vec<LineageInvariantError>,
    label: &str,
    event_id: Option<EventId>,
    parent_id: &str,
    parent: &TrustRecord,
    effects_used: &[EffectRecord],
) {
    if effects_used.is_empty() {
        return;
    }
    let parent_grant = parent.effects_grant();
    for effect in effects_used {
        if !parent_grant.contains(effect) {
            errors.push(LineageInvariantError::new(
                event_id,
                format!(
                    "{label}: effects_used escaped grant from parent {parent_id:?}: {effect:?}"
                ),
            ));
        }
    }
}

fn validate_actor_lineage(
    errors: &mut Vec<LineageInvariantError>,
    label: &str,
    event_id: Option<EventId>,
    parent_id: &str,
    parent: &TrustRecord,
    actor_chain: Option<ActorChain>,
) {
    let Some(actor_chain) = actor_chain else {
        return;
    };
    let parent_actor_chain = match parent.try_actor_chain() {
        Ok(Some(parent_actor_chain)) => parent_actor_chain,
        Ok(None) => {
            errors.push(LineageInvariantError::new(
                event_id,
                format!("{label}: actor_chain parent {parent_id:?} missing actor_chain"),
            ));
            return;
        }
        Err(error) => {
            errors.push(LineageInvariantError::new(
                event_id,
                format!("{label}: parent actor_chain invalid: {error}"),
            ));
            return;
        }
    };
    if !actor_chain_extends_parent(&actor_chain, &parent_actor_chain) {
        errors.push(LineageInvariantError::new(
            event_id,
            format!("{label}: actor_chain escaped parentage from parent {parent_id:?}"),
        ));
    }
}

fn actor_chain_extends_parent(child: &ActorChain, parent: &ActorChain) -> bool {
    if child.origin() != parent.origin() {
        return false;
    }
    let child_actors: Vec<&str> = child.actors().collect();
    let parent_actors: Vec<&str> = parent.actors().collect();
    child_actors.len() == parent_actors.len() + 1 && child_actors[1..] == parent_actors[..]
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

fn matches_graph_filters(record: &TrustGraphRecord, filters: &TrustQueryFilters) -> bool {
    if let Some(agent) = filters.agent.as_deref() {
        if record.actor_id != agent {
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
        if record.autonomy_tier_at_time != tier {
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
        Some(agent) => unique_topics(vec![
            topic_for_agent(agent)?,
            legacy_topic_for_agent(agent)?,
        ]),
        None => unique_topics(vec![global_topic()?, legacy_global_topic()?]),
    }
}

fn append_topics_for_record(record: &TrustRecord) -> Result<Vec<Topic>, LogError> {
    unique_topics(vec![
        global_topic()?,
        legacy_global_topic()?,
        topic_for_agent(&record.agent)?,
        legacy_topic_for_agent(&record.agent)?,
    ])
}

fn unique_topics(topics: Vec<Topic>) -> Result<Vec<Topic>, LogError> {
    let mut seen = HashSet::new();
    Ok(topics
        .into_iter()
        .filter(|topic| seen.insert(topic.as_str().to_string()))
        .collect())
}

async fn append_trust_graph_record_projection(
    log: &Arc<AnyEventLog>,
    record: &TrustRecord,
) -> Result<(), LogError> {
    let payload = serde_json::to_value(TrustGraphRecord::from_trust_record(record))
        .map_err(|error| LogError::Serde(format!("trust graph record encode error: {error}")))?;
    let mut headers = BTreeMap::new();
    headers.insert("trace_id".to_string(), record.trace_id.clone());
    headers.insert("actor_id".to_string(), record.agent.clone());
    headers.insert("action".to_string(), record.action.clone());
    headers.insert(
        "autonomy_tier_at_time".to_string(),
        record.autonomy_tier.as_str().to_string(),
    );
    headers.insert("outcome".to_string(), record.outcome.as_str().to_string());
    log.append(
        &records_topic()?,
        LogEvent::new(TRUST_GRAPH_EVENT_KIND, payload).with_headers(headers),
    )
    .await?;
    Ok(())
}

async fn finalize_trust_record(
    log: &Arc<AnyEventLog>,
    mut record: TrustRecord,
) -> Result<TrustRecord, LogError> {
    attach_current_actor_chain(&mut record);
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

fn attach_current_actor_chain(record: &mut TrustRecord) {
    if record.metadata.contains_key(METADATA_KEY_ACTOR_CHAIN) {
        return;
    }
    if let Some(actor_chain) = crate::agent_sessions::current_actor_chain() {
        record.set_actor_chain(Some(actor_chain));
    }
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

fn trust_graph_record_dedupe_key(record: &TrustGraphRecord) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        record.actor_id,
        record.action,
        record.trace_id,
        record.timestamp,
        record.outcome.as_str()
    )
}

fn evidence_refs_from_metadata(
    metadata: &BTreeMap<String, serde_json::Value>,
) -> Vec<serde_json::Value> {
    metadata
        .get("evidence_refs")
        .or_else(|| metadata.get("evidenceRefs"))
        .or_else(|| {
            metadata
                .get("approval")
                .and_then(|approval| approval.get("evidence_refs"))
        })
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
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
    let recent_cutoff = OffsetDateTime::now_utc() - Duration::days(30);
    let mut recent_successes = 0;
    let mut recent_bad_or_rollback = false;
    for record in records {
        score.total += 1;
        match record.outcome {
            TrustOutcome::Success => score.successes += 1,
            TrustOutcome::Failure => score.failures += 1,
            TrustOutcome::Denied => score.denied += 1,
            TrustOutcome::Timeout => score.timeouts += 1,
        }
        if record.timestamp >= recent_cutoff {
            if record.outcome == TrustOutcome::Success && !is_control_plane_action(&record.action) {
                recent_successes += 1;
            } else if record.outcome != TrustOutcome::Success {
                recent_bad_or_rollback = true;
            }
            if record.action.contains("rollback") {
                recent_bad_or_rollback = true;
            }
        }
        score.latest_outcome = Some(record.outcome);
        score.latest_timestamp = Some(record.timestamp);
    }
    score.success_rate = if score.total == 0 {
        0.0
    } else {
        score.successes as f64 / score.total as f64
    };
    score.policy = policy_from_score(&score, recent_successes, recent_bad_or_rollback);
    score
}

fn policy_from_score(
    score: &TrustScore,
    recent_successes: u64,
    recent_bad_or_rollback: bool,
) -> CapabilityPolicy {
    let mut policy = policy_for_autonomy_tier(score.effective_tier);
    let latest_bad = matches!(
        score.latest_outcome,
        Some(TrustOutcome::Denied | TrustOutcome::Failure | TrustOutcome::Timeout)
    );
    let trusted_recent_track_record = score.effective_tier == AutonomyTier::ActWithApproval
        && recent_successes >= 10
        && !recent_bad_or_rollback;
    if latest_bad || (!trusted_recent_track_record && score.total >= 3 && score.success_rate < 0.5)
    {
        policy.side_effect_level = Some("read_only".to_string());
    } else if trusted_recent_track_record {
        policy.side_effect_level = Some("network".to_string());
    }
    policy
}

pub fn policy_for_autonomy_tier(tier: AutonomyTier) -> CapabilityPolicy {
    use crate::tool_annotations::SideEffectLevel;
    let level = match tier {
        AutonomyTier::Shadow => SideEffectLevel::None,
        AutonomyTier::Suggest => SideEffectLevel::ReadOnly,
        AutonomyTier::ActWithApproval => SideEffectLevel::ReadOnly,
        // Full autonomy carries the outermost ceiling — the TOP of the ladder,
        // not a hardcoded level. This must track the ladder so a newly-added
        // most-invasive level (e.g. `desktop_control`, added above `network`) is
        // not silently capped out of the fully-autonomous tier.
        AutonomyTier::ActAuto => SideEffectLevel::MAX,
    };
    // An autonomy tier bounds how much a handler may *do*, not where it may
    // read and write, so this is an overlay on whatever confinement the run
    // already has. Built on `default()` it would carry `SandboxProfile::
    // Worktree` and confine handlers dispatched from an unsandboxed run.
    CapabilityPolicy {
        side_effect_level: Some(level.as_str().to_string()),
        recursion_limit: matches!(tier, AutonomyTier::Shadow).then_some(0),
        ..CapabilityPolicy::neutral()
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

fn apply_graph_record_limit(records: &mut Vec<TrustGraphRecord>, limit: Option<usize>) {
    let Some(limit) = limit else {
        return;
    };
    if records.len() <= limit {
        return;
    }
    let keep_from = records.len() - limit;
    records.drain(0..keep_from);
}

fn is_control_plane_action(action: &str) -> bool {
    matches!(
        action,
        "trust.promote" | "trust.demote" | "autonomy.tier_transition"
    )
}

#[cfg(test)]
mod tests;
