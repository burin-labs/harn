use std::sync::{Arc, Barrier};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::tests::map;
use crate::security::session_environment::declare_session_environment_if_absent;
use crate::security::{EnvironmentPolicyKind, LauncherEnvironment};

#[test]
fn captured_sdk_ecs_authorization_remains_distinct_in_parallel() {
    let files = tempfile::tempdir().expect("isolated ECS configuration");
    let config = files.path().join("config");
    let credentials = files.path().join("credentials");
    std::fs::write(&config, "").expect("empty config fixture");
    std::fs::write(&credentials, "").expect("empty credential fixture");
    let barrier = Arc::new(Barrier::new(2));
    crate::runtime_stack::scope(|scope| {
        for name in ["first", "second"] {
            let listener =
                std::net::TcpListener::bind("127.0.0.1:0").expect("local credential fixture");
            listener.set_nonblocking(true).expect("async listener");
            let address = listener.local_addr().expect("local fixture address");
            let endpoint = format!("http://{address}/credentials");
            let authorization = format!("synthetic-ecs-{name}");
            let snapshot = map(&[
                ("AWS_CONTAINER_CREDENTIALS_FULL_URI", &endpoint),
                ("AWS_CONTAINER_AUTHORIZATION_TOKEN", &authorization),
                ("AWS_CONFIG_FILE", config.to_str().expect("config path")),
                (
                    "AWS_SHARED_CREDENTIALS_FILE",
                    credentials.to_str().expect("credential path"),
                ),
                ("AWS_EC2_METADATA_DISABLED", "true"),
            ]);
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                let environment = LauncherEnvironment::from_snapshot(snapshot)
                    .launch(EnvironmentPolicyKind::Inherited, Vec::new())
                    .expect("captured ECS launch");
                let _environment =
                    declare_session_environment_if_absent(environment);
                assert!(super::super::implicit_discovery_allowed());
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("ECS runtime");
                barrier.wait();
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)
                        .expect("async local credential listener");
                    let server = tokio::spawn(async move {
                        let mut measured = 0;
                        for _ in 0..2 {
                            let (mut socket, _) = listener.accept().await.expect("SDK request");
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") {
                                let mut bytes = [0; 512];
                                let read = socket.read(&mut bytes).await.expect("request header");
                                assert!(read > 0 && request.len() < 16384, "complete bounded SDK request");
                                request.extend_from_slice(&bytes[..read]);
                            }
                            let request = String::from_utf8(request).expect("HTTP request");
                            assert!(request.starts_with("GET /credentials "), "SDK reached local ECS owner");
                            assert!(request.to_ascii_lowercase().contains(&format!("authorization: {authorization}\r\n")), "ECS request used another context's authorization");
                            let body = format!("{{\"AccessKeyId\":\"synthetic-{name}\",\"SecretAccessKey\":\"synthetic-secret\",\"Token\":\"synthetic-{name}-token\",\"Expiration\":\"2099-01-01T00:00:00Z\"}}");
                            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                            socket.write_all(response.as_bytes()).await.expect("credential response");
                            socket.shutdown().await.expect("complete response");
                            measured += 1;
                        }
                        measured
                    });
                    for _ in 0..2 {
                        let credentials = super::super::resolve_aws_credentials("us-east-1")
                            .await.expect("actual SDK ECS resolution");
                        assert_eq!(credentials.access_key_id, format!("synthetic-{name}"));
                        assert_eq!(credentials.secret_access_key, "synthetic-secret");
                        assert_eq!(credentials.session_token, Some(format!("synthetic-{name}-token")));
                    }
                    assert_eq!(server.await.expect("local ECS fixture fired"), 2);
                });
            });
        }
    });
}
