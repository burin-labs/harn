use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use axum::http::StatusCode;
use axum::Json;

use super::dto::{
    PortalActivity, PortalArtifact, PortalCheckpoint, PortalChildRun, PortalExecutionSummary,
    PortalInsight, PortalPolicySummary, PortalReplayAssertion, PortalReplaySummary,
    PortalRunDetail, PortalRunSummary, PortalSpan, PortalStage, PortalStageDebug, PortalStats,
    PortalStorySection, PortalTranscriptStep, PortalTransition,
};
use super::errors::{bad_request_error, internal_error};
use super::query::{ErrorResponse, ListRunsQuery};
use super::skill_events::{extract as extract_skill_events, ExtractedEvents};
use super::transcript::{build_story, discover_transcript_steps};
use super::util::{
    compact_json, compact_metadata, date_ms, format_duration, humanize_kind, is_completed_status,
    is_failed_status, metadata_pretty_json, metadata_string, owning_stage, preview_text,
    span_kind_totals, string_array_value, system_time_ms,
};

pub(super) fn scan_runs(run_dir: &Path) -> Result<Vec<PortalRunSummary>, String> {
    let mut files = Vec::new();
    collect_run_files(run_dir, run_dir, &mut files)?;

    let mut runs = Vec::new();
    for (path, modified_at_ms) in files {
        if let Ok(run) = harn_vm::orchestration::load_run_record(&path) {
            if run.type_name != "run_record" || run.id.is_empty() || run.workflow_id.is_empty() {
                continue;
            }
            let relative = path
                .strip_prefix(run_dir)
                .ok()
                .unwrap_or(&path)
                .display()
                .to_string();
            runs.push(build_run_summary(&relative, modified_at_ms, &run));
        }
    }

    runs.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.updated_at_ms.cmp(&a.updated_at_ms))
    });
    Ok(runs)
}

pub(super) fn filter_and_sort_runs(
    runs: Vec<PortalRunSummary>,
    query: &ListRunsQuery,
) -> Vec<PortalRunSummary> {
    let search = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let workflow = query
        .workflow
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let status = query.status.as_deref().unwrap_or("all");
    let sort = query.sort.as_deref().unwrap_or("newest");
    let skill_filter = query
        .skill
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let mut filtered = runs
        .into_iter()
        .filter(|run| match workflow.as_deref() {
            Some(name) => run.workflow_name == name,
            None => true,
        })
        .filter(|run| matches_run_status(run, status))
        .filter(|run| match skill_filter.as_deref() {
            Some(skill) => run.skills.iter().any(|s| s == skill),
            None => true,
        })
        .filter(|run| match search.as_deref() {
            Some(needle) => matches_run_search(run, needle),
            None => true,
        })
        .collect::<Vec<_>>();

    filtered.sort_by(|left, right| match sort {
        "duration" => right
            .duration_ms
            .unwrap_or_default()
            .cmp(&left.duration_ms.unwrap_or_default())
            .then_with(|| right.started_at.cmp(&left.started_at)),
        "oldest" => left
            .started_at
            .cmp(&right.started_at)
            .then_with(|| left.updated_at_ms.cmp(&right.updated_at_ms)),
        _ => right
            .started_at
            .cmp(&left.started_at)
            .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms)),
    });
    filtered
}

fn matches_run_status(run: &PortalRunSummary, status: &str) -> bool {
    match status {
        "failed" => is_failed_status(&run.status),
        "completed" => is_completed_status(&run.status),
        "active" => !is_failed_status(&run.status) && !is_completed_status(&run.status),
        _ => true,
    }
}

fn matches_run_search(run: &PortalRunSummary, needle: &str) -> bool {
    let models = run.models.join(" ");
    format!(
        "{} {} {} {} {} {}",
        run.workflow_name,
        run.status,
        run.path,
        run.last_stage_node_id.as_deref().unwrap_or_default(),
        run.failure_summary.as_deref().unwrap_or_default(),
        models
    )
    .to_ascii_lowercase()
    .contains(needle)
}

fn collect_run_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(PathBuf, u128)>,
) -> Result<(), String> {
    let entries = fs::read_dir(current)
        .map_err(|error| format!("failed to read {}: {error}", current.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to iterate {}: {error}", current.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_run_files(root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let modified_at_ms = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(system_time_ms)
            .unwrap_or_else(|| system_time_ms(SystemTime::now()).unwrap_or(0));
        if path.starts_with(root) {
            out.push((path, modified_at_ms));
        }
    }
    Ok(())
}

pub(super) fn resolve_run_path(
    root: &Path,
    relative: &str,
) -> Result<PathBuf, (StatusCode, Json<ErrorResponse>)> {
    if relative.trim().is_empty() {
        return Err(bad_request_error("run path is required"));
    }
    let relative_path = Path::new(relative);
    if relative_path.is_absolute() {
        return Err(bad_request_error(
            "run path must be relative to the configured run directory",
        ));
    }
    if relative_path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(bad_request_error(
            "run path must stay inside the configured run directory",
        ));
    }
    let joined = root.join(relative_path);
    if joined.exists() {
        let canonical_root = root
            .canonicalize()
            .map_err(|error| internal_error(format!("failed to resolve run directory: {error}")))?;
        let canonical_joined = joined
            .canonicalize()
            .map_err(|error| internal_error(format!("failed to resolve run path: {error}")))?;
        if !canonical_joined.starts_with(&canonical_root) {
            return Err(bad_request_error(
                "run path must stay inside the configured run directory",
            ));
        }
    }
    Ok(joined)
}

pub(super) fn build_run_summary(
    path: &str,
    updated_at_ms: u128,
    run: &harn_vm::orchestration::RunRecord,
) -> PortalRunSummary {
    let usage = run.usage.clone().unwrap_or_default();
    let (last_stage_node_id, failure_summary) = latest_stage_summary(run);
    let extracted = extract_skill_events(run);
    PortalRunSummary {
        path: path.to_string(),
        id: run.id.clone(),
        workflow_name: run
            .workflow_name
            .clone()
            .unwrap_or_else(|| run.workflow_id.clone()),
        status: run.status.clone(),
        last_stage_node_id,
        failure_summary,
        started_at: run.started_at.clone(),
        finished_at: run.finished_at.clone(),
        duration_ms: run_duration_ms(run),
        stage_count: run.stages.len(),
        child_run_count: run.child_runs.len(),
        call_count: usage.call_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        models: usage.models,
        updated_at_ms,
        skills: extracted.active_skills,
    }
}

fn latest_stage_summary(
    run: &harn_vm::orchestration::RunRecord,
) -> (Option<String>, Option<String>) {
    let last_stage = run.stages.last();
    let last_stage_node_id = last_stage.map(|stage| stage.node_id.clone());
    let failure_summary = run
        .stages
        .iter()
        .rev()
        .find(|stage| is_failed_status(&stage.status) || is_failed_status(&stage.outcome))
        .map(|stage| {
            let error = stage.metadata.get("error").map(compact_json).or_else(|| {
                stage
                    .attempts
                    .iter()
                    .rev()
                    .find_map(|attempt| attempt.error.clone())
            });
            match error {
                Some(error) if !error.is_empty() => format!("{} failed: {}", stage.node_id, error),
                _ => format!("{} failed with {}", stage.node_id, stage.outcome),
            }
        });
    (last_stage_node_id, failure_summary)
}

pub(super) fn summarize_runs(runs: &[PortalRunSummary]) -> PortalStats {
    let total_runs = runs.len();
    let completed_runs = runs
        .iter()
        .filter(|run| is_completed_status(&run.status))
        .count();
    let active_runs = runs
        .iter()
        .filter(|run| !is_completed_status(&run.status) && !is_failed_status(&run.status))
        .count();
    let failed_runs = runs
        .iter()
        .filter(|run| is_failed_status(&run.status))
        .count();
    let durations: Vec<u64> = runs.iter().filter_map(|run| run.duration_ms).collect();
    let avg_duration_ms = if durations.is_empty() {
        0
    } else {
        durations.iter().sum::<u64>() / durations.len() as u64
    };
    PortalStats {
        total_runs,
        completed_runs,
        active_runs,
        failed_runs,
        avg_duration_ms,
    }
}

pub(super) fn build_run_detail(
    run_dir: &Path,
    relative_path: &str,
    run: &harn_vm::orchestration::RunRecord,
) -> PortalRunDetail {
    let summary = build_run_summary(relative_path, 0, run);
    let spans = build_spans(run);
    let stages = build_stages(run);
    let activities = build_activities(&spans, &stages);
    let transitions = build_transitions(run);
    let checkpoints = build_checkpoints(run);
    let artifacts = build_artifacts(run);
    let transcript_steps = discover_transcript_steps(run_dir, relative_path).unwrap_or_default();
    let story = build_story(run);
    let child_runs = run
        .child_runs
        .iter()
        .map(|child| PortalChildRun {
            worker_name: child.worker_name.clone(),
            status: child.status.clone(),
            started_at: child.started_at.clone(),
            finished_at: child.finished_at.clone(),
            run_id: child.run_id.clone(),
            run_path: child.run_path.as_ref().and_then(|path| {
                PathBuf::from(path)
                    .strip_prefix(run_dir)
                    .ok()
                    .map(|value| value.display().to_string())
                    .or_else(|| Some(path.clone()))
            }),
            task: child.task.clone(),
        })
        .collect::<Vec<_>>();
    let insights = build_insights(run, &summary, &stages, &spans, &story, &transcript_steps);
    let ExtractedEvents {
        skill_timeline,
        skill_matches,
        tool_loads,
        active_skills,
    } = extract_skill_events(run);

    PortalRunDetail {
        summary,
        task: run.task.clone(),
        workflow_id: run.workflow_id.clone(),
        parent_run_id: run.parent_run_id.clone(),
        root_run_id: run.root_run_id.clone(),
        policy_summary: build_policy_summary(run),
        replay_summary: build_replay_summary(run.replay_fixture.as_ref()),
        execution: run.execution.clone(),
        insights,
        stages,
        spans,
        activities,
        transitions,
        checkpoints,
        artifacts,
        execution_summary: build_execution_summary(run.execution.as_ref()),
        transcript_steps,
        story,
        child_runs,
        observability: run.observability.clone().unwrap_or_else(|| {
            let run_path = run_dir.join(relative_path);
            harn_vm::orchestration::derive_run_observability(run, Some(&run_path))
        }),
        skill_timeline,
        skill_match_events: skill_matches,
        tool_load_events: tool_loads,
        active_skills,
    }
}

fn build_insights(
    run: &harn_vm::orchestration::RunRecord,
    summary: &PortalRunSummary,
    stages: &[PortalStage],
    spans: &[PortalSpan],
    story: &[PortalStorySection],
    transcript_steps: &[PortalTranscriptStep],
) -> Vec<PortalInsight> {
    let slowest_stage = stages
        .iter()
        .filter_map(|stage| stage.duration_ms.map(|duration| (stage, duration)))
        .max_by_key(|(_, duration)| *duration);
    let noisiest_kind = span_kind_totals(spans)
        .into_iter()
        .max_by_key(|(_, duration)| *duration);
    let top_story = story.first();
    let heaviest_turn = transcript_steps
        .iter()
        .max_by_key(|step| (step.total_messages, step.input_tokens.unwrap_or_default()));
    vec![
        PortalInsight {
            label: "Run shape".to_string(),
            value: format!(
                "{} stages • {} child runs",
                summary.stage_count, summary.child_run_count
            ),
            detail: format!(
                "status={} • {} transcript sections",
                run.status,
                story.len()
            ),
        },
        PortalInsight {
            label: "Slowest stage".to_string(),
            value: slowest_stage
                .map(|(stage, duration)| {
                    format!("{} ({})", stage.node_id, format_duration(duration))
                })
                .unwrap_or_else(|| "No timed stages".to_string()),
            detail: slowest_stage
                .map(|(stage, _)| {
                    format!(
                        "{} attempts • {} artifacts",
                        stage.attempt_count, stage.artifact_count
                    )
                })
                .unwrap_or_else(|| "Stage timing metadata was not available".to_string()),
        },
        PortalInsight {
            label: "Where time went".to_string(),
            value: noisiest_kind
                .as_ref()
                .map(|(kind, duration)| {
                    format!("{} ({})", humanize_kind(kind), format_duration(*duration))
                })
                .unwrap_or_else(|| "No trace spans".to_string()),
            detail: format!("{} spans captured", spans.len()),
        },
        PortalInsight {
            label: "First thing to read".to_string(),
            value: top_story
                .map(|section| section.title.clone())
                .unwrap_or_else(|| "No transcript sections".to_string()),
            detail: top_story
                .map(|section| section.preview.clone())
                .unwrap_or_else(|| {
                    "This run has trace data but no captured visible transcript".to_string()
                }),
        },
        PortalInsight {
            label: "Heaviest model turn".to_string(),
            value: heaviest_turn
                .map(|step| format!("Step {} ({})", step.call_index, step.total_messages))
                .unwrap_or_else(|| "No saved model transcript".to_string()),
            detail: heaviest_turn
                .map(|step| {
                    format!(
                        "kept {} • added {} • {}",
                        step.kept_messages, step.added_messages, step.summary
                    )
                })
                .unwrap_or_else(|| {
                    "Enable HARN_LLM_TRANSCRIPT_DIR to persist full model-turn detail".to_string()
                }),
        },
    ]
}

fn build_stages(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalStage> {
    run.stages
        .iter()
        .map(|stage| PortalStage {
            id: stage.id.clone(),
            node_id: stage.node_id.clone(),
            kind: stage.kind.clone(),
            status: stage.status.clone(),
            outcome: stage.outcome.clone(),
            branch: stage.branch.clone(),
            started_at: stage.started_at.clone(),
            finished_at: stage.finished_at.clone(),
            duration_ms: stage_duration_ms(stage),
            artifact_count: stage.artifacts.len(),
            attempt_count: stage.attempts.len(),
            verification_summary: stage.verification.as_ref().map(compact_json),
            debug: build_stage_debug(stage),
        })
        .collect()
}

fn build_spans(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalSpan> {
    let mut spans = run.trace_spans.clone();
    spans.sort_by(|a, b| {
        a.start_ms
            .cmp(&b.start_ms)
            .then_with(|| b.duration_ms.cmp(&a.duration_ms))
    });
    let by_id: HashMap<u64, harn_vm::orchestration::RunTraceSpanRecord> = spans
        .iter()
        .map(|span| (span.span_id, span.clone()))
        .collect();
    let mut lane_ends: Vec<u64> = Vec::new();

    spans
        .into_iter()
        .map(|span| {
            let depth = span_depth(&span, &by_id);
            let lane = match lane_ends.iter().position(|end| *end <= span.start_ms) {
                Some(index) => {
                    lane_ends[index] = span.start_ms + span.duration_ms;
                    index
                }
                None => {
                    lane_ends.push(span.start_ms + span.duration_ms);
                    lane_ends.len() - 1
                }
            };

            PortalSpan {
                span_id: span.span_id,
                parent_id: span.parent_id,
                kind: span.kind.clone(),
                name: span.name.clone(),
                start_ms: span.start_ms,
                duration_ms: span.duration_ms,
                end_ms: span.start_ms + span.duration_ms,
                label: span_label(&span),
                lane,
                depth,
                metadata: span.metadata,
            }
        })
        .collect()
}

fn build_activities(spans: &[PortalSpan], stages: &[PortalStage]) -> Vec<PortalActivity> {
    spans
        .iter()
        .filter(|span| span.kind != "pipeline")
        .map(|span| PortalActivity {
            label: span.label.clone(),
            kind: span.kind.clone(),
            started_offset_ms: span.start_ms,
            duration_ms: span.duration_ms,
            stage_node_id: owning_stage(span, stages).map(|stage| stage.node_id.clone()),
            call_id: span
                .metadata
                .get("call_id")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            summary: compact_metadata(&span.metadata),
        })
        .collect()
}

fn build_transitions(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalTransition> {
    run.transitions
        .iter()
        .map(|transition| PortalTransition {
            from_node_id: transition.from_node_id.clone(),
            to_node_id: transition.to_node_id.clone(),
            branch: transition.branch.clone(),
            consumed_count: transition.consumed_artifact_ids.len(),
            produced_count: transition.produced_artifact_ids.len(),
        })
        .collect()
}

fn build_checkpoints(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalCheckpoint> {
    run.checkpoints
        .iter()
        .map(|checkpoint| PortalCheckpoint {
            reason: checkpoint.reason.clone(),
            ready_count: checkpoint.ready_nodes.len(),
            completed_count: checkpoint.completed_nodes.len(),
            last_stage_id: checkpoint.last_stage_id.clone(),
        })
        .collect()
}

fn build_artifacts(run: &harn_vm::orchestration::RunRecord) -> Vec<PortalArtifact> {
    run.artifacts
        .iter()
        .map(|artifact| PortalArtifact {
            id: artifact.id.clone(),
            kind: artifact.kind.clone(),
            title: artifact
                .title
                .clone()
                .unwrap_or_else(|| artifact.kind.clone()),
            source: artifact.source.clone(),
            stage: artifact.stage.clone(),
            estimated_tokens: artifact.estimated_tokens,
            lineage_count: artifact.lineage.len(),
            preview: artifact
                .text
                .as_deref()
                .map(preview_text)
                .or_else(|| artifact.data.as_ref().map(compact_json))
                .unwrap_or_else(|| "No text body persisted".to_string()),
        })
        .collect()
}

fn build_execution_summary(
    execution: Option<&harn_vm::orchestration::RunExecutionRecord>,
) -> Option<PortalExecutionSummary> {
    execution.map(|execution| PortalExecutionSummary {
        cwd: execution.cwd.clone(),
        repo_path: execution.repo_path.clone(),
        worktree_path: execution.worktree_path.clone(),
        branch: execution.branch.clone(),
        adapter: execution.adapter.clone(),
    })
}

pub(super) fn build_policy_summary(run: &harn_vm::orchestration::RunRecord) -> PortalPolicySummary {
    let validation = run.metadata.get("validation").cloned().and_then(|value| {
        serde_json::from_value::<harn_vm::orchestration::WorkflowValidationReport>(value).ok()
    });
    let capabilities = run
        .policy
        .capabilities
        .iter()
        .flat_map(|(capability, ops)| {
            if ops.is_empty() {
                vec![capability.clone()]
            } else {
                ops.iter()
                    .map(|op| format!("{capability}.{op}"))
                    .collect::<Vec<_>>()
            }
        })
        .collect::<Vec<_>>();
    let tool_arg_constraints = run
        .policy
        .tool_arg_constraints
        .iter()
        .map(|constraint| {
            if constraint.arg_patterns.is_empty() {
                constraint.tool.clone()
            } else {
                format!(
                    "{} → {}",
                    constraint.tool,
                    constraint.arg_patterns.join(", ")
                )
            }
        })
        .collect::<Vec<_>>();

    PortalPolicySummary {
        tools: run.policy.tools.clone(),
        capabilities,
        workspace_roots: run.policy.workspace_roots.clone(),
        side_effect_level: run.policy.side_effect_level.clone(),
        recursion_limit: run.policy.recursion_limit,
        tool_arg_constraints,
        validation_valid: validation.as_ref().map(|report| report.valid),
        validation_errors: validation
            .as_ref()
            .map(|report| report.errors.clone())
            .unwrap_or_default(),
        validation_warnings: validation
            .as_ref()
            .map(|report| report.warnings.clone())
            .unwrap_or_default(),
        reachable_nodes: validation
            .as_ref()
            .map(|report| report.reachable_nodes.clone())
            .unwrap_or_default(),
    }
}

pub(super) fn build_replay_summary(
    replay: Option<&harn_vm::orchestration::ReplayFixture>,
) -> Option<PortalReplaySummary> {
    replay.map(|fixture| PortalReplaySummary {
        fixture_id: fixture.id.clone(),
        source_run_id: fixture.source_run_id.clone(),
        created_at: fixture.created_at.clone(),
        expected_status: fixture.expected_status.clone(),
        stage_assertions: fixture
            .stage_assertions
            .iter()
            .map(|stage| PortalReplayAssertion {
                node_id: stage.node_id.clone(),
                expected_status: stage.expected_status.clone(),
                expected_outcome: stage.expected_outcome.clone(),
                expected_branch: stage.expected_branch.clone(),
                required_artifact_kinds: stage.required_artifact_kinds.clone(),
                visible_text_contains: stage.visible_text_contains.clone(),
            })
            .collect(),
    })
}

fn build_stage_debug(stage: &harn_vm::orchestration::RunStageRecord) -> PortalStageDebug {
    let usage = stage.usage.clone().unwrap_or_default();
    PortalStageDebug {
        call_count: usage.call_count,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        consumed_artifact_ids: stage.consumed_artifact_ids.clone(),
        produced_artifact_ids: stage.produced_artifact_ids.clone(),
        selected_artifact_ids: string_array_value(&stage.metadata, "selected_artifact_ids"),
        worker_id: metadata_string(&stage.metadata, "worker_id"),
        error: metadata_pretty_json(&stage.metadata, "error"),
        model_policy: metadata_pretty_json(&stage.metadata, "model_policy"),
        auto_compact: metadata_pretty_json(&stage.metadata, "auto_compact"),
        output_visibility: metadata_string(&stage.metadata, "output_visibility"),
        context_policy: metadata_pretty_json(&stage.metadata, "context_policy"),
        retry_policy: metadata_pretty_json(&stage.metadata, "retry_policy"),
        capability_policy: metadata_pretty_json(&stage.metadata, "effective_capability_policy"),
        input_contract: metadata_pretty_json(&stage.metadata, "input_contract"),
        output_contract: metadata_pretty_json(&stage.metadata, "output_contract"),
        prompt: metadata_pretty_json(&stage.metadata, "prompt"),
        system_prompt: metadata_pretty_json(&stage.metadata, "system_prompt"),
        rendered_context: metadata_pretty_json(&stage.metadata, "rendered_context"),
    }
}

fn stage_duration_ms(stage: &harn_vm::orchestration::RunStageRecord) -> Option<u64> {
    if let Some(finished) = &stage.finished_at {
        let start = date_ms(&stage.started_at)?;
        let finish = date_ms(finished)?;
        return finish.checked_sub(start);
    }
    stage.usage.as_ref().and_then(|usage| {
        let duration = usage.total_duration_ms.max(0) as u64;
        if duration > 0 {
            Some(duration)
        } else {
            None
        }
    })
}

fn run_duration_ms(run: &harn_vm::orchestration::RunRecord) -> Option<u64> {
    if let Some(usage) = &run.usage {
        let duration = usage.total_duration_ms.max(0) as u64;
        if duration > 0 {
            return Some(duration);
        }
    }
    if let Some(max_end) = run
        .trace_spans
        .iter()
        .map(|span| span.start_ms + span.duration_ms)
        .max()
    {
        if max_end > 0 {
            return Some(max_end);
        }
    }
    let stage_total = run.stages.iter().filter_map(stage_duration_ms).sum::<u64>();
    if stage_total > 0 {
        return Some(stage_total);
    }
    if let Some(finished) = &run.finished_at {
        let start = date_ms(&run.started_at)?;
        let finish = date_ms(finished)?;
        return finish.checked_sub(start);
    }
    None
}

fn span_depth(
    span: &harn_vm::orchestration::RunTraceSpanRecord,
    by_id: &HashMap<u64, harn_vm::orchestration::RunTraceSpanRecord>,
) -> usize {
    let mut depth = 0usize;
    let mut cursor = span.parent_id.and_then(|id| by_id.get(&id));
    let mut seen = std::collections::HashSet::new();
    while let Some(parent) = cursor {
        if !seen.insert(parent.span_id) {
            break;
        }
        depth += 1;
        cursor = parent.parent_id.and_then(|id| by_id.get(&id));
    }
    depth
}

fn span_label(span: &harn_vm::orchestration::RunTraceSpanRecord) -> String {
    match span.kind.as_str() {
        "llm_call" => span
            .metadata
            .get("model")
            .and_then(|value| value.as_str())
            .map(|model| format!("Model • {model}"))
            .unwrap_or_else(|| "Model call".to_string()),
        "tool_call" => span
            .metadata
            .get("tool_name")
            .and_then(|value| value.as_str())
            .map(|tool| format!("Tool • {tool}"))
            .unwrap_or_else(|| format!("Tool • {}", span.name)),
        _ => span.name.replace('_', " "),
    }
}
