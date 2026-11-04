//! The one proxy a confined session's children reach the network through.
//!
//! The OS sandbox admits a child's connections only to this proxy, and the
//! proxy admits only the hosts the embedder listed. A tool that ignores proxy
//! settings has no other way out, so the list holds whatever the command line
//! looks like.

use harn_vm::egress::ProcessEgressProxy;
use harn_vm::orchestration::ProcessNetworkProxy;

use super::AcpSandboxConfig;

/// What this server's children get in place of direct sockets.
#[derive(Debug)]
pub(super) enum ChildEgress {
    /// The embedder listed no hosts, so the mode policy alone decides.
    Unmanaged,
    Proxied(ProcessEgressProxy),
    /// The embedder asked for the proxy and it would not start. Turns are
    /// refused: falling back to direct sockets would widen what was asked for.
    Unavailable(String),
}

impl ChildEgress {
    /// Start after the server process is confined, so the proxy's threads
    /// inherit that confinement.
    pub(super) fn start(sandbox: &AcpSandboxConfig) -> Self {
        let Some(hosts) = sandbox.child_network_allow.as_ref() else {
            return Self::Unmanaged;
        };
        if !sandbox.has_process_confinement() {
            return Self::Unmanaged;
        }
        match ProcessEgressProxy::start_allowlist(hosts) {
            Ok(proxy) => Self::Proxied(proxy),
            Err(error) => Self::Unavailable(error.to_string()),
        }
    }

    pub(super) fn endpoints(&self) -> Option<ProcessNetworkProxy> {
        match self {
            Self::Proxied(proxy) => Some(proxy.endpoints()),
            Self::Unmanaged | Self::Unavailable(_) => None,
        }
    }

    pub(super) fn startup_failure(&self) -> Option<&str> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            Self::Unmanaged | Self::Proxied(_) => None,
        }
    }

    #[cfg(test)]
    fn audit(&self) -> Vec<harn_vm::egress::ProcessEgressAudit> {
        match self {
            Self::Proxied(proxy) => proxy.audit_snapshot(),
            Self::Unmanaged | Self::Unavailable(_) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    use super::*;
    use crate::adapters::acp::modes::ModePolicyScope;

    fn confined(hosts: Option<Vec<&str>>) -> AcpSandboxConfig {
        AcpSandboxConfig {
            process: harn_vm::orchestration::ProcessSandboxPolicy {
                allow_child_workspace_write: true,
                ..Default::default()
            },
            child_network_allow: hosts.map(|hosts| hosts.into_iter().map(String::from).collect()),
            ..Default::default()
        }
    }

    fn proxied_status(proxy: ProcessNetworkProxy, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", proxy.http_port)).expect("reach proxy");
        write!(
            stream,
            "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n"
        )
        .expect("send");
        let mut head = Vec::new();
        let mut chunk = [0u8; 256];
        while !head.windows(2).any(|pair| pair == b"\r\n") {
            let read = stream.read(&mut chunk).expect("read status");
            if read == 0 {
                break;
            }
            head.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8_lossy(&head).into_owned()
    }

    /// A proxy that admitted everything would pass every check on the mode
    /// policy, so the decision is read from a real request through it.
    #[test]
    fn a_session_child_reaches_only_a_listed_host() {
        let server = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = server.local_addr().expect("addr").port();
        let egress = ChildEgress::start(&confined(Some(vec!["127.0.0.1"])));
        let proxy = egress.endpoints().expect("proxy runs");

        let refused = proxied_status(proxy, "unlisted.invalid:443");
        assert!(refused.contains("403"), "unlisted host: {refused}");
        let admitted = proxied_status(proxy, &format!("127.0.0.1:{port}"));
        assert!(admitted.contains("200"), "listed host: {admitted}");

        let audit = egress.audit();
        let refusal = audit
            .iter()
            .find(|entry| entry.host == "unlisted.invalid")
            .expect("refusal recorded");
        assert!(!refusal.allowed);
    }

    #[test]
    fn listing_no_hosts_refuses_every_host() {
        let egress = ChildEgress::start(&confined(Some(Vec::new())));
        let proxy = egress
            .endpoints()
            .expect("an empty list still runs the proxy");
        let refused = proxied_status(proxy, "example.com:443");
        assert!(refused.contains("403"), "{refused}");
    }

    #[test]
    fn no_list_or_no_confinement_starts_nothing() {
        assert!(ChildEgress::start(&confined(None)).endpoints().is_none());
        let unconfined = AcpSandboxConfig {
            child_network_allow: Some(vec!["example.com".to_string()]),
            ..Default::default()
        };
        assert!(ChildEgress::start(&unconfined).endpoints().is_none());
    }

    /// A read-only mode has no child network to manage. Handing it the proxy
    /// would be the grant that gives it one.
    #[test]
    fn only_a_mode_with_child_network_gets_the_proxy() {
        let sandbox = confined(Some(vec!["example.com"]));
        let egress = ChildEgress::start(&sandbox);
        let endpoints = egress.endpoints();
        assert!(endpoints.is_some());

        let code = ModePolicyScope::new("code", &sandbox).with_child_proxy(endpoints);
        assert_eq!(
            code.policy()
                .and_then(|policy| policy.process_network_proxy),
            endpoints
        );
        let ask = ModePolicyScope::new("ask", &sandbox).with_child_proxy(endpoints);
        assert_eq!(
            ask.policy().and_then(|policy| policy.process_network_proxy),
            None
        );
    }
}
