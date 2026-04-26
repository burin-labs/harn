use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harn_vm::flow::{IntentClusterer, ObservedAtom, SqliteFlowStore, TextOp, VcsBackend};
use serde::ser::SerializeStruct;
use serde::Serialize;
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, OffsetDateTime, Time};

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
    let repo = args
        .repo
        .canonicalize()
        .unwrap_or_else(|_| args.repo.clone());
    let source_date = OffsetDateTime::now_utc().date().to_string();
    let inventory = inventory_repo(&repo);
    let stack_hints = inventory.stack_hints.clone();
    let manifest = load_archivist_manifest(&repo, args.manifest.as_deref());
    let invariant_files = find_invariant_dirs(&repo);
    let mut seen = BTreeSet::new();
    let mut predicates = Vec::new();
    let mut discovery_diagnostics = Vec::new();
    for dir in &invariant_files {
        for file in harn_vm::flow::discover_invariants(&repo, dir) {
            let relative_dir = file.relative_dir.clone();
            for diagnostic in &file.diagnostics {
                discovery_diagnostics.push(json!({
                    "relative_dir": relative_dir,
                    "path": file.path,
                    "severity": format!("{:?}", diagnostic.severity).to_lowercase(),
                    "message": diagnostic.message,
                }));
            }
            for predicate in file.predicates {
                if !seen.insert(predicate.source_hash.clone()) {
                    continue;
                }
                predicates.push(json!({
                    "name": predicate.name,
                    "hash": predicate.source_hash,
                    "kind": predicate.kind,
                    "fallback": predicate.fallback,
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
    let convention = mine_convention_signals(&repo);
    let motion = mine_motion_signals(&repo);
    let proposals = archivist_proposals(
        &repo,
        &inventory,
        &convention,
        &motion,
        predicates.is_empty(),
        &source_date,
    );
    let shadow_evaluation = shadow_evaluate(&repo, &args.store, args.shadow_days, &proposals)?;
    let payload = json!({
        "status": "proposal_set",
        "persona": {
            "name": "archivist",
            "mode": "propose_only",
            "autonomy": "propose_only",
            "promotion": "human_review_required",
        },
        "repo": repo,
        "manifest": manifest,
        "inventory": inventory,
        "stack_hints": stack_hints,
        "convention_signals": convention,
        "motion_signals": motion,
        "seed_library": {
            "repository": "https://github.com/burin-labs/harn-canon",
            "strategy": "detected-stack seeds are copied into proposals, then repo-local evidence prunes them before review",
        },
        "existing_predicates": predicates,
        "discovery_diagnostics": discovery_diagnostics,
        "proposals": proposals,
        "shadow_evaluation": shadow_evaluation,
    });

    if let Some(path) = &args.out {
        write_json(path, &payload)
            .map_err(|error| format!("failed to write Archivist proposal set: {error}"))?;
    }
    print_payload(args.json, "Archivist proposal set emitted.", &payload);
    Ok(0)
}

#[derive(Clone, Debug, Default, Serialize)]
struct RepoInventory {
    stack_hints: Vec<&'static str>,
    lockfiles: Vec<String>,
    config_files: Vec<String>,
    source_roots: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct Signal {
    kind: &'static str,
    path: String,
    detail: String,
}

#[derive(Clone, Debug, Serialize)]
struct MotionSignal {
    kind: &'static str,
    count: usize,
    examples: Vec<String>,
}

#[derive(Clone, Debug)]
struct ArchivistProposal {
    id: &'static str,
    title: &'static str,
    path: String,
    rationale: String,
    predicate_name: &'static str,
    match_terms: Vec<&'static str>,
    evidence: Vec<String>,
    confidence: f64,
    coverage_examples: Vec<String>,
    source: String,
}

impl Serialize for ArchivistProposal {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("ArchivistProposal", 11)?;
        state.serialize_field("id", self.id)?;
        state.serialize_field("title", self.title)?;
        state.serialize_field("path", &self.path)?;
        state.serialize_field("rationale", &self.rationale)?;
        state.serialize_field("predicate_name", self.predicate_name)?;
        state.serialize_field("autonomy", "propose_only")?;
        state.serialize_field("promotion", "human_review_required")?;
        state.serialize_field("evidence", &self.evidence)?;
        state.serialize_field("confidence", &self.confidence)?;
        state.serialize_field("coverage_examples", &self.coverage_examples)?;
        state.serialize_field("predicate_source", &self.source)?;
        state.end()
    }
}

fn inventory_repo(repo: &Path) -> RepoInventory {
    let mut inventory = RepoInventory::default();
    let known = [
        ("Cargo.toml", "rust", "config"),
        ("Cargo.lock", "rust", "lockfile"),
        ("rust-toolchain.toml", "rust", "config"),
        ("rustfmt.toml", "rust", "config"),
        ("clippy.toml", "rust", "config"),
        ("package.json", "javascript", "config"),
        ("package-lock.json", "javascript", "lockfile"),
        ("pnpm-lock.yaml", "javascript", "lockfile"),
        ("yarn.lock", "javascript", "lockfile"),
        ("tsconfig.json", "typescript", "config"),
        ("pyproject.toml", "python", "config"),
        ("poetry.lock", "python", "lockfile"),
        ("uv.lock", "python", "lockfile"),
        ("go.mod", "go", "config"),
        ("go.sum", "go", "lockfile"),
        ("Package.swift", "swift", "config"),
    ];
    for (path, stack, kind) in known {
        if repo.join(path).exists() {
            push_unique(&mut inventory.stack_hints, stack);
            match kind {
                "lockfile" => inventory.lockfiles.push(path.to_string()),
                _ => inventory.config_files.push(path.to_string()),
            }
        }
    }
    if repo.join(".github/workflows").is_dir() {
        inventory.config_files.push(".github/workflows".to_string());
    }
    for root in ["crates", "src", "docs/src", "conformance/tests", "examples"] {
        if repo.join(root).exists() {
            inventory.source_roots.push(root.to_string());
        }
    }
    inventory
}

fn push_unique(values: &mut Vec<&'static str>, value: &'static str) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn load_archivist_manifest(repo: &Path, explicit: Option<&Path>) -> serde_json::Value {
    let explicit_manifest = explicit.is_some();
    let candidates = explicit
        .map(|path| vec![path.to_path_buf()])
        .unwrap_or_else(|| {
            [
                repo.join("harn.toml"),
                repo.join("examples/personas/flow.harn.toml"),
                repo.join("examples/personas/harn.toml"),
            ]
            .into_iter()
            .filter(|path| path.is_file())
            .collect()
        });
    let mut loaded_without_archivist = None;
    let mut first_invalid = None;
    for candidate in candidates {
        match crate::package::load_personas_from_manifest_path(&candidate) {
            Ok(catalog) => {
                let archivist = catalog
                    .personas
                    .iter()
                    .find(|persona| persona.name.as_deref() == Some("archivist"));
                if let Some(persona) = archivist {
                    return json!({
                        "status": "loaded",
                        "path": catalog.manifest_path,
                        "persona": persona,
                    });
                }
                loaded_without_archivist.get_or_insert_with(|| json!({
                    "status": "loaded_without_archivist",
                    "path": catalog.manifest_path,
                    "personas": catalog.personas.iter().filter_map(|p| p.name.clone()).collect::<Vec<_>>(),
                }));
            }
            Err(errors) => {
                let invalid = json!({
                    "status": "invalid",
                    "path": candidate,
                    "errors": errors.iter().map(ToString::to_string).collect::<Vec<_>>(),
                });
                if explicit_manifest {
                    return invalid;
                }
                first_invalid.get_or_insert(invalid);
            }
        }
    }
    if let Some(loaded) = loaded_without_archivist {
        return loaded;
    }
    if let Some(invalid) = first_invalid {
        return invalid;
    }
    json!({
        "status": "not_found",
        "searched": ["harn.toml", "examples/personas/flow.harn.toml", "examples/personas/harn.toml"],
    })
}

fn mine_convention_signals(repo: &Path) -> Vec<Signal> {
    let mut signals = Vec::new();
    for path in walk_repo_files(repo, 4_000) {
        let relative = relative_path(repo, &path);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if matches!(
            file_name,
            "rustfmt.toml" | "clippy.toml" | "deny.toml" | ".markdownlint.json" | ".prettierrc"
        ) {
            signals.push(Signal {
                kind: "lint_config",
                path: relative.clone(),
                detail: "repo-local style or lint policy".to_string(),
            });
        }
        if relative.ends_with(".harn")
            || relative.ends_with(".rs")
            || relative.ends_with(".md")
            || relative.ends_with(".toml")
        {
            if let Ok(source) = fs::read_to_string(&path) {
                for (index, line) in source.lines().enumerate() {
                    let trimmed = line.trim_start();
                    let is_comment = trimmed.starts_with("//")
                        || trimmed.starts_with('#')
                        || trimmed.starts_with("<!--");
                    if is_comment {
                        if let Some(pos) = trimmed.to_ascii_lowercase().find("invariant:") {
                            signals.push(Signal {
                                kind: "inline_invariant",
                                path: format!("{relative}:{}", index + 1),
                                detail: trimmed[pos..].trim().chars().take(180).collect(),
                            });
                        }
                    }
                    if signals.len() >= 80 {
                        return signals;
                    }
                }
            }
        }
    }
    signals
}

fn mine_motion_signals(repo: &Path) -> Vec<MotionSignal> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "log",
            "--since=90 days ago",
            "--pretty=%s",
            "--max-count=200",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let buckets: [(&str, &[&str]); 4] = [
        ("tests", &["test", "coverage", "conformance"]),
        ("lint_format", &["lint", "format", "fmt", "clippy"]),
        (
            "flow_predicates",
            &["flow", "predicate", "invariant", "archivist"],
        ),
        ("release_docs", &["release", "docs", "changelog"]),
    ];
    let mut counts: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for subject in stdout.lines() {
        let lower = subject.to_ascii_lowercase();
        for (kind, terms) in buckets {
            if terms.iter().any(|term| lower.contains(term)) {
                counts
                    .entry(kind)
                    .or_default()
                    .push(subject.chars().take(140).collect());
            }
        }
    }
    counts
        .into_iter()
        .map(|(kind, examples)| MotionSignal {
            kind,
            count: examples.len(),
            examples: examples.into_iter().take(5).collect(),
        })
        .collect()
}

fn archivist_proposals(
    repo: &Path,
    inventory: &RepoInventory,
    convention: &[Signal],
    motion: &[MotionSignal],
    no_existing_predicates: bool,
    source_date: &str,
) -> Vec<ArchivistProposal> {
    let mut proposals = Vec::new();
    if no_existing_predicates {
        proposals.push(bootstrap_proposal(source_date));
    }
    if inventory.stack_hints.contains(&"rust") {
        proposals.push(rust_unsafe_proposal(repo, source_date));
        proposals.push(rust_panics_proposal(repo, source_date));
    }
    if inventory
        .config_files
        .iter()
        .any(|path| path == ".github/workflows")
    {
        proposals.push(github_actions_permissions_proposal(source_date));
    }
    if motion
        .iter()
        .any(|signal| signal.kind == "tests" && signal.count >= 3)
    {
        proposals.push(test_motion_proposal(source_date));
    }
    let inline_signals = convention
        .iter()
        .filter(|signal| signal.kind == "inline_invariant")
        .take(5)
        .collect::<Vec<_>>();
    if !inline_signals.is_empty() {
        proposals.push(inline_invariant_proposal(&inline_signals, source_date));
    }
    proposals
}

fn bootstrap_proposal(source_date: &str) -> ArchivistProposal {
    let evidence = vec![
        "https://slsa.dev/spec/v1.0/provenance".to_string(),
        "https://in-toto.io/attestation-spec/".to_string(),
    ];
    let coverage_examples = vec![
        "invariants.harn".to_string(),
        "meta-invariants.harn".to_string(),
    ];
    proposal(
        "bootstrap-meta-invariants",
        "Seed repo-wide predicate authorship metadata",
        "invariants.harn",
        "The repository has no discovered Flow predicates; seed review-only bootstrap metadata before expanding policy.",
        "predicate_metadata_is_reviewable",
        vec!["@archivist", "@semantic", "@deterministic"],
        evidence,
        0.72,
        coverage_examples,
        source_date,
        "flow_invariant_warn(\"bootstrap predicate metadata should be reviewed by a human maintainer\")",
    )
}

fn rust_unsafe_proposal(repo: &Path, source_date: &str) -> ArchivistProposal {
    let examples = files_containing(repo, "unsafe", &["rs"], 5);
    proposal(
        "rust-unsafe-safety-comment",
        "Require review evidence near new Rust unsafe blocks",
        "invariants.harn",
        "Rust is detected and unsafe blocks are a recurring high-value review boundary; propose a deterministic guard that warns on unsafe additions without nearby safety rationale.",
        "rust_unsafe_requires_safety_comment",
        vec!["unsafe", "SAFETY:"],
        vec![
            "https://doc.rust-lang.org/clippy/lint_configuration.html#undocumented_unsafe_blocks".to_string(),
            "https://rust-lang.github.io/api-guidelines/documentation.html".to_string(),
        ],
        0.82,
        examples,
        source_date,
        "flow_invariant_warn(\"new unsafe code should include nearby SAFETY rationale or explicit reviewer approval\")",
    )
}

fn rust_panics_proposal(repo: &Path, source_date: &str) -> ArchivistProposal {
    let mut examples = files_containing(repo, "panic!", &["rs"], 5);
    examples.extend(files_containing(
        repo,
        ".unwrap()",
        &["rs"],
        5 - examples.len().min(5),
    ));
    proposal(
        "rust-library-panic-surface",
        "Flag new library panic surfaces without tests or documentation",
        "invariants.harn",
        "The Rust API Guidelines call out documented panic conditions; Flow can cheaply warn when atoms add panic-prone surfaces in library crates.",
        "rust_library_panics_are_documented",
        vec!["panic!", "unwrap()", "expect("],
        vec![
            "https://rust-lang.github.io/api-guidelines/documentation.html#c-failure".to_string(),
            "https://rust-lang.github.io/rust-clippy/beta/".to_string(),
        ],
        0.76,
        examples,
        source_date,
        "flow_invariant_warn(\"new panic-prone Rust paths should include tests or documented panic conditions\")",
    )
}

fn github_actions_permissions_proposal(source_date: &str) -> ArchivistProposal {
    proposal(
        "github-actions-minimal-permissions",
        "Warn on workflow edits without explicit permissions",
        ".github/invariants.harn",
        "GitHub workflow files are present; explicit job/workflow permissions make CI authority reviewable and reduce supply-chain blast radius.",
        "github_actions_permissions_are_explicit",
        vec!["permissions:", "uses:"],
        vec![
            "https://docs.github.com/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions".to_string(),
            "https://docs.github.com/code-security/supply-chain-security/understanding-your-software-supply-chain/about-supply-chain-security".to_string(),
        ],
        0.79,
        vec![".github/workflows".to_string()],
        source_date,
        "flow_invariant_warn(\"workflow edits should keep explicit least-privilege permissions\")",
    )
}

fn test_motion_proposal(source_date: &str) -> ArchivistProposal {
    proposal(
        "motion-tests-near-flow-changes",
        "Keep test coverage close to recurring Flow changes",
        "invariants.harn",
        "Recent history repeatedly touches tests/conformance around Flow work; propose a warning when Flow atoms lack nearby test coverage evidence.",
        "flow_changes_keep_tests_nearby",
        vec!["flow", "predicate", "conformance", "test"],
        vec![
            "git log --since='90 days ago' --pretty=%s".to_string(),
            "conformance/tests/".to_string(),
        ],
        0.68,
        vec!["crates/harn-vm/src/flow".to_string(), "conformance/tests".to_string()],
        source_date,
        "flow_invariant_warn(\"Flow predicate/runtime changes should carry focused tests or conformance coverage\")",
    )
}

fn inline_invariant_proposal(signals: &[&Signal], source_date: &str) -> ArchivistProposal {
    let id = "inline-invariant-crystallization";
    let examples = signals
        .iter()
        .map(|signal| signal.path.clone())
        .collect::<Vec<_>>();
    proposal(
        id,
        "Crystallize inline invariant comment into Flow predicate",
        "invariants.harn",
        "Found inline invariant comments; propose turning recurring comments into reviewable predicate metadata.",
        "inline_invariant_comment_is_crystallized",
        vec!["invariant:"],
        examples.clone(),
        0.64,
        examples,
        source_date,
        "flow_invariant_warn(\"inline invariant comments should graduate into reviewable Flow predicates when they recur\")",
    )
}

#[allow(clippy::too_many_arguments)]
fn proposal(
    id: &'static str,
    title: &'static str,
    path: &str,
    rationale: &str,
    predicate_name: &'static str,
    match_terms: Vec<&'static str>,
    evidence: Vec<String>,
    confidence: f64,
    coverage_examples: Vec<String>,
    source_date: &str,
    result_expr: &str,
) -> ArchivistProposal {
    let evidence_harn = evidence
        .iter()
        .map(|item| format!("{:?}", item))
        .collect::<Vec<_>>()
        .join(", ");
    let coverage_harn = coverage_examples
        .iter()
        .map(|item| format!("{:?}", item))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "@invariant\n@deterministic\n@archivist(evidence: [{evidence_harn}], confidence: {confidence:.2}, source_date: {:?}, coverage_examples: [{coverage_harn}])\nfn {predicate_name}(slice) {{\n  return {result_expr}\n}}\n",
        source_date
    );
    ArchivistProposal {
        id,
        title,
        path: path.to_string(),
        rationale: rationale.to_string(),
        predicate_name,
        match_terms,
        evidence,
        confidence,
        coverage_examples,
        source,
    }
}

fn shadow_evaluate(
    repo: &Path,
    store_path: &Path,
    shadow_days: u32,
    proposals: &[ArchivistProposal],
) -> Result<serde_json::Value, String> {
    let store_path = if store_path.is_absolute() {
        store_path.to_path_buf()
    } else {
        repo.join(store_path)
    };
    if !store_path.is_file() {
        return Ok(json!({
            "status": "no_flow_store",
            "store": store_path,
            "window_days": shadow_days,
            "recent_atoms": 0,
            "proposal_results": empty_shadow_results(proposals),
            "false_positive_candidates": [],
        }));
    }
    let store = SqliteFlowStore::open(&store_path, "archivist-shadow").map_err(|error| {
        format!(
            "failed to open Flow store {}: {error}",
            store_path.display()
        )
    })?;
    let since = OffsetDateTime::now_utc() - Duration::days(i64::from(shadow_days));
    let refs = store
        .list_atoms()
        .map_err(|error| format!("failed to list Flow atoms: {error}"))?;
    let mut recent_atoms = Vec::new();
    for atom_ref in refs {
        let atom = store
            .get_atom(atom_ref.atom_id)
            .map_err(|error| format!("failed to load Flow atom {}: {error}", atom_ref.atom_id))?;
        if atom.provenance.timestamp >= since {
            recent_atoms.push(atom);
        }
    }

    let mut false_positive_candidates = Vec::new();
    let mut results = Vec::new();
    for proposal in proposals {
        let mut matched_atoms = 0usize;
        for atom in &recent_atoms {
            let inserted = inserted_text(atom);
            if proposal.match_terms.iter().any(|term| {
                inserted
                    .to_ascii_lowercase()
                    .contains(&term.to_ascii_lowercase())
            }) {
                matched_atoms += 1;
                if likely_false_positive(proposal, &inserted) {
                    false_positive_candidates.push(json!({
                        "proposal_id": proposal.id,
                        "atom": atom.id,
                        "transcript_ref": atom.provenance.transcript_ref,
                        "diff_span": first_insert_span(atom),
                        "reason": "heuristic match may already contain satisfying context",
                    }));
                }
            }
        }
        results.push(json!({
            "proposal_id": proposal.id,
            "recent_atoms": recent_atoms.len(),
            "matching_atoms": matched_atoms,
            "estimated_coverage": if recent_atoms.is_empty() { 0.0 } else { matched_atoms as f64 / recent_atoms.len() as f64 },
        }));
    }
    Ok(json!({
        "status": "evaluated",
        "store": store_path,
        "window_days": shadow_days,
        "recent_atoms": recent_atoms.len(),
        "proposal_results": results,
        "false_positive_candidates": false_positive_candidates,
    }))
}

fn empty_shadow_results(proposals: &[ArchivistProposal]) -> Vec<serde_json::Value> {
    proposals
        .iter()
        .map(|proposal| {
            json!({
                "proposal_id": proposal.id,
                "recent_atoms": 0,
                "matching_atoms": 0,
                "estimated_coverage": 0.0,
            })
        })
        .collect()
}

fn inserted_text(atom: &harn_vm::flow::Atom) -> String {
    atom.ops
        .iter()
        .filter_map(|op| match op {
            TextOp::Insert { content, .. } => Some(content.as_str()),
            TextOp::Delete { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_insert_span(atom: &harn_vm::flow::Atom) -> serde_json::Value {
    atom.ops
        .iter()
        .find_map(|op| match op {
            TextOp::Insert { offset, content } => Some(json!({
                "start": offset,
                "end": offset.saturating_add(content.len() as u64),
            })),
            TextOp::Delete { .. } => None,
        })
        .unwrap_or_else(|| json!({"start": 0, "end": 0}))
}

fn likely_false_positive(proposal: &ArchivistProposal, inserted: &str) -> bool {
    match proposal.id {
        "rust-unsafe-safety-comment" => {
            inserted.contains("unsafe") && inserted.to_ascii_lowercase().contains("safety")
        }
        "github-actions-minimal-permissions" => {
            inserted.contains("permissions:") && inserted.contains("uses:")
        }
        _ => false,
    }
}

fn files_containing(repo: &Path, needle: &str, extensions: &[&str], limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let needle = needle.to_ascii_lowercase();
    let mut matches = Vec::new();
    for path in walk_repo_files(repo, 4_000) {
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !extensions.contains(&ext) {
            continue;
        }
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        if source.to_ascii_lowercase().contains(&needle) {
            matches.push(relative_path(repo, &path));
            if matches.len() >= limit {
                break;
            }
        }
    }
    matches
}

fn walk_repo_files(repo: &Path, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_repo_files(repo, repo, limit, &mut files);
    files
}

fn collect_repo_files(root: &Path, dir: &Path, limit: usize, out: &mut Vec<PathBuf>) {
    if out.len() >= limit {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        if out.len() >= limit {
            return;
        }
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if should_skip_scan_dir(name) {
                continue;
            }
            collect_repo_files(root, &path, limit, out);
        } else if path.is_file() {
            let relative = relative_path(root, &path);
            if !relative.ends_with(".lock")
                || matches!(name, "Cargo.lock" | "package-lock.json" | "yarn.lock")
            {
                out.push(path);
            }
        }
    }
}

fn should_skip_scan_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | "docs/dist"
            | ".harn"
            | ".harn-runs"
            | ".claude"
            | ".burin"
    )
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
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
    use ed25519_dalek::SigningKey;
    use harn_vm::flow::{Atom, Provenance};

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

    #[test]
    fn archivist_rust_proposal_is_parseable_harn_with_provenance() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/lib.rs"), "pub unsafe fn raw() {}\n").unwrap();

        let inventory = inventory_repo(temp.path());
        let proposals = archivist_proposals(temp.path(), &inventory, &[], &[], true, "2026-04-26");
        let rust = proposals
            .iter()
            .find(|proposal| proposal.id == "rust-unsafe-safety-comment")
            .expect("rust unsafe proposal");

        let parsed = harn_vm::flow::parse_invariants_source(&rust.source);
        assert!(
            parsed.diagnostics.is_empty(),
            "generated source should parse cleanly: {:?}",
            parsed.diagnostics
        );
        assert_eq!(
            parsed.predicates[0].name,
            "rust_unsafe_requires_safety_comment"
        );
        assert!(parsed.predicates[0].archivist.is_some());
    }

    #[test]
    fn shadow_evaluate_reports_false_positive_atom_pointers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        let store_path = temp.path().join(".harn/flow.sqlite");
        fs::create_dir_all(store_path.parent().unwrap()).unwrap();

        {
            let store = SqliteFlowStore::open(&store_path, "test").unwrap();
            let principal = SigningKey::from_bytes(&[7; 32]);
            let persona = SigningKey::from_bytes(&[8; 32]);
            let atom = Atom::sign(
                vec![TextOp::Insert {
                    offset: 0,
                    content: "unsafe { /* SAFETY: fixture */ }".to_string(),
                }],
                Vec::new(),
                Provenance::new("user:test", "archivist-test", "run-1", "trace-1", "tx-1"),
                None,
                &principal,
                &persona,
            )
            .unwrap();
            store.emit_atoms(&[atom]).unwrap();
        }

        let proposal = rust_unsafe_proposal(temp.path(), "2026-04-26");
        let report = shadow_evaluate(temp.path(), &store_path, 30, &[proposal]).unwrap();
        assert_eq!(report["status"], "evaluated");
        assert_eq!(report["recent_atoms"], 1);
        assert_eq!(
            report["false_positive_candidates"][0]["transcript_ref"],
            "tx-1"
        );
        assert!(report["false_positive_candidates"][0]["atom"].is_string());
    }
}
