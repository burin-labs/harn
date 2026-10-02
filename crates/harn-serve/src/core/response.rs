//! The result shared by dispatch adapters and explicit replay.

use harn_vm::TraceId;

use super::DispatchCallReceipt;
use crate::ReplayCacheEntry;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallResponse {
    pub function: String,
    pub value: serde_json::Value,
    pub printed_output: String,
    pub feedback: Option<String>,
    pub trace_id: TraceId,
    pub cached: bool,
    pub duration_ms: u128,
    pub dispatch: DispatchCallReceipt,
}

impl CallResponse {
    pub(super) fn from_replay(
        function: String,
        cached: ReplayCacheEntry,
        trace_id: TraceId,
    ) -> Self {
        Self {
            function,
            value: cached.value,
            printed_output: cached.printed_output,
            feedback: cached.feedback,
            trace_id,
            cached: true,
            duration_ms: 0,
            dispatch: DispatchCallReceipt::default(),
        }
    }
}
