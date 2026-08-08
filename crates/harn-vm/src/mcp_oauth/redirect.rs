use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::mcp_auth::{OAuthAuthorizationServerMetadata, OAuthClientAuthMode};

/// Inputs to an interactive MCP authorization.
#[derive(Clone, Debug, Default)]
pub struct BeginAuthorization {
    pub server_url: String,
    pub redirect_uri: String,
    pub mode: Option<OAuthClientAuthMode>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub static_secret_id: Option<String>,
    pub scopes: Option<String>,
}

/// A legacy token that a thin client found in an older surface-specific store.
#[derive(Clone, Debug)]
pub struct ImportStoredToken {
    pub server_url: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_unix: Option<i64>,
    pub token_endpoint: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub token_endpoint_auth_method: Option<String>,
    pub scopes: Option<String>,
}

/// The browser-facing result of beginning an authorization.
#[derive(Clone, Debug, Serialize)]
pub struct PendingAuthorization {
    pub authorize_url: String,
    pub state: String,
    pub redirect_uri: String,
    /// Canonical RFC 8707 resource indicator for the server.
    pub resource: String,
    /// Authorization server issuer resolved during discovery.
    pub issuer: String,
}

/// Callback capture available to an MCP OAuth surface.
///
/// Protocol adapters normally provide an exact URI that their host captures.
/// The CLI provides a shared loopback listener, which lets the Harn-owned
/// redirect policy try the requested port and, when the selected registration
/// mode permits it, fall back to an operating-system-assigned port.
#[derive(Clone, Debug, Default)]
pub enum AuthorizationCallback {
    #[default]
    Exact,
    Loopback(Arc<LoopbackCallback>),
}

/// A lazily acquired loopback callback shared by one or many authorization
/// flows. Bulk login uses one instance so every flow receives the same
/// effective redirect URI and callbacks can be demultiplexed by OAuth state.
#[derive(Debug)]
pub struct LoopbackCallback {
    requested_redirect_uri: String,
    acquired: Mutex<Option<AcquiredLoopbackCallback>>,
}

#[derive(Debug)]
struct AcquiredLoopbackCallback {
    listener: TcpListener,
    redirect_uri: String,
}

impl LoopbackCallback {
    pub fn new(requested_redirect_uri: impl Into<String>) -> Self {
        Self {
            requested_redirect_uri: requested_redirect_uri.into(),
            acquired: Mutex::new(None),
        }
    }

    /// Take the listener after authorization preparation has selected and
    /// acquired the effective redirect URI.
    pub fn take_listener(&self) -> Result<(TcpListener, String), String> {
        let acquired = self
            .acquired
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .ok_or_else(|| {
                "MCP OAuth loopback callback was not acquired before listener handoff".to_string()
            })?;
        Ok((acquired.listener, acquired.redirect_uri))
    }

    pub(super) fn acquire(&self, policy: &OAuthRedirectPolicy) -> Result<String, String> {
        let mut acquired = self
            .acquired
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(existing) = acquired.as_ref() {
            policy.validate_effective_redirect(&existing.redirect_uri)?;
            return Ok(existing.redirect_uri.clone());
        }
        if self.requested_redirect_uri != policy.requested_redirect_uri {
            return Err(format!(
                "MCP OAuth callback requested `{}` but redirect policy selected `{}` for client mode `{}`",
                self.requested_redirect_uri,
                policy.requested_redirect_uri,
                policy.client_mode.as_str()
            ));
        }
        let callback = acquire_loopback_callback(policy)?;
        let redirect_uri = callback.redirect_uri.clone();
        *acquired = Some(callback);
        Ok(redirect_uri)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CallbackCapabilities {
    Exact,
    LoopbackWithEphemeralPort,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OAuthRedirectPolicy {
    pub(super) requested_redirect_uri: String,
    pub(super) client_mode: OAuthClientAuthMode,
    pub(super) allow_ephemeral_port: bool,
}

impl OAuthRedirectPolicy {
    fn validate_effective_redirect(&self, effective: &str) -> Result<(), String> {
        if effective == self.requested_redirect_uri {
            return Ok(());
        }
        let requested = validate_loopback_redirect_uri(&self.requested_redirect_uri)?;
        let effective_url = validate_loopback_redirect_uri(effective)?;
        let same_except_port = requested.scheme() == effective_url.scheme()
            && requested.host_str() == effective_url.host_str()
            && requested.path() == effective_url.path()
            && requested.query() == effective_url.query();
        if self.allow_ephemeral_port && same_except_port {
            return Ok(());
        }
        Err(redirect_compatibility_error(
            &self.requested_redirect_uri,
            self.client_mode,
            "the callback surface acquired a different redirect URI",
            self.allow_ephemeral_port,
        ))
    }
}

/// Select the one redirect policy used by CLI and protocol surfaces.
///
/// DCR deliberately registers the URI acquired for each authorization, so a
/// changed loopback port is safe. CIMD-native clients may vary only the
/// loopback port under RFC 8252. A BYO client is treated as an exact-match
/// preregistration unless the caller explicitly requests port zero.
pub(super) fn select_redirect_policy(
    metadata: &OAuthAuthorizationServerMetadata,
    client_mode: OAuthClientAuthMode,
    requested_redirect_uri: &str,
    capabilities: CallbackCapabilities,
) -> Result<OAuthRedirectPolicy, String> {
    if capabilities == CallbackCapabilities::Exact {
        return Ok(OAuthRedirectPolicy {
            requested_redirect_uri: requested_redirect_uri.to_string(),
            client_mode,
            allow_ephemeral_port: false,
        });
    }

    let parsed = validate_loopback_redirect_uri(requested_redirect_uri).map_err(|error| {
        redirect_compatibility_error(requested_redirect_uri, client_mode, &error, false)
    })?;
    let explicitly_ephemeral = parsed.port() == Some(0);
    let mode_allows_ephemeral = match client_mode {
        OAuthClientAuthMode::Dcr => metadata.registration_endpoint.is_some(),
        OAuthClientAuthMode::Cimd => metadata.client_id_metadata_document_supported,
        OAuthClientAuthMode::Byo => explicitly_ephemeral,
        OAuthClientAuthMode::Static => false,
    };
    Ok(OAuthRedirectPolicy {
        requested_redirect_uri: requested_redirect_uri.to_string(),
        client_mode,
        allow_ephemeral_port: mode_allows_ephemeral,
    })
}

fn validate_loopback_redirect_uri(redirect_uri: &str) -> Result<url::Url, String> {
    let parsed = url::Url::parse(redirect_uri)
        .map_err(|error| format!("redirect URI is invalid: {error}"))?;
    if parsed.scheme() != "http" {
        return Err("loopback callbacks require the `http` scheme".to_string());
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.query().is_some()
    {
        return Err(
            "loopback redirect URI must not contain credentials, a query, or a fragment"
                .to_string(),
        );
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "loopback redirect URI must include a host".to_string())?;
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !is_loopback {
        return Err(format!(
            "loopback redirect URI host `{host}` is not a loopback address"
        ));
    }
    Ok(parsed)
}

fn acquire_loopback_callback(
    policy: &OAuthRedirectPolicy,
) -> Result<AcquiredLoopbackCallback, String> {
    let requested = validate_loopback_redirect_uri(&policy.requested_redirect_uri)?;
    match bind_loopback_uri(requested.clone()) {
        Ok(callback) => Ok(callback),
        Err(requested_error) if policy.allow_ephemeral_port && requested.port() != Some(0) => {
            let mut fallback = requested;
            fallback.set_port(Some(0)).map_err(|()| {
                redirect_compatibility_error(
                    &policy.requested_redirect_uri,
                    policy.client_mode,
                    "could not construct an ephemeral loopback redirect URI",
                    true,
                )
            })?;
            bind_loopback_uri(fallback).map_err(|fallback_error| {
                redirect_compatibility_error(
                    &policy.requested_redirect_uri,
                    policy.client_mode,
                    &format!(
                        "requested port bind failed ({requested_error}); operating-system port fallback also failed ({fallback_error})"
                    ),
                    true,
                )
            })
        }
        Err(error) => Err(redirect_compatibility_error(
            &policy.requested_redirect_uri,
            policy.client_mode,
            &format!("callback listener bind failed: {error}"),
            policy.allow_ephemeral_port,
        )),
    }
}

fn bind_loopback_uri(mut redirect_uri: url::Url) -> Result<AcquiredLoopbackCallback, String> {
    let host = redirect_uri
        .host_str()
        .ok_or_else(|| "redirect URI must include a host".to_string())?;
    let port = redirect_uri
        .port_or_known_default()
        .ok_or_else(|| "redirect URI must resolve to a callback port".to_string())?;
    let listener = TcpListener::bind((host, port)).map_err(|error| error.to_string())?;
    listener
        .set_nonblocking(false)
        .map_err(|error| format!("failed to configure callback listener: {error}"))?;
    let actual_port = listener
        .local_addr()
        .map_err(|error| format!("failed to read callback listener address: {error}"))?
        .port();
    redirect_uri
        .set_port(Some(actual_port))
        .map_err(|()| "failed to record callback listener port".to_string())?;
    Ok(AcquiredLoopbackCallback {
        listener,
        redirect_uri: redirect_uri.to_string(),
    })
}

pub(super) fn redirect_compatibility_error(
    redirect_uri: &str,
    client_mode: OAuthClientAuthMode,
    detail: &str,
    ephemeral_permitted: bool,
) -> String {
    redirect_compatibility_error_for_mode(
        redirect_uri,
        client_mode.as_str(),
        detail,
        ephemeral_permitted,
    )
}

pub(super) fn redirect_compatibility_error_for_mode(
    redirect_uri: &str,
    client_mode: &str,
    detail: &str,
    ephemeral_permitted: bool,
) -> String {
    let restriction = if ephemeral_permitted {
        "the authorization server or local callback environment may restrict arbitrary loopback ports"
    } else {
        "this client mode may require the exact preregistered redirect URI and port"
    };
    format!(
        "MCP OAuth redirect URI `{redirect_uri}` is unavailable for client mode `{client_mode}`: {detail}; likely compatibility restriction: {restriction}"
    )
}

pub(super) fn generate_pkce_pair() -> (String, String) {
    let verifier = random_hex(32);
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

pub(super) fn random_hex(bytes: usize) -> String {
    (0..bytes)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_auth::OAuthClientAuthSelection;
    use tokio::sync::Mutex as AsyncMutex;

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = tokio::io::AsyncReadExt::read(stream, &mut buffer)
                .await
                .unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or_default();
                if request.len() >= headers_end + 4 + content_length {
                    let target = headers
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap()
                        .to_string();
                    return (target, request[headers_end + 4..].to_vec());
                }
            }
        }
        panic!("incomplete HTTP request");
    }

    async fn write_http_response(
        stream: &mut tokio::net::TcpStream,
        status: &str,
        headers: &[(&str, String)],
        body: &str,
    ) {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(body);
        tokio::io::AsyncWriteExt::write_all(stream, response.as_bytes())
            .await
            .unwrap();
    }

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let (verifier, challenge) = generate_pkce_pair();
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        assert_eq!(verifier.len(), 64);
    }

    fn test_metadata() -> OAuthAuthorizationServerMetadata {
        OAuthAuthorizationServerMetadata {
            issuer: "https://auth.example".to_string(),
            authorization_endpoint: "https://auth.example/authorize".to_string(),
            token_endpoint: "https://auth.example/token".to_string(),
            registration_endpoint: None,
            token_endpoint_auth_methods_supported: vec!["none".to_string()],
            code_challenge_methods_supported: vec!["S256".to_string()],
            scopes_supported: Vec::new(),
            client_id_metadata_document_supported: false,
            authorization_response_iss_parameter_supported: false,
            extra: Default::default(),
        }
    }

    #[test]
    fn exact_match_preregistered_redirect_does_not_change_ports() {
        let blocker = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_uri = format!(
            "http://127.0.0.1:{}/oauth/callback",
            blocker.local_addr().unwrap().port()
        );
        let policy = select_redirect_policy(
            &test_metadata(),
            OAuthClientAuthMode::Byo,
            &redirect_uri,
            CallbackCapabilities::LoopbackWithEphemeralPort,
        )
        .unwrap();
        assert!(!policy.allow_ephemeral_port);

        let error = acquire_loopback_callback(&policy).unwrap_err();
        assert!(error.contains(&redirect_uri), "{error}");
        assert!(error.contains("client mode `byo`"), "{error}");
        assert!(error.contains("exact preregistered"), "{error}");
    }

    #[test]
    fn dynamic_registration_falls_back_after_fixed_port_conflict() {
        let blocker = TcpListener::bind("127.0.0.1:0").unwrap();
        let blocked_port = blocker.local_addr().unwrap().port();
        let redirect_uri = format!("http://127.0.0.1:{blocked_port}/oauth/callback");
        let mut metadata = test_metadata();
        metadata.registration_endpoint = Some("https://auth.example/register".to_string());
        let policy = select_redirect_policy(
            &metadata,
            OAuthClientAuthMode::Dcr,
            &redirect_uri,
            CallbackCapabilities::LoopbackWithEphemeralPort,
        )
        .unwrap();

        let acquired = acquire_loopback_callback(&policy).unwrap();
        let effective = url::Url::parse(&acquired.redirect_uri).unwrap();
        assert_ne!(effective.port(), Some(blocked_port));
        assert_ne!(effective.port(), Some(0));
        assert_eq!(effective.path(), "/oauth/callback");
    }

    #[test]
    fn cimd_loopback_policy_accepts_operating_system_port() {
        let mut metadata = test_metadata();
        metadata.client_id_metadata_document_supported = true;
        let policy = select_redirect_policy(
            &metadata,
            OAuthClientAuthMode::Cimd,
            "http://127.0.0.1:0/oauth/callback",
            CallbackCapabilities::LoopbackWithEphemeralPort,
        )
        .unwrap();
        let acquired = acquire_loopback_callback(&policy).unwrap();
        let effective = url::Url::parse(&acquired.redirect_uri).unwrap();
        assert_ne!(effective.port(), Some(0));
        policy
            .validate_effective_redirect(&acquired.redirect_uri)
            .unwrap();
    }

    #[test]
    fn protocol_host_exact_redirect_is_preserved() {
        let policy = select_redirect_policy(
            &test_metadata(),
            OAuthClientAuthMode::Byo,
            "burin-labs://oauth/callback",
            CallbackCapabilities::Exact,
        )
        .unwrap();
        assert_eq!(policy.requested_redirect_uri, "burin-labs://oauth/callback");
        assert!(!policy.allow_ephemeral_port);
    }

    #[tokio::test]
    async fn strict_server_observes_acquired_redirect_and_oauth_bindings() {
        let authorization_server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", authorization_server.local_addr().unwrap());
        let registered_redirect = Arc::new(AsyncMutex::new(None::<String>));
        let observed_redirect = registered_redirect.clone();
        let server_issuer = issuer.clone();
        let server = tokio::spawn(async move {
            for _ in 0..4 {
                let (mut stream, _) = authorization_server.accept().await.unwrap();
                let (target, body) = read_http_request(&mut stream).await;
                match target.as_str() {
                    "/mcp" => {
                        write_http_response(
                            &mut stream,
                            "401 Unauthorized",
                            &[(
                                "WWW-Authenticate",
                                format!(
                                    "Bearer resource_metadata=\"{server_issuer}/.well-known/oauth-protected-resource/mcp\""
                                ),
                            )],
                            "",
                        )
                        .await;
                    }
                    "/.well-known/oauth-protected-resource/mcp" => {
                        let response = serde_json::json!({
                            "resource": format!("{server_issuer}/mcp"),
                            "authorization_servers": [&server_issuer],
                            "scopes_supported": ["mcp.read"]
                        })
                        .to_string();
                        write_http_response(
                            &mut stream,
                            "200 OK",
                            &[("Content-Type", "application/json".to_string())],
                            &response,
                        )
                        .await;
                    }
                    "/.well-known/oauth-authorization-server" => {
                        let response = serde_json::json!({
                            "issuer": &server_issuer,
                            "authorization_endpoint": format!("{server_issuer}/authorize"),
                            "token_endpoint": format!("{server_issuer}/token"),
                            "registration_endpoint": format!("{server_issuer}/register"),
                            "token_endpoint_auth_methods_supported": ["none"],
                            "code_challenge_methods_supported": ["S256"]
                        })
                        .to_string();
                        write_http_response(
                            &mut stream,
                            "200 OK",
                            &[("Content-Type", "application/json".to_string())],
                            &response,
                        )
                        .await;
                    }
                    "/register" => {
                        let registration: serde_json::Value =
                            serde_json::from_slice(&body).unwrap();
                        *observed_redirect.lock().await = registration["redirect_uris"][0]
                            .as_str()
                            .map(str::to_string);
                        write_http_response(
                            &mut stream,
                            "201 Created",
                            &[("Content-Type", "application/json".to_string())],
                            r#"{"client_id":"strict-client","token_endpoint_auth_method":"none"}"#,
                        )
                        .await;
                    }
                    _ => panic!("unexpected strict-server request target: {target}"),
                }
            }
        });

        let blocker = TcpListener::bind("127.0.0.1:0").unwrap();
        let requested_redirect = format!(
            "http://127.0.0.1:{}/oauth/callback",
            blocker.local_addr().unwrap().port()
        );
        let callback = Arc::new(LoopbackCallback::new(&requested_redirect));
        let pending = super::super::begin_authorization_with_callback(
            BeginAuthorization {
                server_url: format!("{issuer}/mcp"),
                redirect_uri: requested_redirect.clone(),
                mode: Some(OAuthClientAuthMode::Dcr),
                ..BeginAuthorization::default()
            },
            &AuthorizationCallback::Loopback(callback.clone()),
        )
        .await
        .unwrap();
        let (_listener, effective_redirect) = callback.take_listener().unwrap();
        server.await.unwrap();

        assert_ne!(effective_redirect, requested_redirect);
        assert_eq!(pending.redirect_uri, effective_redirect);
        assert_eq!(
            registered_redirect.lock().await.as_deref(),
            Some(effective_redirect.as_str())
        );
        assert_eq!(pending.resource, format!("{issuer}/mcp"));
        assert_eq!(pending.issuer, issuer);
        let authorize_url = url::Url::parse(&pending.authorize_url).unwrap();
        let query = authorize_url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            query.get("redirect_uri").map(|value| value.as_ref()),
            Some(effective_redirect.as_str())
        );
        assert_eq!(
            query.get("resource").map(|value| value.as_ref()),
            Some(pending.resource.as_str())
        );
        assert_eq!(
            query.get("state").map(|value| value.as_ref()),
            Some(pending.state.as_str())
        );
        assert_eq!(
            query
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
        assert!(query.contains_key("code_challenge"));
    }

    #[tokio::test]
    async fn dynamic_registration_reregisters_effective_redirect_after_port_change() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/register", listener.local_addr().unwrap());
        let registered = Arc::new(AsyncMutex::new(Vec::<String>::new()));
        let server_registered = registered.clone();
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer)
                        .await
                        .unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if let Some(headers_end) =
                        request.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..headers_end]);
                        let content_length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or_default();
                        if request.len() >= headers_end + 4 + content_length {
                            break;
                        }
                    }
                }
                let body_start = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .unwrap()
                    + 4;
                let body: serde_json::Value =
                    serde_json::from_slice(&request[body_start..]).unwrap();
                server_registered
                    .lock()
                    .await
                    .push(body["redirect_uris"][0].as_str().unwrap().to_string());
                let response_body = format!(
                    r#"{{"client_id":"client-{index}","token_endpoint_auth_method":"none"}}"#
                );
                let response = format!(
                    "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes())
                    .await
                    .unwrap();
            }
        });

        let mut metadata = test_metadata();
        metadata.registration_endpoint = Some(endpoint);
        for redirect_uri in [
            "http://127.0.0.1:49152/oauth/callback",
            "http://127.0.0.1:49153/oauth/callback",
        ] {
            super::super::resolve_selected_client(
                &metadata,
                OAuthClientAuthSelection {
                    mode: OAuthClientAuthMode::Dcr,
                    client_id: None,
                },
                None,
                redirect_uri,
                Some("mcp.read"),
            )
            .await
            .unwrap();
        }
        server.await.unwrap();
        assert_eq!(
            *registered.lock().await,
            vec![
                "http://127.0.0.1:49152/oauth/callback",
                "http://127.0.0.1:49153/oauth/callback"
            ]
        );
    }
}
