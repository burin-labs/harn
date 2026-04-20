use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_crypto_builtins(vm: &mut Vm) {
    fn display_arg(args: &[VmValue]) -> String {
        args.first().map(|a| a.display()).unwrap_or_default()
    }

    vm.register_builtin("base64_encode", |args, _out| {
        let val = display_arg(args);
        use base64::Engine;
        Ok(VmValue::String(Rc::from(
            base64::engine::general_purpose::STANDARD.encode(val.as_bytes()),
        )))
    });
    vm.register_builtin("base64_decode", |args, _out| {
        let val = display_arg(args);
        use base64::Engine;
        match base64::engine::general_purpose::STANDARD.decode(val.as_bytes()) {
            Ok(bytes) => Ok(VmValue::String(Rc::from(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))),
            Err(e) => Err(VmError::Runtime(format!("base64 decode error: {e}"))),
        }
    });
    vm.register_builtin("base64url_encode", |args, _out| {
        let val = display_arg(args);
        use base64::Engine;
        Ok(VmValue::String(Rc::from(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(val.as_bytes()),
        )))
    });
    vm.register_builtin("base64url_decode", |args, _out| {
        let val = display_arg(args);
        use base64::Engine;
        match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(val.as_bytes()) {
            Ok(bytes) => Ok(VmValue::String(Rc::from(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))),
            Err(e) => Err(VmError::Runtime(format!("base64url decode error: {e}"))),
        }
    });
    vm.register_builtin("base32_encode", |args, _out| {
        let val = display_arg(args);
        Ok(VmValue::String(Rc::from(
            data_encoding::BASE32.encode(val.as_bytes()),
        )))
    });
    vm.register_builtin("base32_decode", |args, _out| {
        let val = display_arg(args);
        match data_encoding::BASE32.decode(val.as_bytes()) {
            Ok(bytes) => Ok(VmValue::String(Rc::from(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))),
            Err(e) => Err(VmError::Runtime(format!("base32 decode error: {e}"))),
        }
    });
    vm.register_builtin("hex_encode", |args, _out| {
        let val = display_arg(args);
        Ok(VmValue::String(Rc::from(hex::encode(val.as_bytes()))))
    });
    vm.register_builtin("hex_decode", |args, _out| {
        let val = display_arg(args);
        match hex::decode(val.as_bytes()) {
            Ok(bytes) => Ok(VmValue::String(Rc::from(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))),
            Err(e) => Err(VmError::Runtime(format!("hex decode error: {e}"))),
        }
    });

    // Stable FNV-1a over the canonical display form so logically-equal values
    // hash identically. For bucketing/indexing only — use sha256 for integrity.
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
        ($vm:expr, $name:expr, $digest:path, $hasher:ty) => {
            $vm.register_builtin($name, |args, _out| {
                use $digest as _;
                let val = display_arg(args);
                let hash = <$hasher>::digest(val.as_bytes());
                let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
                Ok(VmValue::String(Rc::from(hex)))
            });
        };
    }
    register_hash!(vm, "sha256", sha2::Digest, sha2::Sha256);
    register_hash!(vm, "sha224", sha2::Digest, sha2::Sha224);
    register_hash!(vm, "sha384", sha2::Digest, sha2::Sha384);
    register_hash!(vm, "sha512", sha2::Digest, sha2::Sha512);
    register_hash!(vm, "sha512_256", sha2::Digest, sha2::Sha512_256);
    register_hash!(vm, "md5", md5::Digest, md5::Md5);

    // HMAC-SHA256 over (key, message). Both inputs are taken as their byte
    // sequences (string `display()` of nil/numbers stringifies first). The
    // returned hex string is what most webhook providers send in their
    // signature header (e.g. GitHub's `x-hub-signature-256: sha256=<hex>`).
    vm.register_builtin("hmac_sha256", |args, _out| {
        let key = args.first().map(|a| a.display()).unwrap_or_default();
        let msg = args.get(1).map(|a| a.display()).unwrap_or_default();
        let mac = crate::connectors::hmac::hmac_sha256(key.as_bytes(), msg.as_bytes());
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        Ok(VmValue::String(Rc::from(hex)))
    });

    // HMAC-SHA256 returning standard base64 (used by Slack-style signatures).
    vm.register_builtin("hmac_sha256_base64", |args, _out| {
        use base64::Engine;
        let key = args.first().map(|a| a.display()).unwrap_or_default();
        let msg = args.get(1).map(|a| a.display()).unwrap_or_default();
        let mac = crate::connectors::hmac::hmac_sha256(key.as_bytes(), msg.as_bytes());
        Ok(VmValue::String(Rc::from(
            base64::engine::general_purpose::STANDARD.encode(&mac),
        )))
    });

    // Constant-time string equality. The variable-time `==` operator can leak
    // the position of the first differing byte through timing, which lets an
    // attacker recover an HMAC signature byte-by-byte. Always use this for
    // signature comparison.
    vm.register_builtin("constant_time_eq", |args, _out| {
        let a = args.first().map(|a| a.display()).unwrap_or_default();
        let b = args.get(1).map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(crate::connectors::hmac::secure_eq(
            a.as_bytes(),
            b.as_bytes(),
        )))
    });

    vm.register_builtin("url_encode", |args, _out| {
        let val = display_arg(args);
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
        let val = display_arg(args);
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
        let encoded = call(&mut vm, "base64_encode", vec![s("\x00\x01\x02")]).unwrap();
        let decoded = call(&mut vm, "base64_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "\x00\x01\x02");
    }

    #[test]
    fn base64url_known_vector() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base64url_encode", vec![s(">>>???///")]).unwrap();
        assert_eq!(encoded.display(), "Pj4-Pz8_Ly8v");
        let decoded = call(&mut vm, "base64url_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), ">>>???///");
    }

    #[test]
    fn base64url_omits_padding() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base64url_encode", vec![s("f")]).unwrap();
        assert_eq!(encoded.display(), "Zg");
    }

    #[test]
    fn base64url_decode_invalid_input() {
        let mut vm = vm();
        let result = call(&mut vm, "base64url_decode", vec![s("not+url/safe")]);
        assert!(result.is_err());
    }

    #[test]
    fn base32_known_vector() {
        let mut vm = vm();
        let encoded = call(&mut vm, "base32_encode", vec![s("foobar")]).unwrap();
        assert_eq!(encoded.display(), "MZXW6YTBOI======");
        let decoded = call(&mut vm, "base32_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "foobar");
    }

    #[test]
    fn base32_decode_invalid_input() {
        let mut vm = vm();
        let result = call(&mut vm, "base32_decode", vec![s("INVALID-BASE32")]);
        assert!(result.is_err());
    }

    #[test]
    fn hex_round_trip_ascii() {
        let mut vm = vm();
        let encoded = call(&mut vm, "hex_encode", vec![s("hello")]).unwrap();
        assert_eq!(encoded.display(), "68656c6c6f");
        let decoded = call(&mut vm, "hex_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "hello");
    }

    #[test]
    fn hex_round_trip_control_bytes() {
        let mut vm = vm();
        let encoded = call(&mut vm, "hex_encode", vec![s("\x00\x01\x02")]).unwrap();
        assert_eq!(encoded.display(), "000102");
        let decoded = call(&mut vm, "hex_decode", vec![encoded]).unwrap();
        assert_eq!(decoded.display(), "\x00\x01\x02");
    }

    #[test]
    fn hex_decode_invalid_input() {
        let mut vm = vm();
        let result = call(&mut vm, "hex_decode", vec![s("abc")]);
        assert!(result.is_err());
    }

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
        assert_eq!(result.display(), "100%ZZ");
    }

    #[test]
    fn url_round_trip() {
        let mut vm = vm();
        let original = "key=hello world&foo=bar/baz";
        let encoded = call(&mut vm, "url_encode", vec![s(original)]).unwrap();
        let decoded = call(&mut vm, "url_decode", vec![encoded]).unwrap();
        // url_encode emits %20 and url_decode accepts both %20 and +, so the
        // round-trip is exact only as long as encode produces %20.
        assert_eq!(decoded.display(), original);
    }

    #[test]
    fn sha256_known_vector() {
        let mut vm = vm();
        let result = call(&mut vm, "sha256", vec![s("")]).unwrap();
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
        assert_eq!(result.display().len(), 56);
    }

    #[test]
    fn sha384_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha384", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 96);
    }

    #[test]
    fn sha512_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha512", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 128);
    }

    #[test]
    fn sha512_256_length() {
        let mut vm = vm();
        let result = call(&mut vm, "sha512_256", vec![s("test")]).unwrap();
        assert_eq!(result.display().len(), 64);
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
        assert!(matches!(result, VmValue::Int(_)));
    }

    // RFC 4231 test case 2: key="Jefe", data="what do ya want for nothing?".
    #[test]
    fn hmac_sha256_rfc4231_vector_2() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "hmac_sha256",
            vec![s("Jefe"), s("what do ya want for nothing?")],
        )
        .unwrap();
        assert_eq!(
            result.display(),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    // GitHub's published HMAC test vector for webhook signature verification.
    // https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries
    #[test]
    fn hmac_sha256_github_documented_vector() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "hmac_sha256",
            vec![s("It's a Secret to Everybody"), s("Hello, World!")],
        )
        .unwrap();
        assert_eq!(
            result.display(),
            "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
    }

    #[test]
    fn hmac_sha256_base64_known_vector() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "hmac_sha256_base64",
            vec![s("Jefe"), s("what do ya want for nothing?")],
        )
        .unwrap();
        assert_eq!(
            result.display(),
            "W9zBRr9gdU5qBCQmCJV1x1oAPwidJzmDnexYuWTsOEM="
        );
    }

    #[test]
    fn hmac_sha256_empty_inputs() {
        let mut vm = vm();
        let result = call(&mut vm, "hmac_sha256", vec![s(""), s("")]).unwrap();
        assert_eq!(
            result.display(),
            "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad"
        );
    }

    #[test]
    fn constant_time_eq_matches_for_equal() {
        let mut vm = vm();
        let result = call(&mut vm, "constant_time_eq", vec![s("abc"), s("abc")]).unwrap();
        assert!(matches!(result, VmValue::Bool(true)));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        let mut vm = vm();
        let result = call(&mut vm, "constant_time_eq", vec![s("abc"), s("abcd")]).unwrap();
        assert!(matches!(result, VmValue::Bool(false)));
    }

    #[test]
    fn constant_time_eq_rejects_different_content() {
        let mut vm = vm();
        let result = call(&mut vm, "constant_time_eq", vec![s("abc"), s("abd")]).unwrap();
        assert!(matches!(result, VmValue::Bool(false)));
    }
}
