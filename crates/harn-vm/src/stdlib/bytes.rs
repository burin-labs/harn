use std::rc::Rc;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn runtime_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(message.into())
}

fn expect_bytes<'a>(args: &'a [VmValue], index: usize, builtin: &str) -> Result<&'a [u8], VmError> {
    match args.get(index) {
        Some(VmValue::Bytes(bytes)) => Ok(bytes.as_slice()),
        Some(other) => Err(runtime_error(format!(
            "{builtin}: expected bytes at argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(runtime_error(format!(
            "{builtin}: missing argument {}",
            index + 1
        ))),
    }
}

fn expect_string<'a>(args: &'a [VmValue], index: usize, builtin: &str) -> Result<&'a str, VmError> {
    match args.get(index) {
        Some(VmValue::String(text)) => Ok(text.as_ref()),
        Some(other) => Err(runtime_error(format!(
            "{builtin}: expected string at argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(runtime_error(format!(
            "{builtin}: missing argument {}",
            index + 1
        ))),
    }
}

fn expect_int(args: &[VmValue], index: usize, builtin: &str) -> Result<i64, VmError> {
    match args.get(index) {
        Some(VmValue::Int(value)) => Ok(*value),
        Some(other) => Err(runtime_error(format!(
            "{builtin}: expected int at argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(runtime_error(format!(
            "{builtin}: missing argument {}",
            index + 1
        ))),
    }
}

pub(crate) fn register_bytes_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "bytes_from_string(text: string) -> bytes", category = "bytes")]
fn bytes_from_string_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = expect_string(args, 0, "bytes_from_string")?;
    Ok(VmValue::Bytes(Rc::new(text.as_bytes().to_vec())))
}

#[harn_builtin(sig = "bytes_to_string(input: bytes) -> string", category = "bytes")]
fn bytes_to_string_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes(args, 0, "bytes_to_string")?;
    let text = std::str::from_utf8(bytes)
        .map_err(|error| runtime_error(format!("bytes_to_string: {error}")))?;
    Ok(VmValue::String(Rc::from(text)))
}

#[harn_builtin(
    sig = "bytes_to_string_lossy(input: bytes) -> string",
    category = "bytes"
)]
fn bytes_to_string_lossy_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes(args, 0, "bytes_to_string_lossy")?;
    Ok(VmValue::String(Rc::from(
        String::from_utf8_lossy(bytes).into_owned(),
    )))
}

#[harn_builtin(sig = "bytes_to_hex(input: bytes) -> string", category = "bytes")]
fn bytes_to_hex_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes(args, 0, "bytes_to_hex")?;
    Ok(VmValue::String(Rc::from(hex::encode(bytes))))
}

#[harn_builtin(sig = "bytes_from_hex(text: string) -> bytes", category = "bytes")]
fn bytes_from_hex_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = expect_string(args, 0, "bytes_from_hex")?;
    let bytes =
        hex::decode(text).map_err(|error| runtime_error(format!("bytes_from_hex: {error}")))?;
    Ok(VmValue::Bytes(Rc::new(bytes)))
}

#[harn_builtin(sig = "bytes_to_base64(input: bytes) -> string", category = "bytes")]
fn bytes_to_base64_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use base64::Engine;

    let bytes = expect_bytes(args, 0, "bytes_to_base64")?;
    Ok(VmValue::String(Rc::from(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    )))
}

#[harn_builtin(sig = "bytes_from_base64(text: string) -> bytes", category = "bytes")]
fn bytes_from_base64_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use base64::Engine;

    let text = expect_string(args, 0, "bytes_from_base64")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.as_bytes())
        .map_err(|error| runtime_error(format!("bytes_from_base64: {error}")))?;
    Ok(VmValue::Bytes(Rc::new(bytes)))
}

#[harn_builtin(sig = "bytes_len(input: bytes) -> int", category = "bytes")]
fn bytes_len_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes(args, 0, "bytes_len")?;
    Ok(VmValue::Int(bytes.len() as i64))
}

#[harn_builtin(
    sig = "bytes_concat(left: bytes, right: bytes) -> bytes",
    category = "bytes"
)]
fn bytes_concat_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let left = expect_bytes(args, 0, "bytes_concat")?;
    let right = expect_bytes(args, 1, "bytes_concat")?;
    let mut out = Vec::with_capacity(left.len() + right.len());
    out.extend_from_slice(left);
    out.extend_from_slice(right);
    Ok(VmValue::Bytes(Rc::new(out)))
}

#[harn_builtin(
    sig = "bytes_slice(input: bytes, start: int, end: int) -> bytes",
    category = "bytes"
)]
fn bytes_slice_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes(args, 0, "bytes_slice")?;
    let len = bytes.len() as i64;
    let start = expect_int(args, 1, "bytes_slice")?.clamp(0, len) as usize;
    let end = expect_int(args, 2, "bytes_slice")?.clamp(0, len) as usize;
    let slice = if start >= end {
        Vec::new()
    } else {
        bytes[start..end].to_vec()
    };
    Ok(VmValue::Bytes(Rc::new(slice)))
}

#[harn_builtin(
    sig = "bytes_eq(left: bytes, right: bytes) -> bool",
    category = "bytes"
)]
fn bytes_eq_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use subtle::ConstantTimeEq;

    let left = expect_bytes(args, 0, "bytes_eq")?;
    let right = expect_bytes(args, 1, "bytes_eq")?;
    Ok(VmValue::Bool(bool::from(left.ct_eq(right))))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &BYTES_FROM_STRING_IMPL_DEF,
    &BYTES_TO_STRING_IMPL_DEF,
    &BYTES_TO_STRING_LOSSY_IMPL_DEF,
    &BYTES_TO_HEX_IMPL_DEF,
    &BYTES_FROM_HEX_IMPL_DEF,
    &BYTES_TO_BASE64_IMPL_DEF,
    &BYTES_FROM_BASE64_IMPL_DEF,
    &BYTES_LEN_IMPL_DEF,
    &BYTES_CONCAT_IMPL_DEF,
    &BYTES_SLICE_IMPL_DEF,
    &BYTES_EQ_IMPL_DEF,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> Vm {
        let mut vm = Vm::new();
        register_bytes_builtins(&mut vm);
        vm
    }

    fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let f = vm.builtins.get(name).unwrap().clone();
        let mut out = String::new();
        f(&args, &mut out)
    }

    fn s(v: &str) -> VmValue {
        VmValue::String(Rc::from(v))
    }

    fn b(v: &[u8]) -> VmValue {
        VmValue::Bytes(Rc::new(v.to_vec()))
    }

    #[test]
    fn bytes_round_trip_utf8() {
        let mut vm = vm();
        let bytes = call(&mut vm, "bytes_from_string", vec![s("héllo")]).unwrap();
        let text = call(&mut vm, "bytes_to_string", vec![bytes]).unwrap();
        assert_eq!(text.display(), "héllo");
    }

    #[test]
    fn bytes_hex_round_trip() {
        let mut vm = vm();
        let bytes = call(&mut vm, "bytes_from_hex", vec![s("0001ff")]).unwrap();
        let hex = call(&mut vm, "bytes_to_hex", vec![bytes]).unwrap();
        assert_eq!(hex.display(), "0001ff");
    }

    #[test]
    fn bytes_base64_round_trip() {
        let mut vm = vm();
        let encoded = call(&mut vm, "bytes_to_base64", vec![b(&[0, 1, 2, 255])]).unwrap();
        let decoded = call(&mut vm, "bytes_from_base64", vec![encoded]).unwrap();
        assert_eq!(decoded.as_bytes().unwrap(), &[0, 1, 2, 255]);
    }

    #[test]
    fn bytes_slice_clamps() {
        let mut vm = vm();
        let sliced = call(
            &mut vm,
            "bytes_slice",
            vec![b(&[1, 2, 3, 4]), VmValue::Int(-5), VmValue::Int(99)],
        )
        .unwrap();
        assert_eq!(sliced.as_bytes().unwrap(), &[1, 2, 3, 4]);
    }
}
