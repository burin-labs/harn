use super::{SchemaStreamAbort, VmError, VmValue};

impl From<harn_prompt_syntax::TemplateError> for VmError {
    fn from(error: harn_prompt_syntax::TemplateError) -> Self {
        Self::Thrown(VmValue::string(error.message()))
    }
}

impl SchemaStreamAbort {
    /// Preserve the validator's typed abort for the schema retry loop.
    pub fn into_vm_error(self) -> VmError {
        VmError::SchemaStreamAbort(Box::new(self))
    }
}
