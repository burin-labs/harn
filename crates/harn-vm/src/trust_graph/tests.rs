//! Trust-graph unit tests.
//!
//! Split out of `trust_graph.rs` to keep that module within the
//! source-file-length ratchet; the module path is unchanged.

use super::*;
use crate::event_log::MemoryEventLog;
use time::Duration;

#[test]
fn an_autonomy_tier_bounds_side_effects_without_confining_the_filesystem() {
    // An autonomy tier says how much a handler may do, not where it may
    // read and write. The dispatcher pushes this policy verbatim when the
    // run has no policy of its own (`harn run --no-sandbox`, `harn test
    // conformance`), so a `Worktree` profile here would confine a handler
    // more tightly than the pipeline that dispatched it.
    for tier in [
        AutonomyTier::Shadow,
        AutonomyTier::Suggest,
        AutonomyTier::ActWithApproval,
        AutonomyTier::ActAuto,
    ] {
        let policy = policy_for_autonomy_tier(tier);
        assert_eq!(
            policy.sandbox_profile,
            crate::orchestration::SandboxProfile::Unrestricted,
            "{tier:?} must not assert filesystem confinement"
        );
        assert!(
            policy.workspace_roots.is_empty() && policy.read_only_roots.is_empty(),
            "{tier:?} must not assert filesystem roots"
        );
        assert!(
            policy.side_effect_level.is_some(),
            "{tier:?} must still bound the side-effect level"
        );
    }
}

const RECORD_SCHEMA_JSON: &str = include_str!("schemas/trust-record.v0.schema.json");
const RECORD_SCHEMA_V0_1_JSON: &str = include_str!("schemas/trust-record.v0.1.schema.json");
const CHAIN_SCHEMA_JSON: &str = include_str!("schemas/trust-chain.v0.schema.json");
const VALID_DECISION_CHAIN_JSON: &str = include_str!("fixtures/valid/decision-chain.json");
const VALID_TIER_TRANSITION_JSON: &str = include_str!("fixtures/valid/tier-transition.json");
const VALID_EFFECT_INHERITANCE_CHAIN_JSON: &str =
    include_str!("fixtures/valid/effect-inheritance-chain.json");
const INVALID_TAMPERED_CHAIN_JSON: &str = include_str!("fixtures/invalid/tampered-chain.json");
const INVALID_MISSING_APPROVAL_JSON: &str = include_str!("fixtures/invalid/missing-approval.json");
const INVALID_ACTOR_CHAIN_PARENTAGE_JSON: &str =
    include_str!("fixtures/invalid/actor-chain-parentage.json");

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
        let source = std::fs::read_to_string(spec_dir.join(relative))
            .unwrap_or_else(|e| panic!("failed to read opentrustgraph fixture {relative}: {e}"));
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

    second.previous_hash =
        Some("sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string());
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
        EffectRecord::new(EffectKind::Net, EffectScope::Write).with_resource("https://api.example"),
        EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace/src"),
    ];
    let used =
        vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace/src")];
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
fn v0_1_actor_chain_metadata_round_trips_through_json() {
    let chain = crate::ActorChain::new_with_scopes("user:kenneth", ["repo:read", "repo:write"])
        .pushed_with_scopes("agent:burin", ["repo:read"])
        .pushed_with_scopes("agent:merge-captain", ["repo:read"]);
    let alert = serde_json::json!({
        "kind": "scope_attenuation_violation",
        "mode": "non-increasing",
        "parent_subject": "agent:burin",
        "child_subject": "agent:merge-captain",
        "parent_scopes": ["repo:read"],
        "child_scopes": ["repo:read", "repo:write"],
        "extra_scopes": ["repo:write"]
    });
    let record = TrustRecord::new(
        "agent:merge-captain",
        "identity.scope_attenuation",
        None,
        TrustOutcome::Denied,
        "trace-actor-chain-1",
        AutonomyTier::ActAuto,
    )
    .with_actor_chain(chain.clone())
    .with_actor_chain_alert(alert.clone());

    let encoded = serde_json::to_string(&record).unwrap();
    let decoded: TrustRecord = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded.actor_chain(), Some(chain));
    assert_eq!(decoded.actor_chain_alert(), Some(&alert));
}

#[tokio::test]
async fn scope_attenuation_alert_appends_denied_actor_chain_record() {
    let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
    let chain = crate::ActorChain::new_with_scopes("user:owner", ["repo:read"])
        .pushed_with_scopes("agent:writer", ["repo:read", "repo:write"]);
    let violation = chain
        .validate_scope_attenuation(&crate::ScopeAttenuationPolicy::default())
        .unwrap_err();

    let record = append_scope_attenuation_alert(&log, &chain, &violation, "trace-scope-alert")
        .await
        .unwrap();

    assert_eq!(record.agent, "agent:writer");
    assert_eq!(record.action, "identity.scope_attenuation");
    assert_eq!(record.outcome, TrustOutcome::Denied);
    assert_eq!(record.trace_id, "trace-scope-alert");
    assert_eq!(record.actor_chain(), Some(chain));
    assert_eq!(
        record
            .actor_chain_alert()
            .and_then(|value| value.get("kind"))
            .and_then(serde_json::Value::as_str),
        Some("scope_attenuation_violation")
    );

    let records = query_trust_records(
        &log,
        &TrustQueryFilters {
            agent: Some("agent:writer".to_string()),
            action: Some("identity.scope_attenuation".to_string()),
            outcome: Some(TrustOutcome::Denied),
            ..TrustQueryFilters::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(records.len(), 1);
    let report = verify_trust_chain(&log).await.unwrap();
    assert!(report.verified, "verification errors: {:?}", report.errors);
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
    .with_actor_chain(ActorChain::new("user:kenneth").pushed("agent:agent"))
    .with_actor_chain_alert(serde_json::json!({"kind": "test_alert"}));
    assert!(record.metadata.contains_key(METADATA_KEY_EFFECTS_GRANT));
    assert!(record.metadata.contains_key(METADATA_KEY_PARENT_RECORD_ID));
    assert!(record.metadata.contains_key(METADATA_KEY_ACTOR_CHAIN));
    assert!(record.metadata.contains_key(METADATA_KEY_ACTOR_CHAIN_ALERT));

    record.set_effects_grant(Vec::new());
    record.set_parent_record_id(None);
    record.set_actor_chain(None);
    record.set_actor_chain_alert(None);
    assert!(!record.metadata.contains_key(METADATA_KEY_EFFECTS_GRANT));
    assert!(!record.metadata.contains_key(METADATA_KEY_PARENT_RECORD_ID));
    assert!(!record.metadata.contains_key(METADATA_KEY_ACTOR_CHAIN));
    assert!(!record.metadata.contains_key(METADATA_KEY_ACTOR_CHAIN_ALERT));
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
        EffectRecord::new(EffectKind::Net, EffectScope::Write).with_resource("https://api.example"),
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
        EffectRecord::new(EffectKind::Net, EffectScope::Write).with_resource("https://api.example"),
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
        vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace/src")];
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
async fn trust_chain_verifies_actor_chain_parentage() {
    let log: Arc<AnyEventLog> = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(16)));
    let parent_chain = ActorChain::new("user:kenneth").pushed("agent:burin");
    let child_chain = parent_chain.clone().pushed("agent:merge-captain");

    let parent = append_trust_record(
        &log,
        &TrustRecord::new(
            "agent:burin",
            "agent.spawn",
            None,
            TrustOutcome::Success,
            "trace-parent",
            AutonomyTier::ActAuto,
        )
        .with_actor_chain(parent_chain),
    )
    .await
    .unwrap();
    append_trust_record(
        &log,
        &TrustRecord::new(
            "agent:merge-captain",
            "agent.spawn",
            None,
            TrustOutcome::Success,
            "trace-child",
            AutonomyTier::ActAuto,
        )
        .with_actor_chain(child_chain)
        .with_parent_record_id(parent.record_id),
    )
    .await
    .unwrap();

    let report = verify_trust_chain(&log).await.unwrap();
    assert!(report.verified, "verification errors: {:?}", report.errors);
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
