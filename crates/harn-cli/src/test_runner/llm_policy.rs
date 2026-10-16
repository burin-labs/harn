use crate::env_guard::ScopedEnvVar;
use harn_vm::llm::{LlmReplayMode, ReplayModeScope};

/// LLM authority admitted by the test caller, never inherited from another VM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TestLlmPolicy {
    #[default]
    Hermetic,
    Record,
    Replay,
    /// The development watch loop retains ordinary runtime provider selection.
    Live,
}

impl TestLlmPolicy {
    pub(super) fn suite_environment(self) -> (Option<ScopedEnvVar>, Option<ScopedEnvVar>) {
        let provider = (self == Self::Hermetic)
            .then(|| ScopedEnvVar::set_if_unset("HARN_LLM_PROVIDER", "mock"))
            .flatten();
        let disabled = matches!(self, Self::Hermetic | Self::Replay)
            .then(|| ScopedEnvVar::set(harn_vm::llm::LLM_CALLS_DISABLED_ENV, "1"));
        (provider, disabled)
    }

    pub(crate) fn scope(self) -> ReplayModeScope {
        let mode = match self {
            Self::Record => LlmReplayMode::Record,
            Self::Replay => LlmReplayMode::Replay,
            Self::Hermetic | Self::Live => LlmReplayMode::Off,
        };
        harn_vm::llm::scope_replay_mode(mode, ".harn-fixtures")
    }
}
