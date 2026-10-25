use super::*;
use crate::auth::{ApiKeyAuthConfig, ApiKeyEntry, AuthMethodConfig};
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

struct GatePreparation {
    gate: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}

#[async_trait::async_trait(?Send)]
impl crate::adapters::acp::AcpRuntimeConfigurator for GatePreparation {
    async fn configure(&self, _vm: &mut harn_vm::Vm, _source: Option<&Path>) -> Result<(), String> {
        let gate = self.gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            assert!(entered.send(()).is_ok());
            release
                .await
                .map_err(|_| "preparation gate closed".to_owned())?;
        }
        Ok(())
    }
}

fn policy() -> AuthPolicy {
    AuthPolicy {
        methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
            keys: vec![
                ApiKeyEntry::new("server-a", [])
                    .with_subject("actor-a")
                    .unwrap(),
                ApiKeyEntry::new("server-d", [])
                    .with_subject("actor-d")
                    .unwrap(),
                ApiKeyEntry::new("tenant-b", []).with_tenant("tenant-b"),
                ApiKeyEntry::new("scoped-c", ["tasks:read".to_owned()]),
            ],
        })],
        ..AuthPolicy::default()
    }
}

async fn request(
    app: &Router,
    method: Method,
    path: &str,
    key: Option<&str>,
    body: Value,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    app.clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn tenant_bound_credential_cannot_use_shared_workspace_resources() {
    restricted_credential("tenant-b").await;
}

#[tokio::test]
async fn scoped_credential_cannot_use_shared_workspace_resources() {
    restricted_credential("scoped-c").await;
}

async fn restricted_credential(key: &str) {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    std::fs::write(&script, include_str!("fixtures/task_preparation.harn")).unwrap();
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned())
        .with_auth_policy(policy())
        .with_default_session_mode("code")
        .unwrap();
    config.workspace_root = root.path().into();
    config.acp.runtime_configurator = Arc::new(GatePreparation {
        gate: Mutex::new(Some((entered_tx, release_rx))),
    });
    let state = ApiServer::new(config).state;
    let app = api_router(state.clone());
    let session = request(
        &app,
        Method::POST,
        "/v1/sessions",
        Some("server-a"),
        json!({"workspace_id":"local"}),
    )
    .await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let session_id = read_json(session).await["id"].as_str().unwrap().to_owned();
    let task = request(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session_id}/tasks"),
        Some("server-a"),
        json!({"input":"hold"}),
    )
    .await;
    assert_eq!(task.status(), StatusCode::ACCEPTED);
    let task_id = read_json(task).await["id"].as_str().unwrap().to_owned();
    entered.await.unwrap();
    let task_path = format!("/v1/tasks/{task_id}");
    for permitted in ["server-a", "server-d"] {
        let response = request(&app, Method::GET, &task_path, Some(permitted), json!({})).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            read_json(response).await["id"],
            task_id,
            "distinct server-wide actors share this known resource"
        );
    }
    let artifact = request(&app, Method::POST, "/v1/artifacts", Some("server-a"), json!({"workspace_id":"local", "kind":"other", "mime_type":"text/plain", "visibility":"internal", "content":"known artifact"})).await;
    assert_eq!(artifact.status(), StatusCode::CREATED);
    let artifact_id = read_json(artifact).await["id"].as_str().unwrap().to_owned();
    let operations = [
        (Method::GET, task_path.clone()),
        (Method::GET, "/v1/tasks".into()),
        (Method::POST, format!("{task_path}/messages")),
        (Method::POST, format!("{task_path}/cancel")),
        (Method::GET, format!("{task_path}/events")),
        (Method::GET, format!("{task_path}/events/stream")),
        (Method::GET, format!("{task_path}/stream")),
        (Method::GET, "/v1/events/stream".into()),
        (Method::GET, "/v1/sessions".into()),
        (Method::GET, format!("/v1/sessions/{session_id}")),
        (Method::POST, format!("/v1/sessions/{session_id}/messages")),
        (Method::POST, "/v1/tasks".into()),
        (Method::GET, "/v1/artifacts".into()),
        (Method::GET, format!("/v1/artifacts/{artifact_id}")),
        (Method::GET, "/v1/permission-requests".into()),
        (Method::GET, format!("{task_path}/permission-requests")),
        (Method::GET, "/v1/permissions/policy".into()),
        (Method::GET, "/v1/session-store/sessions".into()),
    ];
    for (method, path) in operations {
        let response = request(
            &app,
            method,
            &path,
            Some(key),
            json!({"content":"must not be queued", "session_id":session_id}),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "restricted credential reached {path}"
        );
        assert_eq!(
            read_json(response).await["error"]["code"],
            "unsupported_authorization_policy"
        );
    }
    // Queued behind the gated prompt, so its answer marks that prompt settled.
    let settled = state.acp.call("session/list", json!({}));
    // Cancel while preparation is gated. This distinct server-wide actor has
    // shared-workspace authority; neither restricted actor could cancel above.
    let response = request(
        &app,
        Method::POST,
        &format!("{task_path}/cancel"),
        Some("server-d"),
        json!({}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_json(response).await["status"], "CANCELED");
    assert!(release.send(()).is_ok());
    settled.await.unwrap();
    assert!(!root.path().join("task-preparation.txt").exists());
    for path in ["/health", "/version", "/openapi.json", "/v1"] {
        assert_eq!(
            request(&app, Method::GET, path, Some(key), json!({}))
                .await
                .status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn canonical_store_mount_requires_server_wide_authentication() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    write_test_pipeline(&script);
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned())
        .with_auth_policy(policy());
    config.workspace_root = root.path().into();
    let app = api_router(ApiServer::new(config).state);
    let session = request(
        &app,
        Method::POST,
        "/v1/session-store/sessions",
        Some("server-a"),
        json!({}),
    )
    .await;
    assert_eq!(session.status(), StatusCode::CREATED);
    let session_id = read_json(session).await["id"].as_str().unwrap().to_owned();
    let path = format!("/v1/session-store/sessions/{session_id}");
    let known = request(&app, Method::GET, &path, Some("server-d"), json!({})).await;
    assert_eq!(known.status(), StatusCode::OK);
    assert_eq!(read_json(known).await["id"], session_id);
    let response = request(&app, Method::GET, &path, None, json!({})).await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated caller reached a known canonical session"
    );
}

fn signed_body(path: &str, body: &[u8]) -> String {
    let material = format!("POST\n{path}\n0\n{}", hex::encode(Sha256::digest(body)));
    let mut mac = Hmac::<Sha256>::new_from_slice(b"test-shared-secret").unwrap();
    mac.update(material.as_bytes());
    format!(
        "HMAC-SHA256 timestamp=0,signature={}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    )
}

#[tokio::test]
async fn shared_workspace_authentication_preserves_signed_bodies_and_nested_paths() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.harn");
    write_test_pipeline(&script);
    let policy = AuthPolicy {
        methods: vec![AuthMethodConfig::Hmac(crate::auth::HmacAuthConfig {
            shared_secret: "test-shared-secret".into(),
            provider: "test-host".into(),
            // Fixed timestamp makes this a body/path authentication control,
            // independent of wall-clock time. Expiry has its own auth tests.
            timestamp_window: time::Duration::MAX,
            granted_scopes: BTreeSet::new(),
            tenant_id: None,
        })],
        ..AuthPolicy::default()
    };
    let mut config = ApiServerConfig::for_pipeline(script.to_string_lossy().into_owned())
        .with_auth_policy(policy);
    config.workspace_root = root.path().into();
    let app = api_router(ApiServer::new(config).state);
    for (path, body) in [
        (
            "/v1/workspaces",
            br#"{"name":"signed workspace","root":"."}"#.as_slice(),
        ),
        ("/v1/session-store/sessions", b"{}".as_slice()),
    ] {
        let authorization = signed_body(path, body);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("authorization", &authorization)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "signed {path} did not reach its actual handler"
        );
        let resource = read_json(response).await;
        assert!(resource["id"].as_str().is_some());
        if path == "/v1/workspaces" {
            assert_eq!(resource["name"], "signed workspace");
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("authorization", authorization)
                    .header("content-type", "application/json")
                    .body(Body::from("{\"name\":\"altered\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "changed signed body reached {path}"
        );
    }
    let body = vec![b' '; crate::DEFAULT_HTTP_BODY_LIMIT_BYTES + 1];
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/workspaces")
                .header("authorization", signed_body("/v1/workspaces", &body))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
