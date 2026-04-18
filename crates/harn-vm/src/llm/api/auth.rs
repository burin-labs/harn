//! Auth header application for outbound provider requests.

/// Apply auth headers to a request based on provider config.
pub(crate) fn apply_auth_headers(
    req: reqwest::RequestBuilder,
    api_key: &str,
    pdef: Option<&crate::llm_config::ProviderDef>,
) -> reqwest::RequestBuilder {
    if api_key.is_empty() {
        return req;
    }
    if let Some(p) = pdef {
        match p.auth_style.as_str() {
            "header" => {
                let header_name = p.auth_header.as_deref().unwrap_or("x-api-key");
                req.header(header_name, api_key)
            }
            "bearer" => req.header("Authorization", format!("Bearer {api_key}")),
            "none" => req,
            _ => req.header("Authorization", format!("Bearer {api_key}")),
        }
    } else {
        req.header("Authorization", format!("Bearer {api_key}"))
    }
}
