use std::path::PathBuf;
use std::rc::Rc;

use crate::value::{VmError, VmValue};

#[derive(Debug, Clone)]
pub(crate) struct TemplateError {
    pub path: Option<PathBuf>,
    pub line: usize,
    pub col: usize,
    pub kind: String,
}

impl TemplateError {
    pub(crate) fn new(line: usize, col: usize, msg: impl Into<String>) -> Self {
        Self {
            path: None,
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
            .unwrap_or_default();
        format!("{}at {}:{}: {}", p, self.line, self.col, self.kind)
    }
}

impl From<TemplateError> for VmError {
    fn from(e: TemplateError) -> Self {
        VmError::Thrown(VmValue::String(Rc::from(e.message())))
    }
}
