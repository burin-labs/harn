//! The managed proxy's listeners, and on macOS its claim to their ports on
//! every address of this host.
//!
//! A macOS Seatbelt profile can name a destination only as `localhost:<port>`,
//! and there `localhost` is every address assigned to this host, not only
//! `127.0.0.1`: a confined child admitted to the proxy's port also reaches a
//! listener on that port at the host's LAN or VPN address. So the proxy binds
//! its ports on every local IPv4 address and keeps them for its whole life.
//! Those sockets never listen, so a connection to one goes nowhere. No other
//! socket can bind the same address and port, and a specific bind receives
//! connections ahead of a wildcard listener on that port. The child profile
//! names `tcp4` destinations, which keeps IPv6 and UDP, separate port spaces,
//! out of the grant.
//!
//! A spawn re-checks the claim through [`hold`] before granting the ports, so
//! an address that appeared after the proxy started is claimed too, or the
//! spawn is refused by name.

use std::net::TcpListener;

use super::process_proxy::ProcessEgressProxyError;
use crate::orchestration::ProcessNetworkProxy;

/// Both listeners, bound on `127.0.0.1`, and the claim on their ports.
pub(super) struct ProxyListeners {
    pub(super) http: TcpListener,
    pub(super) socks: TcpListener,
    pub(super) endpoints: ProcessNetworkProxy,
    pub(super) claim: PortClaim,
}

/// Bind the proxy's listeners, choosing ports that no other service on this
/// host holds at any local address.
pub(super) fn bind() -> Result<ProxyListeners, ProcessEgressProxyError> {
    let mut last_collision = None;
    for _ in 0..CLAIM_ATTEMPTS {
        let http = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| ProcessEgressProxyError::from_bind("HTTP", error))?;
        let socks = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| ProcessEgressProxyError::from_bind("SOCKS5", error))?;
        let port = |listener: &TcpListener, protocol: &str| {
            listener
                .local_addr()
                .map(|addr| addr.port())
                .map_err(|error| {
                    ProcessEgressProxyError::Other(format!(
                        "inspect child {protocol} egress proxy: {error}"
                    ))
                })
        };
        let endpoints = ProcessNetworkProxy {
            http_port: port(&http, "HTTP")?,
            socks_port: port(&socks, "SOCKS5")?,
        };
        match PortClaim::new(endpoints) {
            Ok(claim) => {
                return Ok(ProxyListeners {
                    http,
                    socks,
                    endpoints,
                    claim,
                })
            }
            Err(ClaimError::Taken(taken)) => last_collision = Some(taken),
            Err(ClaimError::Io(error)) => {
                return Err(ProcessEgressProxyError::Other(format!(
                    "claim child egress proxy ports: {error}"
                )))
            }
        }
    }
    Err(ProcessEgressProxyError::Other(format!(
        "no free child egress proxy ports after {CLAIM_ATTEMPTS} attempts; last: {}",
        last_collision.map_or_else(String::new, |taken| taken.to_string())
    )))
}

const CLAIM_ATTEMPTS: usize = 16;

/// A local service already holds one of the proxy's ports at another address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProxyPortTaken {
    pub(super) address: std::net::IpAddr,
    pub(super) port: u16,
}

impl std::fmt::Display for ProxyPortTaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "another local service holds port {} at {}, the port of this child's managed \
             network proxy",
            self.port, self.address
        )
    }
}

enum ClaimError {
    Taken(ProxyPortTaken),
    Io(std::io::Error),
}

pub(super) use claim::PortClaim;

#[cfg(not(target_os = "macos"))]
mod claim {
    use super::{ClaimError, ProcessNetworkProxy};

    /// Only macOS grants a port by number on every local address; elsewhere
    /// the child reaches the proxy through a private namespace or not at all.
    #[derive(Debug)]
    pub(in crate::egress) struct PortClaim;

    impl PortClaim {
        pub(super) fn new(_: ProcessNetworkProxy) -> Result<Self, ClaimError> {
            Ok(Self)
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use claim::hold;

#[cfg(target_os = "macos")]
mod claim {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::{Arc, Mutex, PoisonError, Weak};

    use socket2::{Domain, Socket, Type};

    use super::{ClaimError, ProcessNetworkProxy, ProxyPortTaken};

    /// The proxy's sockets on every other local IPv4 address. Dropping it
    /// releases them.
    #[derive(Debug)]
    pub(in crate::egress) struct PortClaim(Arc<Held>);

    #[derive(Debug)]
    struct Held {
        endpoints: ProcessNetworkProxy,
        sockets: Mutex<Vec<(Ipv4Addr, Socket)>>,
    }

    static CLAIMS: Mutex<Vec<Weak<Held>>> = Mutex::new(Vec::new());

    impl PortClaim {
        pub(super) fn new(endpoints: ProcessNetworkProxy) -> Result<Self, ClaimError> {
            let held = Arc::new(Held {
                endpoints,
                sockets: Mutex::new(Vec::new()),
            });
            held.claim_new_addresses()?;
            let mut claims = CLAIMS.lock().unwrap_or_else(PoisonError::into_inner);
            claims.retain(|claim| claim.strong_count() > 0);
            claims.push(Arc::downgrade(&held));
            Ok(Self(held))
        }
    }

    impl Held {
        fn claim_new_addresses(&self) -> Result<(), ClaimError> {
            let mut sockets = self.sockets.lock().unwrap_or_else(PoisonError::into_inner);
            for address in local_ipv4_addresses().map_err(ClaimError::Io)? {
                // The listeners themselves hold 127.0.0.1.
                if address == Ipv4Addr::LOCALHOST
                    || sockets.iter().any(|(held, _)| *held == address)
                {
                    continue;
                }
                let mut claimed = Vec::with_capacity(2);
                for port in [self.endpoints.http_port, self.endpoints.socks_port] {
                    match claim_port(address, port) {
                        Ok(socket) => claimed.push(socket),
                        // The address left the host between listing and binding.
                        Err(error) if error.kind() == std::io::ErrorKind::AddrNotAvailable => {
                            claimed.clear();
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                            return Err(ClaimError::Taken(ProxyPortTaken {
                                address: IpAddr::V4(address),
                                port,
                            }));
                        }
                        Err(error) => return Err(ClaimError::Io(error)),
                    }
                }
                sockets.extend(claimed.into_iter().map(|socket| (address, socket)));
            }
            Ok(())
        }
    }

    /// Bound, never listening. `SO_REUSEADDR` lets the claim shadow an existing
    /// wildcard listener; an exact duplicate still fails without `SO_REUSEPORT`
    /// on both sockets, and neither side here sets it.
    fn claim_port(address: Ipv4Addr, port: u16) -> std::io::Result<Socket> {
        let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
        socket.set_reuse_address(true)?;
        socket.bind(&SocketAddr::from((address, port)).into())?;
        Ok(socket)
    }

    fn local_ipv4_addresses() -> std::io::Result<Vec<Ipv4Addr>> {
        Ok(if_addrs::get_if_addrs()?
            .into_iter()
            .filter_map(|interface| match interface.ip() {
                IpAddr::V4(address) => Some(address),
                IpAddr::V6(_) => None,
            })
            .collect())
    }

    /// Confirm a live proxy in this process holds `endpoints` on every local
    /// address before a child profile grants them, claiming addresses that
    /// appeared since the proxy started.
    pub(crate) fn hold(endpoints: ProcessNetworkProxy) -> Result<(), String> {
        let held = CLAIMS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
            .find(|held| held.endpoints == endpoints);
        let Some(held) = held else {
            return Err(format!(
                "no managed network proxy in this process holds ports {} and {}, so a confined \
                 command granted them could reach whatever local service listens there",
                endpoints.http_port, endpoints.socks_port
            ));
        };
        held.claim_new_addresses().map_err(|error| match error {
            ClaimError::Taken(taken) => {
                format!("{taken}; macOS would let a confined command reach that service directly")
            }
            ClaimError::Io(error) => format!("claim child egress proxy ports: {error}"),
        })
    }
}
