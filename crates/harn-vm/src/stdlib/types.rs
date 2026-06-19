use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{string_char_count, VmError, VmValue};
use crate::vm::Vm;

const I64_FLOAT_UPPER_BOUND_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;

pub(crate) fn register_type_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "type_of(...args: any) -> string", category = "types")]
fn type_of_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    Ok(VmValue::String(std::sync::Arc::from(val.type_name())))
}

#[harn_builtin(sig = "to_string(...args: any) -> string", category = "types")]
fn to_string_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    Ok(VmValue::String(std::sync::Arc::from(val.display())))
}

#[harn_builtin(sig = "to_int(...args: any) -> int", category = "types")]
fn to_int_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    match val {
        VmValue::Int(n) => Ok(VmValue::Int(*n)),
        VmValue::Float(n) => Ok(float_to_int(*n).map(VmValue::Int).unwrap_or(VmValue::Nil)),
        // Truncates toward zero, like the float path.
        VmValue::Decimal(d) => {
            use rust_decimal::prelude::ToPrimitive;
            Ok(d.trunc().to_i64().map(VmValue::Int).unwrap_or(VmValue::Nil))
        }
        VmValue::Bool(value) => Ok(VmValue::Int(i64::from(*value))),
        // Trim surrounding whitespace before parsing, matching `decimal(...)`
        // and Python's `int(" 42 ")`/JS `Number(" 42 ")`. A string is exactly
        // the case `std/coerce` exists for (numbers rendered by an LLM/JSON),
        // and a stray newline should not silently produce `nil`.
        VmValue::String(s) => Ok(s
            .trim()
            .parse::<i64>()
            .map(VmValue::Int)
            .unwrap_or(VmValue::Nil)),
        _ => Ok(VmValue::Nil),
    }
}

fn float_to_int(value: f64) -> Option<i64> {
    if !value.is_finite() || value < i64::MIN as f64 || value >= I64_FLOAT_UPPER_BOUND_EXCLUSIVE {
        return None;
    }
    Some(value as i64)
}

#[harn_builtin(sig = "to_float(...args: any) -> float", category = "types")]
fn to_float_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    match val {
        VmValue::Float(n) => Ok(VmValue::Float(*n)),
        VmValue::Int(n) => Ok(VmValue::Float(*n as f64)),
        // Lossy: a 96-bit decimal may not be exactly representable as f64.
        VmValue::Decimal(d) => {
            use rust_decimal::prelude::ToPrimitive;
            Ok(d.to_f64().map(VmValue::Float).unwrap_or(VmValue::Nil))
        }
        VmValue::String(s) => Ok(s
            .trim()
            .parse::<f64>()
            .map(VmValue::Float)
            .unwrap_or(VmValue::Nil)),
        _ => Ok(VmValue::Nil),
    }
}

/// Construct an exact decimal. Unlike `to_int`/`to_float` (which return `nil`
/// on a bad value), `decimal` THROWS on un-parseable input, because silently
/// dropping a money value to `nil` is dangerous. Accepts a string (exact
/// parse), an int (exact), a float (explicit opt-in to the lossy binary→decimal
/// conversion), or a decimal (identity).
#[harn_builtin(
    sig = "decimal(value: string | int | float | decimal) -> decimal",
    category = "types"
)]
fn decimal_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    fn throw(message: String) -> VmError {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(message)))
    }
    let val = args.first().unwrap_or(&VmValue::Nil);
    match val {
        VmValue::Decimal(d) => Ok(VmValue::Decimal(*d)),
        VmValue::Int(n) => Ok(VmValue::Decimal(rust_decimal::Decimal::from(*n))),
        VmValue::String(s) => s
            .trim()
            .parse::<rust_decimal::Decimal>()
            .map(VmValue::Decimal)
            .map_err(|_| throw(format!("decimal: cannot parse {s:?} as a decimal"))),
        VmValue::Float(f) => rust_decimal::Decimal::from_f64_retain(*f)
            .map(VmValue::Decimal)
            .ok_or_else(|| throw(format!("decimal: cannot represent {f} as a decimal"))),
        other => Err(throw(format!(
            "decimal: cannot convert {} to a decimal",
            other.type_name()
        ))),
    }
}

#[harn_builtin(
    sig = "Ok(value?: any) -> any",
    runtime_only = true,
    category = "types"
)]
fn ok_ctor_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().cloned().unwrap_or(VmValue::Nil);
    Ok(VmValue::enum_variant("Result", "Ok", vec![val]))
}

#[harn_builtin(
    sig = "Err(value?: any) -> any",
    runtime_only = true,
    category = "types"
)]
fn err_ctor_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().cloned().unwrap_or(VmValue::Nil);
    Ok(VmValue::enum_variant("Result", "Err", vec![val]))
}

#[harn_builtin(sig = "is_ok(value: any) -> bool", category = "types")]
fn is_ok_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    Ok(VmValue::Bool(matches!(
        val,
        VmValue::EnumVariant(enum_variant)
        if enum_variant.is_variant("Result", "Ok")
    )))
}

#[harn_builtin(sig = "is_err(value: any) -> bool", category = "types")]
fn is_err_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    Ok(VmValue::Bool(matches!(
        val,
        VmValue::EnumVariant(enum_variant)
        if enum_variant.is_variant("Result", "Err")
    )))
}

#[harn_builtin(sig = "unwrap(...args: any) -> any", category = "types")]
fn unwrap_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    match val {
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Ok") => {
            Ok(enum_variant.fields.first().cloned().unwrap_or(VmValue::Nil))
        }
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Err") => {
            let msg = enum_variant
                .fields
                .first()
                .map(|f| f.display())
                .unwrap_or_default();
            Err(VmError::Runtime(format!("unwrap called on Err: {msg}")))
        }
        _ => Ok(val.clone()),
    }
}

#[harn_builtin(sig = "unwrap_or(...args: any) -> any", category = "types")]
fn unwrap_or_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
    match val {
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Ok") => {
            Ok(enum_variant.fields.first().cloned().unwrap_or(VmValue::Nil))
        }
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Err") => {
            Ok(default)
        }
        _ => Ok(val.clone()),
    }
}

#[harn_builtin(sig = "unwrap_err(...args: any) -> any", category = "types")]
fn unwrap_err_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    match val {
        VmValue::EnumVariant(enum_variant) if enum_variant.is_variant("Result", "Err") => {
            Ok(enum_variant.fields.first().cloned().unwrap_or(VmValue::Nil))
        }
        _ => Err(VmError::Runtime("unwrap_err called on non-Err".into())),
    }
}

#[harn_builtin(sig = "unreachable(...args: any) -> never", category = "types")]
fn unreachable_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let msg = match args.first() {
        Some(val) => format!("unreachable code was reached: {}", val.display()),
        None => "unreachable code was reached".to_string(),
    };
    Err(VmError::Runtime(msg))
}

#[harn_builtin(sig = "to_list(...args: any) -> list", category = "types")]
fn to_list_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::Set(s) => Ok(VmValue::List(s.shared_items())),
        VmValue::List(l) => Ok(VmValue::List(l.clone())),
        other => Ok(VmValue::List(std::sync::Arc::new(vec![other.clone()]))),
    }
}

#[harn_builtin(
    sig = "len(value: string | bytes | list | dict | set | range | nil) -> int",
    category = "types"
)]
fn len_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match args.first().unwrap_or(&VmValue::Nil) {
        VmValue::String(s) => Ok(VmValue::Int(string_char_count(s) as i64)),
        VmValue::Bytes(bytes) => Ok(VmValue::Int(bytes.len() as i64)),
        VmValue::List(items) => Ok(VmValue::Int(items.len() as i64)),
        VmValue::Dict(map) => Ok(VmValue::Int(map.len() as i64)),
        VmValue::Set(s) => Ok(VmValue::Int(s.len() as i64)),
        VmValue::Range(r) => Ok(VmValue::Int(r.len())),
        _ => Ok(VmValue::Int(0)),
    }
}

// `==` is structural. `is_same` is identity (Arc::ptr_eq for heap values);
// for primitive scalars it reduces to structural equality.
#[harn_builtin(sig = "is_same(a: any, b: any) -> bool", category = "types")]
fn is_same_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let a = args.first().unwrap_or(&VmValue::Nil);
    let b = args.get(1).unwrap_or(&VmValue::Nil);
    Ok(VmValue::Bool(crate::value::values_identical(a, b)))
}

// Stable identity key — differs iff two values live at different heap
// allocations. For hashing by identity rather than structure; primitives
// return their display() text.
#[harn_builtin(sig = "addr_of(value: any) -> string", category = "types")]
fn addr_of_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let v = args.first().unwrap_or(&VmValue::Nil);
    Ok(VmValue::String(std::sync::Arc::from(
        crate::value::value_identity_key(v),
    )))
}

// `drop(handle)` — close a stdlib handle deterministically. Dispatch is by
// runtime value tag: each handle variant maps to its existing close verb
// (`Channel` → mark closed, `SyncPermit` → release). Non-drop values are a
// silent no-op so callers can hand `drop` any value without guarding.
// `owned<T>` bindings call this implicitly at scope exit via a synthetic
// `defer { drop(<binding>) }`.
#[harn_builtin(sig = "drop(handle: any) -> nil", category = "types")]
fn drop_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let v = args.first().unwrap_or(&VmValue::Nil);
    match v {
        VmValue::Channel(ch) => {
            ch.close();
        }
        VmValue::SyncPermit(permit) => {
            permit.release();
        }
        _ => {}
    }
    Ok(VmValue::Nil)
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &TYPE_OF_IMPL_DEF,
    &TO_STRING_IMPL_DEF,
    &TO_INT_IMPL_DEF,
    &TO_FLOAT_IMPL_DEF,
    &DECIMAL_IMPL_DEF,
    &OK_CTOR_IMPL_DEF,
    &ERR_CTOR_IMPL_DEF,
    &IS_OK_IMPL_DEF,
    &IS_ERR_IMPL_DEF,
    &UNWRAP_IMPL_DEF,
    &UNWRAP_OR_IMPL_DEF,
    &UNWRAP_ERR_IMPL_DEF,
    &UNREACHABLE_IMPL_DEF,
    &TO_LIST_IMPL_DEF,
    &LEN_IMPL_DEF,
    &IS_SAME_IMPL_DEF,
    &ADDR_OF_IMPL_DEF,
    &DROP_IMPL_DEF,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_int_converts_bools() {
        assert_eq!(
            to_int_impl(&[VmValue::Bool(true)], &mut String::new())
                .unwrap()
                .as_int(),
            Some(1)
        );
        assert_eq!(
            to_int_impl(&[VmValue::Bool(false)], &mut String::new())
                .unwrap()
                .as_int(),
            Some(0)
        );
    }

    #[test]
    fn to_int_rejects_non_finite_and_out_of_range_floats() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1.0e30] {
            let converted = to_int_impl(&[VmValue::Float(value)], &mut String::new()).unwrap();
            assert!(matches!(converted, VmValue::Nil));
        }
    }

    #[test]
    fn to_int_and_to_float_trim_surrounding_whitespace() {
        let s = |text: &str| VmValue::String(std::sync::Arc::from(text));
        assert!(matches!(
            to_int_impl(&[s("  42  ")], &mut String::new()).unwrap(),
            VmValue::Int(42)
        ));
        assert!(matches!(
            to_int_impl(&[s("42\n")], &mut String::new()).unwrap(),
            VmValue::Int(42)
        ));
        match to_float_impl(&[s("  1.5  ")], &mut String::new()).unwrap() {
            VmValue::Float(f) => assert!((f - 1.5).abs() < 1e-9),
            other => panic!("expected 1.5, got {other:?}"),
        }
        // Non-numeric strings still return nil.
        assert!(matches!(
            to_int_impl(&[s("nope")], &mut String::new()).unwrap(),
            VmValue::Nil
        ));
    }
}
