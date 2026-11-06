//! Native persistence of interpreter flight recordings.

pub use crate::vm::flight_recorder::*;
use std::path::{Path, PathBuf};

/// Storage policy for an enabled VM flight recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlightRecordingStorage<'a> {
    /// Write into Harn's dedicated directory and retain the newest files.
    Managed {
        /// Maximum number of newest managed recordings to keep.
        retain_files: usize,
    },
    /// Write to a caller-owned path without rotating sibling files.
    Custom {
        /// Exact destination for a run; prompts append their execution ID.
        output: &'a Path,
    },
}

#[derive(Clone, Debug)]
pub struct FlightRecorderOptions {
    pub enabled: bool,
    pub out: Option<PathBuf>,
    pub max_events: usize,
    pub retain_files: usize,
}

impl Default for FlightRecorderOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            out: None,
            max_events: DEFAULT_MAX_EVENTS,
            retain_files: DEFAULT_RETAIN_FILES,
        }
    }
}

impl FlightRecorderOptions {
    pub fn from_environment() -> Self {
        Self {
            enabled: std::env::var("HARN_FLIGHT_RECORDER").is_ok_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "t" | "yes" | "y" | "on"
                )
            }),
            ..Self::default()
        }
    }

    pub fn storage(&self) -> Option<FlightRecordingStorage<'_>> {
        self.enabled.then(|| {
            self.out.as_deref().map_or(
                FlightRecordingStorage::Managed {
                    retain_files: self.retain_files,
                },
                |output| FlightRecordingStorage::Custom { output },
            )
        })
    }

    pub fn persist_prompt(
        &self,
        vm: &crate::Vm,
        store_base: &Path,
        context: FlightPromptContext<'_>,
    ) -> Result<Option<FlightRecordingArtifact>, String> {
        self.storage()
            .map(|storage| storage.persist(vm, store_base, Some(context)))
            .transpose()
    }
}

impl FlightRecordingStorage<'_> {
    pub(crate) fn persist(
        self,
        vm: &crate::Vm,
        store_base: &Path,
        context: Option<FlightPromptContext<'_>>,
    ) -> Result<FlightRecordingArtifact, String> {
        let recording = vm
            .flight_recording()
            .ok_or("flight recorder was requested but not installed")?;
        let (path, retain) = match self {
            Self::Custom { output } => {
                let output = if context.is_some() {
                    let stem = output
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .ok_or("flight recording output needs a file name")?;
                    output.with_file_name(format!("{stem}.{}.json", recording.execution_id))
                } else {
                    output.to_path_buf()
                };
                (
                    crate::run_tape::branch_path(output, "flight recording"),
                    usize::MAX,
                )
            }
            Self::Managed { retain_files } => (
                crate::runtime_paths::run_root(store_base)
                    .join("flight-recordings")
                    .join(format!("{}.json", recording.execution_id)),
                retain_files,
            ),
        };
        persist_recording_with_context(&recording, &path, retain, context)
    }
}

#[derive(Clone, Copy, serde::Serialize)]
pub struct FlightPromptContext<'a> {
    pub session_id: &'a str,
    pub prompt_id: &'a str,
}

/// Persist one recording atomically and retain only the newest `retain_files`
/// recordings in the same directory. The exact file is never evicted by its
/// own write, even when `retain_files` is zero or one.
pub fn persist_recording(
    recording: &FlightRecording,
    path: &Path,
    retain_files: usize,
) -> Result<FlightRecordingArtifact, String> {
    persist_recording_with_context(recording, path, retain_files, None)
}

pub fn persist_recording_with_context(
    recording: &FlightRecording,
    path: &Path,
    retain_files: usize,
    context: Option<FlightPromptContext<'_>>,
) -> Result<FlightRecordingArtifact, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("flight recording path has no parent: {}", path.display()))?;
    crate::runtime_paths::prepare_run_record_parent(path).map_err(|error| {
        format!(
            "failed to create flight recording directory {}: {error}",
            parent.display()
        )
    })?;
    #[derive(serde::Serialize)]
    struct ContextualRecording<'a> {
        #[serde(flatten)]
        recording: &'a FlightRecording,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<FlightPromptContext<'a>>,
    }
    let bytes = serde_json::to_vec(&ContextualRecording { recording, context })
        .map_err(|error| format!("failed to encode flight recording: {error}"))?;
    let write = || {
        crate::atomic_io::atomic_write(path, &bytes)
            .map_err(|error| format!("failed to persist flight recording: {error}"))?;
        crate::bounded_files::retain_newest_files(parent, path, retain_files, |candidate| {
            candidate.extension().and_then(|ext| ext.to_str()) == Some("json")
        })
    };
    if retain_files == usize::MAX {
        write()?;
    } else {
        crate::bounded_files::with_retention_transaction(parent, write)?;
    }
    Ok(FlightRecordingArtifact {
        schema_version: recording.schema_version,
        execution_id: recording.execution_id.clone(),
        format: FLIGHT_RECORDING_FORMAT.to_string(),
        path: Some(path.to_string_lossy().into_owned()),
        content_hash: format!("blake3:{}", blake3::hash(&bytes).to_hex()),
        byte_length: bytes.len() as u64,
        retained_events: recording.events.len(),
        dropped_events: recording.dropped_events,
        value_policy: recording.value_policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[tokio::test(flavor = "current_thread")]
    async fn vm_records_the_taken_source_path_without_runtime_values() {
        let source = r#"pipeline main(harness: Harness) {
  const secret = "do-not-record-me"
  if secret == "do-not-record-me" {
    return 7
  }
  return 9
}"#;
        let chunk = crate::compile_source(source).expect("source compiles");
        harn_clock::test_support::with_local_tasks(async {
            let mut vm = crate::new_vm();
            crate::register_vm_stdlib(&mut vm);
            vm.set_harness(crate::Harness::real());
            vm.set_source_info("main.harn", source);
            vm.enable_flight_recorder(512);

            let result = vm.execute(&chunk).await.expect("execution succeeds");
            assert!(matches!(result, crate::VmValue::Int(7)));
            let recording = vm.flight_recording().expect("recording exists");
            assert_eq!(recording.terminal.as_ref().unwrap().status, "returned");
            assert!(recording.events.iter().any(|event| event.line == 4));
            assert!(!recording.events.iter().any(|event| event.line == 6));
            assert!(recording.sources.iter().any(|source| source == "main.harn"));
            assert!(!serde_json::to_string(&recording)
                .unwrap()
                .contains("do-not-record-me"));
        })
        .await;
    }

    #[test]
    fn persistence_prunes_old_files_but_keeps_the_file_just_written() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep-me.txt"), "unrelated").unwrap();
        for id in ["one", "two", "three"] {
            let recording = FlightRecording {
                schema_version: FLIGHT_RECORDING_SCHEMA_VERSION,
                execution_id: id.to_string(),
                max_events: 1,
                dropped_events: 0,
                value_policy: FlightValuePolicy::Omitted,
                tasks: Vec::new(),
                functions: Vec::new(),
                sources: Vec::new(),
                opcode_names: BTreeMap::new(),
                events: Vec::new(),
                terminal: None,
            };
            let path = dir.path().join(format!("{id}.json"));
            persist_recording(&recording, &path, 2).unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                serde_json::to_vec(&recording).unwrap()
            );
        }
        let files = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(
            files
                .iter()
                .filter(
                    |entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
                )
                .count(),
            2
        );
        assert!(dir.path().join("keep-me.txt").is_file());
        assert!(dir.path().join("three.json").is_file());
    }
}
