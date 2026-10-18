//! The seam between refactoring tools and the engines that compute edits.
//!
//! A backend answers with a WorkspaceEdit-shaped [`Changeset`]; the tool
//! lowers it through the shared [`super::refactor_core::plan_file`] and
//! [`super::refactor_core::write_plans`] path, so dry runs, staged sessions,
//! previews and undo snapshots do not depend on which backend ran.

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{build_dict, str_value};

use super::refactor_core::EditSpan;

/// One file's edits against the bytes the backend was given.
pub(super) struct FileEdits {
    /// Index-relative path.
    pub path: String,
    pub source: String,
    pub edits: Vec<EditSpan>,
}

pub(super) type Changeset = Vec<FileEdits>;

pub(super) enum BackendOutcome {
    Changeset(Changeset),
    /// The backend understood the request and refused it on semantic grounds;
    /// trying a less precise engine would be wrong.
    Declined(String),
    /// The backend could not answer; the next backend may.
    Unavailable(String),
}

/// Which engines a request may use. `HARN_REFACTOR_BACKEND` sets the
/// process default; the request's `backend` field overrides it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BackendChoice {
    /// The language server, falling back to the syntax-tree engine.
    Auto,
    /// The language server only.
    LanguageServer,
    /// The syntax-tree engine only.
    SyntaxTree,
}

impl BackendChoice {
    pub(super) fn parse(builtin: &'static str, raw: &str) -> Result<Self, HostlibError> {
        match raw {
            "auto" => Ok(Self::Auto),
            "language_server" => Ok(Self::LanguageServer),
            "syntax_tree" => Ok(Self::SyntaxTree),
            other => Err(HostlibError::InvalidParameter {
                builtin,
                param: "backend",
                message: format!(
                    "expected `auto`, `language_server` or `syntax_tree`, got `{other}`"
                ),
            }),
        }
    }

    pub(super) fn resolve(
        builtin: &'static str,
        requested: Option<&str>,
    ) -> Result<Self, HostlibError> {
        match requested {
            Some(raw) => Self::parse(builtin, raw),
            None => match std::env::var("HARN_REFACTOR_BACKEND") {
                Ok(raw) if !raw.is_empty() => Self::parse(builtin, &raw),
                _ => Ok(Self::Auto),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ServerStart {
    /// Started by this call.
    Cold,
    /// Reused from an earlier call in this process.
    Warm,
}

/// Which engine produced a result, reported on every response so evals can
/// measure precision and fallback rate.
#[derive(Clone, Debug, Default)]
pub(super) struct BackendReport {
    pub language_server: Option<ServerRun>,
    /// Set when the syntax-tree engine ran because the server could not.
    pub fallback_reason: Option<String>,
    /// True when the syntax-tree engine produced the result.
    pub syntax_tree: bool,
}

#[derive(Clone, Debug)]
pub(super) struct ServerRun {
    pub server: &'static str,
    pub start: ServerStart,
    pub elapsed_ms: u64,
}

impl BackendReport {
    pub(super) fn value(&self) -> VmValue {
        let name = if self.syntax_tree {
            "syntax_tree"
        } else if self.language_server.is_some() {
            "language_server"
        } else {
            "none"
        };
        let mut entries = vec![("name", str_value(name))];
        if let Some(run) = &self.language_server {
            entries.push(("server", str_value(run.server)));
            entries.push((
                "server_start",
                str_value(match run.start {
                    ServerStart::Cold => "cold",
                    ServerStart::Warm => "warm",
                }),
            ));
            entries.push(("server_elapsed_ms", VmValue::Int(run.elapsed_ms as i64)));
        }
        if let Some(reason) = &self.fallback_reason {
            entries.push(("fallback_reason", str_value(reason)));
        }
        build_dict(entries)
    }
}
