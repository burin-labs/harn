use super::*;
use crate::cli::ConnectGithubArgs;
use crate::package::ProviderOAuthManifest;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use harn_vm::connectors::testkit::{
    http_content_length_from_header_lines, TEST_HTTP_MAX_BODY_BYTES,
};
use harn_vm::secrets::SecretId;
use url::Url;

fn status_config(
    setup: package::ProviderSetupManifest,
) -> package::ResolvedProviderConnectorConfig {
    package::ResolvedProviderConnectorConfig {
        id: harn_vm::ProviderId::from("github"),
        manifest_dir: PathBuf::from("/tmp"),
        connector: package::ResolvedProviderConnectorKind::Harn {
            module: "./lib.harn".to_string(),
        },
        oauth: None,
        setup: Some(setup),
    }
}

fn oauth_setup() -> package::ProviderSetupManifest {
    package::ProviderSetupManifest {
        auth_type: Some("oauth2".to_string()),
        flow: Some("browser".to_string()),
        required_scopes: vec!["issues:read".to_string(), "pull_requests:read".to_string()],
        ..package::ProviderSetupManifest::default()
    }
}

#[test]
fn derives_linear_resource_types_from_trigger_events() {
    let manifest = package::ResolvedTriggerConfig {
        id: "linear-issues".to_string(),
        kind: package::TriggerKind::Webhook,
        provider: harn_vm::ProviderId::from("linear"),
        autonomy_tier: harn_vm::AutonomyTier::Shadow,
        match_: package::TriggerMatchExpr {
            events: vec!["issue.update".to_string(), "comment.create".to_string()],
            extra: Default::default(),
        },
        when: None,
        when_budget: None,
        handler: "handlers::on_linear".to_string(),
        dedupe_key: None,
        retry: package::TriggerRetrySpec::default(),
        dispatch_priority: package::TriggerDispatchPriority::Normal,
        budget: package::TriggerBudgetSpec::default(),
        concurrency: None,
        throttle: None,
        rate_limit: None,
        debounce: None,
        singleton: None,
        batch: None,
        window: None,
        priority_flow: None,
        secrets: Default::default(),
        filter: None,
        kind_specific: Default::default(),
        manifest_dir: PathBuf::from("/tmp"),
        manifest_path: PathBuf::from("/tmp/harn.toml"),
        package_name: None,
        exports: Default::default(),
        table_index: 0,
        shape_error: None,
    };
    let resource_types = derive_linear_resource_types(&[manifest]).expect("resource types");
    assert_eq!(
        resource_types,
        vec!["Comment".to_string(), "Issue".to_string()]
    );
}

#[test]
fn linear_resource_type_mapping_covers_customer_request() {
    assert_eq!(
        linear_resource_type_for_event("customer_request.update"),
        Some("CustomerRequest")
    );
}

#[test]
fn authorization_url_includes_pkce_and_resource_indicator() {
    let url = build_authorization_url(
        "https://auth.example.com/oauth/authorize",
        "client",
        "http://127.0.0.1:49152/oauth/callback",
        "state",
        "challenge",
        "https://api.example.com/resource",
        Some("read write"),
    )
    .expect("authorization URL");
    let pairs = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(pairs.get("code_challenge_method").unwrap(), "S256");
    assert_eq!(pairs.get("code_challenge").unwrap(), "challenge");
    assert_eq!(
        pairs.get("resource").unwrap(),
        "https://api.example.com/resource"
    );
    assert_eq!(pairs.get("scope").unwrap(), "read write");
}

#[test]
fn registered_provider_metadata_builds_oauth_request_with_cli_overrides() {
    let metadata = ProviderOAuthManifest {
        authorization_endpoint: Some("https://auth.example.com/authorize".to_string()),
        token_endpoint: Some("https://auth.example.com/token".to_string()),
        registration_endpoint: Some("https://auth.example.com/register".to_string()),
        resource: Some("https://api.example.com/".to_string()),
        scopes: Some("default.read".to_string()),
        client_id: Some("manifest-client".to_string()),
        client_secret: Some("manifest-secret".to_string()),
        token_endpoint_auth_method: Some("client_secret_post".to_string()),
    };
    let args = ConnectOAuthArgs {
        client_id: Some("cli-client".to_string()),
        client_secret: None,
        scope: Some("cli.read".to_string()),
        resource: None,
        auth_url: None,
        token_url: Some("https://override.example.com/token".to_string()),
        token_auth_method: None,
        redirect_uri: "http://127.0.0.1:0/oauth/callback".to_string(),
        no_open: true,
        json: true,
    };
    let request = oauth_request_from_provider_metadata("acme", &args, &metadata).expect("request");
    assert_eq!(request.provider, "acme");
    assert_eq!(request.resource, "https://api.example.com/");
    assert_eq!(request.client_id.as_deref(), Some("cli-client"));
    assert_eq!(request.client_secret.as_deref(), Some("manifest-secret"));
    assert_eq!(request.scopes.as_deref(), Some("cli.read"));
    assert_eq!(
        request.token_endpoint.as_deref(),
        Some("https://override.example.com/token")
    );
    assert!(request.no_open);
    assert!(request.json);
}

#[test]
fn missing_required_scopes_splits_oauth_scope_strings() {
    let required = vec![
        "issues:read".to_string(),
        "pull_requests:read".to_string(),
        "contents:read".to_string(),
    ];
    assert_eq!(
        missing_required_scopes(
            &required,
            Some("issues:read,pull_requests:read metadata:read")
        ),
        vec!["contents:read".to_string()]
    );
}

#[test]
fn setup_plan_for_missing_connector_is_host_renderable() {
    let dir = tempfile::tempdir().unwrap();
    let plan = connect_setup_plan_at("github", dir.path()).expect("setup plan");

    assert_eq!(plan.connector, "github");
    assert!(!plan.installed);
    assert_eq!(plan.validation_command[0], "harn");
    assert_eq!(plan.steps[0].id, "install");
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_missing_auth_without_credentials() {
    let secrets = harn_vm::connectors::testkit::MemorySecretProvider::empty();
    let index = ConnectIndex::default();
    let config = status_config(oauth_setup());
    let status =
        connector_status("github", Some(&config), &secrets, &index, 100, false, None).await;

    assert_eq!(status.status, "missing_auth");
    assert!(!status.usable);
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_expired_credentials_before_scope_checks() {
    let secrets = harn_vm::connectors::testkit::MemorySecretProvider::empty();
    let index = ConnectIndex {
        providers: vec![ConnectIndexEntry {
            provider: "github".to_string(),
            kind: "oauth".to_string(),
            secret_id: "github/access-token".to_string(),
            expires_at_unix: Some(99),
            scopes: Some("issues:read".to_string()),
            connected_at_unix: 1,
            last_used_at_unix: None,
        }],
    };
    let config = status_config(oauth_setup());
    let status =
        connector_status("github", Some(&config), &secrets, &index, 100, false, None).await;

    assert_eq!(status.status, "expired_credentials");
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_revoked_credentials_when_index_secret_is_missing() {
    let secrets = harn_vm::connectors::testkit::MemorySecretProvider::empty();
    let index = ConnectIndex {
        providers: vec![ConnectIndexEntry {
            provider: "github".to_string(),
            kind: "oauth".to_string(),
            secret_id: "github/access-token".to_string(),
            expires_at_unix: None,
            scopes: Some("issues:read pull_requests:read".to_string()),
            connected_at_unix: 1,
            last_used_at_unix: None,
        }],
    };
    let config = status_config(oauth_setup());
    let status =
        connector_status("github", Some(&config), &secrets, &index, 100, false, None).await;

    assert_eq!(status.status, "revoked_credentials");
}

#[tokio::test(flavor = "current_thread")]
async fn status_reports_missing_scopes_after_secret_checks_pass() {
    let secrets = harn_vm::connectors::testkit::MemorySecretProvider::empty()
        .with_secret(SecretId::new("github", "access-token"), "token");
    let index = ConnectIndex {
        providers: vec![ConnectIndexEntry {
            provider: "github".to_string(),
            kind: "oauth".to_string(),
            secret_id: "github/access-token".to_string(),
            expires_at_unix: None,
            scopes: Some("issues:read".to_string()),
            connected_at_unix: 1,
            last_used_at_unix: None,
        }],
    };
    let config = status_config(oauth_setup());
    let status =
        connector_status("github", Some(&config), &secrets, &index, 100, false, None).await;

    assert_eq!(status.status, "missing_scopes");
    assert_eq!(status.missing_scopes, vec!["pull_requests:read"]);
}

#[test]
fn loopback_listener_rewrites_zero_port() {
    let (_listener, redirect_uri) =
        bind_loopback_listener("http://127.0.0.1:0/oauth/callback").expect("loopback listener");
    let parsed = Url::parse(&redirect_uri).unwrap();
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert_ne!(parsed.port(), Some(0));
    assert_eq!(parsed.path(), "/oauth/callback");
}

#[test]
fn loopback_listener_rejects_non_http_redirect_uri() {
    let error = bind_loopback_listener("https://127.0.0.1:0/oauth/callback").unwrap_err();

    assert!(error.contains("http scheme"));
}

#[test]
fn loopback_listener_requires_explicit_port() {
    let error = bind_loopback_listener("http://127.0.0.1/oauth/callback").unwrap_err();

    assert!(error.contains("include a port"));
}

#[test]
fn callback_request_rejects_wrong_origin() {
    let request =
        "GET /oauth/callback?code=abc&state=xyz HTTP/1.1\r\nOrigin: http://evil.example\r\n\r\n";
    let error = parse_callback_request(
        request,
        "/oauth/callback",
        Some("xyz"),
        "http://127.0.0.1:49152",
    )
    .unwrap_err();
    assert!(error.contains("Origin"));
}

#[test]
fn callback_request_requires_get_method() {
    let request =
        "POST /oauth/callback?code=abc&state=xyz HTTP/1.1\r\nOrigin: http://127.0.0.1:49152\r\n\r\n";
    let error = parse_callback_request(
        request,
        "/oauth/callback",
        Some("xyz"),
        "http://127.0.0.1:49152",
    )
    .unwrap_err();

    assert!(error.contains("must use GET"));
}

#[test]
fn callback_request_rejects_malformed_request_line() {
    let request =
        "GET /oauth/callback?code=abc&state=xyz HTTP/1.1 extra\r\nOrigin: http://127.0.0.1:49152\r\n\r\n";
    let error = parse_callback_request(
        request,
        "/oauth/callback",
        Some("xyz"),
        "http://127.0.0.1:49152",
    )
    .unwrap_err();

    assert!(error.contains("request line"));
}

#[test]
fn github_install_url_adds_state() {
    let args = ConnectGithubArgs {
        app_slug: Some("harn-test".to_string()),
        app_id: None,
        installation_id: None,
        install_url: None,
        redirect_uri: "http://127.0.0.1:0/gh-install-callback".to_string(),
        private_key_file: None,
        webhook_secret: None,
        webhook_secret_file: None,
        no_open: true,
        json: false,
    };
    let url = github_install_url(&args, "state123").expect("install URL");
    assert_eq!(
        url.as_str(),
        "https://github.com/apps/harn-test/installations/new?state=state123"
    );
}

#[test]
fn callback_html_response_escapes_reflected_messages() {
    let response = html_response(400, "<script>alert('x')</script>&");

    assert!(response.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;&amp;"));
    assert!(!response.contains("<script>"));
}

#[test]
fn github_install_callback_captures_installation_id() {
    // The production listener is non-blocking so it can poll the
    // 5-minute OAUTH_CALLBACK_TIMEOUT deadline. This unit test only
    // needs the happy path, so it switches to a blocking listener and
    // uses a barrier to make the client connect after the server is
    // ready to accept. Client-side timeouts keep failures local to this
    // assertion instead of relying on the test runner timeout.
    let (listener, redirect_uri) = bind_loopback_listener("http://127.0.0.1:0/gh-install-callback")
        .expect("loopback listener");
    listener
        .set_nonblocking(false)
        .expect("revert listener to blocking for deterministic test accept");
    let parsed = Url::parse(&redirect_uri).unwrap();
    let port = parsed.port().unwrap();
    let redirect_uri_for_server = redirect_uri;
    let server_ready = Arc::new(Barrier::new(2));
    let client_ready = Arc::clone(&server_ready);
    let server = thread::spawn(move || {
        // The barrier rendezvous happens just before the call into
        // wait_for_github_installation, which immediately calls
        // listener.accept(). The kernel queues a connect() that races
        // here in the listen backlog — but the ordering is still
        // deterministic: accept() will return as soon as the client
        // has called connect().
        server_ready.wait();
        wait_for_github_installation(listener, &redirect_uri_for_server, Some("state-ok"))
    });
    let client = thread::spawn(move || {
        client_ready.wait();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect callback");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set client read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set client write timeout");
        stream
            .write_all(
                b"GET /gh-install-callback?installation_id=12345&state=state-ok HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: null\r\n\r\n",
            )
            .expect("write callback");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .expect("read callback response");
        assert!(response.contains("200 OK"));
    });
    let installation_id = server
        .join()
        .expect("server thread")
        .expect("installation id");
    client.join().expect("callback client");
    assert_eq!(installation_id, "12345");
}

#[tokio::test(flavor = "current_thread")]
async fn mocked_builtin_oauth_token_endpoints_receive_pkce_and_resource_indicators() {
    for provider in ["slack", "linear", "notion"] {
        let defaults = oauth_provider_defaults(provider).expect("provider defaults");
        let expected_resource = defaults.default_resource.to_string();
        let token_endpoint = spawn_token_endpoint(move |form| {
            assert_eq!(
                form.get("grant_type").map(String::as_str),
                Some("authorization_code")
            );
            assert_eq!(form.get("code").map(String::as_str), Some("code-123"));
            assert_eq!(
                form.get("code_verifier").map(String::as_str),
                Some("verifier-123")
            );
            assert_eq!(
                form.get("resource").map(String::as_str),
                Some(expected_resource.as_str())
            );
        });
        let token = exchange_authorization_code(
            &token_endpoint,
            AuthorizationCodeExchange {
                client_id: "client",
                client_secret: Some("secret"),
                token_auth_method: defaults.token_auth_method,
                redirect_uri: "http://127.0.0.1:49152/oauth/callback",
                resource: defaults.default_resource,
                scopes: Some("read write"),
                code: "code-123",
                code_verifier: "verifier-123",
            },
        )
        .await
        .expect("token exchange");
        assert_eq!(token.access_token, "mock-access-token");
        assert_eq!(token.refresh_token.as_deref(), Some("mock-refresh-token"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn generic_mcp_oauth_discovers_metadata_and_registers_client() {
    let (base_url, server) = spawn_generic_mcp_oauth_server();
    let discovery = discover_oauth_server(&format!("{base_url}/mcp/notion"))
        .await
        .expect("discover oauth server");
    assert_eq!(
        discovery.metadata.authorization_endpoint,
        format!("{base_url}/oauth/authorize")
    );
    assert_eq!(
        discovery.metadata.token_endpoint,
        format!("{base_url}/oauth/token")
    );
    assert_eq!(
        discovery.metadata.registration_endpoint.as_deref(),
        Some(format!("{base_url}/oauth/register").as_str())
    );
    ensure_pkce_support(&discovery.metadata).expect("pkce support");
    let registered = dynamic_client_registration(
        discovery.metadata.registration_endpoint.as_deref().unwrap(),
        "http://127.0.0.1:49152/oauth/callback",
        Some("mcp.read"),
    )
    .await
    .expect("dynamic registration");
    assert_eq!(registered.client_id, "dynamic-client");
    server.join().expect("mock oauth server");
}

fn spawn_token_endpoint<F>(assert_form: F) -> String
where
    F: FnOnce(BTreeMap<String, String>) + Send + 'static,
{
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock token listener");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("token request");
        let request = read_http_request(&mut stream);
        let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
        let form = url::form_urlencoded::parse(body.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<BTreeMap<_, _>>();
        assert_form(form);
        write_json_response(
            &mut stream,
            r#"{"access_token":"mock-access-token","refresh_token":"mock-refresh-token","expires_in":3600}"#,
        );
    });
    format!("http://127.0.0.1:{port}/oauth/token")
}

fn spawn_generic_mcp_oauth_server() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock oauth listener");
    let port = listener.local_addr().unwrap().port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server_base_url = base_url.clone();
    let handle = thread::spawn(move || {
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().expect("oauth request");
            let request = read_http_request(&mut stream);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            if path.starts_with("/mcp/notion") {
                write_response(
                    &mut stream,
                    &format!(
                        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer resource_metadata=\"{server_base_url}/.well-known/oauth-protected-resource/mcp/notion\", scope=\"mcp.read\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                );
            } else if path.starts_with("/.well-known/oauth-protected-resource/mcp/notion") {
                write_json_response(
                    &mut stream,
                    &format!(
                        r#"{{"authorization_servers":["{server_base_url}/oauth"],"scopes_supported":["mcp.read"]}}"#
                    ),
                );
            } else if path.starts_with("/.well-known/oauth-authorization-server/oauth") {
                write_json_response(
                    &mut stream,
                    &format!(
                        r#"{{"issuer":"{server_base_url}/oauth","authorization_endpoint":"{server_base_url}/oauth/authorize","token_endpoint":"{server_base_url}/oauth/token","registration_endpoint":"{server_base_url}/oauth/register","code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none","client_secret_post"]}}"#
                    ),
                );
            } else if path.starts_with("/oauth/register") {
                assert!(request.contains("http://127.0.0.1:49152/oauth/callback"));
                assert!(request.contains("mcp.read"));
                assert!(request.contains("\"application_type\":\"native\""));
                write_json_response(
                    &mut stream,
                    r#"{"client_id":"dynamic-client","token_endpoint_auth_method":"none"}"#,
                );
            } else {
                write_response(
                    &mut stream,
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        }
    });
    (base_url, handle)
}

fn read_http_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).expect("read request");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&buffer);
        if let Some((headers, body)) = text.split_once("\r\n\r\n") {
            let content_length = match http_content_length_from_header_lines(
                headers.lines(),
                TEST_HTTP_MAX_BODY_BYTES,
            ) {
                Ok(content_length) => content_length,
                Err(_) => break,
            };
            if body.len() >= content_length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buffer).to_string()
}

fn write_json_response(stream: &mut TcpStream, body: &str) {
    write_response(
        stream,
        &format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        ),
    );
}

fn write_response(stream: &mut TcpStream, response: &str) {
    stream
        .write_all(response.as_bytes())
        .expect("write response");
}
