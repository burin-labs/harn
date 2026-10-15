//! Native persistence of interpreter flight recordings.

pub use crate::vm::flight_recorder::*;
use std::path::Path;

#[cfg(test)]
use crate::chunk::{Chunk, Op};
#[cfg(test)]
use std::sync::Arc;

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
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create flight recording directory {}: {error}",
            parent.display()
        )
    })?;
    crate::runtime_paths::self_ignore_default_run_root(parent);
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

    #[test]
    fn ring_retains_the_exact_newest_path_and_counts_eviction() {
        let mut chunk = Chunk::new();
        chunk.emit(crate::chunk::Op::Nil, 7);
        let recorder = FlightRecorder::new(crate::ExecutionId::mint(), 2);
        for task in ["a", "b", "c"] {
            recorder.record_instruction(task, 1, "main", &chunk, Some("main.harn"), 0, Op::Nil);
        }
        let recording = recorder.snapshot();
        assert_eq!(recording.dropped_events, 1);
        assert_eq!(recording.events.len(), 2);
        assert_eq!(recording.events[0].sequence, 1);
        assert_eq!(recording.tasks[recording.events[1].task_id as usize], "c");
        assert_eq!(recording.events[1].line, 7);
        assert_eq!(
            recording.sources[recording.events[1].source_id.unwrap() as usize],
            "main.harn"
        );
        assert_eq!(recording.opcode_names[&recording.events[1].opcode], "Nil");
    }

    #[test]
    fn concurrent_writers_share_one_gap_free_sequence() {
        let mut chunk = Chunk::new();
        chunk.emit(Op::Nil, 1);
        let chunk = Arc::new(chunk);
        let recorder = FlightRecorder::new(crate::ExecutionId::mint(), 8_000);
        let writers = (0..8)
            .map(|worker| {
                let chunk = Arc::clone(&chunk);
                let recorder = Arc::clone(&recorder);
                crate::runtime_stack::spawn(move || {
                    let task = format!("task-{worker}");
                    for _ in 0..1_000 {
                        recorder.record_instruction(
                            &task,
                            1,
                            "main",
                            &chunk,
                            Some("main.harn"),
                            0,
                            Op::Nil,
                        );
                    }
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap();
        }

        let recording = recorder.snapshot();
        assert_eq!(recording.events.len(), 8_000);
        assert_eq!(recording.dropped_events, 0);
        assert_eq!(recording.tasks.len(), 8);
        assert!(recording
            .events
            .iter()
            .enumerate()
            .all(|(index, event)| event.sequence == index as u64));
    }

    #[test]
    fn only_the_root_vm_owns_terminal_state_and_rollover() {
        let mut root = crate::Vm::new();
        root.enable_flight_recorder(8);
        let configured_id = root.execution_id().to_string();
        root.prepare_execution_for_top_level();
        let first = root.flight_recorder.as_ref().unwrap().clone();
        let child = root.child_vm();
        assert!(root.owns_execution);
        assert!(!child.owns_execution);
        assert_ne!(root.execution_id().as_str(), configured_id);
        assert_eq!(root.execution_id(), first.execution_id());
        assert_eq!(root.execution_id(), child.execution_id());
        assert!(Arc::ptr_eq(
            root.flight_recorder.as_ref().unwrap(),
            child.flight_recorder.as_ref().unwrap()
        ));

        first.finish("returned", None);
        root.prepare_execution_for_top_level();
        let second = root.flight_recorder.as_ref().unwrap();
        assert!(!Arc::ptr_eq(&first, second));
        assert_eq!(second.max_events, 8);
        assert!(second.snapshot().terminal.is_none());
    }

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
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = crate::Vm::new();
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
