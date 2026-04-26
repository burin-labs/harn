use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use harn_vm::flow::{IntentClusterer, ObservedAtom, SqliteFlowStore, VcsBackend};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::{Date, OffsetDateTime, Time};

use crate::cli::{
    FlowArchivistCommand, FlowArgs, FlowCommand, FlowReplayAuditArgs, FlowShipCommand,
};

const SHIP_CAPTAIN_EVAL_PACKS: [&str; 4] = [
    "slice_quality",
    "false_ship_rate",
    "coverage_fidelity",
    "latency_pr_to_merge",
];

pub(crate) fn run_flow(args: &FlowArgs) -> Result<i32, String> {
    match &args.command {
        FlowCommand::ReplayAudit(replay) => run_replay_audit(replay),
        FlowCommand::Ship(ship) => match &ship.command {
            FlowShipCommand::Watch(watch) => run_ship_watch(watch),
        },
        FlowCommand::Archivist(archivist) => match &archivist.command {
            FlowArchivistCommand::Scan(scan) => run_archivist_scan(scan),
        },
    }
}

pub(crate) fn run_replay_audit(args: &FlowReplayAuditArgs) -> Result<i32, String> {
    let since = args.since.as_deref().map(parse_since).transpose()?;
    if !args.store.is_file() {
        return Err(format!(
            "Flow store {} does not exist",
            args.store.display()
        ));
    }
    let store = SqliteFlowStore::open(&args.store, "replay-audit").map_err(|error| {
        format!(
            "failed to open Flow store {}: {error}",
            args.store.display()
        )
    })?;

    let chains = current_predicate_chains(&args.predicate_root, &args.touched_dirs);
    let diagnostics = discovery_diagnostics(&chains);
    if has_discovery_error(&diagnostics) {
        return Err(render_discovery_diagnostics(&diagnostics));
    }
    if !args.json {
        print_discovery_warnings(&diagnostics);
    }

    let current_predicates = harn_vm::flow::resolve_predicates_for_touched_directories(&chains);
    let stored = store
        .shipped_derived_slices_since(since)
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
        print_human_report(
            args.since.as_deref().unwrap_or("beginning"),
            &report,
            &created_at_by_slice,
        );
    }

    Ok(if args.fail_on_drift && report.has_drift() {
        1
    } else {
        0
    })
}

fn run_ship_watch(args: &crate::cli::FlowShipWatchArgs) -> Result<i32, String> {
    let store = open_store(&args.store)?;
    let atom_refs = store
        .list_atoms()
        .map_err(|error| format!("failed to list Flow atoms: {error}"))?;
    if atom_refs.is_empty() {
        let payload = json!({
            "status": "idle",
            "reason": "no_atoms",
            "persona": args.persona,
            "phase": "phase_0",
            "mode": "shadow",
            "autonomy": "propose_with_approval",
            "receipts_required": true,
        });
        print_payload(
            args.json,
            "Ship Captain idle: no atoms in the Flow store.",
            &payload,
        );
        return Ok(0);
    }

    let atoms = atom_refs
        .iter()
        .map(|atom_ref| {
            store
                .get_atom(atom_ref.atom_id)
                .map_err(|error| format!("failed to load atom {}: {error}", atom_ref.atom_id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let intents = IntentClusterer::default().cluster(
        atoms
            .iter()
            .enumerate()
            .map(|(index, atom)| ObservedAtom::from_atom(atom, (index + 1) as u64)),
    );
    let intent_payload = intents
        .iter()
        .map(|intent| {
            json!({
                "id": intent.id,
                "goal_description": intent.goal_description,
                "atoms": intent.atoms,
                "confidence": intent.confidence,
                "origin_transcript_span": intent.origin_transcript_span,
            })
        })
        .collect::<Vec<_>>();

    let chains = current_predicate_chains(&args.predicate_root, &args.touched_dirs);
    let diagnostics = discovery_diagnostics(&chains);
    if has_discovery_error(&diagnostics) {
        return Err(render_discovery_diagnostics(&diagnostics));
    }
    if !args.json {
        print_discovery_warnings(&diagnostics);
    }
    let predicates = harn_vm::flow::resolve_predicates_for_touched_directories(&chains);
    let predicate_payload = predicates
        .iter()
        .map(|predicate| {
            json!({
                "qualified_name": predicate.qualified_name,
                "logical_name": predicate.logical_name,
                "hash": predicate.predicate.source_hash,
                "kind": predicate.predicate.kind,
                "relative_dir": predicate.source.relative_dir,
                "retroactive": predicate.predicate.retroactive,
            })
        })
        .collect::<Vec<_>>();

    let atom_ids: Vec<_> = atom_refs.iter().map(|atom| atom.atom_id).collect();
    let slice = store
        .derive_slice(&atom_ids)
        .map_err(|error| format!("failed to derive candidate slice: {error}"))?;
    let ship_receipt = store
        .ship_slice(&slice)
        .map_err(|error| format!("failed to persist Ship Captain receipt: {error}"))?;
    let created_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| format!("failed to format receipt timestamp: {error}"))?;
    let mock_pr = json!({
        "number": 0,
        "state": "open",
        "url": format!("mock://github/pull/{}", slice.id),
        "title": format!("Flow slice {}", slice.id),
        "body": format!(
            "Shadow-mode Ship Captain candidate slice.\n\nAtoms: {}\nIntents: {}\nPredicates discovered: {}\n\nNo remote PR was opened.",
            slice.atoms.len(),
            intents.len(),
            predicates.len(),
        ),
        "requires_approval": true,
    });
    let payload = json!({
        "status": "mock_pr_opened",
        "persona": args.persona,
        "phase": "phase_0",
        "mode": "shadow",
        "autonomy": "propose_with_approval",
        "receipts_required": true,
        "created_at": created_at,
        "slice": {
            "id": slice.id,
            "atoms": slice.atoms,
            "atom_count": slice.atoms.len(),
        },
        "intents": intent_payload,
        "predicate_validation": {
            "predicate_root": args.predicate_root,
            "touched_dirs": if args.touched_dirs.is_empty() {
                vec![PathBuf::from(".")]
            } else {
                args.touched_dirs.clone()
            },
            "status": "ok",
            "predicates": predicate_payload,
            "diagnostics": diagnostics.iter().map(|(path, diagnostic)| json!({
                "path": path,
                "severity": discovery_severity_label(diagnostic.severity),
                "message": diagnostic.message,
            })).collect::<Vec<_>>(),
        },
        "ship_receipt": {
            "slice_id": ship_receipt.slice_id,
            "commit": ship_receipt.commit,
            "ref_name": ship_receipt.ref_name,
        },
        "mock_pr": mock_pr,
        "eval_packs": SHIP_CAPTAIN_EVAL_PACKS,
    });

    if let Some(path) = &args.mock_pr_out {
        write_json(path, &payload)
            .map_err(|error| format!("failed to write mock PR receipt: {error}"))?;
    }
    print_payload(
        args.json,
        &format!("mock PR opened for candidate slice {}", slice.id),
        &payload,
    );
    Ok(0)
}

fn discovery_severity_label(severity: harn_vm::flow::DiscoveryDiagnosticSeverity) -> &'static str {
    match severity {
        harn_vm::flow::DiscoveryDiagnosticSeverity::Warning => "warning",
        harn_vm::flow::DiscoveryDiagnosticSeverity::Error => "error",
    }
}

fn run_archivist_scan(args: &crate::cli::FlowArchivistScanArgs) -> Result<i32, String> {
    let stack_hints = stack_hints(&args.repo);
    let invariant_files = find_invariant_dirs(&args.repo);
    let mut seen = BTreeSet::new();
    let mut predicates = Vec::new();
    for dir in &invariant_files {
        for file in harn_vm::flow::discover_invariants(&args.repo, dir) {
            let relative_dir = file.relative_dir;
            for predicate in file.predicates {
                if !seen.insert(predicate.source_hash.clone()) {
                    continue;
                }
                predicates.push(json!({
                    "name": predicate.name,
                    "hash": predicate.source_hash,
                    "kind": predicate.kind,
                    "relative_dir": relative_dir.clone(),
                    "retroactive": predicate.retroactive,
                    "archivist": predicate.archivist.map(|archivist| json!({
                        "evidence": archivist.evidence,
                        "confidence": archivist.confidence,
                        "source_date": archivist.source_date,
                        "coverage_examples": archivist.coverage_examples,
                    })),
                }));
            }
        }
    }
    let proposals = default_archivist_proposals(&stack_hints, predicates.is_empty());
    let payload = json!({
        "status": "proposal_set",
        "repo": args.repo,
        "stack_hints": stack_hints,
        "existing_predicates": predicates,
        "proposals": proposals,
    });

    if let Some(path) = &args.out {
        write_json(path, &payload)
            .map_err(|error| format!("failed to write Archivist proposal set: {error}"))?;
    }
    print_payload(args.json, "Archivist proposal set emitted.", &payload);
    Ok(0)
}

fn current_predicate_chains(
    root: &Path,
    touched_dirs: &[PathBuf],
) -> Vec<Vec<harn_vm::flow::DiscoveredInvariantFile>> {
    let dirs: Vec<PathBuf> = if touched_dirs.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        touched_dirs.to_vec()
    };
    dirs.into_iter()
        .map(|dir| harn_vm::flow::discover_invariants(root, &dir))
        .collect()
}

fn open_store(path: &Path) -> Result<SqliteFlowStore, String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    SqliteFlowStore::open(path, "flow-cli").map_err(|error| error.to_string())
}

fn find_invariant_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    collect_invariant_dirs(root, root, &mut dirs);
    dirs
}

fn collect_invariant_dirs(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if matches!(name, ".git" | "target" | "node_modules") {
                continue;
            }
            collect_invariant_dirs(root, &path, out);
        } else if path.file_name().and_then(|name| name.to_str()) == Some("invariants.harn") {
            out.push(path.parent().unwrap_or(root).to_path_buf());
        }
    }
}

fn stack_hints(repo: &Path) -> Vec<&'static str> {
    let mut hints = Vec::new();
    if repo.join("Cargo.toml").exists() {
        hints.push("rust");
    }
    if repo.join("package.json").exists() {
        hints.push("javascript");
    }
    if repo.join("pyproject.toml").exists() {
        hints.push("python");
    }
    if repo.join("go.mod").exists() {
        hints.push("go");
    }
    hints
}

fn default_archivist_proposals(
    stack_hints: &[&str],
    no_existing_predicates: bool,
) -> Vec<serde_json::Value> {
    let mut proposals = Vec::new();
    if no_existing_predicates {
        proposals.push(json!({
            "path": "invariants.harn",
            "title": "Seed repo-wide meta-invariants",
            "body": "Add hand-authored bootstrap rules requiring @archivist evidence and deterministic fallbacks for semantic predicates.",
            "autonomy": "propose_only",
        }));
    }
    if stack_hints.contains(&"rust") {
        proposals.push(json!({
            "path": "invariants.harn",
            "title": "Rust unsafe and panic surface guard",
            "body": "Propose deterministic predicates for new unsafe blocks, panic paths in library code, and missing tests near touched atoms.",
            "autonomy": "propose_only",
        }));
    }
    proposals
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

fn discovery_diagnostics(
    chains: &[Vec<harn_vm::flow::DiscoveredInvariantFile>],
) -> Vec<(String, &harn_vm::flow::DiscoveryDiagnostic)> {
    chains
        .iter()
        .flat_map(|chain| chain.iter())
        .flat_map(|file| {
            file.diagnostics
                .iter()
                .map(move |diagnostic| (file.path.display().to_string(), diagnostic))
        })
        .collect()
}

fn has_discovery_error(diagnostics: &[(String, &harn_vm::flow::DiscoveryDiagnostic)]) -> bool {
    diagnostics.iter().any(|(_, diagnostic)| {
        diagnostic.severity == harn_vm::flow::DiscoveryDiagnosticSeverity::Error
    })
}

fn print_discovery_warnings(diagnostics: &[(String, &harn_vm::flow::DiscoveryDiagnostic)]) {
    for (path, diagnostic) in diagnostics.iter().filter(|(_, diagnostic)| {
        diagnostic.severity == harn_vm::flow::DiscoveryDiagnosticSeverity::Warning
    }) {
        eprintln!("warning: {path}: {}", diagnostic.message);
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

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), std::io::Error> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value).unwrap())
}

fn print_payload(json_output: bool, text: &str, payload: &serde_json::Value) {
    if json_output {
        println!("{}", serde_json::to_string_pretty(payload).unwrap());
    } else {
        println!("{text}");
    }
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
