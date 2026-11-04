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
    /// This host cannot hold a child to a proxy, so commands run with no
    /// network. Direct sockets would widen what the embedder asked for.
    NoRoute,
    /// The embedder asked for the proxy and it would not start. Turns are
    /// refused, for the same reason.
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
        if !host_can_hold_a_child_to_the_proxy(sandbox) {
            return Self::NoRoute;
        }
        match ProcessEgressProxy::start_allowlist(hosts) {
            Ok(proxy) => Self::Proxied(proxy),
            Err(error) => Self::Unavailable(error.to_string()),
        }
    }

    pub(super) fn endpoints(&self) -> Option<ProcessNetworkProxy> {
        match self {
            Self::Proxied(proxy) => Some(proxy.endpoints()),
            Self::Unmanaged | Self::NoRoute | Self::Unavailable(_) => None,
        }
    }

    pub(super) fn denies_network(&self) -> bool {
        matches!(self, Self::NoRoute)
    }

    pub(super) fn startup_failure(&self) -> Option<&str> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            Self::Unmanaged | Self::NoRoute | Self::Proxied(_) => None,
        }
    }

    #[cfg(test)]
    fn audit(&self) -> Vec<harn_vm::egress::ProcessEgressAudit> {
        match self {
            Self::Proxied(proxy) => proxy.audit_snapshot(),
            Self::Unmanaged | Self::NoRoute | Self::Unavailable(_) => Vec::new(),
        }
    }
}

/// A Linux child reaches the proxy only from a private network namespace, and
/// a host may refuse to build one.
#[cfg(target_os = "linux")]
fn host_can_hold_a_child_to_the_proxy(sandbox: &AcpSandboxConfig) -> bool {
    sandbox
        .process
        .netns_launcher_path
        .as_deref()
        .is_some_and(|launcher| {
            harn_vm::process_sandbox::private_namespace_available(launcher).is_ok()
        })
}

#[cfg(not(target_os = "linux"))]
fn host_can_hold_a_child_to_the_proxy(_sandbox: &AcpSandboxConfig) -> bool {
    true
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

    /// Exercises the proxy whatever this host allows for namespaces.
    fn proxied(hosts: Vec<&str>) -> ChildEgress {
        let hosts: Vec<String> = hosts.into_iter().map(String::from).collect();
        ChildEgress::Proxied(ProcessEgressProxy::start_allowlist(&hosts).expect("proxy starts"))
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
        let egress = proxied(vec!["127.0.0.1"]);
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
        let egress = proxied(Vec::new());
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
        let egress = proxied(vec!["example.com"]);
        let endpoints = egress.endpoints();
        assert!(endpoints.is_some());

        let code = ModePolicyScope::new("code", &sandbox).with_child_egress(&egress);
        assert_eq!(
            code.policy()
                .and_then(|policy| policy.process_network_proxy),
            endpoints
        );
        let ask = ModePolicyScope::new("ask", &sandbox).with_child_egress(&egress);
        assert_eq!(
            ask.policy().and_then(|policy| policy.process_network_proxy),
            None
        );
    }

    /// The negative case the whole design rests on: no way to reach the proxy
    /// must mean no network, never the direct sockets a grant would otherwise
    /// give.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_host_that_cannot_build_the_namespace_gives_children_no_network() {
        let mut sandbox = confined(Some(vec!["example.com"]));
        sandbox.process.netns_launcher_path = Some("/nonexistent/launcher".to_string());
        let egress = ChildEgress::start(&sandbox);
        assert!(egress.denies_network());
        assert!(egress.endpoints().is_none());

        let scope = ModePolicyScope::new("code", &sandbox).with_child_egress(&egress);
        let policy = scope.policy().expect("confined mode has a policy");
        assert!(policy.process_sandbox.deny_network);
        assert_eq!(policy.process_network_proxy, None);
    }
}
