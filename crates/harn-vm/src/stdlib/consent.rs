use crate::stdlib::macros::harn_builtin;
use crate::value::VmValue;

harn_vm_core::stdlib_module_installer!(Pure, pub fn register_consent_builtins);

/// The outcomes accepted at command and tool consent boundaries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsentDecision {
    Approved,
    Auto,
    Denied,
}

impl ConsentDecision {
    pub(crate) fn from_value(value: &VmValue) -> Self {
        match value {
            VmValue::Bool(true) => Self::Approved,
            VmValue::Dict(fields) => match fields.get("decision") {
                Some(VmValue::String(value)) => match value.as_ref() {
                    "approved" => Self::Approved,
                    "auto" => Self::Auto,
                    _ => Self::Denied,
                },
                _ => Self::Denied,
            },
            _ => Self::Denied,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Auto => "auto",
            Self::Denied => "denied",
        }
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "__consent_decision(value: any) -> string",
    category = "orchestration"
)]
fn consent_decision_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, crate::value::VmError> {
    let decision = ConsentDecision::from_value(args.first().unwrap_or(&VmValue::Nil));
    Ok(VmValue::string(decision.as_str()))
}
