use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::stdlib::options::{expect_bytes_arg, expect_int_arg, expect_string_arg, ErrorKind};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn runtime_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(message.into())
}

pub(crate) fn register_bytes_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "bytes_from_string(text: string?) -> bytes", category = "bytes")]
fn bytes_from_string_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = expect_string_arg(args, 0, "bytes_from_string", ErrorKind::Runtime)?;
    Ok(VmValue::Bytes(std::sync::Arc::new(
        text.as_bytes().to_vec(),
    )))
}

#[harn_builtin(sig = "bytes_to_string(input: bytes) -> string", category = "bytes")]
fn bytes_to_string_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes_arg(args, 0, "bytes_to_string", ErrorKind::Runtime)?;
    let text = std::str::from_utf8(bytes)
        .map_err(|error| runtime_error(format!("bytes_to_string: {error}")))?;
    Ok(VmValue::String(arcstr::ArcStr::from(text)))
}

#[harn_builtin(
    sig = "bytes_to_string_lossy(input: bytes) -> string",
    category = "bytes"
)]
fn bytes_to_string_lossy_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes_arg(args, 0, "bytes_to_string_lossy", ErrorKind::Runtime)?;
    Ok(VmValue::String(arcstr::ArcStr::from(
        String::from_utf8_lossy(bytes).into_owned(),
    )))
}

#[harn_builtin(sig = "bytes_to_hex(input: bytes) -> string", category = "bytes")]
fn bytes_to_hex_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes_arg(args, 0, "bytes_to_hex", ErrorKind::Runtime)?;
    Ok(VmValue::String(arcstr::ArcStr::from(hex::encode(bytes))))
}

#[harn_builtin(sig = "bytes_from_hex(text: string?) -> bytes", category = "bytes")]
fn bytes_from_hex_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = expect_string_arg(args, 0, "bytes_from_hex", ErrorKind::Runtime)?;
    let bytes =
        hex::decode(text).map_err(|error| runtime_error(format!("bytes_from_hex: {error}")))?;
    Ok(VmValue::Bytes(std::sync::Arc::new(bytes)))
}

#[harn_builtin(sig = "bytes_to_base64(input: bytes) -> string", category = "bytes")]
fn bytes_to_base64_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use base64::Engine;

    let bytes = expect_bytes_arg(args, 0, "bytes_to_base64", ErrorKind::Runtime)?;
    Ok(VmValue::String(arcstr::ArcStr::from(
        base64::engine::general_purpose::STANDARD.encode(bytes),
    )))
}

#[harn_builtin(sig = "bytes_from_base64(text: string?) -> bytes", category = "bytes")]
fn bytes_from_base64_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use base64::Engine;

    let text = expect_string_arg(args, 0, "bytes_from_base64", ErrorKind::Runtime)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text.as_bytes())
        .map_err(|error| runtime_error(format!("bytes_from_base64: {error}")))?;
    Ok(VmValue::Bytes(std::sync::Arc::new(bytes)))
}

/// Decode URL-safe base64 into raw bytes.
///
/// Accepts both padded and unpadded inputs. Rejects the standard base64
/// alphabet (`+` / `/`) so callers get a hard alphabet check rather than a
/// silent transliteration. Pair with `bytes_to_base64url` for lossless binary
/// round trips that `base64url_decode` cannot provide (that builtin returns a
/// UTF-8 string via lossy conversion).
fn decode_base64url_bytes(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::{
        alphabet,
        engine::{general_purpose::GeneralPurpose, DecodePaddingMode, GeneralPurposeConfig},
        Engine,
    };

    // Cached engine: URL-safe alphabet, padding optional on decode.
    const URL_SAFE_PADDING_OPTIONAL: GeneralPurpose = GeneralPurpose::new(
        &alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_encode_padding(false)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );

    URL_SAFE_PADDING_OPTIONAL.decode(text.as_bytes())
}

#[harn_builtin(
    sig = "bytes_from_base64url(text: string?) -> bytes",
    category = "bytes"
)]
fn bytes_from_base64url_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = expect_string_arg(args, 0, "bytes_from_base64url", ErrorKind::Runtime)?;
    let bytes = decode_base64url_bytes(text)
        .map_err(|error| runtime_error(format!("bytes_from_base64url: {error}")))?;
    Ok(VmValue::Bytes(std::sync::Arc::new(bytes)))
}

#[harn_builtin(sig = "bytes_len(input: bytes) -> int", category = "bytes")]
fn bytes_len_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes_arg(args, 0, "bytes_len", ErrorKind::Runtime)?;
    Ok(VmValue::Int(bytes.len() as i64))
}

#[harn_builtin(
    sig = "bytes_concat(left: bytes, right: bytes) -> bytes",
    category = "bytes"
)]
fn bytes_concat_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let left = expect_bytes_arg(args, 0, "bytes_concat", ErrorKind::Runtime)?;
    let right = expect_bytes_arg(args, 1, "bytes_concat", ErrorKind::Runtime)?;
    let mut out = Vec::with_capacity(left.len() + right.len());
    out.extend_from_slice(left);
    out.extend_from_slice(right);
    Ok(VmValue::Bytes(std::sync::Arc::new(out)))
}

#[harn_builtin(
    sig = "bytes_slice(input: bytes, start: int, end: int) -> bytes",
    category = "bytes"
)]
fn bytes_slice_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = expect_bytes_arg(args, 0, "bytes_slice", ErrorKind::Runtime)?;
    let len = bytes.len() as i64;
    let start = expect_int_arg(args, 1, "bytes_slice", ErrorKind::Runtime)?.clamp(0, len) as usize;
    let end = expect_int_arg(args, 2, "bytes_slice", ErrorKind::Runtime)?.clamp(0, len) as usize;
    let slice = if start >= end {
        Vec::new()
    } else {
        bytes[start..end].to_vec()
    };
    Ok(VmValue::Bytes(std::sync::Arc::new(slice)))
}

#[harn_builtin(
    sig = "bytes_eq(left: bytes, right: bytes) -> bool",
    category = "bytes"
)]
fn bytes_eq_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use subtle::ConstantTimeEq;

    let left = expect_bytes_arg(args, 0, "bytes_eq", ErrorKind::Runtime)?;
    let right = expect_bytes_arg(args, 1, "bytes_eq", ErrorKind::Runtime)?;
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
    &BYTES_FROM_BASE64URL_IMPL_DEF,
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
        VmValue::String(arcstr::ArcStr::from(v))
    }

    fn b(v: &[u8]) -> VmValue {
        VmValue::Bytes(std::sync::Arc::new(v.to_vec()))
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
    fn bytes_from_base64url_binary_round_trip() {
        use base64::Engine;

        let mut vm = vm();
        // Non-UTF-8 payload: lossy string decode would replace 0xFF.
        let raw = [0u8, 1, 2, 0xFF, 0x80, 0xFE];
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let decoded = call(&mut vm, "bytes_from_base64url", vec![s(&encoded)]).unwrap();
        assert_eq!(decoded.as_bytes().unwrap(), &raw);
    }

    #[test]
    fn bytes_from_base64url_accepts_padded_and_unpadded() {
        let mut vm = vm();
        // "f" -> "Zg" (unpadded) / "Zg==" (padded)
        let unpadded = call(&mut vm, "bytes_from_base64url", vec![s("Zg")]).unwrap();
        let padded = call(&mut vm, "bytes_from_base64url", vec![s("Zg==")]).unwrap();
        assert_eq!(unpadded.as_bytes().unwrap(), b"f");
        assert_eq!(padded.as_bytes().unwrap(), b"f");
    }

    #[test]
    fn bytes_from_base64url_rejects_standard_alphabet() {
        let mut vm = vm();
        let result = call(&mut vm, "bytes_from_base64url", vec![s("not+url/safe")]);
        assert!(result.is_err(), "standard alphabet must be rejected");
    }

    #[test]
    fn bytes_from_base64url_rejects_malformed_length() {
        let mut vm = vm();
        let result = call(&mut vm, "bytes_from_base64url", vec![s("A")]);
        assert!(result.is_err(), "length-1 input must be rejected");
    }

    #[test]
    fn bytes_from_base64url_empty() {
        let mut vm = vm();
        let decoded = call(&mut vm, "bytes_from_base64url", vec![s("")]).unwrap();
        assert_eq!(decoded.as_bytes().unwrap(), &[] as &[u8]);
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
