pub use harn_prompt_syntax::{TemplateError, TemplateParseError};

use crate::value::{VmError, VmValue};

impl From<TemplateError> for VmError {
    fn from(error: TemplateError) -> Self {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error.message())))
    }
}
