use std::path::PathBuf;

use crate::value::{VmError, VmValue};

#[derive(Debug, Clone)]
pub(crate) struct TemplateError {
    pub path: Option<PathBuf>,
    pub uri: Option<String>,
    pub line: usize,
    pub col: usize,
    pub kind: String,
}

impl TemplateError {
    pub(crate) fn new(line: usize, col: usize, msg: impl Into<String>) -> Self {
        Self {
            path: None,
            uri: None,
            line,
            col,
            kind: msg.into(),
        }
    }

    pub(crate) fn message(&self) -> String {
        let p = self
            .path
            .as_ref()
            .map(|p| format!("{} ", p.display()))
            .or_else(|| self.uri.as_ref().map(|uri| format!("{uri} ")))
            .unwrap_or_default();
        format!("{}at {}:{}: {}", p, self.line, self.col, self.kind)
    }
}

impl From<TemplateError> for VmError {
    fn from(e: TemplateError) -> Self {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(e.message())))
    }
}

/// A template that failed to parse, positioned in its own source.
///
/// The engine's internal error also carries the asset path/URI it came
/// from, which is meaningless to a caller that handed us a source
/// string. This is the projection tools get: what went wrong and where,
/// as data rather than as a pre-formatted sentence, so `harn lint` and
/// the language server can each place the diagnostic their own way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateParseError {
    /// What went wrong, with no position prefix.
    pub message: String,
    /// 1-based line of the directive that failed to parse.
    pub line: usize,
    /// 1-based column of that directive.
    pub col: usize,
}

impl From<TemplateError> for TemplateParseError {
    fn from(error: TemplateError) -> Self {
        Self {
            message: error.kind,
            line: error.line,
            col: error.col,
        }
    }
}

impl std::fmt::Display for TemplateParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for TemplateParseError {}
