//! Trust-graph builtins: recording decisions and querying the resulting graph.
//!
//! Covers both spellings of the same surface — the flat `trust_*` builtins and
//! the `trust.*` namespace — along with the parsers that turn Harn dicts into
//! [`TrustRecord`]s, autonomy tiers, outcomes, and query filters.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use crate::stdlib::macros::harn_builtin;
use crate::triggers::dispatcher::current_dispatch_context;
use crate::trust_graph::{
    group_trust_records_by_trace, policy_for_agent, query_trust_graph_records, query_trust_records,
    trust_score_for, verify_trust_chain, AutonomyTier, TrustOutcome, TrustQueryFilters,
    TrustRecord,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::args::{
    optional_string, optional_string_arg, required_string, required_string_arg, value_from_serde,
};
use super::journal::ensure_trigger_event_log;

#[harn_builtin(
    sig = "trust_record(...args: any) -> TrustRecord",
    kind = "async",
    category = "triggers"
)]
async fn trust_record_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    append_trust_record_from_parts("trust_record", &args)
        .await
        .map(|record| value_from_serde(&record))
}

#[harn_builtin(
    sig = "trust_graph_record(...args: any) -> string",
    kind = "async",
    category = "triggers"
)]
async fn trust_graph_record_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let decision = args.first().ok_or_else(|| {
        VmError::Runtime("trust_graph_record: expected decision dict".to_string())
    })?;
    let record = append_trust_record_from_decision_for("trust_graph_record", decision).await?;
    Ok(VmValue::String(arcstr::ArcStr::from(record.record_id)))
}

#[harn_builtin(
    sig = "trust_query(...args: any) -> list",
    kind = "async",
    category = "triggers"
)]
async fn trust_query_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let filters = args
        .first()
        .map(parse_trust_query_filters)
        .transpose()?
        .unwrap_or_default();
    let log = ensure_trigger_event_log();
    let records = query_trust_records(&log, &filters)
        .await
        .map_err(|error| VmError::Runtime(format!("trust_query: {error}")))?;
    if filters.grouped_by_trace {
        return Ok(value_from_serde(&group_trust_records_by_trace(&records)));
    }
    Ok(VmValue::List(std::sync::Arc::new(
        records
            .into_iter()
            .map(|record| value_from_serde(&record))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "trust_graph_query(...args: any) -> TrustScore",
    kind = "async",
    category = "triggers"
)]
async fn trust_graph_query_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let agent = required_string_arg(&args, 0, "trust_graph_query", "agent")?;
    let action = optional_string_arg(&args, 1, "trust_graph_query", "action")?;
    let log = ensure_trigger_event_log();
    let score = trust_score_for(&log, &agent, action.as_deref())
        .await
        .map_err(|error| VmError::Runtime(format!("trust_graph_query: {error}")))?;
    Ok(value_from_serde(&score))
}

#[harn_builtin(
    sig = "trust_graph_policy_for(...args: any) -> CapabilityPolicy",
    kind = "async",
    category = "triggers"
)]
async fn trust_graph_policy_for_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let agent = required_string_arg(&args, 0, "trust_graph_policy_for", "agent")?;
    let log = ensure_trigger_event_log();
    let policy = policy_for_agent(&log, &agent)
        .await
        .map_err(|error| VmError::Runtime(format!("trust_graph_policy_for: {error}")))?;
    Ok(value_from_serde(&policy))
}

#[harn_builtin(
    sig = "trust_graph_verify_chain(...args: any) -> TrustChainReport",
    kind = "async",
    category = "triggers"
)]
async fn trust_graph_verify_chain_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    _args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let log = ensure_trigger_event_log();
    let report = verify_trust_chain(&log)
        .await
        .map_err(|error| VmError::Runtime(format!("trust_graph_verify_chain: {error}")))?;
    Ok(value_from_serde(&report))
}

#[harn_builtin(
    sig = "trust.query(...args: any) -> list",
    kind = "async",
    category = "triggers"
)]
async fn trust_query_ns_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let filters = args
        .first()
        .map(parse_trust_query_filters)
        .transpose()?
        .unwrap_or_default();
    let log = ensure_trigger_event_log();
    let records = query_trust_graph_records(&log, &filters)
        .await
        .map_err(|error| VmError::Runtime(format!("trust.query: {error}")))?;
    Ok(VmValue::List(std::sync::Arc::new(
        records
            .into_iter()
            .map(|record| value_from_serde(&record))
            .collect(),
    )))
}

#[harn_builtin(
    sig = "trust.record(...args: any) -> string",
    kind = "async",
    category = "triggers"
)]
async fn trust_record_ns_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let decision = args
        .first()
        .ok_or_else(|| VmError::Runtime("trust.record: expected decision dict".to_string()))?;
    let record = append_trust_record_from_decision_for("trust.record", decision).await?;
    Ok(VmValue::String(arcstr::ArcStr::from(record.record_id)))
}

#[harn_builtin(
    sig = "trust.score(...args: any) -> dict",
    kind = "async",
    category = "triggers"
)]
async fn trust_score_ns_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let agent = required_string_arg(&args, 0, "trust.score", "actor_id")?;
    let action = optional_string_arg(&args, 1, "trust.score", "action")?;
    let log = ensure_trigger_event_log();
    let score = trust_score_for(&log, &agent, action.as_deref())
        .await
        .map_err(|error| VmError::Runtime(format!("trust.score: {error}")))?;
    Ok(value_from_serde(&score))
}

#[harn_builtin(
    sig = "trust.policy_for(...args: any) -> dict",
    kind = "async",
    category = "triggers"
)]
async fn trust_policy_for_ns_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let agent = required_string_arg(&args, 0, "trust.policy_for", "actor_id")?;
    let log = ensure_trigger_event_log();
    let policy = policy_for_agent(&log, &agent)
        .await
        .map_err(|error| VmError::Runtime(format!("trust.policy_for: {error}")))?;
    Ok(value_from_serde(&policy))
}

#[harn_builtin(
    sig = "trust.verify_chain(...args: any) -> dict",
    kind = "async",
    category = "triggers"
)]
async fn trust_verify_chain_ns_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    _args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let log = ensure_trigger_event_log();
    let report = verify_trust_chain(&log)
        .await
        .map_err(|error| VmError::Runtime(format!("trust.verify_chain: {error}")))?;
    Ok(value_from_serde(&report))
}

pub(super) fn register_trust_namespace(vm: &mut Vm) {
    let names = ["query", "record", "score", "policy_for", "verify_chain"];
    vm.set_global(
        "trust",
        VmValue::dict(
            std::iter::once((
                "_namespace".to_string(),
                VmValue::String(arcstr::ArcStr::from("trust")),
            ))
            .chain(names.into_iter().map(|name| {
                (
                    name.to_string(),
                    VmValue::BuiltinRef(arcstr::ArcStr::from(format!("trust.{name}"))),
                )
            }))
            .collect::<BTreeMap<_, _>>(),
        ),
    );
}

pub(super) fn parse_autonomy_tier(value: &VmValue) -> Result<AutonomyTier, VmError> {
    let raw = match value {
        VmValue::String(text) => text.as_str(),
        other => {
            return Err(VmError::Runtime(format!(
                "trigger_register: `autonomy_tier` must be a string, got {}",
                other.type_name()
            )))
        }
    };
    match raw {
        "shadow" => Ok(AutonomyTier::Shadow),
        "suggest" => Ok(AutonomyTier::Suggest),
        "act_with_approval" => Ok(AutonomyTier::ActWithApproval),
        "act_auto" => Ok(AutonomyTier::ActAuto),
        other => Err(VmError::Runtime(format!(
            "trigger_register: unsupported autonomy_tier '{other}', expected shadow|suggest|act_with_approval|act_auto"
        ))),
    }
}

fn parse_trust_outcome(value: &VmValue) -> Result<TrustOutcome, VmError> {
    let raw = match value {
        VmValue::String(text) => text.as_str(),
        other => {
            return Err(VmError::Runtime(format!(
                "trust_record: outcome must be a string, got {}",
                other.type_name()
            )))
        }
    };
    match raw {
        "success" => Ok(TrustOutcome::Success),
        "failure" => Ok(TrustOutcome::Failure),
        "denied" => Ok(TrustOutcome::Denied),
        "timeout" => Ok(TrustOutcome::Timeout),
        other => Err(VmError::Runtime(format!(
            "trust_record: unsupported outcome '{other}', expected success|failure|denied|timeout"
        ))),
    }
}

async fn append_trust_record_from_parts(
    builtin: &str,
    args: &[VmValue],
) -> Result<TrustRecord, VmError> {
    let agent = required_string_arg(args, 0, builtin, "agent")?;
    let action = required_string_arg(args, 1, builtin, "action")?;
    let approver = args.get(2).and_then(|value| match value {
        VmValue::String(text) if !text.is_empty() => Some(text.to_string()),
        VmValue::Nil => None,
        _ => None,
    });
    let outcome = args
        .get(3)
        .map(parse_trust_outcome)
        .transpose()?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: expected outcome")))?;
    let tier = args
        .get(4)
        .map(parse_autonomy_tier)
        .transpose()?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: expected autonomy tier")))?;
    let trace_id = current_dispatch_context()
        .map(|context| context.trigger_event.trace_id.0)
        .unwrap_or_else(|| format!("trace-{}", uuid::Uuid::now_v7()));
    append_trust_record_value(
        builtin,
        TrustRecord::new(agent, action, approver, outcome, trace_id, tier),
    )
    .await
}

async fn append_trust_record_from_decision_for(
    builtin: &str,
    value: &VmValue,
) -> Result<TrustRecord, VmError> {
    let VmValue::Dict(map) = value else {
        return Err(VmError::Runtime(format!(
            "{builtin}: expected decision dict"
        )));
    };
    let agent = optional_string(map, "agent")
        .or_else(|| optional_string(map, "actor"))
        .or_else(|| optional_string(map, "actor_id"))
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{builtin}: missing string field `actor_id` (or `agent`)"
            ))
        })?;
    let action = required_string(map, "action", builtin)?;
    let approver = optional_string(map, "approver");
    let outcome = map
        .get("outcome")
        .map(parse_trust_outcome)
        .transpose()?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing outcome")))?;
    let tier = map
        .get("autonomy_tier")
        .or_else(|| map.get("autonomy_tier_at_time"))
        .or_else(|| map.get("tier"))
        .map(parse_autonomy_tier)
        .transpose()?
        .ok_or_else(|| VmError::Runtime(format!("{builtin}: missing autonomy_tier")))?;
    let trace_id = optional_string(map, "trace_id")
        .or_else(|| current_dispatch_context().map(|context| context.trigger_event.trace_id.0))
        .unwrap_or_else(|| format!("trace-{}", uuid::Uuid::now_v7()));
    let mut record = TrustRecord::new(agent, action, approver, outcome, trace_id, tier);
    if let Some(cost_usd) = map.get("cost_usd").and_then(vm_number_as_f64) {
        record.cost_usd = Some(cost_usd);
    }
    if let Some(evidence_refs) = map.get("evidence_refs") {
        let VmValue::List(items) = evidence_refs else {
            return Err(VmError::Runtime(format!(
                "{builtin}: evidence_refs must be a list"
            )));
        };
        record.metadata.insert(
            "evidence_refs".to_string(),
            serde_json::Value::Array(
                items
                    .iter()
                    .map(crate::llm::vm_value_to_json)
                    .collect::<Vec<_>>(),
            ),
        );
    }
    if let Some(metadata) = map.get("metadata") {
        let metadata_json = crate::llm::vm_value_to_json(metadata);
        let serde_json::Value::Object(object) = metadata_json else {
            return Err(VmError::Runtime(format!(
                "{builtin}: metadata must be a dict"
            )));
        };
        record.metadata.extend(object);
    }
    append_trust_record_value(builtin, record).await
}

async fn append_trust_record_value(
    builtin: &str,
    record: TrustRecord,
) -> Result<TrustRecord, VmError> {
    let log = ensure_trigger_event_log();
    crate::append_trust_record(&log, &record)
        .await
        .map_err(|error| VmError::Runtime(format!("{builtin}: {error}")))
}

fn vm_number_as_f64(value: &VmValue) -> Option<f64> {
    match value {
        VmValue::Float(value) => Some(*value),
        VmValue::Int(value) => Some(*value as f64),
        _ => None,
    }
}

fn parse_trust_query_filters(value: &VmValue) -> Result<TrustQueryFilters, VmError> {
    let VmValue::Dict(map) = value else {
        return Err(VmError::Runtime(
            "trust_query: filters must be a dict".to_string(),
        ));
    };
    Ok(TrustQueryFilters {
        agent: optional_string(map, "agent")
            .or_else(|| optional_string(map, "actor"))
            .or_else(|| optional_string(map, "actor_id")),
        action: optional_string(map, "action"),
        since: optional_string(map, "since")
            .map(|raw| parse_query_timestamp("trust_query", "since", &raw))
            .transpose()?,
        until: optional_string(map, "until")
            .map(|raw| parse_query_timestamp("trust_query", "until", &raw))
            .transpose()?,
        tier: map
            .get("tier")
            .or_else(|| map.get("autonomy_tier"))
            .or_else(|| map.get("autonomy_tier_at_time"))
            .map(parse_autonomy_tier)
            .transpose()?,
        outcome: map.get("outcome").map(parse_trust_outcome).transpose()?,
        limit: map.get("limit").map(parse_trust_query_limit).transpose()?,
        grouped_by_trace: map
            .get("grouped_by_trace")
            .map(parse_trust_query_grouped_flag)
            .transpose()?
            .unwrap_or(false),
    })
}

fn parse_trust_query_limit(value: &VmValue) -> Result<usize, VmError> {
    let limit = value.as_int().ok_or_else(|| {
        VmError::Runtime(format!(
            "trust_query: limit must be an int, got {}",
            value.type_name()
        ))
    })?;
    usize::try_from(limit).map_err(|_| {
        VmError::Runtime(format!(
            "trust_query: limit must be non-negative, got {limit}"
        ))
    })
}

fn parse_trust_query_grouped_flag(value: &VmValue) -> Result<bool, VmError> {
    match value {
        VmValue::Bool(flag) => Ok(*flag),
        other => Err(VmError::Runtime(format!(
            "trust_query: grouped_by_trace must be a bool, got {}",
            other.type_name()
        ))),
    }
}

fn parse_query_timestamp(builtin: &str, field: &str, raw: &str) -> Result<OffsetDateTime, VmError> {
    if let Ok(parsed) = OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339) {
        return Ok(parsed);
    }
    if let Ok(unix) = raw.parse::<i64>() {
        let parsed = if raw.len() > 10 {
            OffsetDateTime::from_unix_timestamp_nanos(unix as i128 * 1_000_000)
        } else {
            OffsetDateTime::from_unix_timestamp(unix)
        }
        .map_err(|error| {
            VmError::Runtime(format!(
                "{builtin}: invalid `{field}` timestamp '{raw}': {error}"
            ))
        })?;
        return Ok(parsed);
    }
    Err(VmError::Runtime(format!(
        "{builtin}: invalid `{field}` timestamp '{raw}', expected RFC3339 or unix seconds/milliseconds"
    )))
}
