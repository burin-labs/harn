//! Native persistence of interpreter flight recordings.

pub use crate::vm::flight_recorder::*;
use std::path::Path;

/// Persist one recording atomically and retain only the newest `retain_files`
/// recordings in the same directory. The exact file is never evicted by its
/// own write, even when `retain_files` is zero or one.
pub fn persist_recording(
    recording: &FlightRecording,
    path: &Path,
    retain_files: usize,
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
    let bytes = serde_json::to_vec(recording)
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
