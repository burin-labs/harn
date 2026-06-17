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
/// effects and actors stay inside the parent chain.
///
/// Backwards compatible: v0 records are still accepted (the new keys are
/// optional). One patch release window after this bump, v0 will be
/// dropped per `opentrustgraph-spec/CONFORMANCE.md` §5.
pub const OPENTRUSTGRAPH_SCHEMA_V0_1: &str = "opentrustgraph/v0.1";
/// Set of schema discriminators accepted by the v0.1 validator. v0 stays
/// here for one patch release window before being retired.
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
pub const TRUST_GRAPH_RECORDS_TOPIC: &str = "trust_graph.records";
pub const TRUST_GRAPH_GLOBAL_TOPIC: &str = "trust_graph";
pub const TRUST_GRAPH_LEGACY_GLOBAL_TOPIC: &str = "trust.graph";
pub const TRUST_GRAPH_TOPIC_PREFIX: &str = "trust_graph.";
pub const TRUST_GRAPH_LEGACY_TOPIC_PREFIX: &str = "trust.graph.";
pub const TRUST_GRAPH_EVENT_KIND: &str = "trust_recorded";
pub const TRUST_ACTION_RELEASE: &str = "release";

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
    CapabilityPolicy {
        side_effect_level: Some(
            match tier {
                AutonomyTier::Shadow => "none",
                AutonomyTier::Suggest => "read_only",
                AutonomyTier::ActWithApproval => "read_only",
                AutonomyTier::ActAuto => "network",
            }
            .to_string(),
        ),
        recursion_limit: matches!(tier, AutonomyTier::Shadow).then_some(0),
        ..CapabilityPolicy::default()
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
mod tests {
    use super::*;
    use crate::event_log::MemoryEventLog;
    use time::Duration;

    const RECORD_SCHEMA_JSON: &str =
        include_str!("trust_graph/schemas/trust-record.v0.schema.json");
    const RECORD_SCHEMA_V0_1_JSON: &str =
        include_str!("trust_graph/schemas/trust-record.v0.1.schema.json");
    const CHAIN_SCHEMA_JSON: &str = include_str!("trust_graph/schemas/trust-chain.v0.schema.json");
    const VALID_DECISION_CHAIN_JSON: &str =
        include_str!("trust_graph/fixtures/valid/decision-chain.json");
    const VALID_TIER_TRANSITION_JSON: &str =
        include_str!("trust_graph/fixtures/valid/tier-transition.json");
    const VALID_EFFECT_INHERITANCE_CHAIN_JSON: &str =
        include_str!("trust_graph/fixtures/valid/effect-inheritance-chain.json");
    const INVALID_TAMPERED_CHAIN_JSON: &str =
        include_str!("trust_graph/fixtures/invalid/tampered-chain.json");
    const INVALID_MISSING_APPROVAL_JSON: &str =
        include_str!("trust_graph/fixtures/invalid/missing-approval.json");
    const INVALID_ACTOR_CHAIN_PARENTAGE_JSON: &str =
        include_str!("trust_graph/fixtures/invalid/actor-chain-parentage.json");

    #[derive(Debug, serde::Deserialize)]
    struct TrustChainFixture {
        schema: String,
        chain: TrustChainFixtureMetadata,
        records: Vec<TrustRecord>,
    }

    #[derive(Debug, serde::Deserialize)]
    struct TrustChainFixtureMetadata {
        topic: String,
        total: u64,
        root_hash: Option<String>,
        verified: bool,
        generated_at: String,
        producer: BTreeMap<String, serde_json::Value>,
    }

    #[test]
    fn embedded_trust_graph_fixtures_match_workspace_spec_when_available() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let spec_dir = manifest_dir.join("../../opentrustgraph-spec");
        if !spec_dir.exists() {
            return;
        }

        for (relative, embedded) in [
            ("schemas/trust-record.v0.schema.json", RECORD_SCHEMA_JSON),
            (
                "schemas/trust-record.v0.1.schema.json",
                RECORD_SCHEMA_V0_1_JSON,
            ),
            ("schemas/trust-chain.v0.schema.json", CHAIN_SCHEMA_JSON),
            (
                "fixtures/valid/decision-chain.json",
                VALID_DECISION_CHAIN_JSON,
            ),
            (
                "fixtures/valid/tier-transition.json",
                VALID_TIER_TRANSITION_JSON,
            ),
            (
                "fixtures/valid/effect-inheritance-chain.json",
                VALID_EFFECT_INHERITANCE_CHAIN_JSON,
            ),
            (
                "fixtures/invalid/tampered-chain.json",
                INVALID_TAMPERED_CHAIN_JSON,
            ),
            (
                "fixtures/invalid/missing-approval.json",
                INVALID_MISSING_APPROVAL_JSON,
            ),
            (
                "fixtures/invalid/actor-chain-parentage.json",
                INVALID_ACTOR_CHAIN_PARENTAGE_JSON,
            ),
        ] {
            let source = std::fs::read_to_string(spec_dir.join(relative)).unwrap_or_else(|e| {
                panic!("failed to read opentrustgraph fixture {relative}: {e}")
            });
            assert_eq!(
                embedded, source,
                "embedded trust graph fixture {relative} drifted from opentrustgraph-spec"
            );
        }
    }

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
    async fn export_trust_chain_emits_envelope_matching_chain_schema() {
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        let first = append_trust_record(
            &log,
            &TrustRecord::new(
                "bot",
                "github.issue.opened",
                None,
                TrustOutcome::Success,
                "trace-1",
                AutonomyTier::Suggest,
            ),
        )
        .await
        .unwrap();
        let second = append_trust_record(
            &log,
            &TrustRecord::new(
                "bot",
                "trust.promote",
                Some("maintainer-1".to_string()),
                TrustOutcome::Success,
                "trace-2",
                AutonomyTier::ActAuto,
            ),
        )
        .await
        .unwrap();

        let export = export_trust_chain(&log).await.unwrap();
        assert_eq!(export.schema, OPENTRUSTGRAPH_CHAIN_SCHEMA_V0);
        assert_eq!(export.chain.topic, TRUST_GRAPH_GLOBAL_TOPIC);
        assert_eq!(export.chain.total, 2);
        assert!(export.chain.verified);
        assert_eq!(
            export.chain.root_hash.as_deref(),
            Some(second.entry_hash.as_str())
        );
        assert_eq!(export.records.len(), 2);
        assert_eq!(export.records[0].entry_hash, first.entry_hash);
        assert_eq!(export.records[1].entry_hash, second.entry_hash);
        assert_eq!(export.chain.producer.name, "harn");

        let envelope_json = serde_json::to_value(&export).unwrap();
        assert_eq!(envelope_json["schema"], OPENTRUSTGRAPH_CHAIN_SCHEMA_V0);
        assert_eq!(envelope_json["chain"]["total"], 2);
        assert_eq!(envelope_json["chain"]["verified"], true);
        assert!(envelope_json["records"].as_array().unwrap().len() == 2);
    }

    #[tokio::test]
    async fn export_trust_chain_handles_empty_log() {
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        let export = export_trust_chain(&log).await.unwrap();
        assert_eq!(export.schema, OPENTRUSTGRAPH_CHAIN_SCHEMA_V0);
        assert_eq!(export.chain.total, 0);
        assert!(export.chain.verified);
        assert!(export.chain.root_hash.is_none());
        assert!(export.records.is_empty());
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
        let base = OffsetDateTime::from_unix_timestamp(1_775_000_000).unwrap();
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

    #[test]
    fn opentrustgraph_schema_files_are_parseable_and_match_runtime_enums() {
        let record_schema: serde_json::Value = serde_json::from_str(RECORD_SCHEMA_JSON).unwrap();
        let record_schema_v0_1: serde_json::Value =
            serde_json::from_str(RECORD_SCHEMA_V0_1_JSON).unwrap();
        let chain_schema: serde_json::Value = serde_json::from_str(CHAIN_SCHEMA_JSON).unwrap();

        assert_eq!(
            record_schema["properties"]["schema"]["const"],
            serde_json::json!(OPENTRUSTGRAPH_SCHEMA_V0)
        );
        let v0_1_schema_enum = record_schema_v0_1["properties"]["schema"]["enum"]
            .as_array()
            .expect("v0.1 record schema declares schema as an enum");
        assert!(
            v0_1_schema_enum.contains(&serde_json::json!(OPENTRUSTGRAPH_SCHEMA_V0_1)),
            "v0.1 record schema must accept {OPENTRUSTGRAPH_SCHEMA_V0_1}: {v0_1_schema_enum:?}"
        );
        assert!(
            v0_1_schema_enum.contains(&serde_json::json!(OPENTRUSTGRAPH_SCHEMA_V0)),
            "v0.1 record schema must still accept v0 (one-release back-compat): {v0_1_schema_enum:?}"
        );
        assert_eq!(
            chain_schema["properties"]["schema"]["const"],
            serde_json::json!("opentrustgraph-chain/v0")
        );

        let outcomes = record_schema["properties"]["outcome"]["enum"]
            .as_array()
            .unwrap();
        for outcome in [
            TrustOutcome::Success,
            TrustOutcome::Failure,
            TrustOutcome::Denied,
            TrustOutcome::Timeout,
        ] {
            assert!(outcomes.contains(&serde_json::json!(outcome.as_str())));
        }

        let tiers = record_schema["properties"]["autonomy_tier"]["enum"]
            .as_array()
            .unwrap();
        for tier in [
            AutonomyTier::Shadow,
            AutonomyTier::Suggest,
            AutonomyTier::ActWithApproval,
            AutonomyTier::ActAuto,
        ] {
            assert!(tiers.contains(&serde_json::json!(tier.as_str())));
        }
    }

    #[test]
    fn opentrustgraph_valid_fixtures_match_runtime_contract() {
        for (name, fixture) in [
            ("decision-chain", VALID_DECISION_CHAIN_JSON),
            ("tier-transition", VALID_TIER_TRANSITION_JSON),
            (
                "effect-inheritance-chain",
                VALID_EFFECT_INHERITANCE_CHAIN_JSON,
            ),
        ] {
            let fixture = parse_chain_fixture(fixture);
            let errors = validate_chain_fixture(&fixture);
            assert!(errors.is_empty(), "{name} errors: {errors:?}");
        }
    }

    #[test]
    fn opentrustgraph_invalid_fixtures_exercise_expected_failures() {
        let tampered = parse_chain_fixture(INVALID_TAMPERED_CHAIN_JSON);
        let tampered_errors = validate_chain_fixture(&tampered);
        assert!(
            tampered_errors
                .iter()
                .any(|error| error.contains("previous_hash mismatch")),
            "tampered-chain errors: {tampered_errors:?}"
        );
        assert!(
            !tampered_errors
                .iter()
                .any(|error| error.contains("entry_hash mismatch")),
            "tampered-chain should isolate hash-link tampering: {tampered_errors:?}"
        );

        let missing_approval = parse_chain_fixture(INVALID_MISSING_APPROVAL_JSON);
        let missing_errors = validate_chain_fixture(&missing_approval);
        assert!(
            missing_errors
                .iter()
                .any(|error| error.contains("approval required")),
            "missing-approval errors: {missing_errors:?}"
        );

        let actor_parentage = parse_chain_fixture(INVALID_ACTOR_CHAIN_PARENTAGE_JSON);
        let actor_errors = validate_chain_fixture(&actor_parentage);
        assert!(
            actor_errors
                .iter()
                .any(|error| error.contains("actor_chain escaped parentage")),
            "actor-chain-parentage errors: {actor_errors:?}"
        );
    }

    fn parse_chain_fixture(input: &str) -> TrustChainFixture {
        serde_json::from_str(input).unwrap()
    }

    fn validate_chain_fixture(fixture: &TrustChainFixture) -> Vec<String> {
        let mut errors = Vec::new();
        if fixture.schema != OPENTRUSTGRAPH_CHAIN_SCHEMA_V0 {
            errors.push(format!("unsupported chain schema {}", fixture.schema));
        }
        if fixture.chain.topic.trim().is_empty() {
            errors.push("chain topic is empty".to_string());
        }
        if fixture.chain.total != fixture.records.len() as u64 {
            errors.push(format!(
                "chain total mismatch; expected {}, found {}",
                fixture.records.len(),
                fixture.chain.total
            ));
        }
        if fixture
            .chain
            .producer
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            errors.push("chain producer.name is empty".to_string());
        }
        if OffsetDateTime::parse(
            &fixture.chain.generated_at,
            &time::format_description::well_known::Rfc3339,
        )
        .is_err()
        {
            errors.push("chain generated_at is not RFC3339".to_string());
        }

        for (index, record) in fixture.records.iter().enumerate() {
            errors.extend(validate_fixture_record_contract(index, record));
        }
        errors.extend(validate_fixture_hash_chain(fixture));
        errors.extend(
            validate_lineage_invariants(
                fixture
                    .records
                    .iter()
                    .enumerate()
                    .map(|(index, record)| (format!("record {index}"), None, record)),
            )
            .into_iter()
            .map(|error| error.message),
        );

        let expected_verified = errors.is_empty();
        if fixture.chain.verified != expected_verified {
            errors.push(format!(
                "chain verified flag mismatch; expected {expected_verified}, found {}",
                fixture.chain.verified
            ));
        }
        errors
    }

    fn validate_fixture_record_contract(index: usize, record: &TrustRecord) -> Vec<String> {
        let mut errors = Vec::new();
        let label = format!("record {index}");
        if !OPENTRUSTGRAPH_ACCEPTED_SCHEMAS.contains(&record.schema.as_str()) {
            errors.push(format!("{label}: unsupported schema {}", record.schema));
        }
        if record.record_id.trim().is_empty() {
            errors.push(format!("{label}: record_id is empty"));
        }
        if record.agent.trim().is_empty() {
            errors.push(format!("{label}: agent is empty"));
        }
        if record.action.trim().is_empty() {
            errors.push(format!("{label}: action is empty"));
        }
        if record.trace_id.trim().is_empty() {
            errors.push(format!("{label}: trace_id is empty"));
        }
        if !record.entry_hash.starts_with("sha256:") {
            errors.push(format!("{label}: entry_hash is not sha256-prefixed"));
        }
        if let Some(cost_usd) = record.cost_usd {
            if cost_usd < 0.0 {
                errors.push(format!("{label}: cost_usd is negative"));
            }
        }

        if record.outcome == TrustOutcome::Success
            && record.autonomy_tier == AutonomyTier::ActWithApproval
            && approval_required(record)
        {
            if record
                .approver
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                errors.push(format!("{label}: approval required but approver is empty"));
            }
            if approval_signature_count(record) == 0 {
                errors.push(format!(
                    "{label}: approval required but signatures are empty"
                ));
            }
        }

        errors
    }

    fn validate_fixture_hash_chain(fixture: &TrustChainFixture) -> Vec<String> {
        let mut errors = Vec::new();
        let mut previous_hash: Option<String> = None;

        for (position, record) in fixture.records.iter().enumerate() {
            let expected_index = position as u64 + 1;
            if record.chain_index != expected_index {
                errors.push(format!(
                    "record {position}: expected chain_index {expected_index}, found {}",
                    record.chain_index
                ));
            }
            if record.previous_hash != previous_hash {
                errors.push(format!(
                    "record {position}: previous_hash mismatch; expected {:?}, found {:?}",
                    previous_hash, record.previous_hash
                ));
            }
            let expected_hash = compute_trust_record_hash(record).unwrap();
            if expected_hash != record.entry_hash {
                errors.push(format!(
                    "record {position}: entry_hash mismatch; expected {expected_hash}, found {}",
                    record.entry_hash
                ));
            }
            previous_hash = Some(record.entry_hash.clone());
        }

        if fixture.chain.root_hash != previous_hash {
            errors.push(format!(
                "chain root_hash mismatch; expected {:?}, found {:?}",
                previous_hash, fixture.chain.root_hash
            ));
        }
        errors
    }

    fn approval_required(record: &TrustRecord) -> bool {
        record
            .metadata
            .get("approval")
            .and_then(|approval| approval.get("required"))
            .and_then(|required| required.as_bool())
            .unwrap_or(false)
    }

    fn approval_signature_count(record: &TrustRecord) -> usize {
        record
            .metadata
            .get("approval")
            .and_then(|approval| approval.get("signatures"))
            .and_then(|signatures| signatures.as_array())
            .map(Vec::len)
            .unwrap_or(0)
    }

    // ----- OpenTrustGraph v0.1 schema and lineage metadata -----

    use crate::orchestration::{EffectKind, EffectScope};

    #[test]
    fn new_trust_record_defaults_to_v0_1_schema() {
        let record = TrustRecord::new(
            "agent",
            "deploy.preview",
            None,
            TrustOutcome::Success,
            "trace-1",
            AutonomyTier::Suggest,
        );
        assert_eq!(record.schema, OPENTRUSTGRAPH_SCHEMA_V0_1);
    }

    #[test]
    fn v0_records_still_parse_for_backward_compat() {
        let record_v0 = serde_json::json!({
            "schema": "opentrustgraph/v0",
            "record_id": "01966f4c-0f31-7b5d-b44b-f7f8e7e1d384",
            "agent": "legacy-bot",
            "action": "github.issue.opened",
            "approver": null,
            "outcome": "success",
            "trace_id": "trace-legacy",
            "autonomy_tier": "suggest",
            "timestamp": "2026-04-19T18:42:11Z",
            "cost_usd": null,
            "chain_index": 1,
            "previous_hash": null,
            "entry_hash": "sha256:84facae7d56fd304e040ea18d80bd019e274ad86ddd5a4d732f3ac3d984c48ec",
            "metadata": {"provider": "github"}
        });
        let decoded: TrustRecord = serde_json::from_value(record_v0).unwrap();
        assert_eq!(decoded.schema, OPENTRUSTGRAPH_SCHEMA_V0);
        assert!(OPENTRUSTGRAPH_ACCEPTED_SCHEMAS.contains(&decoded.schema.as_str()));
        assert!(decoded.effects_grant().is_empty());
        assert!(decoded.effects_used().is_empty());
        assert!(decoded.parent_record_id().is_none());
        assert!(decoded.actor_chain().is_none());
    }

    #[test]
    fn v0_1_lineage_metadata_round_trips_through_json() {
        let grant = vec![
            EffectRecord::new(EffectKind::Net, EffectScope::Write)
                .with_resource("https://api.example"),
            EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace/src"),
        ];
        let used =
            vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read)
                .with_resource("/workspace/src")];
        let actor_chain = ActorChain::new("user:kenneth")
            .pushed("agent:parent")
            .pushed("agent:child");
        let record = TrustRecord::new(
            "child-agent",
            "fs.read",
            None,
            TrustOutcome::Success,
            "trace-effects-1",
            AutonomyTier::ActAuto,
        )
        .with_effects_grant(grant.clone())
        .with_effects_used(used.clone())
        .with_parent_record_id("parent-record-001")
        .with_actor_chain(actor_chain.clone());

        let encoded = serde_json::to_string(&record).unwrap();
        let decoded: TrustRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.schema, OPENTRUSTGRAPH_SCHEMA_V0_1);
        assert_eq!(decoded.effects_grant(), grant);
        assert_eq!(decoded.effects_used(), used);
        assert_eq!(
            decoded.parent_record_id().as_deref(),
            Some("parent-record-001")
        );
        assert_eq!(decoded.actor_chain(), Some(actor_chain));
    }

    #[test]
    fn lineage_helpers_remove_keys_on_empty_input() {
        let mut record = TrustRecord::new(
            "agent",
            "noop",
            None,
            TrustOutcome::Success,
            "trace-1",
            AutonomyTier::Suggest,
        )
        .with_effects_grant(vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)])
        .with_parent_record_id("parent-1")
        .with_actor_chain(ActorChain::new("user:kenneth").pushed("agent:agent"));
        assert!(record.metadata.contains_key(METADATA_KEY_EFFECTS_GRANT));
        assert!(record.metadata.contains_key(METADATA_KEY_PARENT_RECORD_ID));
        assert!(record.metadata.contains_key(METADATA_KEY_ACTOR_CHAIN));

        record.set_effects_grant(Vec::new());
        record.set_parent_record_id(None);
        record.set_actor_chain(None);
        assert!(!record.metadata.contains_key(METADATA_KEY_EFFECTS_GRANT));
        assert!(!record.metadata.contains_key(METADATA_KEY_PARENT_RECORD_ID));
        assert!(!record.metadata.contains_key(METADATA_KEY_ACTOR_CHAIN));
    }

    #[tokio::test]
    async fn append_attaches_current_session_actor_chain() {
        crate::reset_thread_local_state();
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        let actor_chain = ActorChain::new("user:kenneth").pushed("agent:reviewer");
        let session_id = crate::agent_sessions::open_or_create_with_actor_chain(
            Some("trust-actor-session".to_string()),
            Some(actor_chain.clone()),
        );
        let _session = crate::agent_sessions::enter_current_session(session_id);

        let appended = append_trust_record(
            &log,
            &TrustRecord::new(
                "reviewer",
                "fs.read",
                None,
                TrustOutcome::Success,
                "trace-actor-session",
                AutonomyTier::ActAuto,
            ),
        )
        .await
        .unwrap();

        assert_eq!(appended.actor_chain(), Some(actor_chain));
    }

    #[tokio::test]
    async fn three_agent_chain_proves_effects_subset_inheritance() {
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));

        let parent_grant = vec![
            EffectRecord::new(EffectKind::Net, EffectScope::Write)
                .with_resource("https://api.example"),
            EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace/src"),
            EffectRecord::new(EffectKind::Fs, EffectScope::Write).with_resource("/workspace/tmp"),
        ];
        let parent = append_trust_record(
            &log,
            &TrustRecord::new(
                "parent",
                "agent.spawn",
                None,
                TrustOutcome::Success,
                "trace-parent",
                AutonomyTier::ActAuto,
            )
            .with_effects_grant(parent_grant.clone())
            .with_actor_chain(ActorChain::new("user:kenneth").pushed("agent:parent")),
        )
        .await
        .unwrap();

        let child_grant = vec![
            EffectRecord::new(EffectKind::Net, EffectScope::Write)
                .with_resource("https://api.example"),
            EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace/src"),
        ];
        let child = append_trust_record(
            &log,
            &TrustRecord::new(
                "child",
                "agent.spawn",
                None,
                TrustOutcome::Success,
                "trace-child",
                AutonomyTier::ActAuto,
            )
            .with_effects_grant(child_grant.clone())
            .with_parent_record_id(parent.record_id.clone())
            .with_actor_chain(
                ActorChain::new("user:kenneth")
                    .pushed("agent:parent")
                    .pushed("agent:child"),
            ),
        )
        .await
        .unwrap();

        let grandchild_used =
            vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read)
                .with_resource("/workspace/src")];
        let grandchild = append_trust_record(
            &log,
            &TrustRecord::new(
                "grandchild",
                "fs.read",
                None,
                TrustOutcome::Success,
                "trace-grandchild",
                AutonomyTier::ActAuto,
            )
            .with_effects_used(grandchild_used.clone())
            .with_parent_record_id(child.record_id.clone())
            .with_actor_chain(
                ActorChain::new("user:kenneth")
                    .pushed("agent:parent")
                    .pushed("agent:child")
                    .pushed("agent:grandchild"),
            ),
        )
        .await
        .unwrap();

        // grandchild.effects_used ⊆ child.effects_grant
        for effect in &grandchild_used {
            assert!(
                child_grant.contains(effect),
                "grandchild used {effect:?} not in child grant"
            );
        }
        // child.effects_grant ⊆ parent.effects_grant
        for effect in &child_grant {
            assert!(
                parent_grant.contains(effect),
                "child grant {effect:?} not in parent grant"
            );
        }

        assert_eq!(
            grandchild.parent_record_id().as_deref(),
            Some(child.record_id.as_str())
        );
        assert_eq!(
            child.parent_record_id().as_deref(),
            Some(parent.record_id.as_str())
        );
        assert!(parent.parent_record_id().is_none());

        // The chain still verifies cleanly (additive metadata change).
        let report = verify_trust_chain(&log).await.unwrap();
        assert!(report.verified, "verification errors: {:?}", report.errors);
        assert_eq!(report.total, 3);
    }

    #[tokio::test]
    async fn verify_chain_rejects_actor_chain_that_escapes_parentage() {
        let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
        let parent = append_trust_record(
            &log,
            &TrustRecord::new(
                "parent",
                "agent.spawn",
                None,
                TrustOutcome::Success,
                "trace-parent",
                AutonomyTier::ActAuto,
            )
            .with_actor_chain(ActorChain::new("user:kenneth").pushed("agent:parent")),
        )
        .await
        .unwrap();

        append_trust_record(
            &log,
            &TrustRecord::new(
                "child",
                "agent.spawn",
                None,
                TrustOutcome::Success,
                "trace-child",
                AutonomyTier::ActAuto,
            )
            .with_parent_record_id(parent.record_id)
            .with_actor_chain(
                ActorChain::new("user:kenneth")
                    .pushed("agent:other-parent")
                    .pushed("agent:child"),
            ),
        )
        .await
        .unwrap();

        let report = verify_trust_chain(&log).await.unwrap();
        assert!(!report.verified);
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("actor_chain escaped parentage")));
    }
}
