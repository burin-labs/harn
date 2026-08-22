//! Cron-expression validation shared by Harn scheduling callers.

use croner::Cron;

use crate::stdlib::args::Args;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_cron_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&CRON_IS_VALID_BUILTIN_DEF];

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "__cron_is_valid(expression: string) -> bool", category = "cron"
)]
fn cron_is_valid_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let expression = Args::runtime("__cron_is_valid", args).string(0, "expression")?;
    Ok(VmValue::Bool(expression.parse::<Cron>().is_ok()))
}
