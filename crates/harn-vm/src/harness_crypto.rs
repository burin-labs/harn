//! Deterministic helpers backing pure cryptographic builtins.

use std::borrow::Cow;

use crate::value::VmValue;

pub(crate) fn sha256_hex(args: &[VmValue]) -> String {
    let bytes: Cow<'_, [u8]> = match args.first() {
        Some(VmValue::Bytes(bytes)) => Cow::Borrowed(bytes.as_slice()),
        Some(other) => Cow::Owned(other.display().into_bytes()),
        None => Cow::Borrowed(&[]),
    };
    harn_kernel::pure::sha256_hex(bytes.as_ref())
}

pub(crate) fn sha256_hex_value(args: &[VmValue]) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(sha256_hex(args)))
}
