//! The one record of a tool call that happened during a run.

use serde::{Deserialize, Serialize};

/// One tool call that happened during a run: the single fact the run record,
/// the run report, and the session bundle all carry.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(from = "ToolCallRecordWire")]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub tool_use_id: String,
    pub args_hash: String,
    pub result: String,
    pub status: ToolCallRecordStatus,
    /// End-to-end tool latency when the terminal session event measured it.
    /// `Some(0)` is a real sub-millisecond measurement; `None` means the
    /// source record did not carry a measurement.
    pub duration_ms: Option<u64>,
    pub iteration: usize,
    pub timestamp: String,
}

/// How a recorded tool call ended.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallRecordStatus {
    /// No terminal update was observed for the call.
    #[default]
    Open,
    Completed,
    /// The tool ran and failed.
    Failed,
    /// The call was refused before it ran.
    Rejected,
}

impl ToolCallRecordStatus {
    /// The status a terminal tool-call update reports, or `None` for a
    /// non-terminal one (`pending`, `in_progress`).
    pub(crate) fn terminal(wire: &str) -> Option<Self> {
        match wire {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }

    pub fn is_rejected(self) -> bool {
        self == Self::Rejected
    }
}

/// Reads a record written before `status` existed, when a call carried only
/// `is_rejected`. A legacy record without a rejection reads as `Open`: it
/// never said how the call ended.
#[derive(Default, Deserialize)]
#[serde(default)]
struct ToolCallRecordWire {
    tool_name: String,
    tool_use_id: String,
    args_hash: String,
    result: String,
    status: Option<ToolCallRecordStatus>,
    is_rejected: bool,
    duration_ms: Option<u64>,
    iteration: usize,
    timestamp: String,
}

impl From<ToolCallRecordWire> for ToolCallRecord {
    fn from(wire: ToolCallRecordWire) -> Self {
        Self {
            tool_name: wire.tool_name,
            tool_use_id: wire.tool_use_id,
            args_hash: wire.args_hash,
            result: wire.result,
            status: wire.status.unwrap_or(if wire.is_rejected {
                ToolCallRecordStatus::Rejected
            } else {
                ToolCallRecordStatus::Open
            }),
            duration_ms: wire.duration_ms,
            iteration: wire.iteration,
            timestamp: wire.timestamp,
        }
    }
}
