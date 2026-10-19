use super::*;
use crate::session_inputs::{InputKind, InputLifetime, InputSpec, InputUsage};

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
