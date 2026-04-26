use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, Time};

use crate::cli::FlowReplayAuditArgs;

pub(crate) fn run_replay_audit(args: &FlowReplayAuditArgs) -> Result<i32, String> {
    let since = parse_since(&args.since)?;
    if !args.store.is_file() {
        return Err(format!(
            "Flow store {} does not exist",
            args.store.display()
        ));
    }
    let store =
        harn_vm::flow::SqliteFlowStore::open(&args.store, "replay-audit").map_err(|error| {
            format!(
                "failed to open Flow store {}: {error}",
                args.store.display()
            )
        })?;

    let files = harn_vm::flow::discover_invariants(&args.root, &args.target_dir);
    let diagnostics = files
        .iter()
        .flat_map(|file| {
            file.diagnostics
                .iter()
                .map(move |diagnostic| (file.path.display().to_string(), diagnostic))
        })
        .collect::<Vec<_>>();
    let has_error = diagnostics.iter().any(|(_, diagnostic)| {
        diagnostic.severity == harn_vm::flow::DiscoveryDiagnosticSeverity::Error
    });
    if has_error {
        return Err(render_discovery_diagnostics(&diagnostics));
    }
    if !args.json {
        let warnings = diagnostics
            .iter()
            .filter(|(_, diagnostic)| {
                diagnostic.severity == harn_vm::flow::DiscoveryDiagnosticSeverity::Warning
            })
            .collect::<Vec<_>>();
        for (path, diagnostic) in warnings {
            eprintln!("warning: {path}: {}", diagnostic.message);
        }
    }

    let current_predicates = harn_vm::flow::resolve_predicates(&files);
    let stored = store
        .shipped_derived_slices_since(Some(since))
        .map_err(|error| format!("failed to list shipped slices: {error}"))?;
    let created_at_by_slice = stored
        .iter()
        .map(|stored| (stored.slice.id, stored.created_at.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let report = harn_vm::flow::replay_audit_report(
        stored.into_iter().map(|stored| stored.slice),
        &current_predicates,
    );

    if args.json {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|error| format!("failed to encode replay-audit report: {error}"))?;
        println!("{json}");
    } else {
        print_human_report(&args.since, &report, &created_at_by_slice);
    }

    Ok(if args.fail_on_drift && report.has_drift() {
        1
    } else {
        0
    })
}

fn print_human_report(
    since: &str,
    report: &harn_vm::flow::ReplayAuditReport,
    created_at_by_slice: &std::collections::BTreeMap<harn_vm::flow::SliceId, String>,
) {
    println!(
        "Audited {} shipped derived slice(s) since {since}; {} slice(s) have advisory drift.",
        report.audited_slices, report.drifted_slices
    );
    if report.slices.is_empty() {
        return;
    }
    for slice in &report.slices {
        let created_at = created_at_by_slice
            .get(&slice.slice_id)
            .map(String::as_str)
            .unwrap_or("unknown");
        println!("slice {} created_at={created_at}", slice.slice_id);
        if !slice.advisory_drift.is_empty() {
            println!("  current @retroactive predicates not pinned:");
            for predicate in &slice.advisory_drift {
                println!("    - {} {}", predicate.name, predicate.hash.as_str());
            }
        }
        if !slice.historical_only_predicates.is_empty() {
            println!("  historical predicate hashes no longer in current set:");
            for hash in &slice.historical_only_predicates {
                println!("    - {}", hash.as_str());
            }
        }
    }
}

fn render_discovery_diagnostics(
    diagnostics: &[(String, &harn_vm::flow::DiscoveryDiagnostic)],
) -> String {
    diagnostics
        .iter()
        .map(|(path, diagnostic)| format!("{path}: {}", diagnostic.message))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_since(raw: &str) -> Result<OffsetDateTime, String> {
    if let Ok(parsed) = OffsetDateTime::parse(raw, &Rfc3339) {
        return Ok(parsed);
    }
    if let Ok(unix) = raw.parse::<i64>() {
        let parsed = if raw.len() > 10 {
            OffsetDateTime::from_unix_timestamp_nanos(unix as i128 * 1_000_000)
        } else {
            OffsetDateTime::from_unix_timestamp(unix)
        };
        return parsed.map_err(|error| format!("invalid --since timestamp '{raw}': {error}"));
    }
    let date_format = time::format_description::parse("[year]-[month]-[day]")
        .map_err(|error| format!("failed to build date parser: {error}"))?;
    let date = Date::parse(raw, &date_format).map_err(|_| {
        format!("invalid --since date '{raw}'; use RFC3339, unix time, or YYYY-MM-DD")
    })?;
    Ok(date.with_time(Time::MIDNIGHT).assume_utc())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_since_accepts_rfc3339_unix_and_date() {
        assert_eq!(
            parse_since("2026-04-26T12:00:00Z")
                .unwrap()
                .unix_timestamp(),
            1_777_204_800
        );
        assert_eq!(
            parse_since("1777205600").unwrap().unix_timestamp(),
            1_777_205_600
        );
        assert_eq!(
            parse_since("2026-04-26").unwrap().unix_timestamp(),
            1_777_161_600
        );
    }
}
