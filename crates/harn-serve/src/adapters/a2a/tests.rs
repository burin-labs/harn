use super::*;
use super::{auth::*, events::*, schema::*};
use crate::{DispatchCore, DispatchCoreConfig};
use axum::body::{to_bytes, Body};
use axum::http::Request;
use axum::routing::any;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Once;
use tower::ServiceExt;

fn test_server(source: &str) -> (tempfile::TempDir, Arc<A2aServer>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(&script, source).expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    (dir, Arc::new(A2aServer::new(A2aServerConfig::new(core))))
}

#[derive(Clone, Debug)]
struct MockRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Clone)]
struct MockState {
    requests: Arc<Mutex<Vec<MockRequest>>>,
    handler: Arc<dyn Fn(MockRequest) -> Response + Send + Sync>,
}

struct MockServer {
    base_url: String,
    requests: Arc<Mutex<Vec<MockRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_mock_server(
    handler: impl Fn(MockRequest) -> Response + Send + Sync + 'static,
) -> MockServer {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        requests: requests.clone(),
        handler: Arc::new(handler),
    };
    let app = Router::new()
        .route("/{*path}", any(mock_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr = listener.local_addr().expect("mock addr");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    MockServer {
        base_url: format!("http://{addr}"),
        requests,
        task,
    }
}

async fn mock_handler(
    axum::extract::State(state): axum::extract::State<MockState>,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let headers = headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect::<BTreeMap<_, _>>();
    let body = String::from_utf8(body.to_vec()).expect("utf8 request body");
    let request = MockRequest {
        path: format!("{} {}", method.as_str(), uri.path()),
        headers,
        body,
    };
    state
        .requests
        .lock()
        .expect("mock requests poisoned")
        .push(request.clone());
    (state.handler)(request)
}

fn ok_json(value: JsonValue) -> Response {
    Json(value).into_response()
}

fn status_text(status: StatusCode, body: &'static str) -> Response {
    (status, body).into_response()
}

fn hs_jwks(kid: &str, secret: &str) -> JsonValue {
    json!({
        "keys": [{
            "kty": "oct",
            "kid": kid,
            "alg": "HS256",
            "k": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
        }]
    })
}

fn oidc_id_token(kid: &str, secret: &[u8], issuer: &str, audience: &str, exp: i64) -> String {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(kid.to_string());
    encode(
        &header,
        &json!({
            "iss": issuer,
            "aud": audience,
            "sub": "push-sender",
            "iat": 1_700_000_000,
            "exp": exp,
        }),
        &EncodingKey::from_secret(secret),
    )
    .expect("encode id token")
}

fn install_rustls_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

#[tokio::test]
async fn push_delivery_uses_oauth2_client_credentials_token() {
    let server = start_mock_server(|request| match request.path.as_str() {
        "POST /token" => {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Basic cHVzaC1jbGllbnQ6c2VjcmV0")
            );
            assert!(request.body.contains("grant_type=client_credentials"));
            assert!(request.body.contains("scope=push.write"));
            ok_json(json!({
                "access_token": "oauth-access",
                "token_type": "Bearer",
                "expires_in": 300,
            }))
        }
        "POST /push" => {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer oauth-access")
            );
            assert!(request.body.contains("statusUpdate"));
            status_text(StatusCode::OK, "ok")
        }
        _ => status_text(StatusCode::NOT_FOUND, "not found"),
    })
    .await;
    let task = json!({"id": "task-oauth", "status": {"state": "completed"}});
    let config = json!({
        "url": format!("{}/push", server.base_url),
        "authentication": {
            "schemes": ["OAuth2"],
            "token_url": format!("{}/token", server.base_url),
            "client_id": "push-client",
            "client_secret": "secret",
            "scope": "push.write"
        }
    });

    let results = deliver_push_configs(vec![config], task).await;
    assert!(results.into_iter().all(|result| result.is_ok()));
    let requests = server.requests.lock().expect("requests").clone();
    assert_eq!(
        requests
            .iter()
            .map(|req| req.path.as_str())
            .collect::<Vec<_>>(),
        ["POST /token", "POST /push"]
    );
}

#[tokio::test]
async fn push_delivery_supports_static_http_auth_schemes() {
    let server = start_mock_server(|request| match request.path.as_str() {
        "POST /bearer" => {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer bearer-secret")
            );
            status_text(StatusCode::OK, "ok")
        }
        "POST /basic" => {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Basic dXNlcjpwYXNz")
            );
            status_text(StatusCode::OK, "ok")
        }
        "POST /api-key" => {
            assert_eq!(
                request.headers.get("x-api-key").map(String::as_str),
                Some("api-secret")
            );
            status_text(StatusCode::OK, "ok")
        }
        _ => status_text(StatusCode::NOT_FOUND, "not found"),
    })
    .await;
    let task = json!({"id": "task-static-auth", "status": {"state": "completed"}});
    let configs = vec![
        json!({
            "url": format!("{}/bearer", server.base_url),
            "authentication": {"schemes": ["Bearer"], "credentials": "bearer-secret"}
        }),
        json!({
            "url": format!("{}/basic", server.base_url),
            "authentication": {"schemes": ["Basic"], "username": "user", "password": "pass"}
        }),
        json!({
            "url": format!("{}/api-key", server.base_url),
            "authentication": {"schemes": ["ApiKey"], "credentials": "api-secret"}
        }),
    ];

    let results = deliver_push_configs(configs, task).await;
    assert!(results.into_iter().all(|result| result.is_ok()));
    let requests = server.requests.lock().expect("requests").clone();
    assert_eq!(
        requests
            .iter()
            .map(|req| req.path.as_str())
            .collect::<Vec<_>>(),
        ["POST /bearer", "POST /basic", "POST /api-key"]
    );
}

#[tokio::test]
async fn push_delivery_validates_oidc_id_token_before_callback() {
    let token = oidc_id_token(
        "oidc-key",
        b"secret",
        "https://issuer.example",
        "push-client",
        4_102_444_800,
    );
    let server = start_mock_server(move |request| match request.path.as_str() {
        "POST /token" => ok_json(json!({
            "access_token": "opaque",
            "id_token": token.clone(),
            "token_type": "Bearer",
        })),
        "GET /jwks" => ok_json(hs_jwks("oidc-key", "secret")),
        "POST /push" => {
            assert!(request
                .headers
                .get("authorization")
                .is_some_and(|value| value.starts_with("Bearer ")));
            status_text(StatusCode::OK, "ok")
        }
        _ => status_text(StatusCode::NOT_FOUND, "not found"),
    })
    .await;
    let task = json!({"id": "task-oidc", "status": {"state": "completed"}});
    let config = json!({
        "url": format!("{}/push", server.base_url),
        "authentication": {
            "schemes": ["OpenIDConnect"],
            "token_url": format!("{}/token", server.base_url),
            "jwks_url": format!("{}/jwks", server.base_url),
            "issuer": "https://issuer.example",
            "client_id": "push-client",
            "client_secret": "secret",
            // Test fixture mints HS256 tokens so the JWKS can be served
            // inline; production OIDC providers ship RS256 (the
            // default), which is why the JWT verifier requires opting
            // in here.
            "algorithm": "HS256"
        }
    });

    let results = deliver_push_configs(vec![config], task).await;
    assert!(results.into_iter().all(|result| result.is_ok()));
    let requests = server.requests.lock().expect("requests").clone();
    assert_eq!(
        requests
            .iter()
            .map(|req| req.path.as_str())
            .collect::<Vec<_>>(),
        ["POST /token", "GET /jwks", "POST /push"]
    );
}

#[tokio::test]
async fn push_delivery_rejects_oidc_bad_signature_without_callback() {
    let token = oidc_id_token(
        "oidc-bad",
        b"real-secret",
        "https://issuer.example",
        "push-client",
        4_102_444_800,
    );
    let server = start_mock_server(move |request| match request.path.as_str() {
        "POST /token" => ok_json(json!({
            "id_token": token.clone(),
            "token_type": "Bearer",
        })),
        "GET /jwks" => ok_json(hs_jwks("oidc-bad", "wrong-secret")),
        "POST /push" => status_text(StatusCode::INTERNAL_SERVER_ERROR, "unexpected callback"),
        _ => status_text(StatusCode::NOT_FOUND, "not found"),
    })
    .await;
    let task = json!({"id": "task-oidc-bad-signature", "status": {"state": "completed"}});
    let config = json!({
        "url": format!("{}/push", server.base_url),
        "authentication": {
            "schemes": ["oidc"],
            "token_url": format!("{}/token", server.base_url),
            "jwks_url": format!("{}/jwks", server.base_url),
            "issuer": "https://issuer.example",
            "client_id": "push-client",
            "algorithm": "HS256"
        }
    });

    let results = deliver_push_configs(vec![config], task).await;
    let error = results
        .into_iter()
        .next()
        .expect("result")
        .expect_err("bad signature");
    assert!(
        error.to_string().contains("validate OIDC ID token"),
        "{error}"
    );
    let requests = server.requests.lock().expect("requests").clone();
    assert_eq!(
        requests
            .iter()
            .map(|req| req.path.as_str())
            .collect::<Vec<_>>(),
        ["POST /token", "GET /jwks"]
    );
}

#[tokio::test]
async fn push_delivery_rejects_expired_oidc_id_token_without_callback() {
    let token = oidc_id_token(
        "oidc-expired",
        b"expired",
        "https://issuer.example",
        "push-client",
        1,
    );
    let server = start_mock_server(move |request| match request.path.as_str() {
        "POST /token" => ok_json(json!({
            "id_token": token.clone(),
            "token_type": "Bearer",
        })),
        "GET /jwks" => ok_json(hs_jwks("oidc-expired", "expired")),
        "POST /push" => status_text(StatusCode::INTERNAL_SERVER_ERROR, "unexpected callback"),
        _ => status_text(StatusCode::NOT_FOUND, "not found"),
    })
    .await;
    let task = json!({"id": "task-oidc-expired", "status": {"state": "completed"}});
    let config = json!({
        "url": format!("{}/push", server.base_url),
        "authentication": {
            "schemes": ["oidc"],
            "token_url": format!("{}/token", server.base_url),
            "jwks_url": format!("{}/jwks", server.base_url),
            "issuer": "https://issuer.example",
            "client_id": "push-client",
            "algorithm": "HS256"
        }
    });

    let results = deliver_push_configs(vec![config], task).await;
    let error = results
        .into_iter()
        .next()
        .expect("result")
        .expect_err("expired token");
    assert!(
        error.to_string().contains("validate OIDC ID token"),
        "{error}"
    );
    let requests = server.requests.lock().expect("requests").clone();
    assert_eq!(
        requests
            .iter()
            .map(|req| req.path.as_str())
            .collect::<Vec<_>>(),
        ["POST /token", "GET /jwks"]
    );
}

#[tokio::test]
async fn push_delivery_refetches_jwks_when_oidc_kid_rotates() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let fetches_for_handler = fetches.clone();
    let first = oidc_id_token(
        "old-key",
        b"old",
        "https://issuer.example",
        "push-client",
        4_102_444_800,
    );
    let second = oidc_id_token(
        "new-key",
        b"new",
        "https://issuer.example",
        "push-client",
        4_102_444_800,
    );
    let token_index = Arc::new(AtomicUsize::new(0));
    let token_index_for_handler = token_index.clone();
    let server = start_mock_server(move |request| match request.path.as_str() {
        "POST /token" => {
            let index = token_index_for_handler.fetch_add(1, AtomicOrdering::SeqCst);
            ok_json(json!({
                "id_token": if index == 0 { first.clone() } else { second.clone() },
                "token_type": "Bearer",
            }))
        }
        "GET /jwks" => {
            let index = fetches_for_handler.fetch_add(1, AtomicOrdering::SeqCst);
            if index == 0 {
                ok_json(hs_jwks("old-key", "old"))
            } else {
                ok_json(hs_jwks("new-key", "new"))
            }
        }
        "POST /push" => status_text(StatusCode::OK, "ok"),
        _ => status_text(StatusCode::NOT_FOUND, "not found"),
    })
    .await;
    let config = json!({
        "url": format!("{}/push", server.base_url),
        "authentication": {
            "schemes": ["oidc"],
            "token_url": format!("{}/token", server.base_url),
            "jwks_url": format!("{}/jwks", server.base_url),
            "issuer": "https://issuer.example",
            "client_id": "push-client",
            "algorithm": "HS256"
        }
    });

    for task_id in ["task-rotated-1", "task-rotated-2"] {
        let results = deliver_push_configs(
            vec![config.clone()],
            json!({"id": task_id, "status": {"state": "completed"}}),
        )
        .await;
        assert!(results.into_iter().all(|result| result.is_ok()));
    }
    assert_eq!(fetches.load(AtomicOrdering::SeqCst), 2);
}

#[tokio::test]
async fn push_delivery_loads_mtls_client_cert_and_key() {
    install_rustls_provider();
    let temp = tempfile::tempdir().expect("tempdir");
    let server_cert =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("server cert");
    let client_cert = rcgen::generate_simple_self_signed(vec!["harn-push-client".to_string()])
        .expect("client cert");
    let server_cert_path = temp.path().join("server-cert.pem");
    let client_cert_path = temp.path().join("client-cert.pem");
    let client_key_path = temp.path().join("client-key.pem");
    std::fs::write(&server_cert_path, server_cert.cert.pem()).expect("server cert file");
    std::fs::write(&client_cert_path, client_cert.cert.pem()).expect("client cert file");
    std::fs::write(&client_key_path, client_cert.signing_key.serialize_pem())
        .expect("client key file");

    let mut client_roots = rustls::RootCertStore::empty();
    client_roots
        .add(client_cert.cert.der().clone())
        .expect("client root");
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(client_roots))
        .build()
        .expect("client verifier");
    let server_config = Arc::new(
        rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![server_cert.cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(
                    server_cert.signing_key.serialize_der(),
                )
                .into(),
            )
            .expect("server config"),
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mtls server");
    let port = listener.local_addr().expect("addr").port();
    let thread = std::thread::spawn(move || {
        use std::io::{Read as _, Write as _};

        let (tcp, _) = listener.accept().expect("accept mtls");
        let conn = rustls::ServerConnection::new(server_config).expect("server conn");
        let mut stream = rustls::StreamOwned::new(conn, tcp);
        let mut request = Vec::new();
        let mut buffer = [0u8; 512];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("read mtls request");
            assert_ne!(read, 0, "client closed before request headers");
            request.extend_from_slice(&buffer[..read]);
        }
        assert!(stream
            .conn
            .peer_certificates()
            .is_some_and(|certs| !certs.is_empty()));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
            .expect("write response");
    });

    let task = json!({"id": "task-mtls", "status": {"state": "completed"}});
    let config = json!({
        "url": format!("https://localhost:{port}/push"),
        "authentication": {
            "schemes": ["mTLS"],
            "client_cert": client_cert_path.display().to_string(),
            "client_key": client_key_path.display().to_string(),
            "ca_cert": server_cert_path.display().to_string()
        }
    });

    let results = deliver_push_configs(vec![config], task).await;
    assert!(results.into_iter().all(|result| result.is_ok()));
    thread.join().expect("mtls server thread");
}

mod protocol;
mod sink_lifecycle;
mod tasks;
