//! Merge Captain end-to-end driver surface (#1019).
//!
//! The driver is intentionally thin: it resolves a backend-specific scenario
//! to the canonical JSONL transcript envelope, runs the same Merge Captain
//! oracle used by `harn merge-captain audit`, and writes a deterministic
//! receipt plus run summary. Backend adapters can later replace the
//! transcript resolver with live connector execution without changing the
//! artifact contract.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent_events::{AgentEvent, PersistedAgentEvent};
use crate::value::VmError;

use super::{
    audit_transcript, load_merge_captain_golden, load_transcript_jsonl, AuditReport,
    LoadedTranscript, MergeCaptainGolden, StateTransition,
};

const RECEIPT_TYPE: &str = "merge_captain_run_receipt";
const SUMMARY_TYPE: &str = "merge_captain_run_summary";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeCaptainDriverBackend {
    Live,
    Mock { playground_dir: PathBuf },
    Replay { fixture: PathBuf },
}

impl MergeCaptainDriverBackend {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Mock { .. } => "mock",
            Self::Replay { .. } => "replay",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeCaptainDriverMode {
    Once,
    Watch,
}

#[derive(Clone, Debug)]
pub struct MergeCaptainDriverOptions {
    pub backend: MergeCaptainDriverBackend,
    pub mode: MergeCaptainDriverMode,
    pub model_route: Option<String>,
    pub timeout_tier: Option<String>,
    pub transcript_out: Option<PathBuf>,
    pub receipt_out: Option<PathBuf>,
    pub run_root: PathBuf,
    pub max_sweeps: u32,
    pub watch_backoff_ms: u64,
    pub stream_stdout: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeCaptainRunReceipt {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub persona: String,
    pub run_id: String,
    pub scenario: Option<String>,
    pub mode: MergeCaptainDriverMode,
    pub model_route: Option<String>,
    pub timeout_tier: Option<String>,
    pub sweeps: u32,
    pub event_count: u64,
    pub model_calls: u64,
    pub tool_calls: u64,
    pub approvals_requested: u64,
    pub unsafe_action_attempts: u64,
    pub prs_touched: Vec<MergeCaptainPrTouch>,
    pub state_transitions: Vec<StateTransition>,
    pub oracle_error_findings: u64,
    pub oracle_warn_findings: u64,
    pub pass: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MergeCaptainPrTouch {
    pub repo: String,
    pub pr_number: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeCaptainRunSummary {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub version: u32,
    pub run_id: String,
    pub backend: String,
    pub backend_source: Option<String>,
    pub scenario: Option<String>,
    pub mode: MergeCaptainDriverMode,
    pub sweeps: u32,
    pub transcript_path: Option<String>,
    pub receipt_path: String,
    pub event_count: u64,
    pub prs_touched: Vec<MergeCaptainPrTouch>,
    pub state_transitions: Vec<StateTransition>,
    pub approvals_requested: u64,
    pub model_calls: u64,
    pub tool_calls: u64,
    pub cost_usd: f64,
    pub oracle_findings: usize,
    pub oracle_error_findings: usize,
    pub oracle_warn_findings: usize,
    pub pass: bool,
}

#[derive(Clone, Debug)]
pub struct MergeCaptainDriverOutput {
    pub summary: MergeCaptainRunSummary,
    pub receipt_path: PathBuf,
    pub transcript_path: Option<PathBuf>,
    pub audit_report: AuditReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct MockScenarioManifest {
    #[serde(rename = "_type")]
    type_name: String,
    scenario: Option<String>,
    transcript: PathBuf,
    golden: Option<PathBuf>,
}

impl Default for MockScenarioManifest {
    fn default() -> Self {
        Self {
            type_name: "merge_captain_mock_scenario".to_string(),
            scenario: None,
            transcript: PathBuf::new(),
            golden: None,
        }
    }
}

#[derive(Clone, Debug)]
struct ResolvedScenario {
    events: Vec<PersistedAgentEvent>,
    golden: Option<MergeCaptainGolden>,
    scenario: Option<String>,
    backend_source: Option<String>,
}

pub fn run_merge_captain_driver(
    options: MergeCaptainDriverOptions,
) -> Result<MergeCaptainDriverOutput, VmError> {
    let mut resolved = resolve_backend(&options.backend)?;
    expand_watch_events(&options, &mut resolved.events);
    if resolved.scenario.is_none() {
        resolved.scenario = resolved
            .golden
            .as_ref()
            .map(|golden| golden.scenario.clone());
    }

    let audit_report = audit_transcript(&resolved.events, resolved.golden.as_ref());
    let run_id = deterministic_run_id(&resolved.events)?;
    let run_dir = options.run_root.join("merge-captain").join(&run_id);
    fs::create_dir_all(&run_dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to create merge-captain run directory {}: {error}",
            run_dir.display()
        ))
    })?;

    let transcript_path = write_transcript(&options, &run_dir, &resolved.events)?;
    let receipt_path = options
        .receipt_out
        .clone()
        .unwrap_or_else(|| run_dir.join("receipt.json"));
    let receipt = build_receipt(&options, &run_id, &resolved, &audit_report);
    write_json_file(&receipt_path, &receipt)?;

    let summary = build_summary(
        &options,
        &run_id,
        &resolved,
        &audit_report,
        &receipt_path,
        transcript_path.as_deref(),
        &receipt,
    );

    Ok(MergeCaptainDriverOutput {
        summary,
        receipt_path,
        transcript_path,
        audit_report,
    })
}

fn resolve_backend(backend: &MergeCaptainDriverBackend) -> Result<ResolvedScenario, VmError> {
    match backend {
        MergeCaptainDriverBackend::Live => Err(VmError::Runtime(
            "merge-captain live backend requires the production connector runtime; use --backend mock <dir> or --backend replay <fixture> in this checkout".to_string(),
        )),
        MergeCaptainDriverBackend::Replay { fixture } => {
            let loaded = load_transcript_jsonl(fixture)?;
            let golden = load_replay_golden_if_present(fixture)?;
            Ok(ResolvedScenario {
                events: loaded.events,
                golden,
                scenario: None,
                backend_source: Some(fixture.display().to_string()),
            })
        }
        MergeCaptainDriverBackend::Mock { playground_dir } => {
            // Prefer the on-disk playground (presence of `playground.json`)
            // — that's the #1020 surface with real bare git repos. Fall
            // back to the legacy transcript-replay manifest for backwards
            // compatibility with examples/merge_captain/playground_3repos.
            if super::playground::playground_marker_path(playground_dir).exists() {
                let (state, manifest) = super::playground::load_playground(playground_dir)?;
                let events = super::playground::synthesize_sweep(
                    &state,
                    &super::playground::TranscriptOptions::default(),
                );
                let golden = load_playground_golden_if_present(playground_dir)?;
                return Ok(ResolvedScenario {
                    events,
                    golden,
                    scenario: Some(manifest.scenario),
                    backend_source: Some(playground_dir.display().to_string()),
                });
            }
            let manifest_path = find_mock_manifest(playground_dir)?;
            let bytes = fs::read(&manifest_path).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to read mock scenario manifest {}: {error}",
                    manifest_path.display()
                ))
            })?;
            let manifest: MockScenarioManifest = serde_json::from_slice(&bytes).map_err(|error| {
                VmError::Runtime(format!(
                    "failed to parse mock scenario manifest {}: {error}",
                    manifest_path.display()
                ))
            })?;
            if manifest.transcript.as_os_str().is_empty() {
                return Err(VmError::Runtime(format!(
                    "mock scenario manifest {} must set transcript",
                    manifest_path.display()
                )));
            }
            let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
            let transcript = resolve_relative(base, &manifest.transcript);
            let LoadedTranscript { events, .. } = load_transcript_jsonl(&transcript)?;
            let golden = match manifest.golden {
                Some(path) => Some(load_merge_captain_golden(&resolve_relative(base, &path))?),
                None => None,
            };
            Ok(ResolvedScenario {
                events,
                golden,
                scenario: manifest.scenario,
                backend_source: Some(playground_dir.display().to_string()),
            })
        }
    }
}

fn load_playground_golden_if_present(
    playground_dir: &Path,
) -> Result<Option<MergeCaptainGolden>, VmError> {
    let candidate = playground_dir.join("golden.json");
    if candidate.exists() {
        return load_merge_captain_golden(&candidate).map(Some);
    }
    Ok(None)
}

fn find_mock_manifest(playground_dir: &Path) -> Result<PathBuf, VmError> {
    if playground_dir.is_file() {
        return Ok(playground_dir.to_path_buf());
    }
    let candidates = [
        playground_dir.join("merge_captain.scenario.json"),
        playground_dir.join("scenario.json"),
        playground_dir.join("harn.merge-captain.json"),
    ];
    candidates
        .into_iter()
        .find(|path| path.exists())
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "mock backend expected a scenario manifest at {}/merge_captain.scenario.json",
                playground_dir.display()
            ))
        })
}

fn resolve_relative(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn load_replay_golden_if_present(fixture: &Path) -> Result<Option<MergeCaptainGolden>, VmError> {
    let Some(stem) = fixture.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(None);
    };
    let mut candidates = Vec::new();
    if let Some(parent) = fixture.parent() {
        candidates.push(parent.join(format!("{stem}.golden.json")));
        if parent.file_name().and_then(|name| name.to_str()) == Some("transcripts") {
            if let Some(root) = parent.parent() {
                candidates.push(root.join("goldens").join(format!("{stem}.json")));
            }
        }
    }
    for candidate in candidates {
        if candidate.exists() {
            return load_merge_captain_golden(&candidate).map(Some);
        }
    }
    Ok(None)
}

fn write_transcript(
    options: &MergeCaptainDriverOptions,
    run_dir: &Path,
    events: &[PersistedAgentEvent],
) -> Result<Option<PathBuf>, VmError> {
    let mut line_buffer = Vec::new();
    for event in events {
        serde_json::to_writer(&mut line_buffer, event).map_err(|error| {
            VmError::Runtime(format!("failed to serialize transcript event: {error}"))
        })?;
        line_buffer.push(b'\n');
    }

    if options.stream_stdout {
        io::stdout().write_all(&line_buffer).map_err(|error| {
            VmError::Runtime(format!(
                "failed to stream merge-captain JSONL to stdout: {error}"
            ))
        })?;
    }

    let transcript_path = options
        .transcript_out
        .clone()
        .or_else(|| Some(run_dir.join("event_log.jsonl")));
    if let Some(path) = &transcript_path {
        write_bytes_file(path, &line_buffer)?;
    }
    Ok(transcript_path)
}

fn build_receipt(
    options: &MergeCaptainDriverOptions,
    run_id: &str,
    resolved: &ResolvedScenario,
    report: &AuditReport,
) -> MergeCaptainRunReceipt {
    let stats = collect_stats(&resolved.events);
    MergeCaptainRunReceipt {
        type_name: RECEIPT_TYPE.to_string(),
        version: 1,
        persona: "merge_captain".to_string(),
        run_id: run_id.to_string(),
        scenario: resolved
            .golden
            .as_ref()
            .map(|golden| golden.scenario.clone())
            .or_else(|| report.scenario.clone())
            .or_else(|| resolved.scenario.clone()),
        mode: options.mode.clone(),
        model_route: options.model_route.clone(),
        timeout_tier: options.timeout_tier.clone(),
        sweeps: sweep_count(options),
        event_count: report.event_count,
        model_calls: report.model_call_count,
        tool_calls: report.tool_call_count,
        approvals_requested: stats.approvals_requested,
        unsafe_action_attempts: stats.unsafe_action_attempts,
        prs_touched: stats.prs_touched,
        state_transitions: report.state_transitions.clone(),
        oracle_error_findings: report.error_findings() as u64,
        oracle_warn_findings: report.warn_findings() as u64,
        pass: report.pass,
    }
}

fn build_summary(
    options: &MergeCaptainDriverOptions,
    run_id: &str,
    resolved: &ResolvedScenario,
    report: &AuditReport,
    receipt_path: &Path,
    transcript_path: Option<&Path>,
    receipt: &MergeCaptainRunReceipt,
) -> MergeCaptainRunSummary {
    MergeCaptainRunSummary {
        type_name: SUMMARY_TYPE.to_string(),
        version: 1,
        run_id: run_id.to_string(),
        backend: options.backend.kind().to_string(),
        backend_source: resolved.backend_source.clone(),
        scenario: receipt.scenario.clone(),
        mode: options.mode.clone(),
        sweeps: receipt.sweeps,
        transcript_path: transcript_path.map(|path| path.display().to_string()),
        receipt_path: receipt_path.display().to_string(),
        event_count: report.event_count,
        prs_touched: receipt.prs_touched.clone(),
        state_transitions: report.state_transitions.clone(),
        approvals_requested: receipt.approvals_requested,
        model_calls: report.model_call_count,
        tool_calls: report.tool_call_count,
        cost_usd: 0.0,
        oracle_findings: report.findings.len(),
        oracle_error_findings: report.error_findings(),
        oracle_warn_findings: report.warn_findings(),
        pass: report.pass,
    }
}

#[derive(Default)]
struct TranscriptStats {
    approvals_requested: u64,
    unsafe_action_attempts: u64,
    prs_touched: Vec<MergeCaptainPrTouch>,
}

fn collect_stats(events: &[PersistedAgentEvent]) -> TranscriptStats {
    let mut stats = TranscriptStats::default();
    let mut prs = BTreeSet::new();

    for env in events {
        match &env.event {
            AgentEvent::Plan { plan, .. } => {
                if plan
                    .get("approval_required")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
                {
                    stats.approvals_requested += 1;
                }
                insert_pr_touch(plan, &mut prs);
            }
            AgentEvent::ToolCall {
                tool_name,
                raw_input,
                ..
            } => {
                if looks_like_unsafe_action(tool_name) {
                    stats.unsafe_action_attempts += 1;
                }
                insert_pr_touch(raw_input, &mut prs);
            }
            AgentEvent::ToolCallUpdate {
                raw_output: Some(output),
                ..
            } => {
                insert_pr_touch(output, &mut prs);
            }
            _ => {}
        }
    }

    stats.prs_touched = prs.into_iter().collect();
    stats
}

fn insert_pr_touch(value: &serde_json::Value, prs: &mut BTreeSet<MergeCaptainPrTouch>) {
    let repo = value
        .get("repo")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let pr_number = value
        .get("pr_number")
        .or_else(|| value.get("number"))
        .and_then(|value| value.as_u64());
    if let (Some(repo), Some(pr_number)) = (repo, pr_number) {
        prs.insert(MergeCaptainPrTouch { repo, pr_number });
    }
}

fn looks_like_unsafe_action(tool_name: &str) -> bool {
    let lower = tool_name.to_lowercase();
    lower.contains("merge")
        || lower.contains("approve")
        || lower.contains("force_push")
        || lower.contains("delete")
        || lower.contains("write")
        || lower.contains("post_comment")
        || lower.contains("set_label")
}

fn sweep_count(options: &MergeCaptainDriverOptions) -> u32 {
    match options.mode {
        MergeCaptainDriverMode::Once => 1,
        MergeCaptainDriverMode::Watch => options.max_sweeps.max(1),
    }
}

fn expand_watch_events(options: &MergeCaptainDriverOptions, events: &mut Vec<PersistedAgentEvent>) {
    if options.mode != MergeCaptainDriverMode::Watch {
        return;
    }
    let sweeps = options.max_sweeps.max(1);
    if sweeps <= 1 || events.is_empty() {
        return;
    }
    let template = events.clone();
    let stride = template
        .iter()
        .map(|event| event.index)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let time_stride = template
        .last()
        .and_then(|last| {
            template
                .first()
                .map(|first| last.emitted_at_ms - first.emitted_at_ms)
        })
        .unwrap_or(0)
        .max(1);
    for sweep in 1..sweeps {
        if options.watch_backoff_ms > 0 {
            thread::sleep(Duration::from_millis(options.watch_backoff_ms));
        }
        let index_offset = stride.saturating_mul(sweep as u64);
        let time_offset = time_stride.saturating_mul(sweep as i64);
        for event in &template {
            let mut next = event.clone();
            next.index = next.index.saturating_add(index_offset);
            next.emitted_at_ms = next.emitted_at_ms.saturating_add(time_offset);
            events.push(next);
        }
    }
}

fn deterministic_run_id(events: &[PersistedAgentEvent]) -> Result<String, VmError> {
    let mut hasher = Sha256::new();
    for event in events {
        let bytes = serde_json::to_vec(event).map_err(|error| {
            VmError::Runtime(format!("failed to hash merge-captain transcript: {error}"))
        })?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    let digest = hasher.finalize();
    let mut suffix = String::with_capacity(16);
    for byte in &digest[..8] {
        suffix.push_str(&format!("{byte:02x}"));
    }
    Ok(format!("merge-captain-{suffix}"))
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), VmError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| VmError::Runtime(format!("failed to serialize JSON artifact: {error}")))?;
    bytes.push(b'\n');
    write_bytes_file(path, &bytes)
}

fn write_bytes_file(path: &Path, bytes: &[u8]) -> Result<(), VmError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "failed to create artifact directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    fs::write(path, bytes).map_err(|error| {
        VmError::Runtime(format!(
            "failed to write artifact {}: {error}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn mock_backend_resolves_manifest_and_builds_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let output = run_merge_captain_driver(MergeCaptainDriverOptions {
            backend: MergeCaptainDriverBackend::Mock {
                playground_dir: repo_root().join("examples/merge_captain/playground_3repos"),
            },
            mode: MergeCaptainDriverMode::Once,
            model_route: Some("mock/value".to_string()),
            timeout_tier: Some("smoke".to_string()),
            transcript_out: Some(temp.path().join("event_log.jsonl")),
            receipt_out: Some(temp.path().join("receipt.json")),
            run_root: temp.path().join("runs"),
            max_sweeps: 1,
            watch_backoff_ms: 0,
            stream_stdout: false,
        })
        .unwrap();

        assert!(output.summary.pass);
        assert_eq!(output.summary.backend, "mock");
        assert_eq!(output.summary.scenario.as_deref(), Some("green_pr"));
        assert!(!output.summary.prs_touched.is_empty());
        assert!(output.receipt_path.exists());
        assert!(output.transcript_path.unwrap().exists());
    }

    #[test]
    fn mock_backend_drives_real_on_disk_playground() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("(skipping — git not on PATH)");
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let playground = temp.path().join("pg");
        let manifest = super::super::playground::load_builtin("single_green").unwrap();
        super::super::playground::init_playground_at(super::super::playground::InitOptions {
            dir: &playground,
            manifest: &manifest,
            allow_existing: false,
        })
        .unwrap();
        let output = run_merge_captain_driver(MergeCaptainDriverOptions {
            backend: MergeCaptainDriverBackend::Mock {
                playground_dir: playground.clone(),
            },
            mode: MergeCaptainDriverMode::Once,
            model_route: None,
            timeout_tier: None,
            transcript_out: Some(temp.path().join("event_log.jsonl")),
            receipt_out: Some(temp.path().join("receipt.json")),
            run_root: temp.path().join("runs"),
            max_sweeps: 1,
            watch_backoff_ms: 0,
            stream_stdout: false,
        })
        .unwrap();
        assert_eq!(output.summary.backend, "mock");
        assert_eq!(output.summary.scenario.as_deref(), Some("single_green"));
        assert!(output.summary.event_count > 0);
        // PR coverage gets populated from the synthesized transcript.
        assert!(!output.summary.prs_touched.is_empty());
    }

    #[test]
    fn replay_backend_surfaces_oracle_failure() {
        let temp = tempfile::tempdir().unwrap();
        let fixture =
            repo_root().join("examples/personas/merge_captain/transcripts/bad_unsafe_merge.jsonl");
        let output = run_merge_captain_driver(MergeCaptainDriverOptions {
            backend: MergeCaptainDriverBackend::Replay { fixture },
            mode: MergeCaptainDriverMode::Once,
            model_route: None,
            timeout_tier: None,
            transcript_out: Some(temp.path().join("event_log.jsonl")),
            receipt_out: Some(temp.path().join("receipt.json")),
            run_root: temp.path().join("runs"),
            max_sweeps: 1,
            watch_backoff_ms: 0,
            stream_stdout: false,
        })
        .unwrap();

        assert!(!output.summary.pass);
        assert!(output.summary.oracle_error_findings > 0);
    }

    #[test]
    fn mock_and_replay_receipts_match_for_same_transcript() {
        let temp = tempfile::tempdir().unwrap();
        let mock_receipt = temp.path().join("mock-receipt.json");
        let replay_receipt = temp.path().join("replay-receipt.json");
        let common_model_route = Some("mock/value".to_string());
        let common_timeout_tier = Some("smoke".to_string());

        run_merge_captain_driver(MergeCaptainDriverOptions {
            backend: MergeCaptainDriverBackend::Mock {
                playground_dir: repo_root().join("examples/merge_captain/playground_3repos"),
            },
            mode: MergeCaptainDriverMode::Once,
            model_route: common_model_route.clone(),
            timeout_tier: common_timeout_tier.clone(),
            transcript_out: Some(temp.path().join("mock-event_log.jsonl")),
            receipt_out: Some(mock_receipt.clone()),
            run_root: temp.path().join("runs"),
            max_sweeps: 1,
            watch_backoff_ms: 0,
            stream_stdout: false,
        })
        .unwrap();

        run_merge_captain_driver(MergeCaptainDriverOptions {
            backend: MergeCaptainDriverBackend::Replay {
                fixture: repo_root()
                    .join("examples/personas/merge_captain/transcripts/green_pr.jsonl"),
            },
            mode: MergeCaptainDriverMode::Once,
            model_route: common_model_route,
            timeout_tier: common_timeout_tier,
            transcript_out: Some(temp.path().join("replay-event_log.jsonl")),
            receipt_out: Some(replay_receipt.clone()),
            run_root: temp.path().join("runs"),
            max_sweeps: 1,
            watch_backoff_ms: 0,
            stream_stdout: false,
        })
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(mock_receipt).unwrap(),
            std::fs::read_to_string(replay_receipt).unwrap()
        );
    }
}
