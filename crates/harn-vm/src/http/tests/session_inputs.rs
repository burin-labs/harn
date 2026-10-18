use super::*;
use crate::session_inputs::{InputKind, InputLifetime, InputSpec, InputUsage};

#[tokio::test]
async fn named_input_reaches_https_without_entering_results_or_redirects() {
    install_rustls_provider();
    for status in [200, 302] {
        reset_http_state();
        let session = crate::agent_sessions::open_or_create(None).unwrap();
        let _session = crate::agent_sessions::enter_current_session(session.clone());
        let temp = TempDir::new().unwrap();
        let cert = generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_path = temp.path().join("cert.pem");
        std::fs::write(&cert_path, cert.cert.pem()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let config = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.cert.der().clone()],
                    PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
                )
                .unwrap(),
        );
        let server = crate::runtime_stack::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let mut stream = StreamOwned::new(ServerConnection::new(config).unwrap(), tcp);
            let request = read_http_request_generic(&mut stream);
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer opaque-session-canary\r\n"));
            write_http_response_generic(
                &mut stream,
                status,
                &[
                    ("content-type", "text/plain".into()),
                    ("location", "https://must-not-contact.invalid/".into()),
                ],
                "echo opaque-session-canary",
            );
        });
        crate::session_inputs::put(
            &session,
            InputSpec {
                name: "SERVICE_KEY".into(),
                kind: InputKind::Secret,
                lifetime: InputLifetime::Session,
                usage: InputUsage::Websites {
                    origins: vec![origin.clone()],
                },
            },
            "opaque-session-canary".into(),
            false,
            0,
        )
        .unwrap();
        let options = crate::value::json::json_to_vm_value(&serde_json::json!({
            "secret_headers":{"Authorization":{"name":"SERVICE_KEY","prefix":"Bearer "}},
            "tls":{"ca_bundle_path":cert_path.to_string_lossy()},
        }));
        let options = options.as_dict().unwrap();
        push_http_mock(
            "GET",
            "https://denied.example.test/",
            vec![HttpMockResponse::new(200, "incorrect dispatch")],
        );
        let denied = vm_execute_http_request("GET", "https://denied.example.test/", options)
            .await
            .unwrap_err();
        assert!(denied.to_string().contains("not allowed"));
        assert!(
            http_mock_calls_snapshot().is_empty(),
            "denial must precede even mock dispatch"
        );
        let spoofed = crate::value::json::json_to_vm_value(&serde_json::json!({
            "secret_headers":{"Authorization":{"name":"SERVICE_KEY"}},
            "headers":{"Host":"unapproved.example.test"},
        }));
        let error = vm_execute_http_request("GET", &origin, spoofed.as_dict().unwrap())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("custom Host"));
        let response = vm_execute_http_request("GET", &format!("{origin}/secure"), options)
            .await
            .unwrap();
        assert_eq!(
            response.as_dict().unwrap()["status"].as_int(),
            Some(i64::from(status))
        );
        assert_eq!(
            response.as_dict().unwrap()["body"].display(),
            "echo [redacted]"
        );
        assert!(!response.display().contains("opaque-session-canary"));
        server.join().unwrap();
        assert!(crate::agent_sessions::close(&session));
    }
}
