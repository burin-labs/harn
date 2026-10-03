//! Legacy OAuth registration recovery: what is recovered, what is never
//! reused, and which inputs count as explicit.
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn legacy_oauth_migration_recovers_registration_without_token_material() {
    use harn_vm::secrets::{MemorySecretProvider, SecretId};

    let legacy_id = SecretId::new("acme", "oauth-token");
    let legacy = MemorySecretProvider::new("harn/legacy-workspace").with_secret(
        legacy_id.clone(),
        br#"{
            "provider":"acme",
            "access_token":"must-not-migrate",
            "refresh_token":"must-not-migrate-either",
            "client_secret":"must-be-prompted-again",
            "client_id":"legacy-client",
            "scope":"tickets.read tickets.write",
            "authorization_url":"https://auth.example.com/authorize",
            "token_url":"https://auth.example.com/token",
            "token_auth_method":"client_secret_post",
            "redirect_uri":"http://127.0.0.1:48765/oauth/callback",
            "resource":"https://api.example.com/"
        }"#,
    );

    let registration = load_legacy_oauth_registration_from(&legacy, &legacy_id)
        .await
        .expect("legacy store is readable")
        .expect("legacy registration is present");
    let request = oauth_request_with_legacy_registration(
        OAuthConnectRequest {
            provider: "acme".to_string(),
            resource: "https://api.example.com/".to_string(),
            authorization_endpoint: None,
            token_endpoint: None,
            registration_endpoint: None,
            client_id: None,
            client_secret: None,
            scopes: None,
            redirect_uri: None,
            token_auth_method: None,
            authorization_params: Default::default(),
            no_open: true,
            json: false,
        },
        registration,
    );

    assert_eq!(request.client_id.as_deref(), Some("legacy-client"));
    assert_eq!(
        request.authorization_endpoint.as_deref(),
        Some("https://auth.example.com/authorize")
    );
    assert_eq!(
        request.token_endpoint.as_deref(),
        Some("https://auth.example.com/token")
    );
    assert_eq!(
        request.scopes.as_deref(),
        Some("tickets.read tickets.write")
    );
    assert_eq!(
        request.token_auth_method.as_deref(),
        Some("client_secret_post")
    );
    assert_eq!(
        request.redirect_uri.as_deref(),
        Some("http://127.0.0.1:48765/oauth/callback")
    );
    assert!(
        request.client_secret.is_none(),
        "the old client secret is never reused"
    );
    assert!(
        migrated_oauth_client_secret_required(&request),
        "a confidential legacy client must ask for its secret again before opening a browser"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn authentic_old_token_requires_the_registered_redirect_again() {
    use harn_vm::secrets::{MemorySecretProvider, SecretId};

    // This is the StoredConnectorToken shape written before the namespace
    // change. That writer did not persist its authorization or redirect URI.
    let id = SecretId::new("acme", "oauth-token");
    let legacy = MemorySecretProvider::new("harn/legacy-workspace").with_secret(
        id.clone(),
        br#"{
            "provider":"acme",
            "access_token":"old-token",
            "token_endpoint":"https://auth.example.com/token",
            "client_id":"legacy-client",
            "token_endpoint_auth_method":"none",
            "resource":"https://api.example.com/",
            "connected_at_unix":1
        }"#,
    );
    let registration = load_legacy_oauth_registration_from(&legacy, &id)
        .await
        .expect("old keyring entry is readable")
        .expect("registration fields exist");
    let request = OAuthConnectRequest {
        provider: "acme".to_string(),
        resource: "https://api.example.com/".to_string(),
        authorization_endpoint: None,
        token_endpoint: None,
        registration_endpoint: None,
        client_id: None,
        client_secret: None,
        scopes: None,
        redirect_uri: None,
        token_auth_method: None,
        authorization_params: Default::default(),
        no_open: true,
        json: false,
    };
    assert!(legacy_registration_missing_redirect(
        &request,
        &registration
    ));
    let explicitly_set = OAuthConnectRequest {
        redirect_uri: Some("http://127.0.0.1:48765/oauth/callback".to_string()),
        ..request.clone()
    };
    assert!(!legacy_registration_missing_redirect(
        &explicitly_set,
        &registration
    ));
    // Passing the default value explicitly is still an explicit choice: the
    // person registered that exact URI with the provider.
    let explicit_default = OAuthConnectRequest {
        redirect_uri: Some(DEFAULT_OAUTH_REDIRECT_URI.to_string()),
        ..request.clone()
    };
    assert!(!legacy_registration_missing_redirect(
        &explicit_default,
        &registration
    ));
    let merged = oauth_request_with_legacy_registration(request, registration);
    assert_eq!(merged.client_id.as_deref(), Some("legacy-client"));
    assert_eq!(merged.redirect_uri, None);
    assert_eq!(merged.redirect_uri(), DEFAULT_OAUTH_REDIRECT_URI);
}

#[test]
fn explicit_default_redirect_uri_parses_as_explicit() {
    let parsed = super::parse_external_provider_connect(
        vec![
            "acme".to_string(),
            "--redirect-uri".to_string(),
            DEFAULT_OAUTH_REDIRECT_URI.to_string(),
        ],
        false,
    )
    .expect("parse");
    assert_eq!(
        parsed.oauth.redirect_uri.as_deref(),
        Some(DEFAULT_OAUTH_REDIRECT_URI)
    );
    let defaulted =
        super::parse_external_provider_connect(vec!["acme".to_string()], false).expect("parse");
    assert_eq!(defaulted.oauth.redirect_uri, None);
}

#[test]
fn client_secret_sources_resolve_and_name_their_failures() {
    let directory = tempfile::tempdir().unwrap();
    let secret_file = directory.path().join("client-secret");
    std::fs::write(&secret_file, "file-secret\n").unwrap();
    let parse = |extra: &[&str]| {
        let mut raw = vec!["acme".to_string()];
        raw.extend(extra.iter().map(|arg| arg.to_string()));
        super::parse_external_provider_connect(raw, false)
    };

    let from_file = parse(&["--client-secret-file", secret_file.to_str().unwrap()]).unwrap();
    assert_eq!(
        resolve_oauth_client_secret(&from_file.oauth).unwrap(),
        Some("file-secret".to_string()),
        "a trailing newline from `echo > file` is not part of the secret"
    );

    let var = "CONNECT_TEST_CLIENT_SECRET_UNSET_7F3A";
    let from_unset_env = parse(&["--client-secret-from-env", var]).unwrap();
    let error = resolve_oauth_client_secret(&from_unset_env.oauth).unwrap_err();
    assert!(error.contains(var), "{error}");

    let empty_file = directory.path().join("empty");
    std::fs::write(&empty_file, "\n").unwrap();
    let from_empty_file = parse(&["--client-secret-file", empty_file.to_str().unwrap()]).unwrap();
    assert!(resolve_oauth_client_secret(&from_empty_file.oauth)
        .unwrap_err()
        .contains("is empty"));

    let none = parse(&[]).unwrap();
    assert_eq!(resolve_oauth_client_secret(&none.oauth).unwrap(), None);

    assert!(
        parse(&[
            "--client-secret",
            "inline",
            "--client-secret-file",
            secret_file.to_str().unwrap(),
        ])
        .is_err(),
        "secret sources are mutually exclusive"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn absent_legacy_oauth_record_is_not_invented() {
    use harn_vm::secrets::{MemorySecretProvider, SecretId};

    let empty = MemorySecretProvider::new("harn/legacy-workspace");
    let missing =
        load_legacy_oauth_registration_from(&empty, &SecretId::new("acme", "oauth-token"))
            .await
            .expect("an absent legacy record is not a store outage");
    assert!(
        missing.is_none(),
        "absence must not synthesize registration metadata"
    );
}

#[test]
fn explicit_oauth_registration_wins_over_every_legacy_field() {
    let request = OAuthConnectRequest {
        provider: "acme".to_string(),
        resource: "https://current.example.com/".to_string(),
        authorization_endpoint: Some("https://current.example.com/authorize".to_string()),
        token_endpoint: Some("https://current.example.com/token".to_string()),
        registration_endpoint: None,
        client_id: Some("current-client".to_string()),
        client_secret: None,
        scopes: Some("current.read".to_string()),
        redirect_uri: Some("http://127.0.0.1:49999/current".to_string()),
        token_auth_method: Some("none".to_string()),
        authorization_params: Default::default(),
        no_open: true,
        json: false,
    };
    let legacy = serde_json::from_value::<LegacyOAuthRegistration>(serde_json::json!({
        "client_id": "legacy-client",
        "scopes": "legacy.read",
        "authorization_endpoint": "https://legacy.example.com/authorize",
        "token_endpoint": "https://legacy.example.com/token",
        "token_endpoint_auth_method": "client_secret_post",
        "redirect_uri": "http://127.0.0.1:48888/legacy",
        "resource": "https://legacy.example.com/"
    }))
    .expect("legacy registration fixture");
    let merged = oauth_request_with_legacy_registration(request, legacy);

    assert_eq!(merged.client_id.as_deref(), Some("current-client"));
    assert_eq!(merged.scopes.as_deref(), Some("current.read"));
    assert_eq!(
        merged.authorization_endpoint.as_deref(),
        Some("https://current.example.com/authorize")
    );
    assert_eq!(
        merged.token_endpoint.as_deref(),
        Some("https://current.example.com/token")
    );
    assert_eq!(merged.token_auth_method.as_deref(), Some("none"));
    assert_eq!(merged.redirect_uri(), "http://127.0.0.1:49999/current");
    assert_eq!(merged.resource, "https://current.example.com/");
}
