//! Draft 2020-12 schema traversal and JSON Pointer primitives.
//!
//! Callers operate only on schema-bearing keyword positions so reference-shaped
//! values under `const` and `enum` remain opaque instance data.

use serde_json::Value as JsonValue;

const SINGLE_SUBSCHEMA_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "contains",
    "contentSchema",
    "else",
    "if",
    "items",
    "not",
    "propertyNames",
    "then",
    "unevaluatedItems",
    "unevaluatedProperties",
];
const ARRAY_SUBSCHEMA_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];
const MAP_SUBSCHEMA_KEYWORDS: &[&str] = &[
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
    "properties",
];

pub(super) fn try_visit_schema_nodes<E>(
    schema: &JsonValue,
    visitor: &mut impl FnMut(&serde_json::Map<String, JsonValue>) -> Result<(), E>,
) -> Result<(), E> {
    let Some(object) = schema.as_object() else {
        return Ok(());
    };
    visitor(object)?;
    for keyword in SINGLE_SUBSCHEMA_KEYWORDS {
        if let Some(child) = object.get(*keyword) {
            try_visit_schema_nodes(child, visitor)?;
        }
    }
    for keyword in ARRAY_SUBSCHEMA_KEYWORDS {
        if let Some(children) = object.get(*keyword).and_then(JsonValue::as_array) {
            for child in children {
                try_visit_schema_nodes(child, visitor)?;
            }
        }
    }
    for keyword in MAP_SUBSCHEMA_KEYWORDS {
        if let Some(children) = object.get(*keyword).and_then(JsonValue::as_object) {
            for child in children.values() {
                try_visit_schema_nodes(child, visitor)?;
            }
        }
    }
    Ok(())
}

pub(super) fn try_transform_schema_nodes<E>(
    schema: &mut JsonValue,
    visitor: &mut impl FnMut(&mut serde_json::Map<String, JsonValue>) -> Result<(), E>,
) -> Result<(), E> {
    let Some(object) = schema.as_object_mut() else {
        return Ok(());
    };
    visitor(object)?;
    for keyword in SINGLE_SUBSCHEMA_KEYWORDS {
        if let Some(child) = object.get_mut(*keyword) {
            try_transform_schema_nodes(child, visitor)?;
        }
    }
    for keyword in ARRAY_SUBSCHEMA_KEYWORDS {
        if let Some(children) = object.get_mut(*keyword).and_then(JsonValue::as_array_mut) {
            for child in children {
                try_transform_schema_nodes(child, visitor)?;
            }
        }
    }
    for keyword in MAP_SUBSCHEMA_KEYWORDS {
        if let Some(children) = object.get_mut(*keyword).and_then(JsonValue::as_object_mut) {
            for child in children.values_mut() {
                try_transform_schema_nodes(child, visitor)?;
            }
        }
    }
    Ok(())
}

pub(super) fn decode_json_pointer_segment(value: &str) -> Option<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '~' {
            decoded.push(character);
            continue;
        }
        decoded.push(match chars.next()? {
            '0' => '~',
            '1' => '/',
            _ => return None,
        });
    }
    Some(decoded)
}

/// Decode one URI-fragment JSON Pointer token in the order required by RFC
/// 3986 and RFC 6901: percent escapes first, then `~0`/`~1` pointer escapes.
/// `+` remains a literal plus because URI fragments do not use HTML form
/// encoding.
pub(super) fn decode_uri_fragment_json_pointer_segment(value: &str) -> Option<String> {
    decode_json_pointer_segment(&decode_uri_fragment(value)?)
}

/// Decode RFC 3986 percent escapes without applying form encoding rules.
/// JSON Pointer parsing happens after this step for URI fragment identifiers.
pub(super) fn decode_uri_fragment(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub(super) fn encode_json_pointer_segment(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

/// Encode one JSON Pointer token for use as a URI-fragment path segment.
pub(super) fn encode_uri_fragment_json_pointer_segment(value: &str) -> String {
    let pointer = encode_json_pointer_segment(value);
    let mut encoded = String::with_capacity(pointer.len());
    for byte in pointer.bytes() {
        if is_fragment_segment_byte(byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    encoded
}

fn is_fragment_segment_byte(value: u8) -> bool {
    value.is_ascii_alphanumeric()
        || matches!(
            value,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'?'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_fragment_pointer_segments_round_trip_reserved_and_unicode_names() {
        for name in ["My Thing", "Result/Envelope", "100%", "résultat"] {
            let encoded = encode_uri_fragment_json_pointer_segment(name);
            assert_eq!(
                decode_uri_fragment_json_pointer_segment(&encoded).as_deref(),
                Some(name)
            );
        }
        assert_eq!(
            decode_uri_fragment_json_pointer_segment("literal+plus").as_deref(),
            Some("literal+plus")
        );
        assert!(decode_uri_fragment_json_pointer_segment("bad%2").is_none());
        assert!(decode_uri_fragment_json_pointer_segment("bad%FF").is_none());
    }
}
