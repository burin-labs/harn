//! Cron-expression validation shared by Harn scheduling callers.

use croner::Cron;

use crate::stdlib::args::Args;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_cron_builtins);

#[harn_builtin(
    exposure = "pure",
    sig = "__cron_is_valid(expression: string) -> bool",
    category = "cron"
)]
fn cron_is_valid_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let expression = Args::runtime("__cron_is_valid", args).string(0, "expression")?;
    Ok(VmValue::Bool(expression.parse::<Cron>().is_ok()))
}
