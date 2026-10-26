use super::*;
use crate::session_inputs::{InputKind, InputLifetime, InputSpec, InputUsage};

#[test]
fn mock_recordings_retain_the_request_filter_after_the_private_session_closes() {
    reset_http_state();
    let session = crate::agent_sessions::open_or_create(None).unwrap();
    let parts = {
        let _session = crate::agent_sessions::enter_current_session(session.clone());
        crate::session_inputs::put(
            &session,
            InputSpec {
                name: "RECORD_CANARY".into(),
                kind: InputKind::Secret,
                lifetime: InputLifetime::Session,
                usage: InputUsage::SessionTools,
            },
            "private-record-fixture".into(),
            false,
            0,
        )
        .unwrap();
        let mut body = vec![0, 255];
        body.extend_from_slice(b"public-private-record-fixture-tail");
        let mut options = crate::value::DictMap::new();
        options.insert("body".into(), VmValue::Bytes(Arc::new(body)));
        super::super::client::parse_http_request_parts("POST", &options).unwrap()
    };
    assert!(crate::agent_sessions::close(&session));
    let registry = HttpMockRegistry::default();
    let url = "https://example.test/closed-session-recording";
    registry.register("POST", url, vec![HttpMockResponse::new(200, "ok").into()]);
    let response = super::super::client::harness_mocks::consume_http_mock(
        Some(&registry),
        "POST",
        url,
        &parts,
    )
    .expect("the prepared request must reach the recorder");
    assert_eq!(response.status, 200);
    let calls = registry.calls_value(false);
    assert_eq!(calls.len(), 1);
    let body = calls[0].as_dict().unwrap()["body_bytes"]
        .as_bytes()
        .unwrap();
    assert!(body.starts_with(&[0, 255]) && body.ends_with(b"-tail"));
    assert!(!body
        .windows(b"private-record-fixture".len())
        .any(|window| window == b"private-record-fixture"));
    reset_http_state();
}

#[tokio::test]
async fn explicit_binary_responses_apply_the_current_private_input_filter() {
    reset_http_state();
    let session = crate::agent_sessions::open_or_create(None).unwrap();
    let _session = crate::agent_sessions::enter_current_session(session.clone());
    crate::session_inputs::put(
        &session,
        InputSpec {
            name: "BYTE_CANARY".into(),
            kind: InputKind::Secret,
            lifetime: InputLifetime::Session,
            usage: InputUsage::SessionTools,
        },
        "private-byte-fixture".into(),
        false,
        0,
    )
    .unwrap();
    let url = "https://example.test/private-byte-response";
    push_http_mock(
        "GET",
        url,
        vec![HttpMockResponse::new(
            200,
            "public-private-byte-fixture-tail",
        )],
    );
    let options =
        crate::value::json::json_to_vm_value(&serde_json::json!({"response_type":"bytes"}));
    let response = vm_execute_http_request("GET", url, options.as_dict().unwrap())
        .await
        .unwrap();
    let body = response.as_dict().unwrap()["body_bytes"]
        .as_bytes()
        .unwrap();
    let text = std::str::from_utf8(body).unwrap();
    assert!(text.starts_with("public-") && text.ends_with("-tail"));
    assert!(!text.contains("private-byte-fixture"));
    let mut raw = vec![0, 255];
    raw.extend_from_slice(b"public-private-byte-fixture-tail");
    raw.push(254);
    let request_body = raw.clone();
    let binary = super::super::client::build_http_response(
        200,
        crate::value::DictMap::new(),
        super::super::client::ResponseBody::Bytes(raw),
        url,
        &crate::session_inputs::OutputFilter::capture(),
    );
    let raw = binary.as_dict().unwrap()["body_bytes"].as_bytes().unwrap();
    assert!(raw.starts_with(&[0, 255]) && raw.ends_with(&[254]));
    assert!(!raw
        .windows(b"private-byte-fixture".len())
        .any(|window| window == b"private-byte-fixture"));
    push_http_mock("POST", url, vec![HttpMockResponse::new(200, "ok")]);
    let mut options = crate::value::DictMap::new();
    options.insert("body".into(), VmValue::Bytes(Arc::new(request_body)));
    vm_execute_http_request("POST", url, &options)
        .await
        .unwrap();
    let snapshots = super::super::http_mock_calls_snapshot();
    assert_eq!(snapshots.len(), 2);
    let recorded = snapshots[1].body_bytes.as_ref().unwrap();
    assert!(recorded.starts_with(&[0, 255]) && recorded.ends_with(&[254]));
    assert!(!recorded
        .windows(b"private-byte-fixture".len())
        .any(|window| window == b"private-byte-fixture"));
    for sensitive in [true, false] {
        let calls = super::super::mock::http_mock_calls_value(sensitive);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1].as_dict().unwrap()["body_bytes"].as_bytes(),
            Some(recorded.as_slice())
        );
        assert!(!calls[1].display().contains("private-byte-fixture"));
    }
    assert!(crate::agent_sessions::close(&session));
    reset_http_state();
}

#[tokio::test]
async fn signed_session_input_headers_stay_private_in_mock_recordings() {
    use crate::security::{EnvironmentPolicyKind, LauncherEnvironment};

    install_rustls_provider();
    reset_http_state();
    let environment = LauncherEnvironment::from_snapshot(std::collections::BTreeMap::from([
        (
            "AWS_ACCESS_KEY_ID".into(),
            "synthetic-signing-access".into(),
        ),
        (
            "AWS_SECRET_ACCESS_KEY".into(),
            "synthetic-signing-secret".into(),
        ),
        ("AWS_REGION".into(), "us-east-1".into()),
        ("AWS_EC2_METADATA_DISABLED".into(), "true".into()),
    ]))
    .launch(EnvironmentPolicyKind::Inherited, Vec::new())
    .unwrap();
    let _environment = crate::stdlib::process::declare_session_environment_if_absent(environment);
    let session = crate::agent_sessions::open_or_create(None).unwrap();
    let _session = crate::agent_sessions::enter_current_session(session.clone());
    crate::session_inputs::put(
        &session,
        InputSpec {
            name: "SERVICE_KEY".into(),
            kind: InputKind::Secret,
            lifetime: InputLifetime::Session,
            usage: InputUsage::SessionTools,
        },
        "signed-header-private-canary".into(),
        false,
        0,
    )
    .unwrap();
    let url = "https://example.test/signed-input";
    push_http_mock("GET", url, vec![HttpMockResponse::new(200, "reached")]);
    let options = crate::value::json::json_to_vm_value(&serde_json::json!({
        "secret_headers":{"X-Workspace":{"name":"SERVICE_KEY"}},
        "headers":{"X-Public":"public-control"},
        "aws_sigv4":{"service":"s3", "region":"us-east-1"}
    }));
    let response = vm_execute_http_request("GET", url, options.as_dict().unwrap())
        .await
        .unwrap();
    assert_eq!(response.as_dict().unwrap()["body"].display(), "reached");
    for redact_sensitive in [true, false] {
        let calls = super::super::mock::http_mock_calls_value(redact_sensitive);
        assert_eq!(calls.len(), 1, "the signed request must reach the recorder");
        let headers = calls[0].as_dict().unwrap()["headers"].as_dict().unwrap();
        assert_eq!(headers["x-public"].display(), "public-control");
        assert_eq!(headers["x-workspace"].display(), "[redacted]");
        assert!(!calls[0].display().contains("signed-header-private-canary"));
    }
    assert!(crate::agent_sessions::close(&session));
    reset_http_state();
}

fn tls_config(directory: &TempDir) -> (std::path::PathBuf, Arc<ServerConfig>) {
    let cert = generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_path = directory.path().join("cert.pem");
    std::fs::write(&cert_path, cert.cert.pem()).unwrap();
    let config = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
            )
            .unwrap(),
    );
    (cert_path, config)
}

fn create_http_session(options: VmValue) -> String {
    let mut vm = crate::new_vm();
    super::super::register_http_builtins(&mut vm);
    vm.try_call_sync_builtin_for_test(None, "__http_session", &[options])
        .unwrap()
        .unwrap()
        .display()
}

#[tokio::test]
async fn named_input_on_an_existing_http_session_never_reaches_a_redirected_origin() {
    install_rustls_provider();
    reset_http_state();
    let agent = crate::agent_sessions::open_or_create(None).unwrap();
    let _agent = crate::agent_sessions::enter_current_session(agent.clone());
    let directory = TempDir::new().unwrap();
    let (ca, config) = tls_config(&directory);
    let first = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("https://localhost:{}", first.local_addr().unwrap().port());
    let second = TcpListener::bind("127.0.0.1:0").unwrap();
    let destination = second.local_addr().unwrap();
    let redirected = format!("https://localhost:{}/redirected", destination.port());
    let first_config = config.clone();
    let first_server = crate::runtime_stack::spawn(move || {
        for _ in 0..2 {
            let (tcp, _) = first.accept().unwrap();
            let mut stream =
                StreamOwned::new(ServerConnection::new(first_config.clone()).unwrap(), tcp);
            let request = read_http_request_generic(&mut stream);
            assert!(request.to_ascii_lowercase().contains("x-api-key:"));
            write_http_response_generic(
                &mut stream,
                302,
                &[("location", redirected.clone())],
                "redirect",
            );
        }
    });
    let second_server = crate::runtime_stack::spawn(move || {
        let mut requests = Vec::new();
        loop {
            let (tcp, _) = second.accept().unwrap();
            let mut marker = [0];
            assert_eq!(tcp.peek(&mut marker).unwrap(), 1);
            if marker[0] == b'!' {
                break;
            }
            let mut stream = StreamOwned::new(ServerConnection::new(config.clone()).unwrap(), tcp);
            requests.push(read_http_request_generic(&mut stream));
            write_http_response_generic(&mut stream, 200, &[], "redirect reached");
        }
        requests
    });
    let ordinary = crate::value::json::json_to_vm_value(&serde_json::json!({
        "tls":{"ca_bundle_path":ca.to_string_lossy()},
    }));
    let handle = create_http_session(ordinary);
    let public = crate::value::json::json_to_vm_value(&serde_json::json!({
        "headers":{"X-Api-Key":"public-redirect-control"},
    }));
    let response = super::super::client::vm_execute_http_session_request(
        &handle,
        "GET",
        &origin,
        public.as_dict().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.as_dict().unwrap()["status"].as_int(), Some(200));
    crate::session_inputs::put(
        &agent,
        InputSpec {
            name: "SERVICE_KEY".into(),
            kind: InputKind::Secret,
            lifetime: InputLifetime::Session,
            usage: InputUsage::SessionTools,
        },
        "session-redirect-canary".into(),
        false,
        0,
    )
    .unwrap();
    let private = crate::value::json::json_to_vm_value(&serde_json::json!({
        "secret_headers":{"X-Api-Key":{"name":"SERVICE_KEY"}},
    }));
    let response = super::super::client::vm_execute_http_session_request(
        &handle,
        "GET",
        &origin,
        private.as_dict().unwrap(),
    )
    .await
    .unwrap();
    std::net::TcpStream::connect(destination)
        .unwrap()
        .write_all(b"!")
        .unwrap();
    first_server.join().unwrap();
    let observed = second_server.join().unwrap();
    assert_eq!(
        observed.len(),
        1,
        "Only the ordinary redirect may reach the second origin"
    );
    assert!(observed[0].contains("public-redirect-control"));
    assert_eq!(response.as_dict().unwrap()["status"].as_int(), Some(302));
    assert!(crate::agent_sessions::close(&agent));
}

#[tokio::test]
async fn named_input_website_limit_rejects_a_caller_supplied_proxy_and_ca() {
    install_rustls_provider();
    reset_http_state();
    let agent = crate::agent_sessions::open_or_create(None).unwrap();
    let _agent = crate::agent_sessions::enter_current_session(agent.clone());
    let directory = TempDir::new().unwrap();
    let (ca, config) = tls_config(&directory);
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = proxy.local_addr().unwrap();
    let server = crate::runtime_stack::spawn(move || {
        let mut observed = Vec::new();
        loop {
            let (mut tcp, _) = proxy.accept().unwrap();
            let mut marker = [0];
            assert_eq!(tcp.peek(&mut marker).unwrap(), 1);
            if marker[0] == b'!' {
                break;
            }
            assert!(read_http_request_generic(&mut tcp).starts_with("CONNECT localhost:443 "));
            tcp.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
            let mut stream = StreamOwned::new(ServerConnection::new(config.clone()).unwrap(), tcp);
            observed.push(read_http_request_generic(&mut stream));
            write_http_response_generic(&mut stream, 200, &[], "proxy received");
        }
        observed
    });
    let url = "https://localhost/secure";
    let options = crate::value::json::json_to_vm_value(&serde_json::json!({
        "proxy":format!("http://{address}"),
        "tls":{"ca_bundle_path":ca.to_string_lossy()},
        "secret_headers":{"X-Api-Key":{"name":"SERVICE_KEY"}},
    }));
    let session_options = crate::value::json::json_to_vm_value(&serde_json::json!({
        "proxy":format!("http://{address}"),
        "tls":{"ca_bundle_path":ca.to_string_lossy()},
    }));
    let handle = create_http_session(session_options);
    let bindings = crate::value::json::json_to_vm_value(&serde_json::json!({
        "secret_headers":{"X-Api-Key":{"name":"SERVICE_KEY"}},
    }));
    let spec = InputSpec {
        name: "SERVICE_KEY".into(),
        kind: InputKind::Secret,
        lifetime: InputLifetime::Session,
        usage: InputUsage::SessionTools,
    };
    crate::session_inputs::put(
        &agent,
        spec.clone(),
        "proxy-boundary-canary".into(),
        false,
        0,
    )
    .unwrap();
    let control = vm_execute_http_request("GET", url, options.as_dict().unwrap())
        .await
        .unwrap();
    assert_eq!(control.as_dict().unwrap()["status"].as_int(), Some(200));
    let control = super::super::client::vm_execute_http_session_request(
        &handle,
        "GET",
        url,
        bindings.as_dict().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(control.as_dict().unwrap()["status"].as_int(), Some(200));
    crate::session_inputs::put(
        &agent,
        InputSpec {
            usage: InputUsage::Websites {
                origins: vec!["https://localhost".into()],
            },
            ..spec
        },
        "proxy-boundary-canary".into(),
        true,
        0,
    )
    .unwrap();
    let restricted = vm_execute_http_request("GET", url, options.as_dict().unwrap()).await;
    let restricted_session = super::super::client::vm_execute_http_session_request(
        &handle,
        "GET",
        url,
        bindings.as_dict().unwrap(),
    )
    .await;
    std::net::TcpStream::connect(address)
        .unwrap()
        .write_all(b"!")
        .unwrap();
    let observed = server.join().unwrap();
    assert_eq!(
        observed.len(),
        2,
        "The website-restricted grant must not reach the caller's proxy"
    );
    assert!(observed.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("x-api-key: proxy-boundary-canary\r\n")));
    assert!(restricted.unwrap_err().to_string().contains("not allowed"));
    assert!(restricted_session
        .unwrap_err()
        .to_string()
        .contains("not allowed"));
    assert!(crate::agent_sessions::close(&agent));
}

#[tokio::test]
async fn named_input_checks_origins_without_transport_overrides_and_filters_late_https_output() {
    install_rustls_provider();
    for status in [200, 302] {
        reset_http_state();
        let session = crate::agent_sessions::open_or_create(None).unwrap();
        let _session = crate::agent_sessions::enter_current_session(session.clone());
        let temp = TempDir::new().unwrap();
        let (cert_path, config) = tls_config(&temp);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!(
            "https://localhost:{}",
            listener.local_addr().unwrap().port()
        );
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let server = crate::runtime_stack::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            let mut stream = StreamOwned::new(ServerConnection::new(config).unwrap(), tcp);
            let request = read_http_request_generic(&mut stream);
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer opaque-session-canary\r\n"));
            received_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
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
        let names = crate::value::json::json_to_vm_value(&serde_json::json!({
            "secret_headers":{"Authorization":{"name":"SERVICE_KEY","prefix":"Bearer "}},
        }));
        let approved = format!("{origin}/approved-control");
        push_http_mock(
            "GET",
            &approved,
            vec![HttpMockResponse::new(200, "approved control")],
        );
        let control = vm_execute_http_request("GET", &approved, names.as_dict().unwrap())
            .await
            .unwrap();
        assert_eq!(control.as_dict().unwrap()["status"].as_int(), Some(200));
        assert_eq!(http_mock_calls_snapshot().len(), 1);
        push_http_mock(
            "GET",
            "https://denied.example.test/",
            vec![HttpMockResponse::new(200, "incorrect dispatch")],
        );
        let denied = vm_execute_http_request(
            "GET",
            "https://denied.example.test/",
            names.as_dict().unwrap(),
        )
        .await
        .unwrap_err();
        assert!(denied.to_string().contains("not allowed"));
        assert!(
            http_mock_calls_snapshot().len() == 1,
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
        let private_host = crate::value::json::json_to_vm_value(&serde_json::json!({
            "secret_headers":{"Host":{"name":"SERVICE_KEY"}},
        }));
        let error = vm_execute_http_request("GET", &origin, private_host.as_dict().unwrap())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("Host header"));
        let request_url = format!("{origin}/secure");
        crate::session_inputs::put(
            &session,
            InputSpec {
                name: "SERVICE_KEY".into(),
                kind: InputKind::Secret,
                lifetime: InputLifetime::Session,
                usage: InputUsage::SessionTools,
            },
            "opaque-session-canary".into(),
            true,
            0,
        )
        .unwrap();
        let request = vm_execute_http_request("GET", &request_url, options);
        let close_during_request = async {
            received_rx.await.unwrap();
            assert!(crate::agent_sessions::close(&session));
            continue_tx.send(()).unwrap();
        };
        let (response, ()) = tokio::join!(request, close_during_request);
        let response = response.unwrap();
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
        assert!(!crate::agent_sessions::exists(&session));
    }
}
