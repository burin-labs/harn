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

    // Fast structural hash of any VmValue. Uses a stable FNV-1a-style hash
    // over the canonical display form so logically-equal values (per `==`)
    // produce the same 64-bit hash. Intended for bucketing/indexing inside
    // user code, NOT for cryptographic integrity — use sha256 for that.
    vm.register_builtin("hash_value", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        let key = crate::value::value_structural_hash_key(val);
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in key.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Ok(VmValue::Int(hash as i64))
    });

    macro_rules! register_hash {
        ($vm:expr, $name:expr, $hasher:ty) => {
            $vm.register_builtin($name, |args, _out| {
                use sha2::Digest as _;
                let val = args.first().map(|a| a.display()).unwrap_or_default();
                let hash = <$hasher>::digest(val.as_bytes());
                Ok(VmValue::String(Rc::from(format!("{hash:x}"))))
            });
        };
    }
    register_hash!(vm, "sha256", sha2::Sha256);
    register_hash!(vm, "sha224", sha2::Sha224);
    register_hash!(vm, "sha384", sha2::Sha384);
    register_hash!(vm, "sha512", sha2::Sha512);
    register_hash!(vm, "sha512_256", sha2::Sha512_256);
    register_hash!(vm, "md5", md5::Md5);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Vm;
    use std::rc::Rc;

    fn vm() -> Vm {
        let mut vm = Vm::new();
        register_crypto_builtins(&mut vm);
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

    // ---- base64 ----

    #[test]
    fn base64_round_trip_ascii() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base64_encode", vec![s("hello world")]).unwrap();
        assert_eq!(encoded.display(), "aGVsbG8gd29ybGQ=");
        let decoded = call(&mut vm, "base64_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "hello world");
    }

    #[test]
    fn base64_empty_string() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base64_encode", vec![s("")]).unwrap();
        assert_eq!(encoded.display(), "");
        let decoded = call(&mut vm, "base64_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "");
    }

    #[test]
    fn base64_decode_invalid_input() {
        let mut vm = vm();
        let result = call(&mut vm, "base64_decode", vec![s("not-valid-base64!!!")]);
        assert!(result.is_err());
    }

    #[test]
    fn base64_binary_content() {
        let mut vm = vm();
        // Encode bytes that include non-UTF8 when decoded
        let encoded = call(&mut vm, "base64_encode", vec![s("\x00\x01\x02")]).unwrap();
        let decoded = call(&mut vm, "base64_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "\x00\x01\x02");
    }

    // ---- URL encode/decode ----

    #[test]
    fn url_encode_preserves_unreserved() {
        let mut vm = vm();
        let result = call(&mut vm, "url_encode", vec![s("hello-world_foo.bar~baz")]).unwrap();
        assert_eq!(result.display(), "hello-world_foo.bar~baz");
    }

    #[test]
    fn url_encode_encodes_special_chars() {
        let mut vm = vm();
        let result = call(&mut vm, "url_encode", vec![s("a b&c=d")]).unwrap();
        assert_eq!(result.display(), "a%20b%26c%3Dd");
    }

    #[test]
    fn url_encode_handles_utf8() {
        let mut vm = vm();
        let result = call(&mut vm, "url_encode", vec![s("café")]).unwrap();
        // 'é' is 0xC3 0xA9 in UTF-8
        assert!(result.display().contains("%C3%A9"));
    }

    #[test]
    fn url_decode_plus_as_space() {
        let mut vm = vm();
        let result = call(&mut vm, "url_decode", vec![s("hello+world")]).unwrap();
        assert_eq!(result.display(), "hello world");
    }

    #[test]
    fn url_decode_percent_encoding() {
        let mut vm = vm();
        let result = call(&mut vm, "url_decode", vec![s("a%20b%26c")]).unwrap();
        assert_eq!(result.display(), "a b&c");
    }

    #[test]
    fn url_decode_invalid_percent_passthrough() {
        let mut vm = vm();
        let result = call(&mut vm, "url_decode", vec![s("100%ZZ")]).unwrap();
        // Invalid %ZZ should pass through as-is
        assert_eq!(result.display(), "100%ZZ");
    }

    #[test]
    fn url_round_trip() {
        let mut vm = vm();
        let original = "key=hello world&foo=bar/baz";
        let encoded = call(&mut vm, "url_encode", vec![s(original)]).unwrap();
        let decoded = call(&mut vm, "url_decode", vec![encoded]).unwrap();
        // Note: url_encode uses %20, url_decode treats + as space,
        // so round-trip is exact when encode produces %20.
        assert_eq!(decoded.display(), original);
    }

    // ---- SHA hashes ----

    #[test]
    fn sha256_known_vector() {
        let mut vm = vm();
        let result = call(&mut vm, "sha256", vec![s("")]).unwrap();
        // SHA-256 of empty string
        assert_eq!(
            result.display(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(result.display().len(), 64);
    }

    #[test]
    fn sha224_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha224", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 56); // 224 bits = 56 hex chars
    }

    #[test]
    fn sha384_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha384", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 96); // 384 bits = 96 hex chars
    }

    #[test]
    fn sha512_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha512", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 128); // 512 bits = 128 hex chars
    }

    #[test]
    fn sha512_256_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha512_256", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 64); // 256 bits = 64 hex chars
    }

    #[test]
    fn md5_known_vector() {
        let mut vm = vm();
        let result = call(&mut vm, "md5", vec![s("")]).unwrap();
        assert_eq!(result.display(), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn sha256_deterministic() {
        let mut vm = vm();
        let a = call(&mut vm, "sha256", vec![s("hello")]).unwrap();
        let b = call(&mut vm, "sha256", vec![s("hello")]).unwrap();
        assert_eq!(a.display(), b.display());
    }

    #[test]
    fn sha256_different_inputs_differ() {
        let mut vm = vm();
        let a = call(&mut vm, "sha256", vec![s("hello")]).unwrap();
        let b = call(&mut vm, "sha256", vec![s("world")]).unwrap();
        assert_ne!(a.display(), b.display());
    }

    // ---- hash_value ----

    #[test]
    fn hash_value_deterministic() {
        let mut vm = vm();
        let a = call(&mut vm, "hash_value", vec![s("test")]).unwrap();
        let b = call(&mut vm, "hash_value", vec![s("test")]).unwrap();
        assert_eq!(a.display(), b.display());
    }

    #[test]
    fn hash_value_different_inputs() {
        let mut vm = vm();
        let a = call(&mut vm, "hash_value", vec![s("foo")]).unwrap();
        let b = call(&mut vm, "hash_value", vec![s("bar")]).unwrap();
        assert_ne!(a.display(), b.display());
    }

    #[test]
    fn hash_value_nil() {
        let mut vm = vm();
        let result = call(&mut vm, "hash_value", vec![VmValue::Nil]).unwrap();
        // Should not panic, should return some int
        assert!(matches!(result, VmValue::Int(_)));
    }
}
