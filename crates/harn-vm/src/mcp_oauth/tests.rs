use super::*;
use std::collections::HashMap as StdHashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn token_store_account_is_stable_and_dimension_sensitive() {
    let first = token_store_account("https://wiki.example", "https://auth.example", "client-a");
    let second = token_store_account("https://wiki.example", "https://auth.example", "client-a");
    let other_issuer =
        token_store_account("https://wiki.example", "https://other.example", "client-a");
    let other_resource =
        token_store_account("https://mcp.linear.app", "https://auth.example", "client-a");
    let other_client =
        token_store_account("https://wiki.example", "https://auth.example", "client-b");
    assert_eq!(first, second);
    assert_ne!(first, other_issuer);
    assert_ne!(first, other_resource);
    assert_ne!(first, other_client);
    assert!(first.starts_with("mcp-token-"));
}

#[test]
fn token_needs_refresh_respects_skew() {
    let mut token = StoredMcpToken {
        access_token: "a".into(),
        refresh_token: Some("r".into()),
        expires_at_unix: None,
        token_endpoint: "https://auth/token".into(),
        client_id: "c".into(),
        client_secret: None,
        token_endpoint_auth_method: "none".into(),
        issuer: "https://auth".into(),
        resource: "https://mcp".into(),
        scopes: None,
        token_response_extra: None,
    };
    assert!(!token_needs_refresh(&token));
    token.expires_at_unix = Some(current_unix_timestamp() + 3600);
    assert!(!token_needs_refresh(&token));
    token.expires_at_unix = Some(current_unix_timestamp() + TOKEN_REFRESH_SKEW_SECS - 1);
    assert!(token_needs_refresh(&token));
}

#[test]
fn token_response_rejects_negative_expiry() {
    let error = expires_at_from_expires_in(Some(-1)).unwrap_err();
    assert!(error.contains("expires_in"), "{error}");
}

#[test]
fn oauth_http_error_omits_response_body() {
    let error = oauth_http_error(
        "Token request failed",
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"access_token":"secret","error":"invalid_grant","error_description":"bad"}"#,
    );
    assert!(error.contains("400 Bad Request"), "{error}");
    assert!(error.contains("invalid_grant"), "{error}");
    assert!(error.contains("response body omitted"), "{error}");
    assert!(!error.contains("secret"), "{error}");
    assert!(!error.contains("bad"), "{error}");
}

fn test_discovery() -> McpOAuthDiscovery {
    McpOAuthDiscovery {
        protected_resource_metadata_url: url::Url::parse(
            "https://mcp.example/.well-known/oauth-protected-resource",
        )
        .unwrap(),
        protected_resource_metadata: Default::default(),
        authorization_server_issuer: "https://auth.example".to_string(),
        authorization_server_metadata_url: url::Url::parse(
            "https://auth.example/.well-known/oauth-authorization-server",
        )
        .unwrap(),
        authorization_server_metadata_kind:
            crate::mcp_auth::OAuthAuthorizationServerMetadataKind::OAuthAuthorizationServer,
        authorization_server_metadata: OAuthAuthorizationServerMetadata {
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
        },
        challenge: None,
        scopes: Vec::new(),
    }
}

#[derive(Clone, Default)]
struct MemoryStore {
    tokens: Arc<AsyncMutex<StdHashMap<OAuthTokenStoreKey, StoredMcpToken>>>,
    index: Arc<AsyncMutex<StdHashMap<(String, String), String>>>,
}

#[async_trait]
impl OAuthTokenStorage for MemoryStore {
    async fn load_token(&self, key: &OAuthTokenStoreKey) -> Result<Option<StoredMcpToken>, String> {
        Ok(self.tokens.lock().await.get(key).cloned())
    }

    async fn save_token(&self, token: &StoredMcpToken) -> Result<(), String> {
        self.tokens
            .lock()
            .await
            .insert(OAuthTokenStoreKey::from_token(token), token.clone());
        self.save_active_client_id(&token.resource, &token.issuer, &token.client_id)
            .await
    }

    async fn delete_token(&self, key: &OAuthTokenStoreKey) -> Result<(), String> {
        self.tokens.lock().await.remove(key);
        Ok(())
    }

    async fn load_active_client_id(
        &self,
        resource: &str,
        issuer: &str,
    ) -> Result<Option<String>, String> {
        Ok(self
            .index
            .lock()
            .await
            .get(&(resource.to_string(), issuer.to_string()))
            .cloned())
    }

    async fn save_active_client_id(
        &self,
        resource: &str,
        issuer: &str,
        client_id: &str,
    ) -> Result<(), String> {
        self.index.lock().await.insert(
            (resource.to_string(), issuer.to_string()),
            client_id.to_string(),
        );
        Ok(())
    }

    async fn delete_active_client_id(&self, resource: &str, issuer: &str) -> Result<(), String> {
        self.index
            .lock()
            .await
            .remove(&(resource.to_string(), issuer.to_string()));
        Ok(())
    }
}

#[test]
fn token_import_uses_discovery_binding_and_legacy_auth_defaults() {
    let discovery = test_discovery();
    let token = stored_token_for_import(
        &ImportStoredToken {
            server_url: "https://mcp.example/mcp".to_string(),
            access_token: "access".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at_unix: Some(123),
            token_endpoint: Some("  ".to_string()),
            client_id: "client".to_string(),
            client_secret: Some(" secret ".to_string()),
            token_endpoint_auth_method: Some(" client_secret_post ".to_string()),
            scopes: Some(" read write ".to_string()),
        },
        &discovery,
    )
    .unwrap();

    assert_eq!(token.issuer, "https://auth.example");
    assert_eq!(token.resource, "https://mcp.example/mcp");
    assert_eq!(token.token_endpoint, "https://auth.example/token");
    assert_eq!(token.client_id, "client");
    assert_eq!(token.client_secret.as_deref(), Some("secret"));
    assert_eq!(token.token_endpoint_auth_method, "client_secret_post");
    assert_eq!(token.refresh_token.as_deref(), Some("refresh"));
    assert_eq!(token.expires_at_unix, Some(123));
    assert_eq!(token.scopes.as_deref(), Some("read write"));
}

#[test]
fn token_import_rejects_negative_expiry() {
    let discovery = test_discovery();
    let error = stored_token_for_import(
        &ImportStoredToken {
            server_url: "https://mcp.example/mcp".to_string(),
            access_token: "access".to_string(),
            refresh_token: None,
            expires_at_unix: Some(-1),
            token_endpoint: None,
            client_id: "client".to_string(),
            client_secret: None,
            token_endpoint_auth_method: None,
            scopes: None,
        },
        &discovery,
    )
    .unwrap_err();
    assert!(error.contains("expires_at"), "{error}");
}

#[tokio::test]
async fn complete_authorization_rejects_unknown_state() {
    let error = complete_authorization("no-such-state", "code", None)
        .await
        .unwrap_err();
    assert!(error.contains("no pending MCP authorization"), "{error}");
}

/// Concurrent refreshers must collapse to a single token-endpoint call and
/// converge on the rotated token.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_token_refresh_is_singleflight() {
    // A minimal token endpoint over a raw TCP listener that counts hits.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_endpoint_url = format!("http://{}/token", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let server_calls = calls.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            server_calls.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
            let body =
                r#"{"access_token":"access-new","refresh_token":"refresh-new","expires_in":3600}"#;
            let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
        }
    });

    let store = Arc::new(MemoryStore::default());
    let stale = StoredMcpToken {
        access_token: "access-old".to_string(),
        refresh_token: Some("refresh-old".to_string()),
        expires_at_unix: Some(current_unix_timestamp().saturating_sub(1)),
        token_endpoint: token_endpoint_url.clone(),
        client_id: "client-a".to_string(),
        client_secret: None,
        token_endpoint_auth_method: "none".to_string(),
        issuer: "https://auth.example".to_string(),
        resource: "https://mcp.example/mcp".to_string(),
        scopes: None,
        token_response_extra: None,
    };
    store.save_token(&stale).await.unwrap();

    let discovery_meta = OAuthAuthorizationServerMetadata {
        issuer: stale.issuer.clone(),
        authorization_endpoint: "https://auth.example/authorize".to_string(),
        token_endpoint: token_endpoint_url,
        registration_endpoint: None,
        token_endpoint_auth_methods_supported: vec!["none".to_string()],
        code_challenge_methods_supported: vec!["S256".to_string()],
        scopes_supported: Vec::new(),
        client_id_metadata_document_supported: false,
        authorization_response_iss_parameter_supported: false,
        extra: Default::default(),
    };
    let discovery = McpOAuthDiscovery {
        protected_resource_metadata_url: url::Url::parse(
            "https://mcp.example/.well-known/oauth-protected-resource",
        )
        .unwrap(),
        protected_resource_metadata: Default::default(),
        authorization_server_issuer: stale.issuer.clone(),
        authorization_server_metadata_url: url::Url::parse(
            "https://auth.example/.well-known/oauth-authorization-server",
        )
        .unwrap(),
        authorization_server_metadata_kind:
            crate::mcp_auth::OAuthAuthorizationServerMetadataKind::OAuthAuthorizationServer,
        authorization_server_metadata: discovery_meta,
        challenge: None,
        scopes: Vec::new(),
    };
    let lock_dir = tempfile::tempdir().unwrap();

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        let token = stale.clone();
        let discovery = discovery.clone();
        let lock_dir = lock_dir.path().to_path_buf();
        tasks.push(tokio::spawn(async move {
            refresh_stored_token_with_store(store.as_ref(), &token, &discovery, Some(lock_dir))
                .await
                .unwrap()
        }));
    }
    for task in tasks {
        let refreshed = task.await.unwrap();
        assert_eq!(refreshed.access_token, "access-new");
        assert_eq!(refreshed.refresh_token.as_deref(), Some("refresh-new"));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "refresh must be single-flight"
    );
    let stored = store
        .load_token(&OAuthTokenStoreKey::from_token(&stale))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.access_token, "access-new");
    server.abort();
}

#[tokio::test]
async fn invalid_grant_refresh_clears_stored_token_and_active_index() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_endpoint_url = format!("http://{}/token", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 4096];
        let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
        let body = r#"{"error":"invalid_grant","error_description":"refresh token was reused","access_token":"secret"}"#;
        let response = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
    });

    let store = MemoryStore::default();
    let stale = StoredMcpToken {
        access_token: "access-old".to_string(),
        refresh_token: Some("refresh-old".to_string()),
        expires_at_unix: Some(current_unix_timestamp().saturating_sub(1)),
        token_endpoint: token_endpoint_url.clone(),
        client_id: "client-a".to_string(),
        client_secret: None,
        token_endpoint_auth_method: "none".to_string(),
        issuer: "https://auth.example".to_string(),
        resource: "https://mcp.example/mcp".to_string(),
        scopes: None,
        token_response_extra: None,
    };
    store.save_token(&stale).await.unwrap();

    let mut discovery = test_discovery();
    discovery.authorization_server_metadata.token_endpoint = token_endpoint_url;
    let lock_dir = tempfile::tempdir().unwrap();

    let error =
        refresh_stored_token_with_store(&store, &stale, &discovery, Some(lock_dir.path().into()))
            .await
            .unwrap_err();

    assert!(error.contains("invalid_grant"), "{error}");
    assert!(error.contains("re-authorization"), "{error}");
    assert!(!error.contains("secret"), "{error}");
    let key = OAuthTokenStoreKey::from_token(&stale);
    assert!(store.load_token(&key).await.unwrap().is_none());
    assert!(store
        .load_active_client_id(&stale.resource, &stale.issuer)
        .await
        .unwrap()
        .is_none());
    server.await.unwrap();
}

/// Restores the production OAuth HTTP timeout when dropped, even if the
/// test panics.
struct HttpTimeoutOverride;

impl HttpTimeoutOverride {
    fn set(ms: u64) -> Self {
        OAUTH_HTTP_TIMEOUT_OVERRIDE_MS.store(ms, Ordering::SeqCst);
        Self
    }
}

impl Drop for HttpTimeoutOverride {
    fn drop(&mut self) {
        OAUTH_HTTP_TIMEOUT_OVERRIDE_MS.store(0, Ordering::SeqCst);
    }
}

/// A token endpoint that accepts TCP connections but never responds must
/// produce a timeout error — not hang the refresh (and the single-flight
/// refresh lock behind it) forever.
#[tokio::test]
async fn refresh_times_out_when_token_endpoint_stalls() {
    // Accept and hold connections without ever reading or responding. The
    // thread parks on `accept` and is reclaimed at process exit.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let token_endpoint_url = format!("http://{}/token", listener.local_addr().unwrap());
    crate::runtime_stack::spawn(move || {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept() {
            held.push(stream);
        }
    });

    let _timeout = HttpTimeoutOverride::set(1_000);

    let store = MemoryStore::default();
    let stale = StoredMcpToken {
        access_token: "access-old".to_string(),
        refresh_token: Some("refresh-old".to_string()),
        expires_at_unix: Some(current_unix_timestamp().saturating_sub(1)),
        token_endpoint: token_endpoint_url.clone(),
        client_id: "client-a".to_string(),
        client_secret: None,
        token_endpoint_auth_method: "none".to_string(),
        issuer: "https://auth.example".to_string(),
        resource: "https://mcp.example/stalled".to_string(),
        scopes: None,
        token_response_extra: None,
    };
    store.save_token(&stale).await.unwrap();
    let mut discovery = test_discovery();
    discovery.authorization_server_metadata.token_endpoint = token_endpoint_url;
    let lock_dir = tempfile::tempdir().unwrap();

    let error = harn_clock::test_support::within(
        "refresh against a stalled token endpoint timing out",
        refresh_stored_token_with_store(&store, &stale, &discovery, Some(lock_dir.path().into())),
    )
    .await
    .unwrap_err();
    assert!(error.contains("Token request failed"), "{error}");

    // The failed refresh must release the single-flight lock so later 401
    // recovery is not blocked behind the wedged attempt.
    let key = OAuthTokenStoreKey::from_token(&stale);
    let guard = harn_clock::test_support::within(
        "refresh lock released after a timed-out refresh",
        acquire_oauth_refresh_lock(&key, Some(lock_dir.path())),
    )
    .await
    .unwrap();
    drop(guard);
    // The stored token must survive a transient timeout (unlike
    // invalid_grant, which deletes it).
    assert!(store.load_token(&key).await.unwrap().is_some());
}

#[tokio::test]
async fn refresh_lock_times_out_when_in_process_holder_is_wedged() {
    let key = OAuthTokenStoreKey::new(
        "https://mcp.example/wedged-mutex",
        "https://auth.example",
        "client-a",
    );
    let lock_dir = tempfile::tempdir().unwrap();
    let held = oauth_refresh_mutex(&key).lock_owned().await;

    let error = acquire_oauth_refresh_lock_with_timeout(
        &key,
        Some(lock_dir.path()),
        std::time::Duration::from_millis(200),
    )
    .await
    .unwrap_err();
    assert!(error.contains("in-process OAuth refresh lock"), "{error}");
    assert!(error.contains("stuck"), "{error}");

    drop(held);
    let _guard = acquire_oauth_refresh_lock_with_timeout(
        &key,
        Some(lock_dir.path()),
        std::time::Duration::from_millis(200),
    )
    .await
    .expect("lock must be acquirable once the holder releases it");
}

#[tokio::test]
async fn refresh_lock_times_out_when_cross_process_holder_is_wedged() {
    let key = OAuthTokenStoreKey::new(
        "https://mcp.example/wedged-file",
        "https://auth.example",
        "client-a",
    );
    let lock_dir = tempfile::tempdir().unwrap();
    let lock_path = oauth_refresh_lock_path(&key, Some(lock_dir.path()));

    // Hold the file lock on a separate descriptor, standing in for a
    // wedged *other process* (flock contention is per open file
    // description, so this contends exactly like another process would).
    let holder = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    holder.lock().unwrap();

    let error = acquire_oauth_refresh_lock_with_timeout(
        &key,
        Some(lock_dir.path()),
        std::time::Duration::from_millis(300),
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("cross-process OAuth refresh lock"),
        "{error}"
    );

    holder.unlock().unwrap();
    let _guard = acquire_oauth_refresh_lock_with_timeout(
        &key,
        Some(lock_dir.path()),
        std::time::Duration::from_millis(300),
    )
    .await
    .expect("lock must be acquirable once the holder releases it");
}
