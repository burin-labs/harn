//! Host-side forwarding proxy for sandboxed child processes.
//!
//! The proxy is deliberately small: it supports HTTP proxy requests, HTTPS
//! `CONNECT`, and SOCKS5 TCP `CONNECT`. The OS sandbox separately restricts
//! the child to these loopback ports, so clearing the injected proxy
//! environment cannot widen authority. Destination decisions reuse the
//! canonical [`super::EgressPolicy`] rules and pin each connection to the
//! addresses resolved during that decision.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use url::Url;

use super::{
    current_policy_context, ensure_env_seeded, evaluate_resolved_addrs, global_state,
    normalize_host, parse_rule_list, ConfiguredPolicy, DefaultAction, EgressPolicy,
    EgressPolicyContext, EgressTarget, SsrfMode,
};
use crate::orchestration::ProcessNetworkProxy;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_mins(5);
const MAX_AUDIT_EVENTS: usize = 256;
const MAX_ACTIVE_CONNECTIONS: usize = 128;

/// Secret-free evidence emitted at the process-egress authority boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEgressAudit {
    pub protocol: &'static str,
    pub host: String,
    pub port: u16,
    pub allowed: bool,
    pub reason: String,
}

/// A running host-side proxy. Keep this handle alive for at least as long as
/// every child process whose policy references [`Self::endpoints`].
#[derive(Debug)]
pub struct ProcessEgressProxy {
    endpoints: ProcessNetworkProxy,
    shutdown: Arc<AtomicBool>,
    joins: Vec<JoinHandle<()>>,
    audit: Arc<Mutex<Vec<ProcessEgressAudit>>>,
}

impl ProcessEgressProxy {
    /// Start a proxy from the current Harn egress policy. `Ok(None)` preserves
    /// the legacy unrestricted child-network grant when no policy was
    /// configured; a configured policy always becomes a managed boundary.
    pub fn start_from_current_policy(default_block_private: bool) -> Result<Option<Self>, String> {
        ensure_env_seeded().map_err(|error| error.to_string())?;
        let require_explicit =
            super::REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH.with(|depth| *depth.borrow() > 0);
        if super::configured_policy().is_none() && !require_explicit {
            return Ok(None);
        }
        Self::start(ProcessPolicySource::Live {
            context: current_policy_context(),
            default_block_private,
            require_explicit,
        })
        .map(Some)
    }

    /// Start the local-cloud projection of a deny-by-default host allowlist.
    /// Parsing goes through the same rule parser as `HARN_EGRESS_ALLOW` and
    /// `harness.net.egress_policy`.
    pub fn start_allowlist(allowed_hosts: &[String]) -> Result<Self, String> {
        let policy = EgressPolicy {
            allow: parse_rule_list(&allowed_hosts.join(",")).map_err(|error| error.to_string())?,
            deny: Vec::new(),
            default: DefaultAction::Deny,
            // NetworkPolicy::Limited is only a destination contract. It does
            // not independently request Harn's SSRF/private-address overlay.
            block_private: Some(SsrfMode::Off),
            allow_loopback: true,
        };
        Self::start(ProcessPolicySource::Fixed(ConfiguredPolicy {
            source: "process_network",
            policy,
        }))
    }

    fn start(policy: ProcessPolicySource) -> Result<Self, String> {
        let http = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("bind child HTTP egress proxy: {error}"))?;
        let socks = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("bind child SOCKS5 egress proxy: {error}"))?;
        let http_port = http
            .local_addr()
            .map_err(|error| format!("inspect child HTTP egress proxy: {error}"))?
            .port();
        let socks_port = socks
            .local_addr()
            .map_err(|error| format!("inspect child SOCKS5 egress proxy: {error}"))?
            .port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_connections = Arc::new(AtomicUsize::new(0));
        let audit = Arc::new(Mutex::new(Vec::new()));
        let policy = Arc::new(policy);

        let http_join = spawn_listener(
            "harn-process-http-proxy",
            http,
            Arc::clone(&policy),
            Arc::clone(&shutdown),
            Arc::clone(&active_connections),
            Arc::clone(&audit),
            handle_http_client,
        )?;
        let socks_join = match spawn_listener(
            "harn-process-socks-proxy",
            socks,
            policy,
            Arc::clone(&shutdown),
            active_connections,
            Arc::clone(&audit),
            handle_socks_client,
        ) {
            Ok(join) => join,
            Err(error) => {
                shutdown.store(true, Ordering::Release);
                let _ = TcpStream::connect(("127.0.0.1", http_port));
                let _ = http_join.join();
                return Err(error);
            }
        };

        Ok(Self {
            endpoints: ProcessNetworkProxy {
                http_port,
                socks_port,
            },
            shutdown,
            joins: vec![http_join, socks_join],
            audit,
        })
    }

    pub fn endpoints(&self) -> ProcessNetworkProxy {
        self.endpoints
    }

    pub fn audit_snapshot(&self) -> Vec<ProcessEgressAudit> {
        self.audit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for ProcessEgressProxy {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Wake both blocking accept loops. These connections are never read.
        let _ = TcpStream::connect(("127.0.0.1", self.endpoints.http_port));
        let _ = TcpStream::connect(("127.0.0.1", self.endpoints.socks_port));
        for join in self.joins.drain(..) {
            let _ = join.join();
        }
    }
}

type ClientHandler =
    fn(TcpStream, Arc<ProcessPolicySource>, Arc<Mutex<Vec<ProcessEgressAudit>>>) -> io::Result<()>;

#[derive(Clone, Debug)]
enum ProcessPolicySource {
    Fixed(ConfiguredPolicy),
    Live {
        context: Option<EgressPolicyContext>,
        default_block_private: bool,
        require_explicit: bool,
    },
}

impl ProcessPolicySource {
    fn configured(&self) -> Result<ConfiguredPolicy, String> {
        let mut configured = match self {
            Self::Fixed(configured) => Some(configured.clone()),
            Self::Live { context, .. } => match context {
                Some(context) => context
                    .0
                    .read()
                    .expect("egress policy state poisoned")
                    .policy
                    .clone(),
                None => global_state()
                    .read()
                    .expect("egress policy state poisoned")
                    .policy
                    .clone(),
            },
        };
        if let Self::Live {
            default_block_private,
            require_explicit,
            ..
        } = self
        {
            let Some(configured) = configured.as_mut() else {
                return Err(if *require_explicit {
                    "no egress policy configured".to_string()
                } else {
                    "managed egress policy is unavailable".to_string()
                });
            };
            if configured.policy.block_private.is_none() && *default_block_private {
                configured.policy.block_private = Some(SsrfMode::BlockPrivate);
            }
        }
        configured.ok_or_else(|| "managed egress policy is unavailable".to_string())
    }
}

fn spawn_listener(
    name: &str,
    listener: TcpListener,
    policy: Arc<ProcessPolicySource>,
    shutdown: Arc<AtomicBool>,
    active_connections: Arc<AtomicUsize>,
    audit: Arc<Mutex<Vec<ProcessEgressAudit>>>,
    handler: ClientHandler,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            while let Ok((stream, _peer)) = listener.accept() {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                if active_connections
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                        (active < MAX_ACTIVE_CONNECTIONS).then_some(active + 1)
                    })
                    .is_err()
                {
                    continue;
                }
                let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
                let _ = stream.set_write_timeout(Some(IDLE_TIMEOUT));
                let policy = Arc::clone(&policy);
                let audit = Arc::clone(&audit);
                let connection_count = Arc::clone(&active_connections);
                if thread::Builder::new()
                    .name("harn-process-egress-connection".to_string())
                    .spawn(move || {
                        let _connection = ActiveConnection(connection_count);
                        let _ = handler(stream, policy, audit);
                    })
                    .is_err()
                {
                    active_connections.fetch_sub(1, Ordering::AcqRel);
                }
            }
        })
        .map_err(|error| format!("start child egress proxy listener: {error}"))
}

struct ActiveConnection(Arc<AtomicUsize>);

impl Drop for ActiveConnection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle_http_client(
    mut client: TcpStream,
    policy: Arc<ProcessPolicySource>,
    audit: Arc<Mutex<Vec<ProcessEgressAudit>>>,
) -> io::Result<()> {
    let request = match read_http_head(&mut client) {
        Ok(request) => request,
        Err(error) => {
            let _ = write_http_error(&mut client, 400, "malformed proxy request");
            return Err(error);
        }
    };
    let first_line = request
        .head
        .split(|byte| *byte == b'\n')
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let first_line = std::str::from_utf8(first_line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request line is not UTF-8"))?
        .trim_end_matches(['\r', '\n']);
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if method.is_empty() || target.is_empty() || !version.starts_with("HTTP/") {
        write_http_error(&mut client, 400, "malformed proxy request")?;
        return Ok(());
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = match parse_authority(target, 443) {
            Ok(value) => value,
            Err(_) => {
                write_http_error(&mut client, 400, "malformed CONNECT authority")?;
                return Ok(());
            }
        };
        let mut upstream = match connect_allowed("http_connect", &host, port, &policy, &audit) {
            Ok(stream) => stream,
            Err(reason) => {
                write_http_error(&mut client, 403, &reason)?;
                return Ok(());
            }
        };
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        // A client may optimistically send the first tunnel bytes in the same
        // packet as CONNECT. `read_http_head` deliberately preserves them.
        upstream.write_all(&request.trailing)?;
        return relay(client, upstream);
    }

    let url = match Url::parse(target) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        _ => {
            write_http_error(
                &mut client,
                400,
                "proxy request target must be an absolute URL",
            )?;
            return Ok(());
        }
    };
    if url.scheme() == "https" {
        write_http_error(&mut client, 400, "HTTPS proxy requests must use CONNECT")?;
        return Ok(());
    }
    let Some(host) = url.host_str() else {
        write_http_error(&mut client, 400, "proxy URL has no host")?;
        return Ok(());
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let mut upstream = match connect_allowed("http", host, port, &policy, &audit) {
        Ok(stream) => stream,
        Err(reason) => {
            write_http_error(&mut client, 403, &reason)?;
            return Ok(());
        }
    };
    let origin_target = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    let rewritten = rewrite_http_head(&request.head, method, &origin_target, version)?;
    upstream.write_all(&rewritten)?;
    upstream.write_all(&request.trailing)?;
    relay(client, upstream)
}

struct HttpHead {
    head: Vec<u8>,
    trailing: Vec<u8>,
}

fn read_http_head(stream: &mut TcpStream) -> io::Result<HttpHead> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 2048];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "proxy request ended before headers",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "proxy request headers exceed limit",
            ));
        }
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let split = end + 4;
            return Ok(HttpHead {
                head: bytes[..split].to_vec(),
                trailing: bytes[split..].to_vec(),
            });
        }
    }
}

fn rewrite_http_head(
    original: &[u8],
    method: &str,
    target: &str,
    version: &str,
) -> io::Result<Vec<u8>> {
    let text = std::str::from_utf8(original)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "headers are not UTF-8"))?;
    let mut out = format!("{method} {target} {version}\r\n");
    for line in text.split("\r\n").skip(1) {
        if line.is_empty() {
            break;
        }
        let name = line.split_once(':').map(|(name, _)| name).unwrap_or(line);
        if name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    Ok(out.into_bytes())
}

fn write_http_error(stream: &mut TcpStream, status: u16, reason: &str) -> io::Result<()> {
    let label = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        _ => "Proxy Error",
    };
    let body = format!("child egress proxy: {reason}\n");
    write!(
        stream,
        "HTTP/1.1 {status} {label}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn handle_socks_client(
    mut client: TcpStream,
    policy: Arc<ProcessPolicySource>,
    audit: Arc<Mutex<Vec<ProcessEgressAudit>>>,
) -> io::Result<()> {
    let mut greeting = [0_u8; 2];
    client.read_exact(&mut greeting)?;
    if greeting[0] != 5 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not SOCKS5"));
    }
    let mut methods = vec![0_u8; greeting[1] as usize];
    client.read_exact(&mut methods)?;
    if !methods.contains(&0) {
        client.write_all(&[5, 0xff])?;
        return Ok(());
    }
    client.write_all(&[5, 0])?;

    let mut request = [0_u8; 4];
    client.read_exact(&mut request)?;
    if request[0] != 5 || request[1] != 1 || request[2] != 0 {
        write_socks_reply(&mut client, 7)?;
        return Ok(());
    }
    let host = match request[3] {
        1 => {
            let mut bytes = [0_u8; 4];
            client.read_exact(&mut bytes)?;
            IpAddr::from(bytes).to_string()
        }
        3 => {
            let mut len = [0_u8; 1];
            client.read_exact(&mut len)?;
            let mut bytes = vec![0_u8; len[0] as usize];
            client.read_exact(&mut bytes)?;
            String::from_utf8(bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SOCKS host"))?
        }
        4 => {
            let mut bytes = [0_u8; 16];
            client.read_exact(&mut bytes)?;
            IpAddr::from(bytes).to_string()
        }
        _ => {
            write_socks_reply(&mut client, 8)?;
            return Ok(());
        }
    };
    let mut port = [0_u8; 2];
    client.read_exact(&mut port)?;
    let port = u16::from_be_bytes(port);
    let upstream = match connect_allowed("socks5", &host, port, &policy, &audit) {
        Ok(stream) => stream,
        Err(_) => {
            write_socks_reply(&mut client, 2)?;
            return Ok(());
        }
    };
    write_socks_reply(&mut client, 0)?;
    relay(client, upstream)
}

fn write_socks_reply(stream: &mut TcpStream, status: u8) -> io::Result<()> {
    stream.write_all(&[5, status, 0, 1, 0, 0, 0, 0, 0, 0])
}

fn connect_allowed(
    protocol: &'static str,
    host: &str,
    port: u16,
    policy: &ProcessPolicySource,
    audit: &Mutex<Vec<ProcessEgressAudit>>,
) -> Result<TcpStream, String> {
    let configured = match policy.configured() {
        Ok(configured) => configured,
        Err(reason) => {
            record_audit(audit, protocol, host, port, false, &reason);
            return Err(reason);
        }
    };
    match resolve_allowed(host, port, &configured) {
        Ok(addresses) => {
            for address in addresses {
                if let Ok(stream) = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
                    let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
                    let _ = stream.set_write_timeout(Some(IDLE_TIMEOUT));
                    record_audit(
                        audit,
                        protocol,
                        host,
                        port,
                        true,
                        "egress policy allowed destination",
                    );
                    return Ok(stream);
                }
            }
            let reason = "allowed destination was unreachable".to_string();
            record_audit(audit, protocol, host, port, false, &reason);
            Err(reason)
        }
        Err(reason) => {
            record_audit(audit, protocol, host, port, false, &reason);
            Err(reason)
        }
    }
}

fn resolve_allowed(
    host: &str,
    port: u16,
    configured: &ConfiguredPolicy,
) -> Result<Vec<SocketAddr>, String> {
    let target = EgressTarget {
        host: normalize_host(host),
        ip: host.parse().ok(),
        port: Some(port),
    };
    if let Some(rule) = configured
        .policy
        .deny
        .iter()
        .find(|rule| rule.matches(&target))
    {
        return Err(format!("matched deny rule `{}`", rule.raw));
    }
    let host_allowed = configured
        .policy
        .allow
        .iter()
        .any(|rule| rule.matches(&target));
    let resolution_required = configured.policy.default == DefaultAction::Deny && !host_allowed;
    let could_allow_on_resolution = target.ip.is_none()
        && configured
            .policy
            .allow
            .iter()
            .any(|rule| rule.is_ip_matcher());
    if resolution_required && !could_allow_on_resolution {
        // Match the canonical URL-layer decision: a denied hostname must not
        // trigger even a host-side DNS query unless an IP/CIDR allow rule could
        // still grant its resolved address.
        return Err("no allow rule matched".to_string());
    }
    let addresses = (target.host.as_str(), port)
        .to_socket_addrs()
        .map_err(|_| "destination hostname did not resolve".to_string())?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("destination hostname did not resolve".to_string());
    }
    let ips = addresses.iter().map(SocketAddr::ip).collect::<Vec<_>>();
    let raw_url = format!("tcp://{}:{port}", authority_host(&target.host));
    if let Some(blocked) = evaluate_resolved_addrs(
        Some(configured),
        "process_proxy",
        &raw_url,
        &target,
        &ips,
        resolution_required,
    ) {
        return Err(blocked.reason);
    }
    // When default=allow, the resolved-address checks above are the only
    // remaining gate. When default=deny, `host_allowed` proves the URL-layer
    // grant and the same checks pin its resolved addresses.
    Ok(addresses)
}

fn authority_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn parse_authority(authority: &str, default_port: u16) -> Result<(String, u16), String> {
    let url =
        Url::parse(&format!("http://{authority}")).map_err(|_| "invalid authority".to_string())?;
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "authority has no host".to_string())?;
    Ok((host.to_string(), url.port().unwrap_or(default_port)))
}

fn record_audit(
    audit: &Mutex<Vec<ProcessEgressAudit>>,
    protocol: &'static str,
    host: &str,
    port: u16,
    allowed: bool,
    reason: &str,
) {
    let mut audit = audit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if audit.len() == MAX_AUDIT_EVENTS {
        audit.remove(0);
    }
    audit.push(ProcessEgressAudit {
        protocol,
        host: normalize_host(host),
        port,
        allowed,
        reason: reason.to_string(),
    });
}

fn relay(left: TcpStream, right: TcpStream) -> io::Result<()> {
    let mut left_read = left.try_clone()?;
    let mut right_write = right.try_clone()?;
    let forward = thread::spawn(move || io::copy(&mut left_read, &mut right_write));
    let mut right_read = right;
    let mut left_write = left;
    let reverse = io::copy(&mut right_read, &mut left_write);
    let _ = left_write.shutdown(Shutdown::Both);
    let _ = right_read.shutdown(Shutdown::Both);
    let forward = forward
        .join()
        .map_err(|_| io::Error::other("child egress relay thread panicked"))?;
    forward?;
    reverse?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn allowlist_uses_canonical_rule_parser() {
        let proxy = ProcessEgressProxy::start_allowlist(&[
            "api.example.com:443".to_string(),
            "*.trusted.example".to_string(),
        ])
        .unwrap();
        assert_ne!(proxy.endpoints().http_port, proxy.endpoints().socks_port);
    }

    #[test]
    fn malformed_allowlist_fails_before_listener_is_exposed() {
        let error = ProcessEgressProxy::start_allowlist(&["[broken".to_string()]).unwrap_err();
        assert!(error.contains("invalid bracketed host rule"), "{error}");
    }

    #[test]
    fn live_policy_source_denies_missing_then_observes_runtime_configuration() {
        let context = EgressPolicyContext(Arc::new(std::sync::RwLock::new(
            super::super::EgressState::default(),
        )));
        let source = ProcessPolicySource::Live {
            context: Some(context.clone()),
            default_block_private: true,
            require_explicit: true,
        };
        assert_eq!(
            source.configured().unwrap_err(),
            "no egress policy configured"
        );

        context.0.write().unwrap().policy = Some(ConfiguredPolicy {
            source: "stdlib",
            policy: EgressPolicy {
                allow: parse_rule_list("api.example.com:443").unwrap(),
                deny: Vec::new(),
                default: DefaultAction::Deny,
                block_private: None,
                allow_loopback: false,
            },
        });
        let configured = source.configured().unwrap();
        assert_eq!(configured.source, "stdlib");
        assert_eq!(
            configured.policy.block_private,
            Some(SsrfMode::BlockPrivate)
        );
    }

    #[test]
    fn proxy_boundary_forwards_allowed_http_and_rejects_denied_destination() {
        let allowed = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let allowed_port = allowed.local_addr().unwrap().port();
        let denied = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let denied_port = denied.local_addr().unwrap().port();
        denied.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = allowed.accept().unwrap();
            let request = read_http_head(&mut stream).unwrap();
            let request = String::from_utf8(request.head).unwrap();
            assert!(!request.to_ascii_lowercase().contains("proxy-authorization"));
            assert!(request.contains("Authorization: Bearer origin-secret"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let proxy =
            ProcessEgressProxy::start_allowlist(&[format!("127.0.0.1:{allowed_port}")]).unwrap();

        let mut allowed_client =
            TcpStream::connect(("127.0.0.1", proxy.endpoints().http_port)).unwrap();
        write!(
            allowed_client,
            "GET http://127.0.0.1:{allowed_port}/allowed?token=url-secret HTTP/1.1\r\nHost: 127.0.0.1\r\nProxy-Authorization: Bearer proxy-secret\r\nAuthorization: Bearer origin-secret\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        allowed_client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        server.join().unwrap();

        let mut denied_client =
            TcpStream::connect(("127.0.0.1", proxy.endpoints().http_port)).unwrap();
        write!(
            denied_client,
            "GET http://127.0.0.1:{denied_port}/denied HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        denied_client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 403 Forbidden"), "{response}");
        assert!(matches!(denied.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));

        let audit = proxy.audit_snapshot();
        assert_eq!(audit.len(), 2, "{audit:?}");
        assert!(audit[0].allowed);
        assert!(!audit[1].allowed);
        let audit = format!("{audit:?}");
        for secret in ["url-secret", "proxy-secret", "origin-secret"] {
            assert!(!audit.contains(secret), "audit leaked {secret}: {audit}");
        }
    }

    #[test]
    fn connect_tunnel_preserves_optimistic_bytes() {
        let destination = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = destination.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = destination.accept().unwrap();
            let mut payload = [0_u8; 10];
            stream.read_exact(&mut payload).unwrap();
            assert_eq!(&payload, b"optimistic");
            stream.write_all(b"tunneled").unwrap();
        });
        let proxy = ProcessEgressProxy::start_allowlist(&[format!("127.0.0.1:{port}")]).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", proxy.endpoints().http_port)).unwrap();
        write!(
            client,
            "CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\noptimistic"
        )
        .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200 Connection Established\r\n\r\n"),
            "{response:?}"
        );
        assert!(response.ends_with("tunneled"), "{response:?}");
        server.join().unwrap();
    }

    #[test]
    fn socks5_connect_uses_the_same_allowlist_boundary() {
        let destination = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = destination.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = destination.accept().unwrap();
            let mut payload = [0_u8; 4];
            stream.read_exact(&mut payload).unwrap();
            assert_eq!(&payload, b"ping");
            stream.write_all(b"pong").unwrap();
        });
        let proxy = ProcessEgressProxy::start_allowlist(&[format!("127.0.0.1:{port}")]).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", proxy.endpoints().socks_port)).unwrap();
        client.write_all(&[5, 1, 0]).unwrap();
        let mut greeting = [0_u8; 2];
        client.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting, [5, 0]);
        let [port_high, port_low] = port.to_be_bytes();
        client
            .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, port_high, port_low])
            .unwrap();
        let mut reply = [0_u8; 10];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(reply[1], 0, "{reply:?}");
        client.write_all(b"ping").unwrap();
        let mut payload = [0_u8; 4];
        client.read_exact(&mut payload).unwrap();
        assert_eq!(&payload, b"pong");
        server.join().unwrap();
    }

    #[test]
    fn audit_shape_never_carries_url_or_headers() {
        let audit = ProcessEgressAudit {
            protocol: "http_connect",
            host: "api.example.com".to_string(),
            port: 443,
            allowed: false,
            reason: "no allow rule matched".to_string(),
        };
        let rendered = format!("{audit:?}");
        assert!(!rendered.contains("token="));
        assert!(!rendered.contains("authorization"));
    }

    #[test]
    fn exact_allowed_denied_and_missing_decisions_share_egress_rules() {
        let configured = ConfiguredPolicy {
            source: "test",
            policy: EgressPolicy {
                allow: parse_rule_list("127.0.0.1:443,*.trusted.example").unwrap(),
                deny: parse_rule_list("blocked.trusted.example").unwrap(),
                default: DefaultAction::Deny,
                block_private: Some(SsrfMode::Off),
                allow_loopback: true,
            },
        };
        assert!(resolve_allowed("127.0.0.1", 443, &configured).is_ok());
        assert_eq!(
            resolve_allowed("127.0.0.1", 80, &configured).unwrap_err(),
            "no allow rule matched"
        );
        let missing_policy = ConfiguredPolicy {
            source: "test",
            policy: EgressPolicy {
                allow: parse_rule_list("*.trusted.example").unwrap(),
                deny: Vec::new(),
                default: DefaultAction::Deny,
                block_private: Some(SsrfMode::Off),
                allow_loopback: true,
            },
        };
        assert_eq!(
            resolve_allowed("missing.example.invalid", 443, &missing_policy).unwrap_err(),
            "no allow rule matched"
        );
        assert_eq!(
            resolve_allowed("blocked.trusted.example", 443, &configured).unwrap_err(),
            "matched deny rule `blocked.trusted.example`"
        );
    }

    #[test]
    fn private_address_guard_still_precedes_allowlist() {
        let configured = ConfiguredPolicy {
            source: "test",
            policy: EgressPolicy {
                allow: parse_rule_list("127.0.0.1:443").unwrap(),
                deny: Vec::new(),
                default: DefaultAction::Deny,
                block_private: Some(SsrfMode::BlockPrivate),
                allow_loopback: false,
            },
        };
        let error = resolve_allowed("127.0.0.1", 443, &configured).unwrap_err();
        assert!(error.contains("disallowed address"), "{error}");
    }

    #[test]
    fn effective_settings_are_stable_for_explicit_proxy_policy() {
        let policy = EgressPolicy {
            allow: Vec::new(),
            deny: Vec::new(),
            default: DefaultAction::Deny,
            block_private: Some(SsrfMode::Off),
            allow_loopback: false,
        };
        assert_eq!(
            super::super::effective_ssrf_settings(Some(&policy)),
            (SsrfMode::Off, false)
        );
    }
}
