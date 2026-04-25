use std::rc::Rc;

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use zeroize::Zeroizing;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

fn jwt_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(format!("jwt_sign: {}", message.into()))
}

fn jwt_algorithm(name: &str) -> Result<Algorithm, VmError> {
    match name {
        "ES256" => Ok(Algorithm::ES256),
        "RS256" => Ok(Algorithm::RS256),
        other => Err(jwt_error(format!(
            "unsupported algorithm `{other}`; expected ES256 or RS256"
        ))),
    }
}

fn jwt_encoding_key(algorithm: Algorithm, pem: &str) -> Result<EncodingKey, VmError> {
    match algorithm {
        Algorithm::ES256 => EncodingKey::from_ec_pem(pem.as_bytes())
            .map_err(|error| jwt_error(format!("invalid ES256 PEM private key: {error}"))),
        Algorithm::RS256 => EncodingKey::from_rsa_pem(pem.as_bytes())
            .map_err(|error| jwt_error(format!("invalid RS256 PEM private key: {error}"))),
        _ => Err(jwt_error("unsupported algorithm")),
    }
}

fn jwt_sign_builtin(args: &[VmValue]) -> Result<VmValue, VmError> {
    if args.len() != 3 {
        return Err(jwt_error("requires 3 arguments: alg, claims, private_key"));
    }

    let alg = match &args[0] {
        VmValue::String(value) => value.as_ref(),
        other => {
            return Err(jwt_error(format!(
                "alg must be a string, got {}",
                other.type_name()
            )));
        }
    };
    let algorithm = jwt_algorithm(alg)?;

    if !matches!(args[1], VmValue::Dict(_)) {
        return Err(jwt_error(format!(
            "claims must be a dict, got {}",
            args[1].type_name()
        )));
    }
    let claims_json = super::json::vm_value_to_json(&args[1]);
    let claims: serde_json::Value = serde_json::from_str(&claims_json)
        .map_err(|error| jwt_error(format!("claims are not JSON-serializable: {error}")))?;

    let private_key = match &args[2] {
        VmValue::String(value) => Zeroizing::new(value.to_string()),
        other => {
            return Err(jwt_error(format!(
                "private_key must be a PEM string, got {}",
                other.type_name()
            )));
        }
    };
    let key = jwt_encoding_key(algorithm, private_key.as_ref())?;
    let token = jsonwebtoken::encode(&Header::new(algorithm), &claims, &key)
        .map_err(|error| jwt_error(format!("signing failed: {error}")))?;

    Ok(VmValue::String(Rc::from(token)))
}

pub(crate) fn register_crypto_builtins(vm: &mut Vm) {
    fn display_arg(args: &[VmValue]) -> String {
        args.first().map(|a| a.display()).unwrap_or_default()
    }

    vm.register_builtin("base64_encode", |args, _out| {
        let bytes = match args.first() {
            Some(VmValue::Bytes(bytes)) => bytes.as_slice().to_vec(),
            Some(other) => other.display().into_bytes(),
            None => Vec::new(),
        };
        use base64::Engine;
        Ok(VmValue::String(Rc::from(
            base64::engine::general_purpose::STANDARD.encode(bytes),
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

    vm.register_builtin("jwt_sign", |args, _out| jwt_sign_builtin(args));

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

    // --- modern hashing -------------------------------------------------

    vm.register_builtin("sha3_256", |args, _out| {
        use sha3::{Digest, Sha3_256};
        let input = bytes_or_string_input(args.first())?;
        let digest = Sha3_256::digest(&input);
        Ok(VmValue::String(Rc::from(hex::encode(digest))))
    });

    vm.register_builtin("sha3_512", |args, _out| {
        use sha3::{Digest, Sha3_512};
        let input = bytes_or_string_input(args.first())?;
        let digest = Sha3_512::digest(&input);
        Ok(VmValue::String(Rc::from(hex::encode(digest))))
    });

    vm.register_builtin("blake3", |args, _out| {
        let input = bytes_or_string_input(args.first())?;
        let digest = blake3::hash(&input);
        Ok(VmValue::String(Rc::from(digest.to_hex().to_string())))
    });

    // --- ed25519 keypair / sign / verify --------------------------------

    vm.register_builtin("ed25519_keypair", |_args, _out| {
        use ed25519_dalek::{SigningKey, VerifyingKey};
        use rand::RngExt;
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        let signing = SigningKey::from_bytes(&bytes);
        let verifying: VerifyingKey = signing.verifying_key();
        let mut dict = std::collections::BTreeMap::new();
        dict.insert(
            "private".to_string(),
            VmValue::String(Rc::from(hex::encode(signing.to_bytes()))),
        );
        dict.insert(
            "public".to_string(),
            VmValue::String(Rc::from(hex::encode(verifying.to_bytes()))),
        );
        Ok(VmValue::Dict(Rc::new(dict)))
    });

    vm.register_builtin("ed25519_sign", |args, _out| {
        use ed25519_dalek::{Signer, SigningKey};
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "ed25519_sign: expected (private_hex, message)",
            ))));
        }
        let priv_hex = args[0].display();
        let msg = bytes_or_string_input(Some(&args[1]))?;
        let priv_bytes = hex::decode(&priv_hex).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "ed25519_sign: invalid hex private key: {e}"
            ))))
        })?;
        let priv_arr: [u8; 32] = priv_bytes.as_slice().try_into().map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "ed25519_sign: private key must be 32 bytes",
            )))
        })?;
        let signing = SigningKey::from_bytes(&priv_arr);
        let sig = signing.sign(&msg);
        Ok(VmValue::String(Rc::from(hex::encode(sig.to_bytes()))))
    });

    vm.register_builtin("ed25519_verify", |args, _out| {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        if args.len() < 3 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "ed25519_verify: expected (public_hex, message, signature_hex)",
            ))));
        }
        let pub_hex = args[0].display();
        let msg = bytes_or_string_input(Some(&args[1]))?;
        let sig_hex = args[2].display();

        let pub_bytes = match hex::decode(&pub_hex) {
            Ok(b) => b,
            Err(_) => return Ok(VmValue::Bool(false)),
        };
        let sig_bytes = match hex::decode(&sig_hex) {
            Ok(b) => b,
            Err(_) => return Ok(VmValue::Bool(false)),
        };
        let pub_arr: [u8; 32] = match pub_bytes.as_slice().try_into() {
            Ok(a) => a,
            Err(_) => return Ok(VmValue::Bool(false)),
        };
        let sig_arr: [u8; 64] = match sig_bytes.as_slice().try_into() {
            Ok(a) => a,
            Err(_) => return Ok(VmValue::Bool(false)),
        };
        let verifying = match VerifyingKey::from_bytes(&pub_arr) {
            Ok(v) => v,
            Err(_) => return Ok(VmValue::Bool(false)),
        };
        let signature = Signature::from_bytes(&sig_arr);
        Ok(VmValue::Bool(verifying.verify(&msg, &signature).is_ok()))
    });

    // --- x25519 keypair / agree -----------------------------------------

    vm.register_builtin("x25519_keypair", |_args, _out| {
        use rand::RngExt;
        use x25519_dalek::{PublicKey, StaticSecret};
        let mut bytes = [0u8; 32];
        rand::rng().fill(&mut bytes);
        let secret = StaticSecret::from(bytes);
        let public = PublicKey::from(&secret);
        let mut dict = std::collections::BTreeMap::new();
        dict.insert(
            "private".to_string(),
            VmValue::String(Rc::from(hex::encode(secret.to_bytes()))),
        );
        dict.insert(
            "public".to_string(),
            VmValue::String(Rc::from(hex::encode(public.to_bytes()))),
        );
        Ok(VmValue::Dict(Rc::new(dict)))
    });

    vm.register_builtin("x25519_agree", |args, _out| {
        use x25519_dalek::{PublicKey, StaticSecret};
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "x25519_agree: expected (private_hex, peer_public_hex)",
            ))));
        }
        let priv_hex = args[0].display();
        let pub_hex = args[1].display();
        let priv_bytes = hex::decode(&priv_hex).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "x25519_agree: invalid private hex: {e}"
            ))))
        })?;
        let pub_bytes = hex::decode(&pub_hex).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "x25519_agree: invalid public hex: {e}"
            ))))
        })?;
        let priv_arr: [u8; 32] = priv_bytes.as_slice().try_into().map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "x25519_agree: private must be 32 bytes",
            )))
        })?;
        let pub_arr: [u8; 32] = pub_bytes.as_slice().try_into().map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "x25519_agree: public must be 32 bytes",
            )))
        })?;
        let secret = StaticSecret::from(priv_arr);
        let peer = PublicKey::from(pub_arr);
        let shared = secret.diffie_hellman(&peer);
        Ok(VmValue::String(Rc::from(hex::encode(shared.as_bytes()))))
    });

    // --- jwt_verify (HS256 / RS256 / ES256) -----------------------------

    vm.register_builtin("jwt_verify", |args, _out| {
        use jsonwebtoken::{decode, DecodingKey, Validation};
        if args.len() < 3 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "jwt_verify: expected (alg, token, key)",
            ))));
        }
        let alg = args[0].display();
        let token = args[1].display();
        let key_str = args[2].display();
        let algorithm = match alg.as_str() {
            "HS256" => Algorithm::HS256,
            "ES256" => Algorithm::ES256,
            "RS256" => Algorithm::RS256,
            other => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "jwt_verify: unsupported algorithm '{other}'"
                )))));
            }
        };
        let decoding_key = match algorithm {
            Algorithm::HS256 => DecodingKey::from_secret(key_str.as_bytes()),
            Algorithm::ES256 => DecodingKey::from_ec_pem(key_str.as_bytes()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "jwt_verify: invalid ES256 public key: {e}"
                ))))
            })?,
            Algorithm::RS256 => DecodingKey::from_rsa_pem(key_str.as_bytes()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "jwt_verify: invalid RS256 public key: {e}"
                ))))
            })?,
            _ => unreachable!(),
        };
        let mut validation = Validation::new(algorithm);
        // Don't enforce exp/nbf/aud automatically — the caller can opt in
        // by validating claims themselves.
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.required_spec_claims.clear();
        let decoded = decode::<serde_json::Value>(&token, &decoding_key, &validation)
            .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("jwt_verify: {e}")))))?;
        let claims_value = crate::schema::json_to_vm_value(&decoded.claims);
        let mut dict = std::collections::BTreeMap::new();
        dict.insert("valid".to_string(), VmValue::Bool(true));
        dict.insert("claims".to_string(), claims_value);
        Ok(VmValue::Dict(Rc::new(dict)))
    });
}

fn bytes_or_string_input(arg: Option<&VmValue>) -> Result<Vec<u8>, VmError> {
    match arg {
        Some(VmValue::Bytes(b)) => Ok(b.to_vec()),
        Some(VmValue::String(s)) => Ok(s.as_bytes().to_vec()),
        Some(other) => Ok(other.display().into_bytes()),
        None => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::Vm;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    const ES256_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgWTFfCGljY6aw3Hrt\n\
kHmPRiazukxPLb6ilpRAewjW8nihRANCAATDskChT+Altkm9X7MI69T3IUmrQU0L\n\
950IxEzvw/x5BMEINRMrXLBJhqzO9Bm+d6JbqA21YQmd1Kt4RzLJR1W+\n\
-----END PRIVATE KEY-----\n";

    const ES256_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEw7JAoU/gJbZJvV+zCOvU9yFJq0FN\n\
C/edCMRM78P8eQTBCDUTK1ywSYaszvQZvneiW6gNtWEJndSreEcyyUdVvg==\n\
-----END PUBLIC KEY-----\n";

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

    fn jwt_claims() -> VmValue {
        VmValue::Dict(Rc::new(BTreeMap::from([
            ("exp".to_string(), VmValue::Int(4_102_444_800)),
            ("iat".to_string(), VmValue::Int(1_700_000_000)),
            ("iss".to_string(), s("12345")),
        ])))
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
    fn base64_encode_accepts_bytes() {
        let mut vm = vm();
        let encoded = call(
            &mut vm,
            "base64_encode",
            vec![VmValue::Bytes(Rc::new(vec![0, 1, 2]))],
        )
        .unwrap();
        assert_eq!(encoded.display(), "AAEC");
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
    fn jwt_sign_es256_produces_verifiable_compact_jws() {
        let mut vm = vm();
        let token = call(
            &mut vm,
            "jwt_sign",
            vec![s("ES256"), jwt_claims(), s(ES256_PRIVATE_KEY)],
        )
        .unwrap()
        .display();

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);

        let mut validation = jsonwebtoken::Validation::new(Algorithm::ES256);
        validation.validate_exp = false;
        let decoded = jsonwebtoken::decode::<serde_json::Value>(
            &token,
            &jsonwebtoken::DecodingKey::from_ec_pem(ES256_PUBLIC_KEY.as_bytes()).unwrap(),
            &validation,
        )
        .unwrap();
        assert_eq!(decoded.header.alg, Algorithm::ES256);
        assert_eq!(decoded.claims["iss"], "12345");
        assert_eq!(decoded.claims["iat"], 1_700_000_000);
    }

    #[test]
    fn jwt_sign_rejects_unsupported_algorithm() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "jwt_sign",
            vec![s("HS256"), jwt_claims(), s("secret")],
        );
        let Err(VmError::Runtime(message)) = result else {
            panic!("expected runtime error");
        };
        assert!(message.contains("unsupported algorithm `HS256`"));
    }

    #[test]
    fn jwt_sign_requires_dict_claims() {
        let mut vm = vm();
        let result = call(
            &mut vm,
            "jwt_sign",
            vec![s("ES256"), s("not a dict"), s(ES256_PRIVATE_KEY)],
        );
        let Err(VmError::Runtime(message)) = result else {
            panic!("expected runtime error");
        };
        assert!(message.contains("claims must be a dict"));
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
