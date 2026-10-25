use super::{ImportStoredToken, StoredMcpToken};
use crate::mcp_auth::{
    canonical_resource_indicator, validate_token_endpoint_auth_method, McpOAuthDiscovery,
};

pub(super) fn stored_token_for_import(
    request: &ImportStoredToken,
    discovery: &McpOAuthDiscovery,
) -> Result<StoredMcpToken, String> {
    let server_url = request.server_url.trim();
    if server_url.is_empty() {
        return Err("MCP token import requires server_url".to_string());
    }
    let access_token = request.access_token.trim();
    if access_token.is_empty() {
        return Err("MCP token import requires access_token".to_string());
    }
    let client_id = request.client_id.trim();
    if client_id.is_empty() {
        return Err("MCP token import requires client_id".to_string());
    }

    let resource = canonical_resource_indicator(server_url).map_err(|error| error.to_string())?;
    let client_secret = request
        .client_secret
        .as_deref()
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .map(str::to_string);
    let token_endpoint_auth_method = request
        .token_endpoint_auth_method
        .as_deref()
        .map(str::trim)
        .filter(|method| !method.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if client_secret.is_some() {
                "client_secret_post".to_string()
            } else {
                "none".to_string()
            }
        });
    validate_token_endpoint_auth_method(&token_endpoint_auth_method)?;

    if request
        .expires_at_unix
        .is_some_and(|expires_at| expires_at < 0)
    {
        return Err("MCP token import expires_at must be non-negative".to_string());
    }

    Ok(StoredMcpToken {
        access_token: access_token.to_string(),
        refresh_token: request
            .refresh_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .map(str::to_string),
        expires_at_unix: request.expires_at_unix,
        token_endpoint: request
            .token_endpoint
            .as_deref()
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                discovery
                    .authorization_server_metadata
                    .token_endpoint
                    .clone()
            }),
        client_id: client_id.to_string(),
        client_secret,
        token_endpoint_auth_method,
        issuer: discovery.authorization_server_issuer.clone(),
        resource,
        scopes: request
            .scopes
            .as_deref()
            .map(str::trim)
            .filter(|scopes| !scopes.is_empty())
            .map(str::to_string),
        // Imported legacy tokens carry no captured token-response payload.
        token_response_extra: None,
    })
}
