//! RFC 3986 percent-encoding helpers shared across the crate.
//!
//! `url::form_urlencoded` encodes for `application/x-www-form-urlencoded`
//! (spaces become `+`, etc.), which is the wrong shape for URL path
//! segments and signed-URL canonical query strings. These helpers preserve
//! the unreserved characters (`ALPHA / DIGIT / "-" / "." / "_" / "~"`) and
//! escape everything else as `%HH`.

use percent_encoding::{percent_encode, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

/// Everything except RFC 3986 unreserved characters.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Unreserved characters plus `/`, for whole URL paths.
const PATH: &AsciiSet = &COMPONENT.remove(b'/');

/// Percent-encode `input` as an RFC 3986 component: every byte outside the
/// unreserved set is escaped, including `/` and `?`.
pub fn percent_encode_component(input: &str) -> String {
    utf8_percent_encode(input, COMPONENT).to_string()
}

/// Percent-encode a URL path: like [`percent_encode_component`] but `/` stays.
pub fn percent_encode_path(input: &str) -> String {
    utf8_percent_encode(input, PATH).to_string()
}

/// Percent-encode a URL path, keeping `/` and any well-formed `%HH` escape
/// already present (normalized to uppercase hex) so a path is not double
/// encoded.
pub fn percent_encode_path_keeping_escapes(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            out.extend(percent_encode(&bytes[start..i], PATH));
            out.push('%');
            out.push(bytes[i + 1].to_ascii_uppercase() as char);
            out.push(bytes[i + 2].to_ascii_uppercase() as char);
            i += 3;
            start = i;
        } else {
            i += 1;
        }
    }
    out.extend(percent_encode(&bytes[start..], PATH));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreserved_pass_through() {
        assert_eq!(
            percent_encode_component("Hello-World_1.0~"),
            "Hello-World_1.0~"
        );
    }

    #[test]
    fn reserved_and_unicode_escaped() {
        assert_eq!(percent_encode_component("a b/c?d"), "a%20b%2Fc%3Fd");
        assert_eq!(percent_encode_component("é"), "%C3%A9");
    }

    #[test]
    fn path_keeps_slash_and_existing_escapes() {
        assert_eq!(
            percent_encode_path_keeping_escapes("/a b/%2f%zz/é"),
            "/a%20b/%2F%25zz/%C3%A9"
        );
    }
}
