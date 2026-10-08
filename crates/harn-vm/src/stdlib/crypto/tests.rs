use super::es256_test_keys::{ES256_PRIVATE_KEY, ES256_PUBLIC_KEY};
use super::*;
use crate::vm::Vm;

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
    VmValue::String(arcstr::ArcStr::from(v))
}

fn jwt_claims() -> VmValue {
    VmValue::dict(crate::value::DictMap::from_iter([
        (crate::value::intern_key("exp"), VmValue::Int(4_102_444_800)),
        (crate::value::intern_key("iat"), VmValue::Int(1_700_000_000)),
        (crate::value::intern_key("iss"), s("12345")),
    ]))
}

fn dict(items: &[(&str, VmValue)]) -> VmValue {
    VmValue::dict(
        items
            .iter()
            .map(|(key, value)| (crate::value::intern_key(key), value.clone()))
            .collect::<crate::value::DictMap>(),
    )
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
fn encode_and_hash_bytes_input_is_lossless_past_preview_cap() {
    // Regression: byte-consuming builtins funneled `Bytes` through
    // `display()`, which emits a hex preview truncated at 32 bytes — so the
    // encode/hash of a long binary value silently corrupted it. They must
    // now operate on the raw bytes and agree with the equivalent string.
    let mut vm = vm();
    // 40 bytes (past the 32-byte display preview cap), all printable ASCII
    // so the string and bytes forms carry identical content.
    let raw = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcd";
    assert!(raw.len() > 32);
    let bytes_val = VmValue::Bytes(std::sync::Arc::new(raw.as_bytes().to_vec()));
    for name in [
        "base64_encode",
        "base64url_encode",
        "base32_encode",
        "hex_encode",
        "sha256",
        "sha224",
        "sha384",
        "sha512",
        "sha512_256",
        "md5",
        "bytes_to_base64url",
        "sha256_base64url",
    ] {
        let from_bytes = call(&mut vm, name, vec![bytes_val.clone()]).unwrap();
        let from_string = call(&mut vm, name, vec![s(raw)]).unwrap();
        assert_eq!(
            from_bytes.display(),
            from_string.display(),
            "{name}: bytes and string inputs must agree (no truncation)"
        );
    }
    // The encoding must cover all 40 bytes, not a 32-byte prefix.
    let hex = call(&mut vm, "hex_encode", vec![bytes_val]).unwrap();
    assert_eq!(
        hex.display().len(),
        raw.len() * 2,
        "hex must cover all bytes"
    );
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
        vec![VmValue::Bytes(std::sync::Arc::new(vec![0, 1, 2]))],
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
fn url_decode_is_byte_based_and_boundary_safe() {
    let mut vm = vm();
    // A `%` followed by a multi-byte character used to slice the string
    // at a non-boundary byte offset and panic.
    let raw = call(&mut vm, "url_decode", vec![s("%日x")]).unwrap();
    assert_eq!(raw.display(), "%日x");
    let multi = call(&mut vm, "url_decode", vec![s("%E6%97%A5+%F0%9F%99%82")]).unwrap();
    assert_eq!(multi.display(), "日 🙂");
    // `u8::from_str_radix` accepts a leading sign, so the old decoder
    // treated `%+A` as byte 0x0A; a malformed escape must pass through.
    let signed = call(&mut vm, "url_decode", vec![s("a%+Ab")]).unwrap();
    assert_eq!(signed.display(), "a% Ab");
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
fn sha224_known_vector() {
    let mut vm = vm();
    let result = call(&mut vm, "sha224", vec![s("test")]).unwrap();
    assert_eq!(
        result.display(),
        "90a3ed9e32b2aaf4c61c410eb925426119e1a9dc53d4286ade99a809"
    );
}

#[test]
fn sha384_known_vector() {
    let mut vm = vm();
    let result = call(&mut vm, "sha384", vec![s("test")]).unwrap();
    assert_eq!(
            result.display(),
            "768412320f7b0aa5812fce428dc4706b3cae50e02a64caa16a782249bfe8efc4b7ef1ccb126255d196047dfedf17a0a9"
        );
}

#[test]
fn sha512_known_vector() {
    let mut vm = vm();
    let result = call(&mut vm, "sha512", vec![s("test")]).unwrap();
    assert_eq!(
            result.display(),
            "ee26b0dd4af7e749aa1a8ee3c10ae9923f618980772e473f8819a5d4940e0db27ac185f8a0e1d5f84f88bc887fd67b143732c304cc5fa9ad8e6f57f50028a8ff"
        );
}

#[test]
fn sha512_256_known_vector() {
    let mut vm = vm();
    let result = call(&mut vm, "sha512_256", vec![s("test")]).unwrap();
    assert_eq!(
        result.display(),
        "3d37fe58435e0d87323dee4a2c1b339ef954de63716ee79f5747f94d974f913f"
    );
}

#[test]
fn md5_known_vector() {
    let mut vm = vm();
    let result = call(&mut vm, "md5", vec![s("")]).unwrap();
    assert_eq!(result.display(), "d41d8cd98f00b204e9800998ecf8427e");
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
fn signed_url_canonicalizes_query_and_uses_url_safe_signature() {
    let mut vm = vm();
    let signed = call(
        &mut vm,
        "signed_url",
        vec![
            s("https://example.test/receipt/abc?b=two+words&a=1"),
            dict(&[("z", s("slash/value")), ("a2", s("!"))]),
            s("secret"),
            VmValue::Int(1_700_000_600),
        ],
    )
    .unwrap()
    .display();

    assert!(signed.starts_with("https://example.test/receipt/abc?"));
    assert!(signed.contains("a=1"));
    assert!(signed.contains("a2=%21"));
    assert!(signed.contains("b=two%20words"));
    assert!(signed.contains("z=slash%2Fvalue"));
    let sig = signed
        .split('&')
        .find_map(|part| part.strip_prefix("sig="))
        .expect("signature param");
    assert!(!sig.contains('+'));
    assert!(!sig.contains('/'));
    assert!(!sig.contains('='));
}

#[test]
fn verify_signed_url_accepts_valid_path_and_returns_claims() {
    let mut vm = vm();
    let signed = call(
        &mut vm,
        "signed_url",
        vec![
            s("/artifacts/run 1?kind=trace"),
            dict(&[("receipt", s("r_123"))]),
            s("secret"),
            VmValue::Int(200),
        ],
    )
    .unwrap();
    assert!(signed.display().starts_with("/artifacts/run%201?"));
    let verified = call(
        &mut vm,
        "verify_signed_url",
        vec![signed, s("secret"), VmValue::Int(199)],
    )
    .unwrap();
    let VmValue::Dict(result) = verified else {
        panic!("expected verification dict");
    };
    assert!(matches!(result.get("valid"), Some(VmValue::Bool(true))));
    assert!(matches!(
        result.get("signature_valid"),
        Some(VmValue::Bool(true))
    ));
    assert!(matches!(result.get("expired"), Some(VmValue::Bool(false))));
    assert_eq!(result.get("reason").unwrap().display(), "ok");
    let claims = result.get("claims").unwrap().as_dict().unwrap();
    assert_eq!(claims.get("kind").unwrap().display(), "trace");
    assert_eq!(claims.get("receipt").unwrap().display(), "r_123");
}

#[test]
fn verify_signed_url_rejects_tampering() {
    let mut vm = vm();
    let signed = call(
        &mut vm,
        "signed_url",
        vec![
            s("/receipts/r_123"),
            dict(&[("download", s("true"))]),
            s("secret"),
            VmValue::Int(200),
        ],
    )
    .unwrap()
    .display();
    let tampered = signed.replace("download=true", "download=false");
    let verified = call(
        &mut vm,
        "verify_signed_url",
        vec![s(&tampered), s("secret"), VmValue::Int(100)],
    )
    .unwrap();
    let result = verified.as_dict().unwrap();
    assert!(matches!(result.get("valid"), Some(VmValue::Bool(false))));
    assert!(matches!(
        result.get("signature_valid"),
        Some(VmValue::Bool(false))
    ));
    assert_eq!(result.get("reason").unwrap().display(), "bad_signature");
}

#[test]
fn verify_signed_url_handles_expiry_and_skew() {
    let mut vm = vm();
    let signed = call(
        &mut vm,
        "signed_url",
        vec![
            s("/receipts/r_123"),
            dict(&[]),
            s("secret"),
            VmValue::Int(200),
        ],
    )
    .unwrap();
    let expired = call(
        &mut vm,
        "verify_signed_url",
        vec![signed.clone(), s("secret"), VmValue::Int(201)],
    )
    .unwrap();
    let expired_result = expired.as_dict().unwrap();
    assert!(matches!(
        expired_result.get("valid"),
        Some(VmValue::Bool(false))
    ));
    assert!(matches!(
        expired_result.get("signature_valid"),
        Some(VmValue::Bool(true))
    ));
    assert_eq!(expired_result.get("reason").unwrap().display(), "expired");

    let within_skew = call(
        &mut vm,
        "verify_signed_url",
        vec![
            signed,
            s("secret"),
            VmValue::Int(205),
            dict(&[("skew_seconds", VmValue::Int(5))]),
        ],
    )
    .unwrap();
    assert!(matches!(
        within_skew.as_dict().unwrap().get("valid"),
        Some(VmValue::Bool(true))
    ));
}

#[test]
fn signed_url_supports_key_rotation_id() {
    let mut vm = vm();
    let options = dict(&[("kid", s("v2"))]);
    let signed = call(
        &mut vm,
        "signed_url",
        vec![
            s("https://example.test/receipts/r_123"),
            dict(&[("format", s("json"))]),
            s("new-secret"),
            VmValue::Int(200),
            options,
        ],
    )
    .unwrap();
    let keys = dict(&[("v1", s("old-secret")), ("v2", s("new-secret"))]);
    let verified = call(
        &mut vm,
        "verify_signed_url",
        vec![signed, keys, VmValue::Int(100)],
    )
    .unwrap();
    let result = verified.as_dict().unwrap();
    assert!(matches!(result.get("valid"), Some(VmValue::Bool(true))));
    assert_eq!(result.get("kid").unwrap().display(), "v2");
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
