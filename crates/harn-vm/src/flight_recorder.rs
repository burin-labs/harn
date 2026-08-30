//! Bounded, opt-in source-path recording for one VM execution tree.
//!
//! Unlike tracing spans, a flight recording preserves every executed bytecode
//! location in order. It deliberately records no stack values, arguments, or
//! results. Child VMs share one recorder, so parallel work receives one
//! monotonic sequence rather than one irreconcilable file per isolate.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::chunk::{Chunk, Op};

pub const FLIGHT_RECORDING_SCHEMA_VERSION: u32 = 1;
pub const FLIGHT_RECORDING_FORMAT: &str = "harn.flight.v1+json";
pub const DEFAULT_MAX_EVENTS: usize = 250_000;
pub const DEFAULT_RETAIN_FILES: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FlightEvent {
    pub sequence: u64,
    pub task_id: u32,
    pub frame_depth: usize,
    pub function_id: u32,
    pub source_id: Option<u32>,
    pub line: u32,
    pub column: u32,
    pub instruction_offset: usize,
    pub opcode: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FlightRecording {
    pub schema_version: u32,
    pub execution_id: String,
    pub max_events: usize,
    pub dropped_events: u64,
    pub value_policy: FlightValuePolicy,
    pub tasks: Vec<String>,
    pub functions: Vec<String>,
    pub sources: Vec<String>,
    pub opcode_names: BTreeMap<u8, String>,
    pub events: Vec<FlightEvent>,
    pub terminal: Option<FlightTerminal>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FlightValuePolicy {
    /// Runtime values, arguments, results, and stack contents are never stored.
    #[default]
    Omitted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FlightRecordingArtifact {
    pub schema_version: u32,
    pub execution_id: String,
    pub format: String,
    pub path: String,
    pub content_hash: String,
    pub byte_length: u64,
    pub retained_events: usize,
    pub dropped_events: u64,
    pub value_policy: FlightValuePolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FlightTerminal {
    pub status: String,
    pub process_exit_code: Option<i32>,
}

#[derive(Debug)]
struct RecorderState {
    next_sequence: u64,
    dropped_events: u64,
    events: VecDeque<FlightEvent>,
    terminal: Option<FlightTerminal>,
    tasks: InternTable,
    functions: InternTable,
    sources: InternTable,
    opcode_names: BTreeMap<u8, String>,
}

#[derive(Debug, Default)]
struct InternTable {
    ids: HashMap<String, u32>,
    values: Vec<String>,
}

impl InternTable {
    fn intern(&mut self, value: &str) -> u32 {
        if let Some(id) = self.ids.get(value) {
            return *id;
        }
        let id = u32::try_from(self.values.len())
            .expect("flight recorder intern table cannot exceed its u32 event-id space");
        self.values.push(value.to_string());
        self.ids.insert(value.to_string(), id);
        id
    }
}

/// Shared ring buffer for an execution tree.
#[derive(Debug)]
pub struct FlightRecorder {
    execution_id: String,
    max_events: usize,
    state: Mutex<RecorderState>,
}

impl FlightRecorder {
    pub fn new(execution_id: impl Into<String>, max_events: usize) -> Arc<Self> {
        Arc::new(Self {
            execution_id: execution_id.into(),
            max_events: max_events.max(1),
            state: Mutex::new(RecorderState {
                next_sequence: 0,
                dropped_events: 0,
                events: VecDeque::with_capacity(max_events.clamp(1, 16_384)),
                terminal: None,
                tasks: InternTable::default(),
                functions: InternTable::default(),
                sources: InternTable::default(),
                opcode_names: BTreeMap::new(),
            }),
        })
    }

    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }

    #[inline]
    pub(crate) fn record_instruction(
        &self,
        task_id: &str,
        frame_depth: usize,
        function: &str,
        chunk: &Chunk,
        primary_file: Option<&str>,
        instruction_offset: usize,
        op: Op,
    ) {
        let mut state = self.state.lock().expect("flight recorder mutex poisoned");
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        if state.events.len() == self.max_events {
            state.events.pop_front();
            state.dropped_events = state.dropped_events.saturating_add(1);
        }
        let task_id = state.tasks.intern(task_id);
        let function_id = state.functions.intern(function);
        let source_id = chunk
            .source_file
            .as_deref()
            .or(primary_file)
            .map(|source| state.sources.intern(source));
        state
            .opcode_names
            .entry(op as u8)
            .or_insert_with(|| format!("{op:?}"));
        state.events.push_back(FlightEvent {
            sequence,
            task_id,
            frame_depth,
            function_id,
            source_id,
            line: chunk.lines.get(instruction_offset).copied().unwrap_or(0),
            column: chunk.columns.get(instruction_offset).copied().unwrap_or(0),
            instruction_offset,
            opcode: op as u8,
        });
    }

    pub fn snapshot(&self) -> FlightRecording {
        let state = self.state.lock().expect("flight recorder mutex poisoned");
        FlightRecording {
            schema_version: FLIGHT_RECORDING_SCHEMA_VERSION,
            execution_id: self.execution_id.clone(),
            max_events: self.max_events,
            dropped_events: state.dropped_events,
            value_policy: FlightValuePolicy::Omitted,
            tasks: state.tasks.values.clone(),
            functions: state.functions.values.clone(),
            sources: state.sources.values.clone(),
            opcode_names: state.opcode_names.clone(),
            events: state.events.iter().cloned().collect(),
            terminal: state.terminal.clone(),
        }
    }

    pub(crate) fn finish(&self, status: &str, process_exit_code: Option<i32>) {
        self.state
            .lock()
            .expect("flight recorder mutex poisoned")
            .terminal = Some(FlightTerminal {
            status: status.to_string(),
            process_exit_code,
        });
    }
}

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
        path: path.to_string_lossy().into_owned(),
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
        let recorder = FlightRecorder::new("exec-1", 2);
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
        let recorder = FlightRecorder::new("exec-concurrent", 8_000);
        let writers = (0..8)
            .map(|worker| {
                let chunk = Arc::clone(&chunk);
                let recorder = Arc::clone(&recorder);
                std::thread::spawn(move || {
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
        assert_ne!(root.execution_id(), configured_id);
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
