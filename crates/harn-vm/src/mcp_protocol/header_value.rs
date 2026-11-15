//! Streamable HTTP value encoding shared by Harn and its generated SDKs.

use base64::{prelude::BASE64_STANDARD, Engine};

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct McpHeaderValueEncoding {
    pub prefix: &'static str,
    pub suffix: &'static str,
    pub plain_minimum: u8,
    pub plain_maximum: u8,
}

pub const MCP_HEADER_VALUE_ENCODING: McpHeaderValueEncoding = McpHeaderValueEncoding {
    prefix: rmcp::transport::common::http_header::BASE64_HEADER_PREFIX,
    suffix: rmcp::transport::common::http_header::BASE64_HEADER_SUFFIX,
    plain_minimum: 0x20,
    plain_maximum: 0x7e,
};

/// Preserve name identity across HTTP's ASCII and whitespace normalization.
pub fn encode_header_value(value: &str) -> String {
    let policy = MCP_HEADER_VALUE_ENCODING;
    let bytes = value.as_bytes();
    let encoded = bytes.first() == Some(&policy.plain_minimum)
        || bytes.last() == Some(&policy.plain_minimum)
        || bytes
            .iter()
            .any(|byte| !(policy.plain_minimum..=policy.plain_maximum).contains(byte))
        || (value.starts_with(policy.prefix) && value.ends_with(policy.suffix));
    if encoded {
        format!(
            "{}{}{}",
            policy.prefix,
            BASE64_STANDARD.encode(bytes),
            policy.suffix
        )
    } else {
        value.to_owned()
    }
}

/// Refuse malformed sentinel payloads instead of comparing their encoded text.
pub fn decode_header_value(value: &str) -> Option<String> {
    let policy = MCP_HEADER_VALUE_ENCODING;
    match value
        .strip_prefix(policy.prefix)
        .and_then(|value| value.strip_suffix(policy.suffix))
    {
        Some(encoded) => String::from_utf8(BASE64_STANDARD.decode(encoded).ok()?).ok(),
        None => Some(value.to_owned()),
    }
}

pub fn header_value_examples() -> serde_json::Value {
    serde_json::Value::Array(
        [
            "echo",
            "Δοκιμή",
            " padded ",
            "line1\nline2",
            "=?base64?literal?=",
            "a\tb",
            "",
        ]
        .into_iter()
        .map(|value| serde_json::json!({"value": value, "encoded": encode_header_value(value)}))
        .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_encoding_preserves_unicode_whitespace_and_literal_sentinels() {
        assert_eq!(encode_header_value("echo"), "echo");
        assert_eq!(encode_header_value(" padded "), "=?base64?IHBhZGRlZCA=?=");
        for case in header_value_examples().as_array().unwrap() {
            let value = case["value"].as_str().unwrap();
            let encoded = case["encoded"].as_str().unwrap();
            assert!(encoded.is_ascii());
            assert_eq!(decode_header_value(encoded).as_deref(), Some(value));
        }
        for invalid in ["=?base64?!!!?=", "=?base64?/w==?=", "=?base64?Zh==?="] {
            assert_eq!(decode_header_value(invalid), None, "{invalid}");
        }
    }
}
