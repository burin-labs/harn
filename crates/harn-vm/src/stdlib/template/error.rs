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
        VmError::Thrown(VmValue::String(std::sync::Arc::from(e.message())))
    }
}
