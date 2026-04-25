use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::value::{VmError, VmRange, VmRngHandle, VmValue};
use crate::vm::Vm;

pub(crate) fn register_math_builtins(vm: &mut Vm) {
    vm.register_builtin("abs", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Int(i64::MIN) => Ok(VmValue::Float(9_223_372_036_854_775_808.0)),
            VmValue::Int(n) => Ok(VmValue::Int(n.abs())),
            VmValue::Float(n) => Ok(VmValue::Float(n.abs())),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("min", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(*x.min(y))),
                (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x.min(*y))),
                (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float((*x as f64).min(*y))),
                (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x.min(*y as f64))),
                _ => Ok(VmValue::Nil),
            }
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("max", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(*x.max(y))),
                (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x.max(*y))),
                (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float((*x as f64).max(*y))),
                (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x.max(*y as f64))),
                _ => Ok(VmValue::Nil),
            }
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("floor", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => finite_float_to_i64(n.floor()).map(VmValue::Int),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("ceil", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => finite_float_to_i64(n.ceil()).map(VmValue::Int),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("round", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => finite_float_to_i64(n.round()).map(VmValue::Int),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("sqrt", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Float(n.sqrt())),
            VmValue::Int(n) => Ok(VmValue::Float((*n as f64).sqrt())),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("pow", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (VmValue::Int(base), VmValue::Int(exp)) => {
                    if *exp >= 0 && *exp <= u32::MAX as i64 {
                        match base.checked_pow(*exp as u32) {
                            Some(value) => Ok(VmValue::Int(value)),
                            None => Ok(VmValue::Float((*base as f64).powf(*exp as f64))),
                        }
                    } else {
                        Ok(VmValue::Float((*base as f64).powf(*exp as f64)))
                    }
                }
                (VmValue::Float(base), VmValue::Int(exp)) => {
                    if *exp >= i32::MIN as i64 && *exp <= i32::MAX as i64 {
                        Ok(VmValue::Float(base.powi(*exp as i32)))
                    } else {
                        Ok(VmValue::Float(base.powf(*exp as f64)))
                    }
                }
                (VmValue::Int(base), VmValue::Float(exp)) => {
                    Ok(VmValue::Float((*base as f64).powf(*exp)))
                }
                (VmValue::Float(base), VmValue::Float(exp)) => Ok(VmValue::Float(base.powf(*exp))),
                _ => Ok(VmValue::Nil),
            }
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("rng_seed", |args, _out| {
        use rand::SeedableRng;
        let seed = args.first().and_then(|arg| arg.as_int()).ok_or_else(|| {
            VmError::TypeError("rng_seed(seed): seed must be an integer".to_string())
        })?;
        Ok(VmValue::Rng(VmRngHandle {
            rng: Arc::new(Mutex::new(rand::rngs::StdRng::seed_from_u64(seed as u64))),
        }))
    });

    vm.register_builtin("random", |args, _out| {
        use rand::RngExt;
        let val: f64 = if let Some(VmValue::Rng(handle)) = args.first() {
            handle.rng.lock().expect("rng mutex poisoned").random()
        } else {
            rand::rng().random()
        };
        Ok(VmValue::Float(val))
    });

    vm.register_builtin("random_int", |args, _out| {
        use rand::RngExt;
        let (rng, min_idx) = match args.first() {
            Some(VmValue::Rng(handle)) => (Some(handle), 1),
            _ => (None, 0),
        };
        if args.len() >= min_idx + 2 {
            let min = args[min_idx].as_int().ok_or_else(|| {
                VmError::TypeError("random_int: min must be an integer".to_string())
            })?;
            let max = args[min_idx + 1].as_int().ok_or_else(|| {
                VmError::TypeError("random_int: max must be an integer".to_string())
            })?;
            if min > max {
                return Ok(VmValue::Nil);
            }
            let val = if let Some(handle) = rng {
                handle
                    .rng
                    .lock()
                    .expect("rng mutex poisoned")
                    .random_range(min..=max)
            } else {
                rand::rng().random_range(min..=max)
            };
            return Ok(VmValue::Int(val));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("random_choice", |args, _out| {
        use rand::RngExt;
        let (rng, list_idx) = match args.first() {
            Some(VmValue::Rng(handle)) => (Some(handle), 1),
            _ => (None, 0),
        };
        let Some(VmValue::List(items)) = args.get(list_idx) else {
            return Ok(VmValue::Nil);
        };
        if items.is_empty() {
            return Ok(VmValue::Nil);
        }
        let idx = if let Some(handle) = rng {
            handle
                .rng
                .lock()
                .expect("rng mutex poisoned")
                .random_range(0..items.len())
        } else {
            rand::rng().random_range(0..items.len())
        };
        Ok(items[idx].clone())
    });

    vm.register_builtin("random_shuffle", |args, _out| {
        use rand::seq::SliceRandom;
        let (rng, list_idx) = match args.first() {
            Some(VmValue::Rng(handle)) => (Some(handle), 1),
            _ => (None, 0),
        };
        let Some(VmValue::List(items)) = args.get(list_idx) else {
            return Ok(VmValue::Nil);
        };
        let mut shuffled = items.as_ref().clone();
        if let Some(handle) = rng {
            shuffled.shuffle(&mut *handle.rng.lock().expect("rng mutex poisoned"));
        } else {
            shuffled.shuffle(&mut rand::rng());
        }
        Ok(VmValue::List(Rc::new(shuffled)))
    });

    vm.register_builtin("mean", |args, _out| {
        let values = numeric_list_arg(args, "mean")?;
        if values.is_empty() {
            return Ok(VmValue::Float(0.0));
        }
        Ok(VmValue::Float(
            values.iter().sum::<f64>() / values.len() as f64,
        ))
    });

    vm.register_builtin("median", |args, _out| {
        let mut values = non_empty_numeric_list_arg(args, "median")?;
        values.sort_by(|a, b| a.total_cmp(b));
        let mid = values.len() / 2;
        if values.len() % 2 == 1 {
            Ok(VmValue::Float(values[mid]))
        } else {
            Ok(VmValue::Float((values[mid - 1] + values[mid]) / 2.0))
        }
    });

    vm.register_builtin("percentile", |args, _out| {
        let mut values = non_empty_numeric_list_arg(args, "percentile")?;
        let p = number_arg(args.get(1), "percentile")?;
        if !(0.0..=100.0).contains(&p) {
            return Err(VmError::Runtime(
                "percentile must be between 0 and 100".to_string(),
            ));
        }
        values.sort_by(|a, b| a.total_cmp(b));
        if values.len() == 1 {
            return Ok(VmValue::Float(values[0]));
        }
        let h = 1.0 + (values.len() as f64 - 1.0) * (p / 100.0);
        let lower = h.floor();
        let upper = h.ceil();
        if lower == upper {
            return Ok(VmValue::Float(values[lower as usize - 1]));
        }
        let weight = h - lower;
        let lo = values[lower as usize - 1];
        let hi = values[upper as usize - 1];
        Ok(VmValue::Float(lo + weight * (hi - lo)))
    });

    vm.register_builtin("variance", |args, _out| {
        let values = non_empty_numeric_list_arg(args, "variance")?;
        let sample = args.get(1).is_some_and(VmValue::is_truthy);
        if sample && values.len() < 2 {
            return Err(VmError::Runtime(
                "sample variance requires at least 2 values".to_string(),
            ));
        }
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let total = values
            .iter()
            .map(|value| {
                let delta = value - mean;
                delta * delta
            })
            .sum::<f64>();
        let denom = if sample {
            values.len() - 1
        } else {
            values.len()
        };
        Ok(VmValue::Float(total / denom as f64))
    });

    vm.register_builtin("stddev", |args, _out| {
        let variance = {
            let values = non_empty_numeric_list_arg(args, "stddev")?;
            let sample = args.get(1).is_some_and(VmValue::is_truthy);
            if sample && values.len() < 2 {
                return Err(VmError::Runtime(
                    "sample variance requires at least 2 values".to_string(),
                ));
            }
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let total = values
                .iter()
                .map(|value| {
                    let delta = value - mean;
                    delta * delta
                })
                .sum::<f64>();
            let denom = if sample {
                values.len() - 1
            } else {
                values.len()
            };
            total / denom as f64
        };
        Ok(VmValue::Float(variance.sqrt()))
    });

    register_unary_float(vm, "sin", f64::sin);
    register_unary_float(vm, "cos", f64::cos);
    register_unary_float(vm, "tan", f64::tan);
    register_unary_float(vm, "asin", f64::asin);
    register_unary_float(vm, "acos", f64::acos);
    register_unary_float(vm, "atan", f64::atan);
    register_unary_float(vm, "log2", f64::log2);
    register_unary_float(vm, "log10", f64::log10);
    register_unary_float(vm, "ln", f64::ln);
    register_unary_float(vm, "exp", f64::exp);

    vm.register_builtin("atan2", |args, _out| {
        if args.len() >= 2 {
            let y = match &args[0] {
                VmValue::Float(n) => *n,
                VmValue::Int(n) => *n as f64,
                _ => return Ok(VmValue::Nil),
            };
            let x = match &args[1] {
                VmValue::Float(n) => *n,
                VmValue::Int(n) => *n as f64,
                _ => return Ok(VmValue::Nil),
            };
            Ok(VmValue::Float(y.atan2(x)))
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("sign", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Int(n) => Ok(VmValue::Int(n.signum())),
            VmValue::Float(n) => {
                if n.is_nan() {
                    Ok(VmValue::Float(f64::NAN))
                } else if *n == 0.0 {
                    Ok(VmValue::Int(0))
                } else if *n > 0.0 {
                    Ok(VmValue::Int(1))
                } else {
                    Ok(VmValue::Int(-1))
                }
            }
            _ => Ok(VmValue::Nil),
        }
    });

    vm.set_global("pi", VmValue::Float(std::f64::consts::PI));
    vm.set_global("e", VmValue::Float(std::f64::consts::E));

    vm.register_builtin("is_nan", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Bool(n.is_nan())),
            _ => Ok(VmValue::Bool(false)),
        }
    });

    vm.register_builtin("is_infinite", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Bool(n.is_infinite())),
            _ => Ok(VmValue::Bool(false)),
        }
    });

    vm.register_builtin("__range__", |args, _out| {
        let start = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        let end = args.get(1).and_then(|a| a.as_int()).unwrap_or(0);
        let inclusive = args.get(2).map(|a| a.is_truthy()).unwrap_or(false);
        Ok(VmValue::Range(VmRange {
            start,
            end,
            inclusive,
        }))
    });

    // `range()` is Python-style and always half-open. Use `a to b [exclusive]`
    // for human-readable inclusive math.
    vm.register_builtin("range", |args, _out| {
        let (start, end) = match args.len() {
            1 => {
                let n = args[0].as_int().ok_or_else(|| {
                    VmError::TypeError("range(n): expected integer argument".to_string())
                })?;
                (0, n)
            }
            2 => {
                let a = args[0].as_int().ok_or_else(|| {
                    VmError::TypeError("range(a, b): expected integer arguments".to_string())
                })?;
                let b = args[1].as_int().ok_or_else(|| {
                    VmError::TypeError("range(a, b): expected integer arguments".to_string())
                })?;
                (a, b)
            }
            n => {
                return Err(VmError::TypeError(format!(
                    "range expects 1 or 2 integer arguments, got {n}"
                )));
            }
        };
        Ok(VmValue::Range(VmRange {
            start,
            end,
            inclusive: false,
        }))
    });
}

fn number_arg(value: Option<&VmValue>, label: &str) -> Result<f64, VmError> {
    match value {
        Some(VmValue::Int(n)) => Ok(*n as f64),
        Some(VmValue::Float(n)) => Ok(*n),
        _ => Err(VmError::TypeError(format!("{label} must be numeric"))),
    }
}

fn numeric_list_arg(args: &[VmValue], label: &str) -> Result<Vec<f64>, VmError> {
    let Some(VmValue::List(items)) = args.first() else {
        return Err(VmError::TypeError(format!("{label}: items must be a list")));
    };
    items
        .iter()
        .map(|item| number_arg(Some(item), label))
        .collect()
}

fn non_empty_numeric_list_arg(args: &[VmValue], label: &str) -> Result<Vec<f64>, VmError> {
    let values = numeric_list_arg(args, label)?;
    if values.is_empty() {
        return Err(VmError::Runtime(format!(
            "{label}: items must not be empty"
        )));
    }
    Ok(values)
}

fn finite_float_to_i64(n: f64) -> Result<i64, VmError> {
    if !n.is_finite() {
        return Err(VmError::Runtime(
            "cannot convert non-finite float to int".to_string(),
        ));
    }
    if n < i64::MIN as f64 || n >= 9_223_372_036_854_775_808.0 {
        return Err(VmError::Runtime(
            "float is outside the representable int range".to_string(),
        ));
    }
    Ok(n as i64)
}

/// Helper to reduce boilerplate for unary float functions (sin, cos, etc.)
fn register_unary_float(vm: &mut Vm, name: &'static str, f: fn(f64) -> f64) {
    vm.register_builtin(name, move |args, _out| {
        let n = match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => *n,
            VmValue::Int(n) => *n as f64,
            _ => return Ok(VmValue::Nil),
        };
        Ok(VmValue::Float(f(n)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> Vm {
        let mut vm = Vm::new();
        register_math_builtins(&mut vm);
        vm
    }

    fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let f = vm.builtins.get(name).unwrap().clone();
        let mut out = String::new();
        f(&args, &mut out)
    }

    #[test]
    fn abs_does_not_wrap_i64_min() {
        let mut vm = vm();
        let value = call(&mut vm, "abs", vec![VmValue::Int(i64::MIN)]).unwrap();
        assert_eq!(value.display(), "9223372036854776000");
    }

    #[test]
    fn integer_pow_does_not_wrap_on_overflow() {
        let mut vm = vm();
        let value = call(&mut vm, "pow", vec![VmValue::Int(2), VmValue::Int(63)]).unwrap();
        assert_eq!(value.display(), "9223372036854776000");
    }

    #[test]
    fn rounding_rejects_non_finite_float_to_int() {
        let mut vm = vm();
        let error = call(&mut vm, "floor", vec![VmValue::Float(f64::INFINITY)])
            .expect_err("infinite float cannot become int");
        assert!(error.to_string().contains("non-finite"));
    }
}
