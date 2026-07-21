//! Egress policy unit and loopback-integration tests, relocated out of
//! `mod.rs` to keep the module under the source-length ratchet.

use super::*;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Once};

fn install(config: &[(&str, VmValue)]) -> EgressTestEnvGuard {
    // `test_env_guard` clears ambient `HARN_EGRESS_*` and resets policy.
    let guard = test_env_guard();
    let map = config
        .iter()
        .cloned()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    let policy = policy_from_config(&map).expect("policy parses");
    install_policy(policy, "test").expect("policy installs");
    guard
}

fn strings(values: &[&str]) -> VmValue {
    VmValue::List(std::sync::Arc::new(
        values
            .iter()
            .map(|value| VmValue::String(arcstr::ArcStr::from(*value)))
            .collect(),
    ))
}

#[test]
fn exact_host_and_port_restriction() {
    let _guard = install(&[
        ("allow", strings(&["api.example.com:443"])),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    assert!(check_url("http_get", "https://api.example.com/users")
        .unwrap()
        .is_none());
    let blocked = check_url("http_get", "http://api.example.com/users")
        .unwrap()
        .expect("port mismatch blocks");
    assert_eq!(blocked.host, "api.example.com");
    assert_eq!(blocked.port, Some(80));
}

#[test]
fn suffix_wildcard_matches_subdomains_only() {
    let _guard = install(&[
        ("allow", strings(&["*.example.com"])),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    assert!(check_url("http_get", "https://api.example.com")
        .unwrap()
        .is_none());
    assert!(check_url("http_get", "https://example.com")
        .unwrap()
        .is_some());
}

#[test]
fn cidr_matches_ip_literals() {
    let _guard = install(&[
        ("allow", strings(&["127.0.0.0/8"])),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    assert!(check_url("http_get", "http://127.10.20.30:8080")
        .unwrap()
        .is_none());
    assert!(check_url("http_get", "http://192.168.1.1")
        .unwrap()
        .is_some());
}

#[test]
fn deny_overrides_allow() {
    let _guard = install(&[
        ("allow", strings(&["*.example.com"])),
        ("deny", strings(&["blocked.example.com"])),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    let blocked = check_url("http_get", "https://blocked.example.com")
        .unwrap()
        .expect("deny wins");
    assert!(blocked.reason.contains("deny rule"));
}

#[test]
fn blocked_urls_redact_sensitive_query_values() {
    let _guard = install(&[("default", VmValue::String(arcstr::ArcStr::from("deny")))]);
    let blocked = check_url(
        "http_get",
        "https://api.example.com/resource?access_token=secret-token&ok=1",
    )
    .unwrap()
    .expect("default deny blocks");

    assert_eq!(
        blocked.url,
        "https://api.example.com/resource?access_token=%5Bredacted%5D&ok=1"
    );
    assert!(!blocked.to_string().contains("secret-token"));
}

#[test]
fn require_explicit_policy_blocks_unconfigured_egress() {
    let _guard = test_env_guard();
    {
        let _scope = require_explicit_egress_policy_for_host();
        let blocked = check_url("http_get", "https://api.example.com/status")
            .unwrap()
            .expect("missing policy blocks");
        assert_eq!(blocked.reason, "no egress policy configured");
        assert_eq!(blocked.host, "api.example.com");
    }
    assert!(check_url("http_get", "https://api.example.com/status")
        .unwrap()
        .is_none());
}

#[test]
fn reset_thread_local_state_clears_required_policy_scope() {
    let _guard = test_env_guard();
    let _scope = require_explicit_egress_policy_for_host();

    crate::reset_thread_local_state();

    assert!(check_url("http_get", "https://api.example.com/status")
        .unwrap()
        .is_none());
}

#[test]
fn explicit_environment_policy_satisfies_required_policy_scope() {
    let guard = test_env_guard();
    guard.set(HARN_EGRESS_ALLOW_ENV, "api.example.com");
    guard.set(HARN_EGRESS_DEFAULT_ENV, "deny");
    let _scope = require_explicit_egress_policy_for_host();

    assert!(check_url("http_get", "https://api.example.com/status")
        .unwrap()
        .is_none());
    assert!(check_url("http_get", "https://other.example.com/status")
        .unwrap()
        .is_some());
}

#[test]
fn env_seeding_is_honored() {
    let guard = test_env_guard();
    guard.set(HARN_EGRESS_ALLOW_ENV, "");
    guard.set(HARN_EGRESS_DENY_ENV, "blocked-env.example.com");
    guard.set(HARN_EGRESS_DEFAULT_ENV, "allow");
    assert!(check_url("http_get", "https://env.example.com")
        .unwrap()
        .is_none());
    assert!(check_url("http_get", "https://blocked-env.example.com")
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn reqwest_redirect_policy_blocks_https_to_http_downgrade() {
    install_rustls_provider();

    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .expect("generate cert");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls listener");
    let port = listener.local_addr().expect("tls addr").port();
    let server_config = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
            )
            .expect("build tls config"),
    );
    let thread = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().expect("accept tls client");
        let conn = ServerConnection::new(server_config).expect("server connection");
        let mut stream = StreamOwned::new(conn, tcp);
        let request = read_http_request(&mut stream);
        assert!(request.starts_with("GET /start HTTP/1.1\r\n"));
        write_http_redirect_response(
            &mut stream,
            "http://public.example.com/next?access_token=target-secret",
        );
    });

    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(2))
        .redirect(redirect_policy("test_redirect", 10))
        .build()
        .expect("client builds");
    let error = client
        .get(format!("https://localhost:{port}/start"))
        .send()
        .await
        .expect_err("downgrade redirect should be blocked by policy");
    assert!(error.is_redirect(), "{error}");
    let redacted = redact_reqwest_error(&error);
    assert!(!redacted.contains("target-secret"), "{redacted}");

    thread.join().expect("tls thread");
}

#[test]
fn redirect_blocks_public_https_to_http_downgrade() {
    let _guard = test_env_guard();
    let blocked = check_redirect_url(
        "http_redirect",
        Some("https://api.example.com/start?access_token=source-secret"),
        "http://public.example.com/next?access_token=target-secret",
    )
    .unwrap()
    .expect("public https-to-http redirect is blocked");

    assert_eq!(blocked.host, "public.example.com");
    assert_eq!(blocked.port, Some(80));
    assert_eq!(
        blocked.reason,
        "https redirect target downgrades to insecure http"
    );
    assert!(!blocked.url.contains("target-secret"), "{}", blocked.url);
}

#[test]
fn redirect_allows_same_scheme_and_secure_upgrade() {
    let _guard = test_env_guard();

    for (previous, target) in [
        (
            "https://api.example.com/start",
            "https://cdn.example.com/next",
        ),
        (
            "http://api.example.com/start",
            "http://cdn.example.com/next",
        ),
        (
            "http://api.example.com/start",
            "https://cdn.example.com/next",
        ),
    ] {
        assert!(
            check_redirect_url("http_redirect", Some(previous), target)
                .unwrap()
                .is_none(),
            "{previous} -> {target} should be allowed"
        );
    }
}

#[test]
fn loopback_https_to_http_redirect_requires_loopback_hatch() {
    let _guard = test_env_guard();
    assert!(
        check_redirect_url(
            "http_redirect",
            Some("https://api.example.com/start"),
            "http://127.0.0.1:7777/callback",
        )
        .unwrap()
        .is_some(),
        "loopback downgrade is blocked without the explicit loopback hatch"
    );

    install_test_policy(&[
        (
            "block_private",
            VmValue::String(arcstr::ArcStr::from("private")),
        ),
        ("allow_loopback", VmValue::Bool(true)),
    ]);
    assert!(
        check_redirect_url(
            "http_redirect",
            Some("https://api.example.com/start"),
            "http://127.0.0.1:7777/callback",
        )
        .unwrap()
        .is_none(),
        "explicit allow_loopback keeps loopback redirect workflows available"
    );
    assert!(
        check_redirect_url(
            "http_redirect",
            Some("https://api.example.com/start"),
            "http://LOCALHOST.:7777/callback",
        )
        .unwrap()
        .is_none(),
        "localhost follows the same explicit loopback hatch after normalization"
    );
    assert!(
        check_redirect_url(
            "http_redirect",
            Some("https://api.example.com/start"),
            "http://[::1]:7777/callback",
        )
        .unwrap()
        .is_none(),
        "IPv6 loopback follows the same explicit loopback hatch"
    );
    assert!(
        check_redirect_url(
            "http_redirect",
            Some("https://api.example.com/start"),
            "http://[::ffff:127.0.0.1]:7777/callback",
        )
        .unwrap()
        .is_none(),
        "IPv4-mapped IPv6 loopback follows the same explicit loopback hatch"
    );
    assert!(
        check_redirect_url(
            "http_redirect",
            Some("https://api.example.com/start"),
            "http://public.example.com/callback",
        )
        .unwrap()
        .is_some(),
        "allow_loopback does not exempt public insecure redirects"
    );
}

fn install_rustls_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn read_http_request<T: Read>(stream: &mut T) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).expect("read request");
        assert!(read > 0, "request closed before headers");
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8_lossy(&buffer).into_owned();
        }
    }
}

fn write_http_redirect_response<T: Write>(stream: &mut T, location: &str) {
    let response = format!(
        "HTTP/1.1 302 Found\r\ncontent-length: 0\r\nconnection: close\r\nlocation: {location}\r\n\r\n"
    );
    stream
        .write_all(response.as_bytes())
        .expect("write redirect response");
    stream.flush().expect("flush redirect response");
}

// --- SSRF private-address block (literal IPs; host-name DNS is covered by
//     GuardedResolver tests in `ssrf.rs`). ---

/// Install an explicit `block_private:"private"` policy with default allow,
/// so the SSRF block is the only thing that can deny.
fn install_block_private() -> EgressTestEnvGuard {
    install(&[(
        "block_private",
        VmValue::String(arcstr::ArcStr::from("private")),
    )])
}

#[test]
fn block_private_rejects_loopback_and_metadata_literals() {
    let _guard = install_block_private();
    for url in [
        "http://127.0.0.1",
        "https://169.254.169.254",
        "http://10.0.0.1",
        "https://192.168.1.1",
        "https://[::1]",
        "https://[fe80::1]",
        "https://[::ffff:127.0.0.1]",
        "https://100.64.0.1",
    ] {
        let blocked = check_url("http_get", url)
            .unwrap()
            .unwrap_or_else(|| panic!("expected block for {url}"));
        assert!(
            blocked.reason.contains("disallowed address"),
            "{url}: {}",
            blocked.reason
        );
    }
}

#[test]
fn block_private_allows_public_literal() {
    let _guard = install_block_private();
    assert!(check_url("http_get", "https://93.184.216.34")
        .unwrap()
        .is_none());
    assert!(check_url("http_get", "http://8.8.8.8").unwrap().is_none());
}

#[test]
fn block_private_wins_over_allow_rule() {
    // Deny-private wins: an explicit allow for the literal must NOT re-open it.
    let _guard = install(&[
        ("allow", strings(&["127.0.0.1"])),
        (
            "block_private",
            VmValue::String(arcstr::ArcStr::from("private")),
        ),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    assert!(check_url("http_get", "http://127.0.0.1").unwrap().is_some());
}

#[test]
fn block_private_redacts_secret_in_url() {
    let _guard = install_block_private();
    let blocked = check_url(
        "http_get",
        "http://127.0.0.1/resource?token=SECRET&access_token=zzz&ok=1",
    )
    .unwrap()
    .expect("loopback blocked");
    assert!(!blocked.url.contains("SECRET"), "{}", blocked.url);
    assert!(!blocked.url.contains("zzz"), "{}", blocked.url);
    assert!(!blocked.to_string().contains("SECRET"));
    assert!(!blocked.reason.contains("SECRET"));
    // Reason names only the host.
    assert!(blocked.reason.contains("127.0.0.1"));
}

#[test]
fn allow_loopback_hatch_permits_loopback_literal() {
    let _guard = install(&[
        (
            "block_private",
            VmValue::String(arcstr::ArcStr::from("private")),
        ),
        ("allow_loopback", VmValue::Bool(true)),
    ]);
    assert!(check_url("http_get", "http://127.0.0.1").unwrap().is_none());
    assert!(check_url("http_get", "https://[::1]").unwrap().is_none());
    // Metadata stays blocked even with the loopback hatch.
    assert!(check_url("http_get", "https://169.254.169.254")
        .unwrap()
        .is_some());
}

#[test]
fn default_on_blocks_loopback_under_run_scope() {
    let _guard = test_env_guard();
    {
        let _scope = require_ssrf_guard_for_host();
        // No policy configured at all, but the guard scope defaults to
        // BlockPrivate.
        assert!(check_url("http_get", "http://127.0.0.1").unwrap().is_some());
        assert!(check_url("http_get", "https://169.254.169.254")
            .unwrap()
            .is_some());
        // Public literal still allowed.
        assert!(check_url("http_get", "https://8.8.8.8").unwrap().is_none());
    }
    // Out of scope: allow-all restored.
    assert!(check_url("http_get", "http://127.0.0.1").unwrap().is_none());
}

#[test]
fn block_private_off_opts_out_under_run_scope() {
    let _guard = test_env_guard();
    install_test_policy(&[(
        "block_private",
        VmValue::String(arcstr::ArcStr::from("off")),
    )]);
    let _scope = require_ssrf_guard_for_host();
    // Explicit off overrides the default-on guard.
    assert!(check_url("http_get", "http://127.0.0.1").unwrap().is_none());
}

#[test]
fn env_allow_loopback_opts_out_under_run_scope() {
    let guard = test_env_guard();
    guard.set(HARN_EGRESS_ALLOW_LOOPBACK_ENV, "1");
    let _scope = require_ssrf_guard_for_host();
    // Loopback opened by env hatch...
    assert!(check_url("http_get", "http://127.0.0.1").unwrap().is_none());
    // ...but metadata stays blocked.
    assert!(check_url("http_get", "https://169.254.169.254")
        .unwrap()
        .is_some());
}

#[test]
fn current_ssrf_client_settings_reflects_guard_scope() {
    let _guard = test_env_guard();
    assert_eq!(current_ssrf_client_settings(), (false, false));
    let _scope = require_ssrf_guard_for_host();
    assert_eq!(current_ssrf_client_settings(), (true, false));
    drop(_scope);
    reset_egress_policy_for_tests();
    assert_eq!(current_ssrf_client_settings(), (false, false));
}

/// Finding A: a client built through `install_ssrf_guard` under the run's
/// SSRF scope must NOT be able to reach a hostname that resolves to a
/// loopback/private address (the connect-time backstop drops it), while the
/// same hostname stays reachable when the private block is off — proving the
/// guard is actually installed and not over-blocking when disabled.
#[tokio::test]
async fn install_ssrf_guard_blocks_loopback_hostname_and_passes_when_off() {
    // Under the guard scope, `localhost` resolves only to loopback, which the
    // backstop resolver drops → the send fails before any server bytes flow.
    let server = test_support::OneShotHttpServer::start();
    let client = {
        let _config = EgressTestConfigGuard::new();
        let _scope = require_ssrf_guard_for_host();
        install_ssrf_guard(reqwest::Client::builder())
            .build()
            .expect("guarded client builds")
    };
    let result = client.get(server.url()).send().await;
    assert!(
        result.is_err(),
        "guarded client must not reach a loopback hostname, got {result:?}"
    );
    server.unblock_and_join();

    // With the private block explicitly off, the guard installs no resolver
    // and the very same hostname is reachable (no over-blocking).
    let server = test_support::OneShotHttpServer::start();
    let client = {
        let _config = EgressTestConfigGuard::new();
        install_test_policy(&[(
            "block_private",
            VmValue::String(arcstr::ArcStr::from("off")),
        )]);
        let _scope = require_ssrf_guard_for_host();
        install_ssrf_guard(reqwest::Client::builder())
            .build()
            .expect("unguarded client builds")
    };
    let response = client
        .get(server.url())
        .send()
        .await
        .expect("block_private:off permits loopback hostname");
    assert_eq!(response.status().as_u16(), 200);
    server.join();
}

// --- NetPolicy CIDR/IP rules applied to RESOLVED host IPs (#3174). ---
//
// `evaluate_resolved_addrs` is the pure decision the async resolution path
// and the connect-time GuardedResolver both rely on; testing it with
// synthetic addresses pins the resolve-once-and-pin semantics without DNS.

/// Build a `ConfiguredPolicy` from `(key, value)` config pairs for the
/// resolved-IP unit tests (no global state mutation, so no env lock needed).
fn policy(config: &[(&str, VmValue)]) -> ConfiguredPolicy {
    let map = config
        .iter()
        .cloned()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    ConfiguredPolicy {
        source: "test",
        policy: policy_from_config(&map).expect("policy parses"),
    }
}

fn ips(values: &[&str]) -> Vec<IpAddr> {
    values.iter().map(|v| v.parse().unwrap()).collect()
}

fn target(url: &str) -> EgressTarget {
    EgressTarget::parse(url).expect("target parses")
}

#[test]
fn resolved_ip_in_deny_cidr_blocks_hostname() {
    // A deny CIDR must apply to a hostname that RESOLVES into it, not only
    // to URL-literal IPs. This is the core #3174 bypass.
    let pol = policy(&[("deny", strings(&["203.0.113.0/24"]))]);
    let blocked = evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "https://evil.example.com",
        &target("https://evil.example.com"),
        &ips(&["203.0.113.7"]),
        false,
    )
    .expect("resolved address in deny CIDR is blocked");
    assert!(
        blocked.reason.contains("deny rule `203.0.113.0/24`"),
        "{}",
        blocked.reason
    );
    assert_eq!(blocked.host, "evil.example.com");
}

#[test]
fn resolved_ip_in_deny_ip_literal_blocks_hostname() {
    let pol = policy(&[("deny", strings(&["198.51.100.9"]))]);
    let blocked = evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "https://name.example.com",
        &target("https://name.example.com"),
        &ips(&["198.51.100.9"]),
        false,
    )
    .expect("resolved address matching deny IP is blocked");
    assert!(
        blocked.reason.contains("deny rule `198.51.100.9`"),
        "{}",
        blocked.reason
    );
}

#[test]
fn resolved_ip_outside_deny_cidr_is_allowed() {
    let pol = policy(&[("deny", strings(&["203.0.113.0/24"]))]);
    assert!(evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "https://ok.example.com",
        &target("https://ok.example.com"),
        &ips(&["198.51.100.7"]),
        false,
    )
    .is_none());
}

#[test]
fn resolved_ip_in_allow_cidr_permits_hostname_under_default_deny() {
    // default=deny + CIDR allowlist: a hostname resolving INTO the allow
    // CIDR must be permitted (resolution_required carries the deferred allow
    // decision from the sync layer).
    let pol = policy(&[
        ("allow", strings(&["10.0.0.0/8"])),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    assert!(evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "https://internal.example.com",
        &target("https://internal.example.com"),
        &ips(&["10.1.2.3"]),
        true,
    )
    .is_none());
}

#[test]
fn resolved_ip_outside_allow_cidr_blocks_hostname_under_default_deny() {
    let pol = policy(&[
        ("allow", strings(&["10.0.0.0/8"])),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    let blocked = evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "https://outside.example.com",
        &target("https://outside.example.com"),
        &ips(&["8.8.8.8"]),
        true,
    )
    .expect("resolved address outside allow CIDR is blocked under default deny");
    assert_eq!(blocked.reason, "no allow rule matched");
}

#[test]
fn deny_cidr_wins_over_allow_cidr_on_resolved_ip() {
    // Deny precedence: even if a resolved address is in an allow CIDR, a
    // matching deny CIDR blocks it.
    let pol = policy(&[
        ("allow", strings(&["10.0.0.0/8"])),
        ("deny", strings(&["10.6.0.0/16"])),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    let blocked = evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "https://internal.example.com",
        &target("https://internal.example.com"),
        &ips(&["10.6.1.2"]),
        true,
    )
    .expect("deny CIDR wins over allow CIDR");
    assert!(
        blocked.reason.contains("deny rule `10.6.0.0/16`"),
        "{}",
        blocked.reason
    );
}

#[test]
fn mixed_resolution_blocks_if_any_address_is_denied() {
    // A host answering with both an allowed and a denied address is blocked:
    // the denied address is a rebinding vector the backstop would pin to.
    let pol = policy(&[("deny", strings(&["203.0.113.0/24"]))]);
    assert!(evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "https://mixed.example.com",
        &target("https://mixed.example.com"),
        &ips(&["8.8.8.8", "203.0.113.9"]),
        false,
    )
    .is_some());
}

#[test]
fn port_scoped_deny_cidr_only_matches_that_port_on_resolved_ip() {
    // `:443` qualifier is honored against the resolved address using the
    // request's destination port.
    let pol = policy(&[("deny", strings(&["203.0.113.0/24:443"]))]);
    // https → port 443 → blocked.
    assert!(evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "https://p.example.com",
        &target("https://p.example.com"),
        &ips(&["203.0.113.7"]),
        false,
    )
    .is_some());
    // http → port 80 → not blocked by the :443 rule.
    assert!(evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "http://p.example.com",
        &target("http://p.example.com"),
        &ips(&["203.0.113.7"]),
        false,
    )
    .is_none());
}

#[test]
fn ssrf_block_still_applies_to_resolved_ip() {
    // Even with no NetPolicy IP rules, block_private rejects a host that
    // resolves to a private address (existing behavior, kept).
    let pol = policy(&[(
        "block_private",
        VmValue::String(arcstr::ArcStr::from("private")),
    )]);
    let blocked = evaluate_resolved_addrs(
        Some(&pol),
        "http_get",
        "https://rebind.example.com",
        &target("https://rebind.example.com"),
        &ips(&["10.0.0.1"]),
        false,
    )
    .expect("private resolved address blocked");
    assert!(
        blocked.reason.contains("disallowed address"),
        "{}",
        blocked.reason
    );
}

#[test]
fn check_url_defers_hostname_with_cidr_allowlist_under_default_deny() {
    // The sync layer must NOT block a hostname outright when a CIDR allow
    // rule could still grant it after resolution.
    let _guard = install(&[
        ("allow", strings(&["10.0.0.0/8"])),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    match check_url_decision("http_get", "https://internal.example.com").unwrap() {
        SyncCheck::DeferToResolution(_) => {}
        other => panic!(
            "expected DeferToResolution, got {}",
            match other {
                SyncCheck::Allowed => "Allowed",
                SyncCheck::Blocked(_) => "Blocked",
                SyncCheck::DeferToResolution(_) => unreachable!(),
            }
        ),
    }
    // A literal IP outside the allow CIDR is still blocked outright (no
    // resolution can change a literal).
    assert!(matches!(
        check_url_decision("http_get", "https://8.8.8.8").unwrap(),
        SyncCheck::Blocked(_)
    ));
    // With only host/suffix allow rules, an unknown host is blocked outright.
    drop(_guard);
    let _guard = install(&[
        ("allow", strings(&["api.example.com"])),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    assert!(matches!(
        check_url_decision("http_get", "https://other.example.com").unwrap(),
        SyncCheck::Blocked(_)
    ));
}

#[test]
fn resolved_ip_rules_for_drops_host_suffix_and_port_scoped_rules() {
    // The connect-time backstop carries only port-unscoped IP/CIDR rules:
    // host/suffix rules pin to the hostname, and port-scoped rules can't be
    // honored by the resolver (it sees no destination port).
    let pol = policy(&[
        (
            "deny",
            strings(&[
                "203.0.113.0/24",
                "198.51.100.5",
                "*.evil.com",
                "10.0.0.0/8:443",
            ]),
        ),
        ("default", VmValue::String(arcstr::ArcStr::from("deny"))),
    ]);
    let rules = resolved_ip_rules_for(&pol.policy);
    assert_eq!(
        rules.deny.len(),
        2,
        "host/suffix + port-scoped rules dropped"
    );
    assert!(rules.deny.contains(&"203.0.113.0/24".parse().unwrap()));
    assert!(rules.deny.contains(&"198.51.100.5/32".parse().unwrap()));
    assert!(rules.allow_active);
}

#[test]
fn current_resolved_ip_rules_reflects_installed_policy() {
    let _guard = install(&[("deny", strings(&["203.0.113.0/24"]))]);
    let rules = current_resolved_ip_rules();
    assert_eq!(rules.deny, vec!["203.0.113.0/24".parse().unwrap()]);
}
