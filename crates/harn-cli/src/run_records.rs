//! Loading persisted run records and eval manifests off disk — including
//! sniffing which manifest shape a file holds — and rendering a run diff.

use crate::*;

pub(crate) fn load_run_record_or_exit(path: &Path) -> harn_vm::orchestration::RunRecord {
    match harn_vm::orchestration::load_run_record(path) {
        Ok(run) => run,
        Err(error) => {
            eprintln!("Failed to load run record: {error}");
            process::exit(1);
        }
    }
}

pub(crate) fn load_eval_suite_manifest_or_exit(
    path: &Path,
) -> harn_vm::orchestration::EvalSuiteManifest {
    harn_vm::orchestration::load_eval_suite_manifest(path).unwrap_or_else(|error| {
        eprintln!("Failed to load eval manifest {}: {error}", path.display());
        process::exit(1);
    })
}

pub(crate) fn load_eval_pack_manifest_or_exit(
    path: &Path,
) -> harn_vm::orchestration::EvalPackManifest {
    harn_vm::orchestration::load_eval_pack_manifest(path).unwrap_or_else(|error| {
        eprintln!("Failed to load eval pack {}: {error}", path.display());
        process::exit(1);
    })
}

pub(crate) fn load_persona_eval_ladder_manifest_or_exit(
    path: &Path,
) -> harn_vm::orchestration::PersonaEvalLadderManifest {
    harn_vm::orchestration::load_persona_eval_ladder_manifest(path).unwrap_or_else(|error| {
        eprintln!(
            "Failed to load persona eval ladder {}: {error}",
            path.display()
        );
        process::exit(1);
    })
}

pub(crate) fn file_looks_like_eval_manifest(path: &Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) == Some("harn.eval.toml") {
        return true;
    }
    if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        let Ok(content) = fs::read_to_string(path) else {
            return false;
        };
        return toml::from_str::<harn_vm::orchestration::EvalPackManifest>(&content)
            .is_ok_and(|manifest| !manifest.cases.is_empty() || !manifest.ladders.is_empty());
    }
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    json.get("_type").and_then(|value| value.as_str()) == Some("eval_suite_manifest")
        || json.get("cases").is_some()
}

pub(crate) fn file_looks_like_eval_pack_manifest(path: &Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) == Some("harn.eval.toml") {
        return true;
    }
    if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        return file_looks_like_eval_manifest(path);
    }
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    json.get("version").is_some()
        && (json.get("cases").is_some() || json.get("ladders").is_some())
        && json.get("_type").and_then(|value| value.as_str()) != Some("eval_suite_manifest")
}

pub(crate) fn file_looks_like_persona_eval_ladder_manifest(path: &Path) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
            return false;
        };
        return json.get("_type").and_then(|value| value.as_str())
            == Some("persona_eval_ladder_manifest")
            || json.get("timeout_tiers").is_some()
            || json.get("timeout-tiers").is_some();
    }
    toml::from_str::<harn_vm::orchestration::PersonaEvalLadderManifest>(&content).is_ok_and(
        |manifest| {
            manifest
                .type_name
                .eq_ignore_ascii_case("persona_eval_ladder_manifest")
                || (!manifest.timeout_tiers.is_empty() && manifest.backend.path.is_some())
        },
    )
}

pub(crate) fn collect_run_record_paths(path: &str) -> Vec<PathBuf> {
    let path = Path::new(path);
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .unwrap_or_else(|error| {
                eprintln!("Failed to read run directory {}: {error}", path.display());
                process::exit(1);
            })
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|entry| entry.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect();
        entries.sort();
        return entries;
    }
    eprintln!("Run path does not exist: {}", path.display());
    process::exit(1);
}

pub(crate) fn print_run_diff(diff: &harn_vm::orchestration::RunDiffReport) {
    println!(
        "Diff: {} -> {} [{} -> {}]",
        diff.left_run_id, diff.right_run_id, diff.left_status, diff.right_status
    );
    println!("Identical: {}", diff.identical);
    println!("Stage diffs: {}", diff.stage_diffs.len());
    println!("Tool diffs: {}", diff.tool_diffs.len());
    println!("Observability diffs: {}", diff.observability_diffs.len());
    println!("Transition delta: {}", diff.transition_count_delta);
    println!("Artifact delta: {}", diff.artifact_count_delta);
    println!("Checkpoint delta: {}", diff.checkpoint_count_delta);
    for stage in &diff.stage_diffs {
        println!("- {} [{}]", stage.node_id, stage.change);
        for detail in &stage.details {
            println!("  {detail}");
        }
    }
    for tool in &diff.tool_diffs {
        println!("- tool {} [{}]", tool.tool_name, tool.args_hash);
        println!("  left: {:?}", tool.left_result);
        println!("  right: {:?}", tool.right_result);
    }
    for item in &diff.observability_diffs {
        println!("- {} [{}]", item.label, item.section);
        for detail in &item.details {
            println!("  {detail}");
        }
    }
}

pub(crate) fn inspect_run_record(path: &str, compare: Option<&str>) {
    let run = load_run_record_or_exit(Path::new(path));
    println!("Run: {}", run.id);
    println!(
        "Workflow: {}",
        run.workflow_name
            .clone()
            .unwrap_or_else(|| run.workflow_id.clone())
    );
    println!("Status: {}", run.status);
    println!("Task: {}", run.task);
    println!("Stages: {}", run.stages.len());
    println!("Artifacts: {}", run.artifacts.len());
    println!("Transitions: {}", run.transitions.len());
    println!("Checkpoints: {}", run.checkpoints.len());
    println!("HITL questions: {}", run.hitl_questions.len());
    if let Some(observability) = &run.observability {
        println!("Planner rounds: {}", observability.planner_rounds.len());
        println!("Research facts: {}", observability.research_fact_count);
        println!("Workers: {}", observability.worker_lineage.len());
        println!(
            "Action graph: {} nodes / {} edges",
            observability.action_graph_nodes.len(),
            observability.action_graph_edges.len()
        );
        println!(
            "Transcript pointers: {}",
            observability.transcript_pointers.len()
        );
        println!("Daemon events: {}", observability.daemon_events.len());
    }
    if let Some(parent_worker_id) = run
        .metadata
        .get("parent_worker_id")
        .and_then(|value| value.as_str())
    {
        println!("Parent worker: {parent_worker_id}");
    }
    if let Some(parent_stage_id) = run
        .metadata
        .get("parent_stage_id")
        .and_then(|value| value.as_str())
    {
        println!("Parent stage: {parent_stage_id}");
    }
    if run
        .metadata
        .get("delegated")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        println!("Delegated: true");
    }
    println!(
        "Pending nodes: {}",
        if run.pending_nodes.is_empty() {
            "-".to_string()
        } else {
            run.pending_nodes.join(", ")
        }
    );
    println!(
        "Replay fixture: {}",
        if run.replay_fixture.is_some() {
            "embedded"
        } else {
            "derived"
        }
    );
    for stage in &run.stages {
        let worker = stage.metadata.get("worker");
        let worker_suffix = worker
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            .map(|name| format!(" worker={name}"))
            .unwrap_or_default();
        println!(
            "- {} [{}] status={} outcome={} branch={}{}",
            stage.node_id,
            stage.kind,
            stage.status,
            stage.outcome,
            stage.branch.clone().unwrap_or_else(|| "-".to_string()),
            worker_suffix,
        );
        if let Some(worker) = worker {
            if let Some(worker_id) = worker.get("id").and_then(|value| value.as_str()) {
                println!("  worker_id: {worker_id}");
            }
            if let Some(child_run_id) = worker.get("child_run_id").and_then(|value| value.as_str())
            {
                println!("  child_run_id: {child_run_id}");
            }
            if let Some(child_run_path) = worker
                .get("child_run_path")
                .and_then(|value| value.as_str())
            {
                println!("  child_run_path: {child_run_path}");
            }
        }
    }
    if let Some(observability) = &run.observability {
        for round in &observability.planner_rounds {
            println!(
                "- planner {} iterations={} llm_calls={} tools={} research_facts={}",
                round.node_id,
                round.iteration_count,
                round.llm_call_count,
                round.tool_execution_count,
                round.research_facts.len()
            );
        }
        for pointer in &observability.transcript_pointers {
            println!(
                "- transcript {} [{}] available={} {}",
                pointer.label,
                pointer.kind,
                pointer.available,
                pointer
                    .path
                    .clone()
                    .unwrap_or_else(|| pointer.location.clone())
            );
        }
        for event in &observability.daemon_events {
            println!(
                "- daemon {} [{:?}] at {}",
                event.name, event.kind, event.timestamp
            );
            println!("  id: {}", event.daemon_id);
            println!("  persist_path: {}", event.persist_path);
            if let Some(summary) = &event.payload_summary {
                println!("  payload: {summary}");
            }
        }
    }
    if let Some(compare_path) = compare {
        let baseline = load_run_record_or_exit(Path::new(compare_path));
        print_run_diff(&harn_vm::orchestration::diff_run_records(&baseline, &run));
    }
}
