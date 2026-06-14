use std::sync::Arc;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmRange, VmRngHandle, VmValue};
use crate::vm::Vm;
use parking_lot::Mutex;

pub(crate) fn register_math_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }

    vm.set_global("pi", VmValue::Float(std::f64::consts::PI));
    vm.set_global("e", VmValue::Float(std::f64::consts::E));
}

#[harn_builtin(sig = "abs(value: number) -> number", category = "math")]
fn abs_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Int(i64::MIN) => Ok(VmValue::Float(9_223_372_036_854_775_808.0)),
        VmValue::Int(n) => Ok(VmValue::Int(n.abs())),
        VmValue::Float(n) => Ok(VmValue::Float(n.abs())),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(sig = "min(...args: any) -> any", category = "math")]
fn min_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        match (&args[0], &args[1]) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(*x.min(y))),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x.min(*y))),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float((*x as f64).min(*y))),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x.min(*y as f64))),
            (VmValue::Decimal(x), VmValue::Decimal(y)) => Ok(VmValue::Decimal((*x).min(*y))),
            _ => Ok(VmValue::Nil),
        }
    } else {
        Ok(VmValue::Nil)
    }
}

#[harn_builtin(sig = "max(...args: any) -> any", category = "math")]
fn max_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        match (&args[0], &args[1]) {
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(*x.max(y))),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x.max(*y))),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float((*x as f64).max(*y))),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x.max(*y as f64))),
            (VmValue::Decimal(x), VmValue::Decimal(y)) => Ok(VmValue::Decimal((*x).max(*y))),
            _ => Ok(VmValue::Nil),
        }
    } else {
        Ok(VmValue::Nil)
    }
}

#[harn_builtin(sig = "floor(value: number) -> int", category = "math")]
fn floor_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => finite_float_to_i64(n.floor()).map(VmValue::Int),
        VmValue::Int(n) => Ok(VmValue::Int(*n)),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(sig = "ceil(value: number) -> int", category = "math")]
fn ceil_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => finite_float_to_i64(n.ceil()).map(VmValue::Int),
        VmValue::Int(n) => Ok(VmValue::Int(*n)),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(sig = "round(...args: any) -> any", category = "math")]
fn round_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => finite_float_to_i64(n.round()).map(VmValue::Int),
        VmValue::Int(n) => Ok(VmValue::Int(*n)),
        // Round to a whole-unit decimal (stays exact + keeps the decimal type),
        // rather than collapsing money to an int.
        VmValue::Decimal(d) => Ok(VmValue::Decimal(d.round())),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(sig = "sqrt(...args: any) -> any", category = "math")]
fn sqrt_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => Ok(VmValue::Float(n.sqrt())),
        VmValue::Int(n) => Ok(VmValue::Float((*n as f64).sqrt())),
        // No exact decimal square root; refuse rather than silently return nil.
        VmValue::Decimal(_) => Err(decimal_not_supported("sqrt")),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(sig = "pow(...args: any) -> any", category = "math")]
fn pow_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        match (&args[0], &args[1]) {
            (VmValue::Int(base), VmValue::Int(exp)) => {
                if u32::try_from(*exp).is_ok() {
                    match base.checked_pow(*exp as u32) {
                        Some(value) => Ok(VmValue::Int(value)),
                        None => Ok(VmValue::Float((*base as f64).powf(*exp as f64))),
                    }
                } else {
                    Ok(VmValue::Float((*base as f64).powf(*exp as f64)))
                }
            }
            (VmValue::Float(base), VmValue::Int(exp)) => {
                if i32::try_from(*exp).is_ok() {
                    Ok(VmValue::Float(base.powi(*exp as i32)))
                } else {
                    Ok(VmValue::Float(base.powf(*exp as f64)))
                }
            }
            (VmValue::Int(base), VmValue::Float(exp)) => {
                Ok(VmValue::Float((*base as f64).powf(*exp)))
            }
            (VmValue::Float(base), VmValue::Float(exp)) => Ok(VmValue::Float(base.powf(*exp))),
            // No exact decimal exponentiation; refuse rather than return nil.
            (VmValue::Decimal(_), _) | (_, VmValue::Decimal(_)) => {
                Err(decimal_not_supported("pow"))
            }
            _ => Ok(VmValue::Nil),
        }
    } else {
        Ok(VmValue::Nil)
    }
}

#[harn_builtin(sig = "rng_seed(...args: any) -> any", category = "math")]
fn rng_seed_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use rand::SeedableRng;
    let seed = args
        .first()
        .and_then(|arg| arg.as_int())
        .ok_or_else(|| VmError::TypeError("rng_seed(seed): seed must be an integer".to_string()))?;
    Ok(VmValue::rng(VmRngHandle {
        rng: Arc::new(Mutex::new(rand::rngs::StdRng::seed_from_u64(seed as u64))),
    }))
}

#[harn_builtin(sig = "random(...args: any) -> float", category = "math")]
fn random_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use rand::RngExt;
    let val: f64 = if let Some(VmValue::Rng(handle)) = args.first() {
        handle.rng.lock().random()
    } else {
        rand::rng().random()
    };
    Ok(VmValue::Float(val))
}

#[harn_builtin(sig = "random_int(...args: any) -> any", category = "math")]
fn random_int_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use rand::RngExt;
    let (rng, min_idx) = match args.first() {
        Some(VmValue::Rng(handle)) => (Some(handle), 1),
        _ => (None, 0),
    };
    if args.len() >= min_idx + 2 {
        let min = args[min_idx]
            .as_int()
            .ok_or_else(|| VmError::TypeError("random_int: min must be an integer".to_string()))?;
        let max = args[min_idx + 1]
            .as_int()
            .ok_or_else(|| VmError::TypeError("random_int: max must be an integer".to_string()))?;
        if min > max {
            return Ok(VmValue::Nil);
        }
        let val = if let Some(handle) = rng {
            handle.rng.lock().random_range(min..=max)
        } else {
            rand::rng().random_range(min..=max)
        };
        return Ok(VmValue::Int(val));
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "random_choice(...args: any) -> any", category = "math")]
fn random_choice_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
        handle.rng.lock().random_range(0..items.len())
    } else {
        rand::rng().random_range(0..items.len())
    };
    Ok(items[idx].clone())
}

#[harn_builtin(sig = "random_shuffle(...args: any) -> list", category = "math")]
fn random_shuffle_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
        shuffled.shuffle(&mut *handle.rng.lock());
    } else {
        shuffled.shuffle(&mut rand::rng());
    }
    Ok(VmValue::List(std::sync::Arc::new(shuffled)))
}

#[harn_builtin(sig = "mean(...args: any) -> float", category = "math")]
fn mean_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let values = numeric_list_arg(args, "mean")?;
    if values.is_empty() {
        return Ok(VmValue::Float(0.0));
    }
    Ok(VmValue::Float(
        values.iter().sum::<f64>() / values.len() as f64,
    ))
}

#[harn_builtin(sig = "median(...args: any) -> float", category = "math")]
fn median_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut values = non_empty_numeric_list_arg(args, "median")?;
    values.sort_by(|a, b| a.total_cmp(b));
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        Ok(VmValue::Float(values[mid]))
    } else {
        Ok(VmValue::Float(f64::midpoint(values[mid - 1], values[mid])))
    }
}

#[harn_builtin(sig = "percentile(...args: any) -> float", category = "math")]
fn percentile_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "variance(...args: any) -> float", category = "math")]
fn variance_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let values = non_empty_numeric_list_arg(args, "variance")?;
    let sample = args.get(1).is_some_and(VmValue::is_truthy);
    Ok(VmValue::Float(variance_for(&values, sample, "variance")?))
}

#[harn_builtin(sig = "stddev(...args: any) -> float", category = "math")]
fn stddev_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let values = non_empty_numeric_list_arg(args, "stddev")?;
    let sample = args.get(1).is_some_and(VmValue::is_truthy);
    Ok(VmValue::Float(
        variance_for(&values, sample, "stddev")?.sqrt(),
    ))
}

#[harn_builtin(sig = "sin(...args: any) -> float", category = "math")]
fn sin_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::sin)
}

#[harn_builtin(sig = "cos(value: number) -> float", category = "math")]
fn cos_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::cos)
}

#[harn_builtin(sig = "tan(...args: any) -> float", category = "math")]
fn tan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::tan)
}

#[harn_builtin(sig = "asin(value: number) -> float", category = "math")]
fn asin_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::asin)
}

#[harn_builtin(sig = "acos(value: number) -> float", category = "math")]
fn acos_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::acos)
}

#[harn_builtin(sig = "atan(value: number) -> float", category = "math")]
fn atan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::atan)
}

#[harn_builtin(sig = "log2(value: number) -> float", category = "math")]
fn log2_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::log2)
}

#[harn_builtin(sig = "log10(value: number) -> float", category = "math")]
fn log10_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::log10)
}

#[harn_builtin(sig = "ln(value: number) -> float", category = "math")]
fn ln_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::ln)
}

#[harn_builtin(sig = "exp(value: number) -> float", category = "math")]
fn exp_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, f64::exp)
}

#[harn_builtin(sig = "atan2(y: number, x: number) -> float", category = "math")]
fn atan2_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "sign(...args: any) -> int", category = "math")]
fn sign_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
        VmValue::Decimal(d) => Ok(VmValue::Int(match d.cmp(&rust_decimal::Decimal::ZERO) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        })),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(sig = "is_nan(value: number) -> bool", category = "math")]
fn is_nan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => Ok(VmValue::Bool(n.is_nan())),
        _ => Ok(VmValue::Bool(false)),
    }
}

#[harn_builtin(sig = "is_infinite(value: number) -> bool", category = "math")]
fn is_infinite_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => Ok(VmValue::Bool(n.is_infinite())),
        _ => Ok(VmValue::Bool(false)),
    }
}

#[harn_builtin(
    sig = "__range__(start: int, end: int, inclusive?: bool) -> any",
    runtime_only = true,
    category = "math"
)]
fn range_internal_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let start = args.first().and_then(|a| a.as_int()).unwrap_or(0);
    let end = args.get(1).and_then(|a| a.as_int()).unwrap_or(0);
    let inclusive = args.get(2).map(|a| a.is_truthy()).unwrap_or(false);
    Ok(VmValue::range(VmRange {
        start,
        end,
        inclusive,
    }))
}

// `range()` is Python-style and always half-open. Use `a to b [exclusive]`
// for human-readable inclusive math.
#[harn_builtin(sig = "range(...args: any) -> list", category = "math")]
fn range_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
    Ok(VmValue::range(VmRange {
        start,
        end,
        inclusive: false,
    }))
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

fn variance_for(values: &[f64], sample: bool, label: &str) -> Result<f64, VmError> {
    if sample && values.len() < 2 {
        return Err(VmError::Runtime(format!(
            "sample {label} requires at least 2 values"
        )));
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
    Ok(total / denom as f64)
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

/// Helper used by sin/cos/tan/etc. Coerces the first arg to `f64` (Int → cast),
/// applies `f`, and wraps in `VmValue::Float`. Non-numeric arg returns `Nil`
/// to preserve the existing closure semantics.
fn unary_float(args: &[VmValue], f: fn(f64) -> f64) -> Result<VmValue, VmError> {
    let n = match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => *n,
        VmValue::Int(n) => *n as f64,
        // Transcendental float math has no exact decimal result; refuse rather
        // than silently return nil.
        VmValue::Decimal(_) => return Err(decimal_not_supported("this function")),
        _ => return Ok(VmValue::Nil),
    };
    Ok(VmValue::Float(f(n)))
}

/// A `decimal` operand was passed to a float-only math builtin (sqrt, pow,
/// trig, …). These have no exact base-10 result, so we error with guidance
/// instead of silently returning `nil`.
fn decimal_not_supported(name: &str) -> VmError {
    VmError::TypeError(format!(
        "{name} is not defined for decimal; convert with to_float(value) first"
    ))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &ABS_IMPL_DEF,
    &MIN_IMPL_DEF,
    &MAX_IMPL_DEF,
    &FLOOR_IMPL_DEF,
    &CEIL_IMPL_DEF,
    &ROUND_IMPL_DEF,
    &SQRT_IMPL_DEF,
    &POW_IMPL_DEF,
    &RNG_SEED_IMPL_DEF,
    &RANDOM_IMPL_DEF,
    &RANDOM_INT_IMPL_DEF,
    &RANDOM_CHOICE_IMPL_DEF,
    &RANDOM_SHUFFLE_IMPL_DEF,
    &MEAN_IMPL_DEF,
    &MEDIAN_IMPL_DEF,
    &PERCENTILE_IMPL_DEF,
    &VARIANCE_IMPL_DEF,
    &STDDEV_IMPL_DEF,
    &SIN_IMPL_DEF,
    &COS_IMPL_DEF,
    &TAN_IMPL_DEF,
    &ASIN_IMPL_DEF,
    &ACOS_IMPL_DEF,
    &ATAN_IMPL_DEF,
    &LOG2_IMPL_DEF,
    &LOG10_IMPL_DEF,
    &LN_IMPL_DEF,
    &EXP_IMPL_DEF,
    &ATAN2_IMPL_DEF,
    &SIGN_IMPL_DEF,
    &IS_NAN_IMPL_DEF,
    &IS_INFINITE_IMPL_DEF,
    &RANGE_INTERNAL_IMPL_DEF,
    &RANGE_IMPL_DEF,
];

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

    #[test]
    fn stddev_sample_error_names_stddev() {
        let mut vm = vm();
        let values = VmValue::List(std::sync::Arc::new(vec![VmValue::Int(1)]));
        let error = call(&mut vm, "stddev", vec![values, VmValue::Bool(true)])
            .expect_err("sample stddev needs at least two values");
        assert!(error.to_string().contains("sample stddev"));
    }
}
