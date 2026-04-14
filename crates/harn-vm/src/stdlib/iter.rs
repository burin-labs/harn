//! Iterator builtins. Step (a) exposes only `iter(x)` — the explicit lift
//! from an iterable source into a lazy `VmValue::Iter`. Combinators (`map`,
//! `filter`, ...) and sinks (`to_list`, `to_set`, ...) land in subsequent
//! steps of the lazy-iterator plan.

use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::iter::iter_from_value;
use crate::vm::Vm;

pub(crate) fn register_iter_builtins(vm: &mut Vm) {
    vm.register_builtin("iter", |args, _out| {
        let v = args
            .first()
            .cloned()
            .ok_or_else(|| VmError::TypeError("iter: expected 1 argument".to_string()))?;
        iter_from_value(v)
    });
    vm.register_builtin("pair", |args, _out| {
        if args.len() != 2 {
            return Err(VmError::TypeError(format!(
                "pair: expected 2 arguments, got {}",
                args.len()
            )));
        }
        Ok(VmValue::Pair(Rc::new((args[0].clone(), args[1].clone()))))
    });
}
