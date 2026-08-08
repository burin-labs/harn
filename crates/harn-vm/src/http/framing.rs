#[cfg(test)]
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

/// Shared cap for lightweight HTTP readers used by in-process test servers.
pub const TEST_HTTP_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HttpContentLengthLimitError {
    pub content_length: usize,
    pub max_bytes: usize,
}

impl fmt::Display for HttpContentLengthLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HTTP Content-Length {} exceeds limit {} bytes",
            self.content_length, self.max_bytes
        )
    }
}

impl Error for HttpContentLengthLimitError {}

pub fn enforce_http_content_length_limit(
    content_length: usize,
    max_bytes: usize,
) -> Result<usize, HttpContentLengthLimitError> {
    if content_length > max_bytes {
        Err(HttpContentLengthLimitError {
            content_length,
            max_bytes,
        })
    } else {
        Ok(content_length)
    }
}

pub fn parse_http_content_length(
    value: Option<&str>,
    max_bytes: usize,
) -> Result<usize, HttpContentLengthLimitError> {
    let Some(value) = value else {
        return Ok(0);
    };
    let Ok(content_length) = value.trim().parse::<usize>() else {
        return Ok(0);
    };
    enforce_http_content_length_limit(content_length, max_bytes)
}

#[cfg(test)]
pub fn http_content_length_from_headers(
    headers: &BTreeMap<String, String>,
    max_bytes: usize,
) -> Result<usize, HttpContentLengthLimitError> {
    parse_http_content_length(headers.get("content-length").map(String::as_str), max_bytes)
}

pub fn http_content_length_from_header_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    max_bytes: usize,
) -> Result<usize, HttpContentLengthLimitError> {
    let value = lines.into_iter().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then_some(value.trim())
    });
    parse_http_content_length(value, max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_invalid_content_length_defaults_to_zero() {
        assert_eq!(
            parse_http_content_length(None, TEST_HTTP_MAX_BODY_BYTES).expect("missing length"),
            0
        );
        assert_eq!(
            parse_http_content_length(Some("not-a-number"), TEST_HTTP_MAX_BODY_BYTES)
                .expect("invalid length"),
            0
        );
    }

    #[test]
    fn header_line_lookup_is_case_insensitive() {
        let headers = ["Host: example.test", "Content-Length: 12"];
        assert_eq!(
            http_content_length_from_header_lines(headers, TEST_HTTP_MAX_BODY_BYTES)
                .expect("content length"),
            12
        );
    }

    #[test]
    fn oversized_content_length_is_rejected() {
        let error = parse_http_content_length(
            Some(&(TEST_HTTP_MAX_BODY_BYTES + 1).to_string()),
            TEST_HTTP_MAX_BODY_BYTES,
        )
        .expect_err("oversized content length");
        assert_eq!(error.content_length, TEST_HTTP_MAX_BODY_BYTES + 1);
        assert_eq!(error.max_bytes, TEST_HTTP_MAX_BODY_BYTES);
    }
}
