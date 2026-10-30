use super::*;
use harn_vm::mcp_auth::{
    authorization_server_metadata_candidates, build_oauth_authorization_url,
    canonical_resource_indicator, protected_resource_metadata_candidates,
    OAuthAuthorizationUrlOptions,
};

#[test]
fn protected_resource_candidate_prefers_path_specific_url() {
    let url = Url::parse("https://example.com/mcp/wiki").unwrap();
    let candidates = protected_resource_metadata_candidates(&url);
    assert_eq!(
        candidates[0].as_str(),
        "https://example.com/.well-known/oauth-protected-resource/mcp/wiki"
    );
    assert_eq!(
        candidates[1].as_str(),
        "https://example.com/.well-known/oauth-protected-resource"
    );
}

#[test]
fn authorization_server_candidate_prefers_path_specific_metadata() {
    let url = Url::parse("https://auth.example.com/oauth").unwrap();
    let candidates = authorization_server_metadata_candidates(&url);
    assert_eq!(
        candidates[0].url.as_str(),
        "https://auth.example.com/.well-known/oauth-authorization-server/oauth"
    );
    assert_eq!(
        candidates[1].url.as_str(),
        "https://auth.example.com/.well-known/openid-configuration/oauth"
    );
    assert_eq!(
        candidates[2].url.as_str(),
        "https://auth.example.com/oauth/.well-known/openid-configuration"
    );
}

#[test]
fn callback_html_response_escapes_message() {
    let response = html_response(400, "<script>alert('x')</script>&");
    assert!(response.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;&amp;"));
    assert!(!response.contains("<script>"));
}

#[test]
fn callback_page_localizes_owned_chrome_and_preserves_external_diagnostics() {
    use harn_hostlib::localization::Locale;
    for locale in Locale::ALL {
        let body =
            crate::localization::translate_for(locale, crate::localization::OAUTH_SUCCESS_MESSAGE);
        let response = html_response_in(200, &body, locale);
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains(&format!("Content-Language: {}", locale.id())));
        assert!(response.contains(&format!("<html lang=\"{}\">", locale.id())));
        assert!(response.contains(&crate::format::escape_html(&body)));
        if locale != Locale::English {
            assert!(!response.contains("<title>Authorization Complete</title>"));
        }
    }
    let japanese = html_response_in(200, "provider-message", Locale::Japanese);
    assert!(japanese.contains("<title>認可が完了しました</title>"));
    let external = html_response_in(400, "Authorization Complete <script>", Locale::French);
    assert!(external.contains("<p>Authorization Complete &lt;script&gt;</p>"));
}

#[test]
fn authorization_url_includes_canonical_resource_indicator() {
    let resource = canonical_resource_indicator("https://MCP.Example.com:443/mcp/").unwrap();
    let url = build_oauth_authorization_url(OAuthAuthorizationUrlOptions {
        authorization_endpoint: "https://auth.example.com/authorize",
        client_id: "client-123",
        redirect_uri: "http://127.0.0.1:9783/oauth/callback",
        state: "state-abc",
        code_challenge: "challenge-xyz",
        resource: &resource,
        scopes: Some("mcp.read"),
    })
    .unwrap();
    let resource_param = url
        .query_pairs()
        .find(|(key, _)| key == "resource")
        .map(|(_, value)| value.into_owned());
    assert_eq!(
        resource_param.as_deref(),
        Some("https://mcp.example.com/mcp")
    );
}

fn parse_server(toml_table: &str) -> McpServerConfig {
    toml::from_str::<McpServerConfig>(toml_table).expect("mcp server config")
}

#[test]
fn mcp_server_config_parses_token_exchange_row() {
    let server = parse_server(
        r#"
name = "api"
transport = "http"
url = "https://mcp.example/mcp"

[token_exchange]
token_url = "https://auth.example/token"
actor_token = "agent.jwt"
actor_token_type = "jwt"
subject_token_type = "access_token"
client_id = "agent-client"
client_secret = "agent-secret"
token_endpoint_auth_method = "client_secret_basic"
scope = "repo"

[token_exchange.extra_params]
deployment = "enterprise"
"#,
    );
    let exchange = server
        .token_exchange
        .as_ref()
        .expect("token exchange row parsed");
    assert_eq!(
        exchange.token_url.as_deref(),
        Some("https://auth.example/token")
    );
    assert_eq!(exchange.actor_token.as_deref(), Some("agent.jwt"));
    assert_eq!(
        exchange.token_endpoint_auth_method.as_deref(),
        Some("client_secret_basic")
    );
    assert_eq!(
        exchange.extra_params.get("deployment"),
        Some(&serde_json::json!("enterprise"))
    );
}

#[test]
fn oauth_server_classification() {
    let oauth = parse_server(
        "name = \"wiki\"\ntransport = \"http\"\nurl = \"https://mcp.wiki.example/mcp\"\n",
    );
    assert!(is_oauth_server(&oauth), "http server with a url is OAuth");

    let stdio = parse_server("name = \"fs\"\ntransport = \"stdio\"\ncommand = \"npx\"\n");
    assert!(!is_oauth_server(&stdio), "stdio is not OAuth");

    let static_bearer = parse_server(
            "name = \"api\"\ntransport = \"http\"\nurl = \"https://mcp.example/mcp\"\nauth_token = \"static\"\n",
        );
    assert!(
        !is_oauth_server(&static_bearer),
        "a static bearer token is not interactive OAuth"
    );

    let static_secret = parse_server(
            "name = \"api\"\ntransport = \"http\"\nurl = \"https://mcp.example/mcp\"\n[auth]\nsecret_id = \"my-secret\"\n",
        );
    assert!(
        !is_oauth_server(&static_secret),
        "an [auth].secret_id static token is not interactive OAuth"
    );

    let no_url = parse_server("name = \"x\"\ntransport = \"http\"\n");
    assert!(!is_oauth_server(&no_url), "http without a url is not OAuth");
}

#[test]
fn callback_query_parsing_extracts_code_state_issuer() {
    let url = Url::parse(
        "http://127.0.0.1:9783/oauth/callback?code=abc&state=xyz&iss=https%3A%2F%2Fauth.example",
    )
    .unwrap();
    let query = parse_callback_query(&url);
    assert_eq!(query_get(&query, "code").as_deref(), Some("abc"));
    assert_eq!(query_get(&query, "state").as_deref(), Some("xyz"));
    assert_eq!(
        query_get(&query, "iss").as_deref(),
        Some("https://auth.example")
    );
    assert!(query_get(&query, "error").is_none());
}

#[tokio::test]
async fn stdio_server_without_live_handle_is_disconnected() {
    let server = parse_server("name = \"fs\"\ntransport = \"stdio\"\ncommand = \"/bin/true\"\n");
    let status = derive_server_status(&server).await;
    assert_eq!(status.name, "fs");
    assert_eq!(status.transport, "stdio");
    assert_eq!(status.state, "disconnected");
    assert_eq!(status.url, "");
    assert!(status.tools.is_none());
    assert!(status.last_error.is_none());
}

#[tokio::test]
async fn http_server_with_static_token_is_connected() {
    let server = parse_server(
            "name = \"api\"\ntransport = \"http\"\nurl = \"https://mcp.example.com/mcp\"\nauth_token = \"static-bearer\"\n",
        );
    let status = derive_server_status(&server).await;
    assert_eq!(status.transport, "http");
    assert_eq!(status.state, "connected");
    assert_eq!(status.url, "https://mcp.example.com/mcp");
}

#[test]
fn status_report_serializes_with_stable_keys() {
    let report = McpStatusReport {
        schema_version: MCP_STATUS_SCHEMA_VERSION,
        manifest: Some("/repo/harn.toml".to_string()),
        servers: vec![McpServerStatus {
            name: "fs".to_string(),
            transport: "stdio".to_string(),
            state: "disconnected".to_string(),
            url: String::new(),
            lazy: true,
            tools: None,
            resources: None,
            prompts: None,
            last_error: None,
            token_expires_at_unix: None,
            token_client_id: None,
            token_issuer: None,
            display_identity: None,
        }],
    };
    let value: serde_json::Value = serde_json::to_value(&report).expect("serialize");
    assert_eq!(value["schema_version"], 3);
    assert_eq!(value["manifest"], "/repo/harn.toml");
    assert_eq!(value["servers"][0]["name"], "fs");
    assert_eq!(value["servers"][0]["transport"], "stdio");
    assert_eq!(value["servers"][0]["state"], "disconnected");
    assert_eq!(value["servers"][0]["lazy"], true);
    assert!(value["servers"][0]["tools"].is_null());
    assert!(value["servers"][0]["last_error"].is_null());
    assert!(value["servers"][0]["token_expires_at_unix"].is_null());
    assert!(value["servers"][0]["display_identity"].is_null());
}
