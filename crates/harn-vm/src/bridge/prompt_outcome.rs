use std::sync::Mutex;

use crate::agent_events::AgentTerminalOutcome;

/// One finalized prompt's producer-owned outcomes, without a new success verdict.
#[derive(Clone, Debug)]
pub struct PromptTerminalReceipt {
    pub stop_reason: String,
    pub terminal: AgentTerminalOutcome,
    /// The completion owner's assessment. Absence means it was not measured.
    pub goal_outcome: Option<serde_json::Value>,
}

#[derive(Default)]
pub(super) struct PromptOutcomeSlot(Mutex<Option<PromptTerminalReceipt>>);

impl PromptOutcomeSlot {
    pub(super) fn set(&self, receipt: PromptTerminalReceipt) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(receipt);
    }

    pub(super) fn take(&self) -> Option<PromptTerminalReceipt> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
}
