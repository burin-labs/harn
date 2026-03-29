use crate::value::VmValue;
use crate::vm::Vm;

pub(crate) fn register_math_builtins(vm: &mut Vm) {
    vm.register_builtin("abs", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Int(n) => Ok(VmValue::Int(n.wrapping_abs())),
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
            VmValue::Float(n) => Ok(VmValue::Int(n.floor() as i64)),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("ceil", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Int(n.ceil() as i64)),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("round", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Int(n.round() as i64)),
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
                        Ok(VmValue::Int(base.wrapping_pow(*exp as u32)))
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

    vm.register_builtin("random", |_args, _out| {
        use rand::Rng;
        let val: f64 = rand::thread_rng().gen();
        Ok(VmValue::Float(val))
    });

    vm.register_builtin("random_int", |args, _out| {
        use rand::Rng;
        if args.len() >= 2 {
            let min = args[0].as_int().unwrap_or(0);
            let max = args[1].as_int().unwrap_or(0);
            if min <= max {
                let val = rand::thread_rng().gen_range(min..=max);
                return Ok(VmValue::Int(val));
            }
        }
        Ok(VmValue::Nil)
    });

    // --- Trigonometric and transcendental ---

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

    vm.register_builtin("pi", |_args, _out| Ok(VmValue::Float(std::f64::consts::PI)));
    vm.register_builtin("e", |_args, _out| Ok(VmValue::Float(std::f64::consts::E)));

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
        let items: Vec<VmValue> = if inclusive {
            (start..=end).map(VmValue::Int).collect()
        } else {
            (start..end).map(VmValue::Int).collect()
        };
        Ok(VmValue::List(std::rc::Rc::new(items)))
    });
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
