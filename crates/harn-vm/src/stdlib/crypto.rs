use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_crypto_builtins(vm: &mut Vm) {
    vm.register_builtin("base64_encode", |args, _out| {
        let val = args.first().map(|a| a.display()).unwrap_or_default();
        use base64::Engine;
        Ok(VmValue::String(Rc::from(
            base64::engine::general_purpose::STANDARD.encode(val.as_bytes()),
        )))
    });
    vm.register_builtin("base64_decode", |args, _out| {
        let val = args.first().map(|a| a.display()).unwrap_or_default();
        use base64::Engine;
        match base64::engine::general_purpose::STANDARD.decode(val.as_bytes()) {
            Ok(bytes) => Ok(VmValue::String(Rc::from(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))),
            Err(e) => Err(VmError::Runtime(format!("base64 decode error: {e}"))),
        }
    });

    vm.register_builtin("sha256", |args, _out| {
        use sha2::Digest;
        let val = args.first().map(|a| a.display()).unwrap_or_default();
        let hash = sha2::Sha256::digest(val.as_bytes());
        Ok(VmValue::String(Rc::from(format!("{hash:x}"))))
    });
    vm.register_builtin("md5", |args, _out| {
        use md5::Digest;
        let val = args.first().map(|a| a.display()).unwrap_or_default();
        let hash = md5::Md5::digest(val.as_bytes());
        Ok(VmValue::String(Rc::from(format!("{hash:x}"))))
    });

    vm.register_builtin("url_encode", |args, _out| {
        let val = args.first().map(|a| a.display()).unwrap_or_default();
        let encoded: String = val
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{:02X}", b),
            })
            .collect();
        Ok(VmValue::String(Rc::from(encoded)))
    });

    vm.register_builtin("url_decode", |args, _out| {
        let val = args.first().map(|a| a.display()).unwrap_or_default();
        let mut result = Vec::new();
        let bytes = val.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(byte) = u8::from_str_radix(&val[i + 1..i + 3], 16) {
                    result.push(byte);
                    i += 3;
                    continue;
                }
            }
            if bytes[i] == b'+' {
                result.push(b' ');
            } else {
                result.push(bytes[i]);
            }
            i += 1;
        }
        Ok(VmValue::String(Rc::from(
            String::from_utf8_lossy(&result).into_owned(),
        )))
    });
}
