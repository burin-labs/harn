//! Raw provider-response capture shared by streaming transport paths.

use std::sync::{Arc, Mutex};

pub(super) fn capture_stream_bytes(capture: Option<&Arc<Mutex<Vec<u8>>>>, bytes: &bytes::Bytes) {
    let Some(capture) = capture else {
        return;
    };
    let mut raw = capture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    raw.extend_from_slice(bytes);
}

pub(super) fn captured_stream_text(capture: &Arc<Mutex<Vec<u8>>>) -> String {
    let raw = capture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    String::from_utf8_lossy(&raw).into_owned()
}

#[derive(Clone)]
pub(super) struct RawProviderCaptureTarget {
    context: Option<crate::llm::agent_observe::RawProviderCaptureContext>,
    pub(super) attempt: Option<usize>,
}

impl RawProviderCaptureTarget {
    pub(super) fn new(
        context: Option<crate::llm::agent_observe::RawProviderCaptureContext>,
        attempt: Option<usize>,
    ) -> Self {
        Self { context, attempt }
    }

    pub(super) fn context(&self) -> Option<&crate::llm::agent_observe::RawProviderCaptureContext> {
        self.context.as_ref()
    }

    pub(super) fn enabled(&self) -> bool {
        crate::llm::agent_observe::raw_provider_capture_enabled(self.context())
    }
}
