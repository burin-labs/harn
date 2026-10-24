use std::sync::Arc;

use crate::stdlib::args::Args;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmRange, VmRngHandle, VmValue};
use crate::vm::Vm;
use parking_lot::Mutex;

pub(crate) fn register_math_builtins(vm: &mut Vm) {
    crate::stdlib::macros::register_module_builtins(vm, module_path!());

    vm.set_global("pi", VmValue::Float(std::f64::consts::PI));
    vm.set_global("e", VmValue::Float(std::f64::consts::E));
}

harn_vm_core::stdlib_installer!(Pure, register_math_builtins);

#[harn_builtin(
    exposure = "pure",
    sig = "abs(value: number) -> number",
    category = "math"
)]
fn abs_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Int(i64::MIN) => Ok(VmValue::Float(9_223_372_036_854_775_808.0)),
        VmValue::Int(n) => Ok(VmValue::Int(n.abs())),
        VmValue::Float(n) => Ok(VmValue::Float(n.abs())),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(exposure = "pure", sig = "min(...args: any) -> any", category = "math")]
fn min_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(fold_extreme(args, std::cmp::Ordering::Less))
}

#[harn_builtin(exposure = "pure", sig = "max(...args: any) -> any", category = "math")]
fn max_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(fold_extreme(args, std::cmp::Ordering::Greater))
}

/// Folds `min`/`max` across every argument with numeric promotion. Fewer than
/// two arguments, or any non-numeric pair, yields nil.
fn fold_extreme(args: &[VmValue], keep: std::cmp::Ordering) -> VmValue {
    use rust_decimal::Decimal;
    if args.len() < 2 {
        return VmValue::Nil;
    }
    let pick = |x: f64, y: f64| if keep.is_lt() { x.min(y) } else { x.max(y) };
    let pick_decimal = |x: Decimal, y: Decimal| if x.cmp(&y) == keep { x } else { y };
    args[1..]
        .iter()
        .fold(args[0].clone(), |acc, next| match (&acc, next) {
            (VmValue::Int(x), VmValue::Int(y)) => {
                VmValue::Int(if x.cmp(y) == keep { *x } else { *y })
            }
            (VmValue::Float(x), VmValue::Float(y)) => VmValue::Float(pick(*x, *y)),
            (VmValue::Int(x), VmValue::Float(y)) => VmValue::Float(pick(*x as f64, *y)),
            (VmValue::Float(x), VmValue::Int(y)) => VmValue::Float(pick(*x, *y as f64)),
            (VmValue::Decimal(x), VmValue::Decimal(y)) => VmValue::decimal(pick_decimal(**x, **y)),
            (VmValue::Decimal(x), VmValue::Int(y)) => {
                VmValue::decimal(pick_decimal(**x, Decimal::from(*y)))
            }
            (VmValue::Int(x), VmValue::Decimal(y)) => {
                VmValue::decimal(pick_decimal(Decimal::from(*x), **y))
            }
            _ => VmValue::Nil,
        })
}

#[harn_builtin(
    exposure = "pure",
    sig = "floor(value: number | decimal) -> int",
    category = "math"
)]
fn floor_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => finite_float_to_i64(n.floor()).map(VmValue::Int),
        VmValue::Int(n) => Ok(VmValue::Int(*n)),
        VmValue::Decimal(d) => decimal_to_i64(d.floor()).map(VmValue::Int),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "ceil(value: number | decimal) -> int",
    category = "math"
)]
fn ceil_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => finite_float_to_i64(n.ceil()).map(VmValue::Int),
        VmValue::Int(n) => Ok(VmValue::Int(*n)),
        VmValue::Decimal(d) => decimal_to_i64(d.ceil()).map(VmValue::Int),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "round(value: number | decimal, digits?: int) -> number | decimal",
    category = "math"
)]
fn round_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let digits = Args::new("round", args).opt_int(1, "digits")?;
    match (args.first().unwrap_or(&VmValue::Nil), digits) {
        // 1-arg form: nearest integer, halves away from zero.
        (VmValue::Float(n), None) => finite_float_to_i64(n.round()).map(VmValue::Int),
        (VmValue::Int(n), None) => Ok(VmValue::Int(*n)),
        // Round to a whole-unit decimal (stays exact + keeps the decimal type),
        // rather than collapsing money to an int. Note: decimal keeps
        // rust_decimal's banker's rounding for the whole-unit form.
        (VmValue::Decimal(d), None) => Ok(VmValue::decimal(d.round())),
        // 2-arg form: round to `digits` decimal places (negative digits round
        // to tens / hundreds / ...). Floats stay floats and keep the 1-arg
        // half-away-from-zero midpoint rule; ints stay ints when possible.
        (VmValue::Float(n), Some(d)) => Ok(VmValue::Float(round_float_to_digits(*n, d))),
        (VmValue::Int(n), Some(d)) => Ok(round_int_to_digits(*n, d)),
        (VmValue::Decimal(x), Some(d)) => Ok(VmValue::decimal(round_decimal_to_digits(**x, d))),
        _ => Ok(VmValue::Nil),
    }
}

/// `round(x, digits)` for floats: half-away-from-zero at the requested
/// decimal place, implemented by scale → `f64::round` → unscale. Non-finite
/// inputs pass through unchanged.
fn round_float_to_digits(x: f64, digits: i64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    if digits == 0 {
        return x.round();
    }
    // f64 decimal exponents span roughly ±308; beyond that the scale factor
    // is not representable, and the answer is degenerate anyway: rounding to
    // more fractional digits than the value resolves is the identity, and
    // rounding to a coarser bucket than the largest float magnitude is zero.
    if digits > 308 {
        return x;
    }
    if digits < -308 {
        return 0.0 * x.signum();
    }
    let factor = 10f64.powi(digits as i32);
    let scaled = x * factor;
    if !scaled.is_finite() {
        // Scaling overflowed (huge value, large positive `digits`): every
        // such value is already integral well past `digits` places.
        return x;
    }
    scaled.round() / factor
}

/// `round(n, digits)` for ints: identity for `digits >= 0`; negative digits
/// round to the nearest power-of-ten bucket, halves away from zero. Uses
/// exact i128 arithmetic; a result outside the i64 range promotes to float,
/// matching integer arithmetic overflow behavior elsewhere.
fn round_int_to_digits(n: i64, digits: i64) -> VmValue {
    if digits >= 0 || n == 0 {
        return VmValue::Int(n);
    }
    if digits <= -19 {
        // 10^19 exceeds i64::MAX, so every i64 rounds to the zero bucket.
        return VmValue::Int(0);
    }
    let factor = 10i128.pow((-digits) as u32);
    let n128 = n as i128;
    let rem = n128 % factor;
    let base = n128 - rem;
    let rounded = if rem.abs() * 2 >= factor {
        base + factor * n128.signum()
    } else {
        base
    };
    match i64::try_from(rounded) {
        Ok(v) => VmValue::Int(v),
        Err(_) => VmValue::Float(rounded as f64),
    }
}

/// `round(d, digits)` for decimals: keeps the decimal type and rust_decimal's
/// banker's (midpoint-nearest-even) rounding, consistent with the 1-arg
/// `round(decimal)` form. Negative digits round to power-of-ten buckets via
/// exact scale/unscale.
fn round_decimal_to_digits(x: rust_decimal::Decimal, digits: i64) -> rust_decimal::Decimal {
    use rust_decimal::Decimal;
    if digits >= 0 {
        // Decimal carries at most 28 fractional digits, so any larger request
        // is the identity; `round_dp` already treats it that way.
        let dp = u32::try_from(digits).unwrap_or(u32::MAX).min(28);
        return x.round_dp(dp);
    }
    let neg = (-digits) as u32;
    // 96-bit decimals top out just under 8e28, so any coarser bucket is zero.
    if neg > 28 {
        return Decimal::ZERO;
    }
    let factor = Decimal::from_i128_with_scale(10i128.pow(neg), 0);
    match x.checked_div(factor).map(|scaled| scaled.round()) {
        Some(rounded) => rounded.checked_mul(factor).unwrap_or(x),
        None => x,
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "sqrt(...args: any) -> any",
    category = "math"
)]
fn sqrt_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => Ok(VmValue::Float(n.sqrt())),
        VmValue::Int(n) => Ok(VmValue::Float((*n as f64).sqrt())),
        // No exact decimal square root; refuse rather than silently return nil.
        VmValue::Decimal(_) => Err(decimal_not_supported("sqrt")),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(exposure = "pure", sig = "pow(...args: any) -> any", category = "math")]
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

#[harn_builtin(
    exposure = "harness.random.seed",
    effects = ["random.mutate@const=generator"],
    sig = "rng_seed(seed: int) -> HarnessRandom", category = "math"
)]
fn rng_seed_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use rand::SeedableRng;
    let seed = Args::new("rng_seed", args).int(0, "seed")?;
    Ok(VmValue::rng(VmRngHandle {
        rng: Arc::new(Mutex::new(rand::rngs::StdRng::seed_from_u64(seed as u64))),
    }))
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "random(...args: any) -> float",
    category = "math"
)]
fn random_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    random_call(crate::value::RandomOperation::Float, args)
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "random_int(...args: any) -> any",
    category = "math"
)]
fn random_int_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    random_call(crate::value::RandomOperation::Range, args)
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "random_choice(...args: any) -> any",
    category = "math"
)]
fn random_choice_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    random_call(crate::value::RandomOperation::Choice, args)
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "random_shuffle(...args: any) -> list",
    category = "math"
)]
fn random_shuffle_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    random_call(crate::value::RandomOperation::Shuffle, args)
}

fn random_call(
    operation: crate::value::RandomOperation,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    match args.split_first() {
        Some((VmValue::Rng(handle), args)) => operation.apply(&mut *handle.rng.lock(), args),
        _ => operation.apply(&mut rand::rng(), args),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "mean(...args: any) -> float",
    category = "math"
)]
fn mean_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let values = numeric_list_arg(args, "mean")?;
    if values.is_empty() {
        return Ok(VmValue::Float(0.0));
    }
    Ok(VmValue::Float(
        values.iter().sum::<f64>() / values.len() as f64,
    ))
}

#[harn_builtin(
    exposure = "pure",
    sig = "median(...args: any) -> float",
    category = "math"
)]
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

#[harn_builtin(
    exposure = "pure",
    sig = "percentile(...args: any) -> float",
    category = "math"
)]
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

#[harn_builtin(
    exposure = "pure",
    sig = "variance(...args: any) -> float",
    category = "math"
)]
fn variance_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let values = non_empty_numeric_list_arg(args, "variance")?;
    let sample = args.get(1).is_some_and(VmValue::is_truthy);
    Ok(VmValue::Float(variance_for(&values, sample, "variance")?))
}

#[harn_builtin(
    exposure = "pure",
    sig = "stddev(...args: any) -> float",
    category = "math"
)]
fn stddev_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let values = non_empty_numeric_list_arg(args, "stddev")?;
    let sample = args.get(1).is_some_and(VmValue::is_truthy);
    Ok(VmValue::Float(
        variance_for(&values, sample, "stddev")?.sqrt(),
    ))
}

#[harn_builtin(
    exposure = "pure",
    sig = "sin(...args: any) -> float",
    category = "math"
)]
fn sin_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "sin", f64::sin)
}

#[harn_builtin(
    exposure = "pure",
    sig = "cos(value: number) -> float",
    category = "math"
)]
fn cos_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "cos", f64::cos)
}

#[harn_builtin(
    exposure = "pure",
    sig = "tan(...args: any) -> float",
    category = "math"
)]
fn tan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "tan", f64::tan)
}

#[harn_builtin(
    exposure = "pure",
    sig = "asin(value: number) -> float",
    category = "math"
)]
fn asin_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "asin", f64::asin)
}

#[harn_builtin(
    exposure = "pure",
    sig = "acos(value: number) -> float",
    category = "math"
)]
fn acos_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "acos", f64::acos)
}

#[harn_builtin(
    exposure = "pure",
    sig = "atan(value: number) -> float",
    category = "math"
)]
fn atan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "atan", f64::atan)
}

#[harn_builtin(
    exposure = "pure",
    sig = "log2(value: number) -> float",
    category = "math"
)]
fn log2_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "log2", f64::log2)
}

#[harn_builtin(
    exposure = "pure",
    sig = "log10(value: number) -> float",
    category = "math"
)]
fn log10_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "log10", f64::log10)
}

#[harn_builtin(
    exposure = "pure",
    sig = "ln(value: number) -> float",
    category = "math"
)]
fn ln_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "ln", f64::ln)
}

#[harn_builtin(
    exposure = "pure",
    sig = "exp(value: number) -> float",
    category = "math"
)]
fn exp_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    unary_float(args, "exp", f64::exp)
}

#[harn_builtin(
    exposure = "pure",
    sig = "atan2(y: number, x: number) -> float",
    category = "math"
)]
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

#[harn_builtin(
    exposure = "pure",
    sig = "sign(...args: any) -> int",
    category = "math"
)]
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
        VmValue::Decimal(d) => Ok(VmValue::Int(
            match (**d).cmp(&rust_decimal::Decimal::ZERO) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            },
        )),
        _ => Ok(VmValue::Nil),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "is_nan(value: number) -> bool",
    category = "math"
)]
fn is_nan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => Ok(VmValue::Bool(n.is_nan())),
        _ => Ok(VmValue::Bool(false)),
    }
}

#[harn_builtin(
    exposure = "pure",
    sig = "is_infinite(value: number) -> bool",
    category = "math"
)]
fn is_infinite_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => Ok(VmValue::Bool(n.is_infinite())),
        _ => Ok(VmValue::Bool(false)),
    }
}

#[harn_builtin(
    exposure = "runtime_internal",
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
#[harn_builtin(
    exposure = "pure",
    sig = "range(...args: any) -> range",
    category = "math"
)]
fn range_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bounds = Args::new("range", args);
    let (start, end) = match args.len() {
        1 => (0, bounds.int(0, "end")?),
        2 => (bounds.int(0, "start")?, bounds.int(1, "end")?),
        _ => {
            bounds.arity(1, 2)?;
            unreachable!("arity rejects every length outside 1..=2");
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
fn unary_float(args: &[VmValue], name: &str, f: fn(f64) -> f64) -> Result<VmValue, VmError> {
    let n = match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Float(n) => *n,
        VmValue::Int(n) => *n as f64,
        // Transcendental float math has no exact decimal result; refuse rather
        // than silently return nil.
        VmValue::Decimal(_) => return Err(decimal_not_supported(name)),
        _ => return Ok(VmValue::Nil),
    };
    Ok(VmValue::Float(f(n)))
}

fn decimal_to_i64(d: rust_decimal::Decimal) -> Result<i64, VmError> {
    use rust_decimal::prelude::ToPrimitive;
    d.to_i64().ok_or_else(|| {
        VmError::Runtime("decimal is outside the representable int range".to_string())
    })
}

/// A `decimal` operand was passed to a float-only math builtin (sqrt, pow,
/// trig, …). These have no exact base-10 result, so we error with guidance
/// instead of silently returning `nil`.
fn decimal_not_supported(name: &str) -> VmError {
    VmError::TypeError(format!(
        "{name} is not defined for decimal; convert with to_float(value) first"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> Vm {
        let mut vm = crate::new_vm();
        register_math_builtins(&mut vm);
        vm
    }

    fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let f = vm.builtin_handler_for_test(name).unwrap().clone();
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
    fn round_digits_float_half_away_from_zero() {
        assert_eq!(round_float_to_digits(2.567, 2), 2.57);
        assert_eq!(round_float_to_digits(-2.567, 2), -2.57);
        // Exactly representable midpoints round away from zero.
        assert_eq!(round_float_to_digits(1.25, 1), 1.3);
        assert_eq!(round_float_to_digits(-1.25, 1), -1.3);
        // Binary-arithmetic noise just above the target still lands on it.
        assert_eq!(round_float_to_digits(0.1 + 0.2, 2), 0.3);
        // Negative digits round to power-of-ten buckets.
        assert_eq!(round_float_to_digits(1234.5678, -2), 1200.0);
        assert_eq!(round_float_to_digits(1250.0, -2), 1300.0);
        // Extreme digit counts degrade gracefully.
        assert_eq!(round_float_to_digits(1.5, 400), 1.5);
        assert_eq!(round_float_to_digits(1.5, -400), 0.0);
        assert!(round_float_to_digits(f64::NAN, 2).is_nan());
        assert_eq!(round_float_to_digits(f64::INFINITY, 2), f64::INFINITY);
    }

    #[test]
    fn round_digits_int_negative_buckets() {
        assert!(matches!(round_int_to_digits(1250, -2), VmValue::Int(1300)));
        assert!(matches!(
            round_int_to_digits(-1250, -2),
            VmValue::Int(-1300)
        ));
        assert!(matches!(round_int_to_digits(1249, -2), VmValue::Int(1200)));
        assert!(matches!(round_int_to_digits(7, 3), VmValue::Int(7)));
        assert!(matches!(round_int_to_digits(123, -19), VmValue::Int(0)));
        // A bucket boundary past i64::MAX promotes to float instead of wrapping
        // (i64::MAX rounds up to 9_223_400_000_000_000_000 at -14 digits).
        assert!(matches!(round_int_to_digits(i64::MAX, -14), VmValue::Float(f) if f == 9.2234e18));
    }

    #[test]
    fn stddev_sample_error_names_stddev() {
        let mut vm = vm();
        let values = VmValue::list(vec![VmValue::Int(1)]);
        let error = call(&mut vm, "stddev", vec![values, VmValue::Bool(true)])
            .expect_err("sample stddev needs at least two values");
        assert!(error.to_string().contains("sample stddev"));
    }

    #[test]
    fn decimal_trig_error_names_the_builtin() {
        let mut vm = vm();
        let value = VmValue::decimal(rust_decimal::Decimal::from(1));
        let error = call(&mut vm, "sin", vec![value]).expect_err("decimal sin is refused");
        let message = error.to_string();
        assert!(
            message.contains("sin is not defined for decimal"),
            "{message}"
        );
    }
}
