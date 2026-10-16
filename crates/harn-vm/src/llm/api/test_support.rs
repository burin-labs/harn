//! Shared test scaffolding for the `llm::api` test modules.
//!
//! `ScopedEnvVar` restores the previous value on drop, so a test that flips a
//! transport-gating variable cannot leak it into whatever nextest schedules
//! next on the same process.

pub(super) use crate::llm::test_env::ScopedEnvVar;

pub(super) fn allow_stubbed_llm_transport() -> ScopedEnvVar {
    ScopedEnvVar::remove(crate::llm::LLM_CALLS_DISABLED_ENV)
}

pub(super) struct ReleaseBody(pub(super) Option<std::sync::mpsc::Sender<()>>);

impl ReleaseBody {
    pub(super) fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl Drop for ReleaseBody {
    fn drop(&mut self) {
        self.release();
    }
}
