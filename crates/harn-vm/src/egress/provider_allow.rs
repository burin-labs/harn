use std::sync::Arc;

use url::Url;

use super::{current_resolved_ip_rules, current_ssrf_client_settings, GuardedResolver};

/// Install the connect-time SSRF guard while allowing exact configured
/// provider hosts to resolve to loopback/private-LAN addresses.
///
/// This is intended for operator-configured LLM endpoints only. Generic
/// tool/web/MCP traffic must keep using `install_ssrf_guard` so attacker-
/// controlled URLs cannot opt into private network reachability.
pub fn install_ssrf_guard_with_private_host_allowlist(
    builder: reqwest::ClientBuilder,
    allow_private_for_hosts: &[String],
) -> reqwest::ClientBuilder {
    let (block_private, allow_loopback) = current_ssrf_client_settings();
    let rules = current_resolved_ip_rules();
    if block_private || !rules.deny.is_empty() {
        builder.dns_resolver(Arc::new(
            GuardedResolver::with_policy_and_private_host_allowlist(
                block_private,
                allow_loopback,
                &rules,
                allow_private_for_hosts,
            ),
        ))
    } else {
        builder
    }
}

pub fn configured_provider_private_allow_host(base_url: &str) -> Option<String> {
    Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(normalize_private_allow_host))
        .filter(|host| !host.is_empty())
}

fn normalize_private_allow_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

pub fn ssrf_client_cache_key(allow_private_for_hosts: &[String]) -> String {
    let (block_private, allow_loopback) = current_ssrf_client_settings();
    let rules = current_resolved_ip_rules();
    let deny_nets = rules
        .deny
        .iter()
        .map(|net| net.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let hosts = allow_private_for_hosts
        .iter()
        .map(|host| normalize_private_allow_host(host))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "ssrf={block_private};loopback={allow_loopback};deny_nets={deny_nets};allow_private_hosts={hosts}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress::{require_ssrf_guard_for_host, reset_egress_policy_for_tests};

    fn spawn_one_shot_ok_server() -> (u16, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback probe server");
        let port = listener.local_addr().expect("probe server addr").port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
                let _ = stream.flush();
            }
        });
        (port, handle)
    }

    #[tokio::test]
    async fn install_ssrf_guard_private_host_allowlist_permits_configured_provider_host_only() {
        let (port, server) = spawn_one_shot_ok_server();
        reset_egress_policy_for_tests();
        {
            let _scope = require_ssrf_guard_for_host();
            let client = install_ssrf_guard_with_private_host_allowlist(
                reqwest::Client::builder(),
                &[String::from("localhost")],
            )
            .build()
            .expect("guarded client builds");
            let response = client
                .get(format!("http://localhost:{port}/probe"))
                .send()
                .await
                .expect("configured provider host may reach local inference endpoint");
            assert_eq!(response.status().as_u16(), 200);
        }
        server.join().expect("probe server thread");

        let (port, server) = spawn_one_shot_ok_server();
        reset_egress_policy_for_tests();
        {
            let _scope = require_ssrf_guard_for_host();
            let client = install_ssrf_guard_with_private_host_allowlist(
                reqwest::Client::builder(),
                &[String::from("other.local")],
            )
            .build()
            .expect("guarded client builds");
            let result = client
                .get(format!("http://localhost:{port}/probe"))
                .send()
                .await;
            assert!(
                result.is_err(),
                "unlisted loopback hostname must remain guarded, got {result:?}"
            );
        }
        drop(server);
        reset_egress_policy_for_tests();
    }
}
