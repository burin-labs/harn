use std::path::PathBuf;

/// Opt-in profiling. When `text` is true the run prints a categorical
/// breakdown to stderr after execution; when `json_path` is set the same
/// rollup is serialized to that path. Either flag enables span tracing
/// (i.e. `harn_vm::tracing::set_tracing_enabled(true)`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunProfileOptions {
    pub text: bool,
    pub json_path: Option<PathBuf>,
}

impl RunProfileOptions {
    pub fn is_enabled(&self) -> bool {
        self.text || self.json_path.is_some()
    }
}

/// Terminal VM outcomes that still complete through the normal CLI cleanup
/// path. An explicit Harn `exit(code)` is not a runtime error: the CLI must
/// preserve its captured output and emit the same receipts it would for a
/// returned value.
pub(super) enum TerminalRun {
    Returned(harn_vm::VmValue),
    ProcessExited(i32),
}

impl TerminalRun {
    pub(super) fn exit_code(&self) -> i32 {
        match self {
            Self::Returned(value) => super::exit_code_from_return_value(value),
            Self::ProcessExited(code) => *code,
        }
    }

    pub(super) fn json_value(&self) -> serde_json::Value {
        match self {
            Self::Returned(value) => harn_vm::llm::vm_value_to_json(value),
            Self::ProcessExited(_) => serde_json::Value::Null,
        }
    }

    pub(super) fn nonzero_return_diagnostic(&self) -> Option<String> {
        match self {
            Self::Returned(value) if self.exit_code() != 0 => {
                Some(super::render_return_value_error(value))
            }
            Self::Returned(_) | Self::ProcessExited(_) => None,
        }
    }
}

pub(super) enum RunExecution {
    Terminal(TerminalRun),
    Failed(String),
}
