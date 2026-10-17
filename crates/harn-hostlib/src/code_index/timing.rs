//! Optional phase measurements on the same path used by hosts.

use std::time::Instant;

pub(super) fn skipped(path: &std::path::Path) {
    tracing::debug!(target: "harn_hostlib::code_index::timing",
        phase = "ingest.skipped", elapsed_us = 0_u64,
        path = path.to_string_lossy().as_ref(), "code-index file skipped");
}

pub(super) struct Phase {
    name: &'static str,
    start: Option<Instant>,
}

impl Phase {
    pub fn start(name: &'static str) -> Self {
        Self {
            name,
            start:
                tracing::enabled!(target: "harn_hostlib::code_index::timing", tracing::Level::DEBUG)
                    .then(Instant::now),
        }
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            tracing::debug!(target: "harn_hostlib::code_index::timing",
                phase = self.name, elapsed_us = start.elapsed().as_micros() as u64,
                "code-index phase completed");
        }
    }
}
