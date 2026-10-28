//! Bounded, opt-in source-path recording for one VM execution tree.
//!
//! Unlike tracing spans, a flight recording preserves every executed bytecode
//! location in order. It deliberately records no stack values, arguments, or
//! results. Child VMs share one recorder, so parallel work receives one
//! monotonic sequence rather than one irreconcilable file per isolate.

use std::collections::{BTreeMap, HashMap, VecDeque};
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

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[derive(ts_rs::TS)]
pub enum FlightValuePolicy {
    /// Runtime values, arguments, results, and stack contents are never stored.
    #[default]
    Omitted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema, ts_rs::TS)]
#[ts(rename = "PortalFlightRecording")]
pub struct FlightRecordingArtifact {
    pub schema_version: u32,
    pub execution_id: String,
    pub format: String,
    /// Local locator for the recording payload. Public projections omit it;
    /// local hosts may opt in when they can actually open the artifact.
    pub path: Option<String>,
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
    execution_id: crate::ExecutionId,
    max_events: usize,
    state: Mutex<RecorderState>,
}

impl FlightRecorder {
    pub fn new(execution_id: crate::ExecutionId, max_events: usize) -> Arc<Self> {
        Arc::new(Self {
            execution_id,
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

    pub fn execution_id(&self) -> &crate::ExecutionId {
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
            execution_id: self.execution_id.to_string(),
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

#[cfg(test)]
#[path = "flight_recorder_tests.rs"]
mod tests;
