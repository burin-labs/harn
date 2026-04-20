use std::collections::BTreeMap;
use std::path::Path;

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::cli::{TrustArgs, TrustCommand, TrustQueryArgs};

pub(crate) async fn handle(args: TrustArgs) -> Result<(), String> {
    match args.command {
        TrustCommand::Query(args) => run_query(args).await,
        TrustCommand::Promote(args) => run_control_change(args.agent, args.to.into(), None).await,
        TrustCommand::Demote(args) => {
            run_control_change(args.agent, args.to.into(), Some(args.reason)).await
        }
    }
}

async fn run_query(args: TrustQueryArgs) -> Result<(), String> {
    let log = open_trust_log()?;
    let filters = harn_vm::TrustQueryFilters {
        agent: args.agent,
        action: args.action,
        since: args.since.as_deref().map(parse_timestamp).transpose()?,
        until: args.until.as_deref().map(parse_timestamp).transpose()?,
        tier: args.tier.map(Into::into),
        outcome: args.outcome.map(Into::into),
    };
    let records = harn_vm::query_trust_records(&log, &filters)
        .await
        .map_err(|error| format!("failed to query trust records: {error}"))?;

    if args.summary {
        let summary = harn_vm::summarize_trust_records(&records);
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&summary).map_err(|error| error.to_string())?
            );
        } else {
            for item in summary {
                let mean_cost = item
                    .mean_cost_usd
                    .map(|value| format!("{value:.4}"))
                    .unwrap_or_else(|| "n/a".to_string());
                println!(
                    "{} total={} success_rate={:.2}% mean_cost_usd={} tiers={} outcomes={}",
                    item.agent,
                    item.total,
                    item.success_rate * 100.0,
                    mean_cost,
                    compact_counts(&item.tier_distribution),
                    compact_counts(&item.outcome_distribution),
                );
            }
        }
        return Ok(());
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&records).map_err(|error| error.to_string())?
        );
        return Ok(());
    }

    for record in records {
        println!(
            "{} agent={} action={} outcome={} tier={} trace_id={} approver={}",
            format_timestamp(record.timestamp),
            record.agent,
            record.action,
            record.outcome.as_str(),
            record.autonomy_tier.as_str(),
            record.trace_id,
            record.approver.unwrap_or_else(|| "-".to_string()),
        );
    }
    Ok(())
}

async fn run_control_change(
    agent: String,
    target_tier: harn_vm::AutonomyTier,
    reason: Option<String>,
) -> Result<(), String> {
    let log = open_trust_log()?;
    let action = if reason.is_some() {
        "trust.demote"
    } else {
        "trust.promote"
    };
    let actor = std::env::var("USER")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let mut record = harn_vm::TrustRecord::new(
        agent.clone(),
        action,
        actor.clone(),
        harn_vm::TrustOutcome::Success,
        format!("trustctl-{}", uuid::Uuid::now_v7()),
        target_tier,
    );
    record
        .metadata
        .insert("control".to_string(), serde_json::json!(true));
    if let Some(reason) = reason {
        record
            .metadata
            .insert("reason".to_string(), serde_json::json!(reason));
    }
    harn_vm::append_trust_record(&log, &record)
        .await
        .map_err(|error| format!("failed to append trust control record: {error}"))?;
    println!(
        "{} {} -> {}",
        if action == "trust.demote" {
            "demoted"
        } else {
            "promoted"
        },
        agent,
        target_tier.as_str(),
    );
    Ok(())
}

fn open_trust_log() -> Result<std::sync::Arc<harn_vm::event_log::AnyEventLog>, String> {
    harn_vm::reset_thread_local_state();
    let cwd = std::env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    let workspace_root = harn_vm::stdlib::process::find_project_root(&cwd).unwrap_or(cwd.clone());
    harn_vm::event_log::install_default_for_base_dir(Path::new(&workspace_root))
        .map_err(|error| format!("failed to open event log: {error}"))
}

fn parse_timestamp(raw: &str) -> Result<OffsetDateTime, String> {
    if let Ok(parsed) = OffsetDateTime::parse(raw, &Rfc3339) {
        return Ok(parsed);
    }
    if let Ok(unix) = raw.parse::<i64>() {
        let parsed = if raw.len() > 10 {
            OffsetDateTime::from_unix_timestamp_nanos(unix as i128 * 1_000_000)
        } else {
            OffsetDateTime::from_unix_timestamp(unix)
        };
        return parsed.map_err(|error| format!("invalid timestamp '{raw}': {error}"));
    }
    Err(format!(
        "invalid timestamp '{raw}': expected RFC3339 or unix seconds/milliseconds"
    ))
}

fn format_timestamp(value: OffsetDateTime) -> String {
    value.format(&Rfc3339).unwrap_or_else(|_| value.to_string())
}

fn compact_counts(counts: &BTreeMap<String, u64>) -> String {
    counts
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}
