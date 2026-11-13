//! HTTP, HTTPS `CONNECT`, and SOCKS5 TCP proxy for sandboxed children.
//! The child sandbox permits only this proxy's ports, which it holds on every
//! local address, so ignoring proxy settings cannot widen access. Canonical [`super::EgressPolicy`] rules
//! decide destinations and pin each connection to its checked addresses.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use url::Url;

use super::ssrf::is_disallowed_ip;
use super::{
    current_policy_context, ensure_env_seeded, evaluate_resolved_addrs, global_state,
    normalize_host, parse_rule_list, ConfiguredPolicy, DefaultAction, EgressPolicy,
    EgressPolicyContext, EgressTarget, SsrfMode,
};
use crate::orchestration::{current_execution_policy, ProcessNetworkProxy, SandboxProfile};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_mins(5);
const MAX_AUDIT_EVENTS: usize = 256;
const UNREACHABLE_REASON: &str = "allowed destination was unreachable";
const MAX_ACTIVE_CONNECTIONS: usize = 128;

/// Why the proxy could not start. A denied loopback bind means existing
/// confinement prevents mediation, rather than an egress-policy refusal.
/// Only `PermissionDenied` receives that classification; other failures keep
/// their OS error so this variant cannot absorb unrelated errors.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProcessEgressProxyError {
    /// `HARN-CAP-202`: the host process may not open a loopback listener.
    HostLoopbackBindDenied {
        /// Which listener failed, `HTTP` or `SOCKS5`.
        protocol: &'static str,
        /// The sandbox profile this process reports from its own execution
        /// policy. `Unrestricted` means the confinement is inherited from a
        /// parent process or the operating system rather than declared here.
        profile: SandboxProfile,
        /// The OS error text.
        source: String,
    },
    /// Any other start failure, unchanged from before classification.
    Other(String),
}

impl ProcessEgressProxyError {
    pub const HOST_LOOPBACK_BIND_DENIED_CODE: &'static str = "HARN-CAP-202";

    /// Classify one loopback bind failure. `PermissionDenied` becomes the
    /// typed variant; anything else keeps the protocol-prefixed OS text.
    pub(super) fn from_bind(protocol: &'static str, error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::PermissionDenied {
            return Self::HostLoopbackBindDenied {
                protocol,
                profile: current_execution_policy()
                    .map(|policy| policy.sandbox_profile)
                    .unwrap_or(SandboxProfile::Unrestricted),
                source: error.to_string(),
            };
        }
        Self::Other(format!("bind child {protocol} egress proxy: {error}"))
    }
}

impl std::fmt::Display for ProcessEgressProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostLoopbackBindDenied {
                protocol,
                profile,
                source,
            } => {
                let confinement = if *profile == SandboxProfile::Unrestricted {
                    "has no Harn sandbox profile of its own, so the confinement is inherited \
                     from a parent process or the operating system"
                        .to_string()
                } else {
                    format!(
                        "is confined under Harn sandbox profile `{}`",
                        profile.as_str()
                    )
                };
                write!(
                    f,
                    "{code}: this Harn process cannot open the loopback listener its child's \
                     {protocol} egress proxy needs ({source}); the process {confinement}. \
                     Give the confining `harn run` --allow-process-loopback, or spawn the \
                     child from a process that is not confined",
                    code = Self::HOST_LOOPBACK_BIND_DENIED_CODE,
                )
            }
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ProcessEgressProxyError {}

/// Secret-free evidence emitted at the process-egress authority boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessEgressAudit {
    pub protocol: &'static str,
    pub host: String,
    pub port: u16,
    pub allowed: bool,
    pub reason: String,
}

#[derive(Debug, Default)]
struct AuditLog {
    entries: Vec<ProcessEgressAudit>,
    /// Destinations refused since the host last asked. The policy refused
    /// these; a listed host that failed to connect is not among them.
    unread_refusals: Vec<ProcessEgressAudit>,
}

/// Control for the allowlist proxy named by the current execution policy.
#[derive(Debug)]
struct AllowlistControl {
    http_port: u16,
    policy: Arc<ProcessPolicySource>,
    audit: Arc<Mutex<AuditLog>>,
}

static ALLOWLIST_PROXIES: Mutex<Vec<Weak<AllowlistControl>>> = Mutex::new(Vec::new());

fn allowlist_control(proxy: ProcessNetworkProxy) -> Option<Arc<AllowlistControl>> {
    ALLOWLIST_PROXIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter_map(Weak::upgrade)
        .find(|control| control.http_port == proxy.http_port)
}

/// Destinations the allowlist proxy behind `proxy` refused since the last call.
/// Empty when `proxy` is not an allowlist proxy.
pub(super) fn take_child_network_refusals(proxy: ProcessNetworkProxy) -> Vec<ProcessEgressAudit> {
    let Some(control) = allowlist_control(proxy) else {
        return Vec::new();
    };
    let mut log = control
        .audit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::mem::take(&mut log.unread_refusals)
}

/// Add a host to the allowlist proxy behind `proxy`, for the life of that proxy.
/// Returns false when `proxy` is not an allowlist proxy, so nothing was granted.
pub(super) fn allow_child_network_host(
    proxy: ProcessNetworkProxy,
    host: &str,
    port: Option<u16>,
) -> Result<bool, String> {
    let Some(control) = allowlist_control(proxy) else {
        return Ok(false);
    };
    let ProcessPolicySource::Fixed(fixed) = control.policy.as_ref() else {
        return Ok(false);
    };
    if !can_approve_child_network_host(host) {
        return Err(format!("`{host}` is not one public host name"));
    }
    let name = normalize_host(host);
    let mut configured = fixed
        .configured
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for port in port.map_or_else(|| vec![80, 443], |port| vec![port]) {
        let target = EgressTarget {
            host: name.clone(),
            ip: None,
            port: Some(port),
        };
        // A listed host keeps what the list gave it, private answers included.
        if !configured
            .policy
            .allow
            .iter()
            .any(|rule| rule.matches(&target))
        {
            configured.policy.allow.extend(
                parse_rule_list(&format!("{name}:{port}")).map_err(|error| error.to_string())?,
            );
            let mut approved = fixed
                .approved
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let destination = (name.clone(), port);
            if !approved.contains(&destination) {
                approved.push(destination);
            }
        }
    }
    Ok(true)
}

/// An approval names one DNS host, never an address, list, or wildcard.
/// Numeric labels are excluded because resolvers accept alternate IP spellings.
/// Connect-time checks hold the name to public remote addresses.
pub(super) fn can_approve_child_network_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !label.bytes().all(|byte| byte.is_ascii_digit())
        })
        && labels.last().is_some_and(|top| {
            top.starts_with("xn--") || top.bytes().all(|byte| byte.is_ascii_alphabetic())
        })
}

/// A running host-side proxy. Keep this handle alive for at least as long as
/// every child process whose policy references [`Self::endpoints`].
#[derive(Debug)]
pub struct ProcessEgressProxy {
    endpoints: ProcessNetworkProxy,
    shutdown: Arc<AtomicBool>,
    joins: Vec<JoinHandle<()>>,
    audit: Arc<Mutex<AuditLog>>,
    policy: Arc<ProcessPolicySource>,
    /// Holds the registration that hosts look this proxy up by.
    _control: Option<Arc<AllowlistControl>>,
    /// Keeps these ports from every other local service; see `proxy_ports`.
    _claim: super::proxy_ports::PortClaim,
    /// How a child in a private network namespace reaches these listeners.
    #[cfg(target_os = "linux")]
    bridge: Option<super::namespace_bridge::NamespaceBridge>,
}

impl ProcessEgressProxy {
    /// Without configured policy, `Ok(None)` preserves legacy unrestricted grants.
    /// Inside an explicit-policy scope, public-host policy and SSRF rules apply.
    pub fn start_from_current_policy(
        default_block_private: bool,
    ) -> Result<Option<Self>, ProcessEgressProxyError> {
        ensure_env_seeded().map_err(|error| ProcessEgressProxyError::Other(error.to_string()))?;
        let require_explicit =
            super::REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH.with(|depth| *depth.borrow() > 0);
        if super::configured_policy().is_none() && !require_explicit {
            return Ok(None);
        }
        Self::start(ProcessPolicySource::Live {
            context: current_policy_context(),
            default_block_private,
        })
        .map(Some)
    }

    /// Start a deny-by-default host allowlist using the canonical egress rule parser.
    pub fn start_allowlist(allowed_hosts: &[String]) -> Result<Self, ProcessEgressProxyError> {
        let policy = EgressPolicy {
            allow: parse_rule_list(&allowed_hosts.join(","))
                .map_err(|error| ProcessEgressProxyError::Other(error.to_string()))?,
            deny: Vec::new(),
            default: DefaultAction::Deny,
            // NetworkPolicy::Limited is only a destination contract. It does
            // not independently request Harn's SSRF/private-address overlay.
            block_private: Some(SsrfMode::Off),
            allow_loopback: true,
        };
        let mut proxy = Self::start(ProcessPolicySource::Fixed(Box::new(FixedPolicy {
            configured: RwLock::new(ConfiguredPolicy::single("process_network", policy)),
            approved: RwLock::default(),
            local_addresses: local_interface_addresses,
        })))?;
        let control = Arc::new(AllowlistControl {
            http_port: proxy.endpoints.http_port,
            policy: Arc::clone(&proxy.policy),
            audit: Arc::clone(&proxy.audit),
        });
        let mut registered = ALLOWLIST_PROXIES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registered.retain(|entry| entry.strong_count() > 0);
        registered.push(Arc::downgrade(&control));
        drop(registered);
        proxy._control = Some(control);
        Ok(proxy)
    }

    fn start(policy: ProcessPolicySource) -> Result<Self, ProcessEgressProxyError> {
        let super::proxy_ports::ProxyListeners {
            http,
            socks,
            endpoints,
            claim,
        } = super::proxy_ports::bind()?;
        let http_port = endpoints.http_port;
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_connections = Arc::new(AtomicUsize::new(0));
        let audit = Arc::new(Mutex::new(AuditLog::default()));
        let policy = Arc::new(policy);

        let http_join = spawn_listener(
            "harn-process-http-proxy",
            http,
            Arc::clone(&policy),
            Arc::clone(&shutdown),
            Arc::clone(&active_connections),
            Arc::clone(&audit),
            handle_http_client,
        )
        .map_err(ProcessEgressProxyError::Other)?;
        let socks_join = match spawn_listener(
            "harn-process-socks-proxy",
            socks,
            Arc::clone(&policy),
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
                return Err(ProcessEgressProxyError::Other(error));
            }
        };

        #[allow(unused_mut)]
        let mut proxy = Self {
            endpoints,
            _claim: claim,
            shutdown,
            joins: vec![http_join, socks_join],
            audit,
            policy,
            _control: None,
            #[cfg(target_os = "linux")]
            bridge: None,
        };
        // A failure here drops `proxy`, which stops the listeners.
        #[cfg(target_os = "linux")]
        {
            proxy.bridge = Some(
                super::namespace_bridge::NamespaceBridge::start(proxy.endpoints)
                    .map_err(ProcessEgressProxyError::Other)?,
            );
        }
        Ok(proxy)
    }

    pub fn endpoints(&self) -> ProcessNetworkProxy {
        self.endpoints
    }

    pub fn audit_snapshot(&self) -> Vec<ProcessEgressAudit> {
        self.audit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
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
    fn(TcpStream, Arc<ProcessPolicySource>, Arc<Mutex<AuditLog>>) -> io::Result<()>;

#[derive(Debug)]
enum ProcessPolicySource {
    Fixed(Box<FixedPolicy>),
    Live {
        context: Option<EgressPolicyContext>,
        default_block_private: bool,
    },
}

/// An allowlist and the hosts added to it after it started. A started list can
/// name an internal registry, so its hosts may answer with a private address. A
/// host approved later came from a refusal and may not.
#[derive(Debug)]
struct FixedPolicy {
    configured: RwLock<ConfiguredPolicy>,
    approved: RwLock<Vec<(String, u16)>>,
    local_addresses: fn() -> io::Result<Vec<IpAddr>>,
}

fn local_interface_addresses() -> io::Result<Vec<IpAddr>> {
    let addresses: Vec<_> = if_addrs::get_if_addrs()?
        .into_iter()
        .map(|interface| interface.ip())
        .collect();
    if addresses.is_empty() {
        return Err(io::Error::other("no local interface addresses reported"));
    }
    Ok(addresses)
}

/// Destination policy for a granted child network with no configured egress
/// policy: every public host, no loopback, private ranges per the SSRF default.
fn process_network_grant_policy(default_block_private: bool) -> ConfiguredPolicy {
    ConfiguredPolicy::single(
        "process_network_grant",
        EgressPolicy {
            allow: Vec::new(),
            deny: Vec::new(),
            default: DefaultAction::Allow,
            block_private: Some(if default_block_private {
                SsrfMode::BlockPrivate
            } else {
                SsrfMode::Off
            }),
            allow_loopback: false,
        },
    )
}

impl ProcessPolicySource {
    /// Refuse approved names resolving to private or local addresses at connect time.
    fn hold_approved_host_public(
        &self,
        host: &str,
        port: u16,
        addresses: Vec<SocketAddr>,
    ) -> Result<Vec<SocketAddr>, String> {
        let Self::Fixed(fixed) = self else {
            return Ok(addresses);
        };
        let name = normalize_host(host);
        let approved = fixed
            .approved
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&(name.clone(), port));
        if approved {
            // Refresh for every connection: interfaces may change after approval.
            let local = (fixed.local_addresses)().map_err(|_| {
                format!("cannot inspect local addresses for approved host `{name}`")
            })?;
            if addresses.iter().any(|address| {
                is_disallowed_ip(address.ip(), false)
                    || local
                        .iter()
                        .any(|ip| ip.to_canonical() == address.ip().to_canonical())
            }) {
                return Err(format!(
                    "approved host `{name}` resolves to a private or local address"
                ));
            }
        }
        Ok(addresses)
    }

    fn configured(&self) -> Result<ConfiguredPolicy, String> {
        let mut configured = match self {
            Self::Fixed(fixed) => Some(
                fixed
                    .configured
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            ),
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
            ..
        } = self
        {
            let configured = configured
                .get_or_insert_with(|| process_network_grant_policy(*default_block_private));
            if configured.policy.block_private.is_none() && *default_block_private {
                configured.policy.block_private = Some(SsrfMode::BlockPrivate);
            }
        }
        configured.ok_or_else(|| "managed egress policy is unavailable".to_string())
    }
}

// Shared proxy infrastructure: it serves every run, so its threads carry no run's scope.
#[allow(clippy::disallowed_methods)]
fn spawn_listener(
    name: &str,
    listener: TcpListener,
    policy: Arc<ProcessPolicySource>,
    shutdown: Arc<AtomicBool>,
    active_connections: Arc<AtomicUsize>,
    audit: Arc<Mutex<AuditLog>>,
    handler: ClientHandler,
) -> Result<JoinHandle<()>, String> {
    crate::runtime_stack::builder()
        .name(name.to_string())
        .spawn(move || {
            while let Ok((stream, _peer)) = listener.accept() {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                if active_connections
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
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
                if crate::runtime_stack::builder()
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
    audit: Arc<Mutex<AuditLog>>,
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
    audit: Arc<Mutex<AuditLog>>,
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
    audit: &Mutex<AuditLog>,
) -> Result<TcpStream, String> {
    let configured = match policy.configured() {
        Ok(configured) => configured,
        Err(reason) => {
            record_audit(audit, protocol, host, port, false, &reason);
            return Err(reason);
        }
    };
    match resolve_allowed(host, port, &configured)
        .and_then(|addresses| policy.hold_approved_host_public(host, port, addresses))
    {
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
            let reason = UNREACHABLE_REASON.to_string();
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
    audit: &Mutex<AuditLog>,
    protocol: &'static str,
    host: &str,
    port: u16,
    allowed: bool,
    reason: &str,
) {
    let mut audit = audit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = ProcessEgressAudit {
        protocol,
        host: normalize_host(host),
        port,
        allowed,
        reason: reason.to_string(),
    };
    if !allowed
        && reason != UNREACHABLE_REASON
        && audit.unread_refusals.len() < MAX_AUDIT_EVENTS
        && !audit
            .unread_refusals
            .iter()
            .any(|seen| seen.host == entry.host && seen.port == entry.port)
    {
        audit.unread_refusals.push(entry.clone());
    }
    if audit.entries.len() == MAX_AUDIT_EVENTS {
        audit.entries.remove(0);
    }
    audit.entries.push(entry);
}

fn relay(left: TcpStream, right: TcpStream) -> io::Result<()> {
    let mut left_read = left.try_clone()?;
    let mut right_write = right.try_clone()?;
    #[allow(clippy::disallowed_methods)] // A proxy relay, as above.
    let forward = crate::runtime_stack::spawn(move || io::copy(&mut left_read, &mut right_write));
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
        let error = ProcessEgressProxy::start_allowlist(&["[broken".to_string()])
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid bracketed host rule"), "{error}");
    }

    /// A refused loopback bind is one diagnosable condition: the host is
    /// confined. The message must carry the code, name the listener, say the
    /// confinement is inherited when this process declares no profile, and
    /// name the remedy, or a reader is back to chasing a bare EPERM.
    #[test]
    fn permission_denied_bind_is_classified_as_confined_host() {
        let error = ProcessEgressProxyError::from_bind(
            "HTTP",
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Operation not permitted (os error 1)",
            ),
        );
        let ProcessEgressProxyError::HostLoopbackBindDenied {
            protocol,
            profile,
            source,
        } = &error
        else {
            panic!("expected the confined-host variant, got {error:?}");
        };
        assert_eq!(*protocol, "HTTP");
        assert_eq!(*profile, SandboxProfile::Unrestricted);
        assert!(source.contains("Operation not permitted"), "{source}");
        let rendered = error.to_string();
        assert!(rendered.starts_with("HARN-CAP-202: "), "{rendered}");
        assert!(rendered.contains("HTTP egress proxy"), "{rendered}");
        assert!(
            rendered.contains("inherited from a parent process"),
            "{rendered}"
        );
        assert!(rendered.contains("--allow-process-loopback"), "{rendered}");
        assert!(rendered.contains("Operation not permitted"), "{rendered}");
    }

    /// Negative control for the classifier: a bind that fails for any other
    /// reason keeps the old protocol-prefixed shape and never earns the code.
    #[test]
    fn other_bind_failures_are_not_classified_as_confinement() {
        for kind in [
            io::ErrorKind::AddrInUse,
            io::ErrorKind::AddrNotAvailable,
            io::ErrorKind::Other,
        ] {
            let error = ProcessEgressProxyError::from_bind(
                "SOCKS5",
                io::Error::new(kind, "listener refused"),
            );
            let ProcessEgressProxyError::Other(message) = &error else {
                panic!("{kind:?} must not classify as confinement, got {error:?}");
            };
            assert_eq!(message, "bind child SOCKS5 egress proxy: listener refused");
            assert!(!error.to_string().contains("HARN-CAP-202"), "{error}");
        }
    }

    #[test]
    fn live_policy_source_grants_public_hosts_then_observes_runtime_configuration() {
        let context = EgressPolicyContext(Arc::new(std::sync::RwLock::new(
            super::super::EgressState::default(),
        )));
        let source = ProcessPolicySource::Live {
            context: Some(context.clone()),
            default_block_private: true,
        };
        let granted = source.configured().unwrap();
        assert_eq!(granted.sources, vec!["process_network_grant"]);
        assert_eq!(
            resolve_allowed("140.82.112.3", 443, &granted).unwrap(),
            vec!["140.82.112.3:443".parse::<SocketAddr>().unwrap()],
            "a granted child network reaches public hosts without HARN_EGRESS_*"
        );
        for private in ["127.0.0.1", "10.0.0.5", "169.254.169.254"] {
            let reason = resolve_allowed(private, 443, &granted).unwrap_err();
            assert!(reason.contains("private"), "{private}: {reason}");
        }

        context.0.write().unwrap().policy = Some(ConfiguredPolicy::single(
            "stdlib",
            EgressPolicy {
                allow: parse_rule_list("api.example.com:443").unwrap(),
                deny: Vec::new(),
                default: DefaultAction::Deny,
                block_private: None,
                allow_loopback: false,
            },
        ));
        let configured = source.configured().unwrap();
        assert_eq!(configured.sources, vec!["stdlib"]);
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
        let server = crate::runtime_stack::spawn(move || {
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
        let server = crate::runtime_stack::spawn(move || {
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
        let server = crate::runtime_stack::spawn(move || {
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
        let configured = ConfiguredPolicy::single(
            "test",
            EgressPolicy {
                allow: parse_rule_list("127.0.0.1:443,*.trusted.example").unwrap(),
                deny: parse_rule_list("blocked.trusted.example").unwrap(),
                default: DefaultAction::Deny,
                block_private: Some(SsrfMode::Off),
                allow_loopback: true,
            },
        );
        assert!(resolve_allowed("127.0.0.1", 443, &configured).is_ok());
        assert_eq!(
            resolve_allowed("127.0.0.1", 80, &configured).unwrap_err(),
            "no allow rule matched"
        );
        let missing_policy = ConfiguredPolicy::single(
            "test",
            EgressPolicy {
                allow: parse_rule_list("*.trusted.example").unwrap(),
                deny: Vec::new(),
                default: DefaultAction::Deny,
                block_private: Some(SsrfMode::Off),
                allow_loopback: true,
            },
        );
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
        let configured = ConfiguredPolicy::single(
            "test",
            EgressPolicy {
                allow: parse_rule_list("127.0.0.1:443").unwrap(),
                deny: Vec::new(),
                default: DefaultAction::Deny,
                block_private: Some(SsrfMode::BlockPrivate),
                allow_loopback: false,
            },
        );
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

    /// The grant must change what a running proxy decides, and the refusal
    /// must read back once, or an approval could not rerun the command.
    #[test]
    fn an_approved_host_is_admitted_after_its_refusal_is_read() {
        let proxy = ProcessEgressProxy::start_allowlist(&[]).expect("start");
        let endpoints = proxy.endpoints();
        let connect = |target: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", endpoints.http_port)).expect("reach");
            write!(
                stream,
                "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n"
            )
            .expect("send");
            let mut head = Vec::new();
            let mut chunk = [0u8; 256];
            while !head.windows(2).any(|pair| pair == b"\r\n") {
                let read = stream.read(&mut chunk).expect("read");
                if read == 0 {
                    break;
                }
                head.extend_from_slice(&chunk[..read]);
            }
            String::from_utf8_lossy(&head).into_owned()
        };
        let last_reason = || {
            let log = proxy.audit.lock().unwrap();
            log.entries.last().expect("audited").reason.clone()
        };
        let target = "registry.example:443";

        assert!(connect(target).contains("403"));
        assert_eq!(last_reason(), "no allow rule matched");
        let refusals = take_child_network_refusals(endpoints);
        assert_eq!(refusals.len(), 1, "{refusals:?}");
        assert_eq!(refusals[0].host, "registry.example");
        assert!(take_child_network_refusals(endpoints).is_empty());

        assert!(allow_child_network_host(endpoints, "registry.example", None).expect("grant"));
        connect(target);
        assert_ne!(
            last_reason(),
            "no allow rule matched",
            "the list now has it"
        );
        assert!(connect("registry.example:22").contains("403"));
        assert_eq!(
            last_reason(),
            "no allow rule matched",
            "an omitted port must not open SSH"
        );
        assert!(connect("registry.example:8443").contains("403"));
        assert_eq!(last_reason(), "no allow rule matched");
        assert!(
            allow_child_network_host(endpoints, "registry.example", Some(8443))
                .expect("grant port")
        );
        connect("registry.example:8443");
        assert_ne!(
            last_reason(),
            "no allow rule matched",
            "the explicit port reaches address resolution"
        );
        assert!(connect("registry.example:8444").contains("403"));
        assert_eq!(
            last_reason(),
            "no allow rule matched",
            "adjacent ports remain refused"
        );

        // An approval opens one name: another still gets no lookup on the host.
        assert!(connect("unlisted.invalid:443").contains("403"));
        assert_eq!(last_reason(), "no allow rule matched");
        take_child_network_refusals(endpoints);

        // Nor can an approved name reach the host's own services. It resolves to
        // loopback on a host with DNS and to nothing without; neither connects.
        let server = TcpListener::bind("127.0.0.1:0").expect("bind");
        server.set_nonblocking(true).expect("nonblocking");
        let port = server.local_addr().expect("addr").port();
        assert!(allow_child_network_host(endpoints, "localtest.me", Some(port)).expect("grant"));
        assert!(connect(&format!("localtest.me:{port}")).contains("403"));
        assert!(server.accept().is_err(), "loopback must see no connection");

        drop(proxy);
        assert!(!allow_child_network_host(endpoints, "registry.example", None).expect("gone"));
    }

    #[test]
    fn an_approved_host_may_not_answer_with_a_private_address() {
        let listed = ConfiguredPolicy::single(
            "process_network",
            EgressPolicy {
                allow: parse_rule_list("listed.example").unwrap(),
                deny: Vec::new(),
                default: DefaultAction::Deny,
                block_private: Some(SsrfMode::Off),
                allow_loopback: true,
            },
        );
        let source = ProcessPolicySource::Fixed(Box::new(FixedPolicy {
            configured: RwLock::new(listed),
            approved: RwLock::new(vec![("approved.example".to_string(), 443)]),
            local_addresses: || Ok(vec!["93.184.216.34".parse().unwrap()]),
        }));
        let answer = |ip: &str| vec![SocketAddr::new(ip.parse().unwrap(), 443)];
        for private in [
            "127.0.0.1",
            "169.254.169.254",
            "10.0.0.1",
            "192.168.1.1",
            "0.0.0.0",
            "::1",
            "::ffff:127.0.0.1",
            "93.184.216.34",
            "::ffff:93.184.216.34",
        ] {
            assert!(
                source
                    .hold_approved_host_public("Approved.Example", 443, answer(private))
                    .is_err(),
                "{private} must be refused"
            );
        }
        let mixed = [answer("140.82.112.3"), answer("127.0.0.1")].concat();
        assert!(source
            .hold_approved_host_public("approved.example", 443, mixed)
            .is_err());
        assert!(source
            .hold_approved_host_public("approved.example", 443, answer("140.82.112.3"))
            .is_ok());
        assert!(
            source
                .hold_approved_host_public("listed.example", 443, answer("10.0.0.1"))
                .is_ok(),
            "a list that named an internal registry keeps it"
        );
        assert!(
            source
                .hold_approved_host_public(
                    "approved.example",
                    80,
                    vec![SocketAddr::new("10.0.0.1".parse().unwrap(), 80)]
                )
                .is_ok(),
            "an approval on 443 does not change another port's static grant"
        );
        let ProcessPolicySource::Fixed(mut fixed) = source else {
            unreachable!()
        };
        fixed.local_addresses = || Err(io::Error::other("interface lookup unavailable"));
        let source = ProcessPolicySource::Fixed(fixed);
        assert!(source
            .hold_approved_host_public("approved.example", 443, answer("140.82.112.3"))
            .is_err());
        assert!(source
            .hold_approved_host_public("listed.example", 443, answer("140.82.112.3"))
            .is_ok());
        static LOOKUPS: AtomicUsize = AtomicUsize::new(0);
        let ProcessPolicySource::Fixed(mut fixed) = source else {
            unreachable!()
        };
        fixed.local_addresses = || {
            Ok(if LOOKUPS.fetch_add(1, Ordering::SeqCst) == 0 {
                vec!["93.184.216.34".parse().unwrap()]
            } else {
                vec!["140.82.112.3".parse().unwrap()]
            })
        };
        let source = ProcessPolicySource::Fixed(fixed);
        assert!(source
            .hold_approved_host_public("approved.example", 443, answer("140.82.112.3"))
            .is_ok());
        assert!(
            source
                .hold_approved_host_public("approved.example", 443, answer("140.82.112.3"))
                .is_err(),
            "a newly assigned local address is refused on the next connection"
        );
        assert_eq!(LOOKUPS.load(Ordering::SeqCst), 2);
    }

    /// One approval names one host. The rule parser takes lists and wildcards, so
    /// a card showing one name must not be able to grant more than that.
    #[test]
    fn an_approval_grants_exactly_one_host() {
        let proxy = ProcessEgressProxy::start_allowlist(&[]).expect("start");
        let endpoints = proxy.endpoints();
        for wider in [
            "a.example,b.example",
            "*",
            "*.example",
            "a.example b.example",
            "",
            "169.254.169.254",
            "127.0.0.1",
            "0.0.0.0",
            "::1",
            "2130706433",
            "0x7f.1",
            "1.2.example",
            "localhost",
            "registry.example.",
        ] {
            assert!(!can_approve_child_network_host(wider), "{wider:?}");
            assert!(
                allow_child_network_host(endpoints, wider, None).is_err(),
                "{wider:?} must be refused"
            );
        }
        assert!(allow_child_network_host(endpoints, "registry.example", None).expect("grant"));
    }
}
