//! Unofficial `/.well-known/mcp.json` server discovery.
//!
//! This descriptor is not part of the MCP protocol, but a few clients and
//! providers use it as a low-friction way to discover the actual Streamable
//! HTTP endpoint from a website URL. Keep this module intentionally narrow:
//! it fetches and validates the descriptor, while OAuth metadata discovery and
//! MCP protocol negotiation remain in their dedicated modules.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

pub const WELL_KNOWN_MCP_JSON_PATH: &str = "/.well-known/mcp.json";
const MCP_JSON_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpJsonDescriptor {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub endpoint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpJsonDiscovery {
    pub source: String,
    pub descriptor: McpJsonDescriptor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpJsonDiscoveryReport {
    pub found: bool,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<McpJsonDescriptor>,
}

#[derive(Debug, Deserialize)]
struct RawMcpJsonDescriptor {
    name: String,
    description: String,
    #[serde(default)]
    icon: Option<String>,
    endpoint: String,
}

/// Return the canonical `/.well-known/mcp.json` URL for a website or server URL.
pub fn mcp_json_url_for(input: &str) -> Result<Url, String> {
    let url = Url::parse(input.trim())
        .map_err(|error| format!("invalid MCP discovery URL `{input}`: {error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(format!(
                "MCP discovery URL must use http or https, got `{scheme}`"
            ))
        }
    }
    let origin = url.origin().ascii_serialization();
    Url::parse(&format!("{origin}{WELL_KNOWN_MCP_JSON_PATH}"))
        .map_err(|error| format!("failed to build MCP discovery URL for `{input}`: {error}"))
}

/// Fetch and validate the unofficial MCP discovery descriptor. A missing
/// descriptor is reported as `Ok(None)`; malformed or non-404 error responses
/// are surfaced as diagnostics because they usually indicate server setup drift.
pub async fn discover_mcp_json(input: &str) -> Result<Option<McpJsonDiscovery>, String> {
    let source = mcp_json_url_for(input)?;
    let client = reqwest::Client::builder()
        .timeout(MCP_JSON_DISCOVERY_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to build MCP discovery client: {error}"))?;
    discover_mcp_json_with_client(&client, source).await
}

async fn discover_mcp_json_with_client(
    client: &reqwest::Client,
    source: Url,
) -> Result<Option<McpJsonDiscovery>, String> {
    let response = client.get(source.clone()).send().await.map_err(|error| {
        format!("failed to fetch MCP discovery descriptor at {source}: {error}")
    })?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!(
            "MCP discovery descriptor at {source} returned {}",
            response.status()
        ));
    }
    let descriptor = response
        .json::<RawMcpJsonDescriptor>()
        .await
        .map_err(|error| format!("invalid MCP discovery descriptor JSON at {source}: {error}"))?;
    let descriptor = validate_descriptor(&source, descriptor)?;
    Ok(Some(McpJsonDiscovery {
        source: source.to_string(),
        descriptor,
    }))
}

fn validate_descriptor(
    source: &Url,
    descriptor: RawMcpJsonDescriptor,
) -> Result<McpJsonDescriptor, String> {
    let name = required_string("name", descriptor.name)?;
    let description = required_string("description", descriptor.description)?;
    let endpoint = required_string("endpoint", descriptor.endpoint)?;
    let endpoint_url = origin_base_url(source)?
        .join(&endpoint)
        .map_err(|error| format!("MCP discovery endpoint `{endpoint}` is not a URL: {error}"))?;
    match endpoint_url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(format!(
                "MCP discovery endpoint must use http or https, got `{scheme}`"
            ))
        }
    }
    let icon = descriptor.icon.and_then(|icon| {
        let icon = icon.trim();
        (!icon.is_empty()).then(|| icon.to_string())
    });
    Ok(McpJsonDescriptor {
        name,
        description,
        icon,
        endpoint: endpoint_url.to_string(),
    })
}

fn origin_base_url(source: &Url) -> Result<Url, String> {
    Url::parse(&format!("{}/", source.origin().ascii_serialization()))
        .map_err(|error| format!("failed to build MCP discovery origin URL: {error}"))
}

fn required_string(field: &str, value: String) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!(
            "MCP discovery descriptor `{field}` must not be empty"
        ));
    }
    Ok(value.to_string())
}

pub fn discovery_report(
    input: &str,
    discovery: Option<McpJsonDiscovery>,
) -> Result<McpJsonDiscoveryReport, String> {
    let source = match discovery.as_ref() {
        Some(discovery) => discovery.source.clone(),
        None => mcp_json_url_for(input)?.to_string(),
    };
    Ok(McpJsonDiscoveryReport {
        found: discovery.is_some(),
        source,
        descriptor: discovery.map(|discovery| discovery.descriptor),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_json_url_uses_origin_well_known_path() {
        let url = mcp_json_url_for("https://example.com/docs/mcp?x=1").unwrap();
        assert_eq!(url.as_str(), "https://example.com/.well-known/mcp.json");
    }

    #[test]
    fn mcp_json_url_rejects_non_http_schemes() {
        let error = mcp_json_url_for("file:///tmp/mcp.json").unwrap_err();
        assert!(error.contains("http or https"), "{error}");
    }

    #[tokio::test]
    async fn discover_mcp_json_resolves_relative_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
            let body = r#"{"name":"Demo","description":"Demo MCP server","endpoint":"mcp","icon":"https://example.com/icon.png"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
        });

        let discovery = discover_mcp_json(&format!("{origin}/docs"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            discovery.source,
            format!("{origin}{WELL_KNOWN_MCP_JSON_PATH}")
        );
        assert_eq!(discovery.descriptor.name, "Demo");
        assert_eq!(discovery.descriptor.endpoint, format!("{origin}/mcp"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn discover_mcp_json_returns_none_for_404() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
            let response =
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
        });

        assert!(discover_mcp_json(&origin).await.unwrap().is_none());
        server.await.unwrap();
    }
}
