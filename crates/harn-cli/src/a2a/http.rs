use std::collections::HashMap;

use super::rpc::{error_response, A2A_VERSION_NOT_SUPPORTED};

/// The supported A2A protocol version.
const SUPPORTED_A2A_VERSION: &str = "1.0.0";

/// Parsed HTTP request including headers.
pub(super) struct ParsedRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) headers: HashMap<String, String>,
    pub(super) body: String,
}

/// Parse an HTTP request from raw bytes. Returns a `ParsedRequest`.
pub(super) fn parse_http_request(raw: &[u8]) -> Option<ParsedRequest> {
    let text = String::from_utf8_lossy(raw);

    let (header_section, body) = if let Some(pos) = text.find("\r\n\r\n") {
        (&text[..pos], text[pos + 4..].to_string())
    } else if let Some(pos) = text.find("\n\n") {
        (&text[..pos], text[pos + 2..].to_string())
    } else {
        return None;
    };

    let mut lines = header_section.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_lowercase(), value.trim().to_string());
        }
    }

    Some(ParsedRequest {
        method,
        path,
        headers,
        body,
    })
}

/// Check the A2A-Version header. Returns an error response if the version is
/// present but not supported.
pub(super) fn check_version_header(
    headers: &HashMap<String, String>,
    rpc_id: &serde_json::Value,
) -> Option<serde_json::Value> {
    if let Some(version) = headers.get("a2a-version") {
        if version != SUPPORTED_A2A_VERSION {
            return Some(error_response(
                rpc_id,
                A2A_VERSION_NOT_SUPPORTED,
                &format!(
                    "VersionNotSupportedError: requested version {version}, supported: {SUPPORTED_A2A_VERSION}"
                ),
            ));
        }
    }
    None
}
