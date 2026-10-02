//! Unattended recovery of a legacy OAuth registration, end to end.
//!
//! A provider manifest that declares endpoints but no client id, plus a stored
//! credential from an older release (client id, token auth method, no
//! redirect URI, no client secret), is the shape that failed without a
//! terminal. This drives the real `harn connect <provider>` with stdin
//! detached, an explicit `--redirect-uri` equal to the default, and
//! `--client-secret-from-env`, against an isolated file store and a local
//! OAuth server, through to the stored credential.
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::test_util::process::harn_e2e_command;

const SECRET_VARIABLE: &str = "CONNECT_LEGACY_PROBE_CLIENT_SECRET";
const DEFAULT_REDIRECT: &str = "http://127.0.0.1:0/oauth/callback";

fn harn(directory: &Path) -> Command {
    let mut command = harn_e2e_command();
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory)
        .env("HARN_SECRET_PROVIDERS", "env,file")
        .env("HARN_SECRET_FILE_PATH", directory.join("secrets.json"))
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .env(SECRET_VARIABLE, "synthetic-client-secret")
        .current_dir(directory)
        .stdin(Stdio::null());
    command
}

/// Serves exactly one token request and records its form body.
fn spawn_token_server(listener: TcpListener, seen: Arc<Mutex<Option<String>>>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut content_length = 0usize;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).unwrap();
            if request_line.contains("/token") {
                *seen.lock().unwrap() = Some(String::from_utf8_lossy(&body).into_owned());
                let payload = r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600,"token_type":"Bearer"}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                )
                .unwrap();
                return;
            }
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        }
    });
}

fn query_value(url: &url::Url, key: &str) -> String {
    url.query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| panic!("authorization URL lacks {key}: {url}"))
}

#[test]
fn legacy_registration_recovers_without_a_terminal_and_stores_a_refreshable_credential() {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    let token_body = Arc::new(Mutex::new(None));
    spawn_token_server(listener, token_body.clone());

    fs::write(
        directory.path().join("harn.toml"),
        format!(
            r#"[package]
name = "legacy-fixture"
version = "0.1.0"

[[providers]]
id = "acme"
connector = {{ harn = "connector.harn" }}

[providers.oauth]
resource = "{server}/"
authorization_endpoint = "{server}/authorize"
token_endpoint = "{server}/token"
"#
        ),
    )
    .unwrap();
    fs::write(directory.path().join("connector.harn"), "").unwrap();

    // The credential shape an older release wrote: no redirect URI, no
    // authorization URL, and a confidential client whose secret migration
    // never copies.
    let legacy = serde_json::json!({
        "provider": "acme",
        "access_token": "old-access",
        "token_endpoint": format!("{server}/token"),
        "client_id": "legacy-client",
        "token_endpoint_auth_method": "client_secret_post",
        "resource": format!("{server}/"),
        "connected_at_unix": 1,
    });
    let seed = harn(directory.path())
        .args([
            "run",
            "--environment-policy",
            "isolated",
            "-e",
            &format!(
                "harness.secrets.write(\"acme/oauth-token\", {})",
                serde_json::to_string(&legacy.to_string()).unwrap()
            ),
        ])
        .output()
        .expect("seed legacy record");
    assert!(
        seed.status.success(),
        "{}",
        String::from_utf8_lossy(&seed.stderr)
    );

    let mut child = harn(directory.path())
        .args([
            "connect",
            "acme",
            "--redirect-uri",
            DEFAULT_REDIRECT,
            "--client-secret-from-env",
            SECRET_VARIABLE,
            "--no-open",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn harn connect");

    // `--no-open` prints the authorization URL; act as the browser.
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let authorization_url = loop {
        let line = lines
            .next()
            .expect("connect exited before printing the authorization URL")
            .unwrap();
        if line.starts_with(&format!("{server}/authorize")) {
            break url::Url::parse(&line).unwrap();
        }
    };
    assert_eq!(
        query_value(&authorization_url, "client_id"),
        "legacy-client"
    );
    let redirect = url::Url::parse(&query_value(&authorization_url, "redirect_uri")).unwrap();
    let state = query_value(&authorization_url, "state");
    let mut browser = TcpStream::connect(redirect.socket_addrs(|| None).unwrap()[0]).unwrap();
    write!(
        browser,
        "GET {}?code=synthetic-code&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        redirect.path()
    )
    .unwrap();
    let mut ignored = String::new();
    let _ = browser.read_to_string(&mut ignored);

    let status = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "connect failed:\n{stderr}");
    assert!(!stderr.contains("synthetic-client-secret"), "{stderr}");

    let form = token_body.lock().unwrap().clone().expect("token request");
    let form: std::collections::BTreeMap<String, String> =
        url::form_urlencoded::parse(form.as_bytes())
            .into_owned()
            .collect();
    assert_eq!(form["client_id"], "legacy-client");
    assert_eq!(form["client_secret"], "synthetic-client-secret");
    assert_eq!(form["code"], "synthetic-code");

    // A fresh process reads the recovered, refreshable credential from the
    // same isolated store.
    let read = harn(directory.path())
        .args([
            "run",
            "--environment-policy",
            "isolated",
            "-e",
            "harness.stdio.println(harness.secrets.read(\"acme/oauth-token\"))",
        ])
        .output()
        .expect("read stored credential");
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    let stored: serde_json::Value = serde_json::from_slice(&read.stdout).unwrap();
    assert_eq!(stored["client_id"], "legacy-client");
    assert_eq!(stored["access_token"], "new-access");
    assert_eq!(stored["refresh_token"], "new-refresh");
}
