//! Verified descriptors for LLM transcript sidecars.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use super::types::{RunRecord, RunTranscriptArtifactDescriptor, RunTranscriptPointerRecord};

const DESCRIPTOR_SCHEMA_VERSION: &str = "harn.llm_transcript_artifact.v1";
const ARTIFACT_KIND: &str = "llm_jsonl";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmTranscriptDescriptorError {
    pub kind: &'static str,
    pub message: String,
}

impl LlmTranscriptDescriptorError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for LlmTranscriptDescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for LlmTranscriptDescriptorError {}

#[derive(Default)]
struct DescriptorScan {
    event_count: usize,
    first_event_type: Option<String>,
    first_event_id: Option<String>,
    last_event_type: Option<String>,
    last_event_id: Option<String>,
    complete: bool,
    terminal_status: Option<String>,
    session_id: Option<String>,
    effective_tool_format: Option<String>,
    tool_schema_hash: Option<String>,
}

pub fn describe_llm_transcript_sidecar(
    run: &RunRecord,
    run_record_path: &Path,
    transcript_path: &Path,
) -> Result<RunTranscriptArtifactDescriptor, LlmTranscriptDescriptorError> {
    let bytes = std::fs::read(transcript_path).map_err(|error| {
        LlmTranscriptDescriptorError::new(
            "unavailable",
            format!("failed to read {}: {error}", transcript_path.display()),
        )
    })?;
    let scan = scan_jsonl(&bytes, transcript_path)?;
    let sha256 = format!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    let relative_path = relative_to_run_record(run_record_path, transcript_path);
    Ok(RunTranscriptArtifactDescriptor {
        schema_version: DESCRIPTOR_SCHEMA_VERSION.to_string(),
        artifact_kind: ARTIFACT_KIND.to_string(),
        run_id: run.id.clone(),
        session_id: scan.session_id,
        path: transcript_path.to_string_lossy().into_owned(),
        relative_path,
        sha256,
        byte_len: bytes.len() as u64,
        event_count: scan.event_count,
        first_event_type: scan.first_event_type,
        first_event_id: scan.first_event_id,
        last_event_type: scan.last_event_type,
        last_event_id: scan.last_event_id,
        complete: scan.complete,
        terminal_status: scan.terminal_status,
        effective_tool_format: scan.effective_tool_format,
        tool_schema_hash: scan.tool_schema_hash,
    })
}

pub fn pointer_for_llm_transcript_sidecar(
    run: &RunRecord,
    run_record_path: &Path,
    transcript_path: &Path,
) -> RunTranscriptPointerRecord {
    match describe_llm_transcript_sidecar(run, run_record_path, transcript_path) {
        Ok(descriptor) => RunTranscriptPointerRecord {
            id: "run:llm_transcript".to_string(),
            label: "LLM transcript sidecar".to_string(),
            kind: ARTIFACT_KIND.to_string(),
            location: "run sidecar".to_string(),
            path: Some(transcript_path.to_string_lossy().into_owned()),
            available: true,
            verification_status: if descriptor.complete {
                "verified".to_string()
            } else {
                "incomplete".to_string()
            },
            verification_error: None,
            descriptor: Some(descriptor),
        },
        Err(error) => RunTranscriptPointerRecord {
            id: "run:llm_transcript".to_string(),
            label: "LLM transcript sidecar".to_string(),
            kind: ARTIFACT_KIND.to_string(),
            location: "run sidecar".to_string(),
            path: Some(transcript_path.to_string_lossy().into_owned()),
            available: false,
            verification_status: error.kind.to_string(),
            verification_error: Some(error.message),
            descriptor: None,
        },
    }
}

pub fn verified_llm_transcript_pointer_path(
    run: &RunRecord,
    run_record_path: &Path,
) -> Result<PathBuf, LlmTranscriptDescriptorError> {
    let pointer = run
        .observability
        .as_ref()
        .and_then(|observability| {
            observability
                .transcript_pointers
                .iter()
                .find(|pointer| pointer.kind == ARTIFACT_KIND)
        })
        .ok_or_else(|| {
            LlmTranscriptDescriptorError::new(
                "missing_descriptor",
                "no llm_jsonl transcript pointer",
            )
        })?;
    if pointer.verification_status != "verified" && pointer.verification_status != "incomplete" {
        return Err(LlmTranscriptDescriptorError::new(
            "unverified_descriptor",
            format!(
                "llm_jsonl transcript pointer status is {}",
                pointer.verification_status
            ),
        ));
    }
    let descriptor = pointer.descriptor.as_ref().ok_or_else(|| {
        LlmTranscriptDescriptorError::new(
            "missing_descriptor",
            "llm_jsonl pointer has no descriptor",
        )
    })?;
    validate_descriptor_identity(run, descriptor)?;
    let path = resolve_descriptor_path(run_record_path, descriptor)?;
    let actual = describe_llm_transcript_sidecar(run, run_record_path, &path)?;
    if actual.sha256 != descriptor.sha256 || actual.byte_len != descriptor.byte_len {
        return Err(LlmTranscriptDescriptorError::new(
            "digest_mismatch",
            format!("{} changed since descriptor was written", path.display()),
        ));
    }
    Ok(path)
}

fn validate_descriptor_identity(
    run: &RunRecord,
    descriptor: &RunTranscriptArtifactDescriptor,
) -> Result<(), LlmTranscriptDescriptorError> {
    if descriptor.schema_version != DESCRIPTOR_SCHEMA_VERSION {
        return Err(LlmTranscriptDescriptorError::new(
            "schema_mismatch",
            format!(
                "llm_jsonl descriptor schema is {}",
                descriptor.schema_version
            ),
        ));
    }
    if descriptor.artifact_kind != ARTIFACT_KIND {
        return Err(LlmTranscriptDescriptorError::new(
            "artifact_kind_mismatch",
            format!(
                "llm_jsonl descriptor artifact kind is {}",
                descriptor.artifact_kind
            ),
        ));
    }
    if descriptor.run_id != run.id {
        return Err(LlmTranscriptDescriptorError::new(
            "run_id_mismatch",
            format!(
                "llm_jsonl descriptor is for run {}, not {}",
                descriptor.run_id, run.id
            ),
        ));
    }
    Ok(())
}

fn scan_jsonl(
    bytes: &[u8],
    transcript_path: &Path,
) -> Result<DescriptorScan, LlmTranscriptDescriptorError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        LlmTranscriptDescriptorError::new(
            "invalid_utf8",
            format!("{} is not UTF-8 JSONL: {error}", transcript_path.display()),
        )
    })?;
    let mut scan = DescriptorScan::default();
    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let event: JsonValue = serde_json::from_str(line).map_err(|error| {
            LlmTranscriptDescriptorError::new(
                "malformed_jsonl",
                format!(
                    "{}:{} failed to parse JSONL row: {error}",
                    transcript_path.display(),
                    idx + 1
                ),
            )
        })?;
        let event_type = event
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let event_id = event_identity(&event, scan.event_count + 1);
        if scan.first_event_type.is_none() {
            scan.first_event_type = Some(event_type.clone());
            scan.first_event_id = Some(event_id.clone());
        }
        scan.last_event_type = Some(event_type.clone());
        scan.last_event_id = Some(event_id);
        scan.event_count += 1;
        update_scan_facts(&mut scan, &event_type, &event);
    }
    if scan.event_count == 0 {
        return Err(LlmTranscriptDescriptorError::new(
            "empty_transcript",
            format!("{} contains no JSONL events", transcript_path.display()),
        ));
    }
    Ok(scan)
}

fn event_identity(event: &JsonValue, ordinal: usize) -> String {
    event
        .get("id")
        .or_else(|| event.get("event_id"))
        .or_else(|| event.get("call_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("row:{ordinal}"))
}

fn update_scan_facts(scan: &mut DescriptorScan, event_type: &str, event: &JsonValue) {
    if scan.session_id.is_none() {
        scan.session_id = event
            .get("session_id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
    }
    if scan.effective_tool_format.is_none() {
        scan.effective_tool_format = event
            .get("tool_format")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    if event_type == "tool_schemas" {
        scan.tool_schema_hash = event
            .get("hash")
            .map(|value| match value {
                JsonValue::String(value) => value.clone(),
                other => other.to_string(),
            })
            .filter(|value| !value.is_empty());
    }
    if is_terminal_event(event_type, event) {
        scan.complete = true;
        scan.terminal_status = event
            .get("final_status")
            .or_else(|| event.get("status"))
            .or_else(|| event.get("stop_reason"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| Some(event_type.to_string()));
    }
}

fn is_terminal_event(event_type: &str, event: &JsonValue) -> bool {
    matches!(
        event_type,
        "agent_session_finalized" | "agent_session_terminal" | "run_terminal" | "terminal"
    ) || event
        .get("terminal")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn relative_to_run_record(run_record_path: &Path, transcript_path: &Path) -> Option<String> {
    let parent = run_record_path.parent()?;
    transcript_path
        .strip_prefix(parent)
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn resolve_descriptor_path(
    run_record_path: &Path,
    descriptor: &RunTranscriptArtifactDescriptor,
) -> Result<PathBuf, LlmTranscriptDescriptorError> {
    let parent = run_record_path.parent().ok_or_else(|| {
        LlmTranscriptDescriptorError::new("invalid_run_path", "run record path has no parent")
    })?;
    if let Some(relative) = descriptor
        .relative_path
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(LlmTranscriptDescriptorError::new(
                "path_traversal",
                "descriptor relative_path must stay inside the run directory",
            ));
        }
        return Ok(parent.join(relative_path));
    }
    let fallback_path = Path::new(&descriptor.path);
    if descriptor.path.is_empty() {
        return Err(LlmTranscriptDescriptorError::new(
            "missing_path",
            "llm_jsonl descriptor has no path",
        ));
    }
    if fallback_path.is_absolute() {
        if fallback_path.strip_prefix(parent).is_err() {
            return Err(LlmTranscriptDescriptorError::new(
                "path_traversal",
                "descriptor path must stay inside the run directory",
            ));
        }
        return Ok(fallback_path.to_path_buf());
    }
    if fallback_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(LlmTranscriptDescriptorError::new(
            "path_traversal",
            "descriptor path must stay inside the run directory",
        ));
    }
    Ok(parent.join(fallback_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn descriptor_hashes_and_scans_strict_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let run_path = temp.path().join("run.json");
        let transcript_path = temp.path().join("agent-llm/llm_transcript.jsonl");
        std::fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript_path,
            concat!(
                "{\"type\":\"system_prompt\",\"content\":\"x\",\"hash\":1,\"session_id\":\"s1\"}\n",
                "{\"type\":\"tool_schemas\",\"hash\":\"schema-1\",\"schemas\":[]}\n",
                "{\"type\":\"provider_call_request\",\"call_id\":\"call-1\",\"tool_format\":\"native\"}\n",
                "{\"type\":\"agent_session_finalized\",\"final_status\":\"done\",\"terminal\":true}\n",
            ),
        )
        .unwrap();

        let descriptor =
            describe_llm_transcript_sidecar(&run("run-1"), &run_path, &transcript_path).unwrap();
        assert_eq!(descriptor.schema_version, DESCRIPTOR_SCHEMA_VERSION);
        assert_eq!(descriptor.run_id, "run-1");
        assert_eq!(descriptor.session_id.as_deref(), Some("s1"));
        assert_eq!(
            descriptor.relative_path.as_deref(),
            Some("agent-llm/llm_transcript.jsonl")
        );
        assert_eq!(descriptor.event_count, 4);
        assert_eq!(
            descriptor.first_event_type.as_deref(),
            Some("system_prompt")
        );
        assert_eq!(
            descriptor.last_event_type.as_deref(),
            Some("agent_session_finalized")
        );
        assert_eq!(descriptor.effective_tool_format.as_deref(), Some("native"));
        assert_eq!(descriptor.tool_schema_hash.as_deref(), Some("schema-1"));
        assert!(descriptor.complete);
        assert!(descriptor.sha256.starts_with("sha256:"));
        assert_eq!(
            descriptor.byte_len,
            std::fs::metadata(&transcript_path).unwrap().len()
        );
    }

    #[test]
    fn descriptor_rejects_malformed_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let run_path = temp.path().join("run.json");
        let transcript_path = temp.path().join("agent-llm/llm_transcript.jsonl");
        std::fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
        std::fs::write(&transcript_path, "{\"type\":\"system_prompt\"}\nnot-json\n").unwrap();

        let error = describe_llm_transcript_sidecar(&run("run-1"), &run_path, &transcript_path)
            .unwrap_err();
        assert_eq!(error.kind, "malformed_jsonl");
    }

    #[test]
    fn verified_pointer_rejects_mutated_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        let run_path = temp.path().join("run.json");
        let transcript_path = temp.path().join("agent-llm/llm_transcript.jsonl");
        std::fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript_path,
            "{\"type\":\"agent_session_finalized\",\"terminal\":true}\n",
        )
        .unwrap();
        let descriptor =
            describe_llm_transcript_sidecar(&run("run-1"), &run_path, &transcript_path).unwrap();
        let mut run = run("run-1");
        run.observability = Some(super::super::types::RunObservabilityRecord {
            transcript_pointers: vec![RunTranscriptPointerRecord {
                kind: ARTIFACT_KIND.to_string(),
                verification_status: "verified".to_string(),
                descriptor: Some(descriptor),
                ..Default::default()
            }],
            ..Default::default()
        });
        std::fs::write(
            &transcript_path,
            "{\"type\":\"agent_session_finalized\",\"terminal\":true} \n",
        )
        .unwrap();

        let error = verified_llm_transcript_pointer_path(&run, &run_path).unwrap_err();
        assert_eq!(error.kind, "digest_mismatch");
    }

    #[test]
    fn verified_pointer_accepts_legacy_absolute_path_inside_run_dir() {
        let temp = tempfile::tempdir().unwrap();
        let run_path = temp.path().join("run.json");
        let transcript_path = temp.path().join("agent-llm/llm_transcript.jsonl");
        std::fs::create_dir_all(transcript_path.parent().unwrap()).unwrap();
        std::fs::write(
            &transcript_path,
            "{\"type\":\"agent_session_finalized\",\"terminal\":true}\n",
        )
        .unwrap();
        let mut descriptor =
            describe_llm_transcript_sidecar(&run("run-1"), &run_path, &transcript_path).unwrap();
        descriptor.relative_path = None;
        let mut run = run("run-1");
        run.observability = Some(super::super::types::RunObservabilityRecord {
            transcript_pointers: vec![RunTranscriptPointerRecord {
                kind: ARTIFACT_KIND.to_string(),
                verification_status: "verified".to_string(),
                descriptor: Some(descriptor),
                ..Default::default()
            }],
            ..Default::default()
        });

        let path = verified_llm_transcript_pointer_path(&run, &run_path).unwrap();
        assert_eq!(path, transcript_path);
    }

    #[test]
    fn verified_pointer_rejects_fallback_path_outside_run_dir() {
        let run_dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let run_path = run_dir.path().join("run.json");
        let transcript_path = outside.path().join("llm_transcript.jsonl");
        std::fs::write(
            &transcript_path,
            "{\"type\":\"agent_session_finalized\",\"terminal\":true}\n",
        )
        .unwrap();
        let mut descriptor =
            describe_llm_transcript_sidecar(&run("run-1"), &run_path, &transcript_path).unwrap();
        descriptor.relative_path = None;
        let mut run = run("run-1");
        run.observability = Some(super::super::types::RunObservabilityRecord {
            transcript_pointers: vec![RunTranscriptPointerRecord {
                kind: ARTIFACT_KIND.to_string(),
                verification_status: "verified".to_string(),
                descriptor: Some(descriptor),
                ..Default::default()
            }],
            ..Default::default()
        });

        let error = verified_llm_transcript_pointer_path(&run, &run_path).unwrap_err();
        assert_eq!(error.kind, "path_traversal");
    }
}
