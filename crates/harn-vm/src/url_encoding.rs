//! RFC 3986 percent-encoding helpers shared across the crate.
//!
//! `url::form_urlencoded` encodes for `application/x-www-form-urlencoded`
//! (spaces become `+`, etc.), which is the wrong shape for URL path
//! segments and signed-URL canonical query strings. These helpers preserve
//! the unreserved characters (`ALPHA / DIGIT / "-" / "." / "_" / "~"`) and
//! escape everything else as `%HH`.

/// Percent-encode `input` as an RFC 3986 component: every byte outside the
/// unreserved set is escaped, including `/` and `?`.
pub(crate) fn percent_encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
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
}
