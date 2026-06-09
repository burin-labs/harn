//! SSRF / egress IP classifier and a connect-time DNS guard.
//!
//! The [`is_disallowed_ip`] classifier is ported verbatim from
//! harn-cloud's `harn-cloud-gateway::url_validation` so the runtime and the
//! cloud control plane agree on exactly which addresses are off-limits
//! (loopback, RFC 1918, link-local incl. the `169.254.169.254` cloud-metadata
//! endpoint, broadcast, unspecified, multicast, documentation, CGNAT
//! `100.64/10`, benchmark `198.18/15`; and the IPv6 analogues plus IPv4-mapped
//! addresses re-applying the v4 rules).
//!
//! [`GuardedResolver`] implements [`reqwest::dns::Resolve`] and filters the
//! resolved [`SocketAddr`]s through the classifier *at connect time*. Because
//! reqwest establishes the TCP connection to exactly the addresses the
//! resolver returns, this closes the DNS-rebinding TOCTOU window: even if a
//! caller's `check_url` pre-check resolved a host to a public IP, the actual
//! connection can only be made to an address the guard re-validated here.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use ipnet::IpNet;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use super::ResolvedIpRules;

/// The error type carried by [`reqwest::dns::Resolving`]. reqwest keeps its
/// own alias `pub(crate)`, so we spell out the equivalent boxed trait object.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// True when `ip` must not be the target of an outbound connection.
///
/// `allow_loopback` relaxes the loopback rule only; every other private,
/// link-local, metadata, CGNAT, benchmark, and reserved range stays blocked.
pub fn is_disallowed_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => is_disallowed_ipv4(v4, allow_loopback),
        IpAddr::V6(v6) => is_disallowed_ipv6(v6, allow_loopback),
    }
}

fn is_disallowed_ipv4(ip: Ipv4Addr, allow_loopback: bool) -> bool {
    if ip.is_loopback() {
        return !allow_loopback;
    }
    if ip.is_private() || ip.is_link_local() || ip.is_broadcast() {
        return true;
    }
    if ip.is_unspecified() || ip.is_multicast() || ip.is_documentation() {
        return true;
    }
    // Cloud-metadata endpoint (AWS / GCP / Azure all front this IP).
    if ip.octets() == [169, 254, 169, 254] {
        return true;
    }
    // Carrier-grade NAT (RFC 6598).
    let [a, b, _, _] = ip.octets();
    if a == 100 && (64..=127).contains(&b) {
        return true;
    }
    // Benchmark range 198.18/15.
    if a == 198 && (b == 18 || b == 19) {
        return true;
    }
    false
}

fn is_disallowed_ipv6(ip: Ipv6Addr, allow_loopback: bool) -> bool {
    if ip.is_loopback() {
        return !allow_loopback;
    }
    if ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    let segments = ip.segments();
    // Link-local fe80::/10.
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // Unique-local fc00::/7.
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // IPv4-mapped (::ffff:0:0/96) — collapse and reapply the v4 rules.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_disallowed_ipv4(v4, allow_loopback);
    }
    false
}

/// Resolve a host through the OS resolver, dropping any disallowed addresses.
///
/// Shared by [`GuardedResolver`] and the synchronous `check_url` host
/// resolution path so both apply identical filtering. The inner resolver is
/// injectable so tests can feed deterministic fixtures instead of relying on
/// real DNS.
trait InnerResolver: Send + Sync {
    fn lookup(&self, host: &str) -> std::io::Result<Vec<SocketAddr>>;
}

/// Production resolver: the blocking OS getaddrinfo via `to_socket_addrs`.
///
/// Port `0` is requested because reqwest overrides the port from the URL (or
/// substitutes the scheme default) before connecting; we only care about the
/// A/AAAA records.
struct GaiInnerResolver;

impl InnerResolver for GaiInnerResolver {
    fn lookup(&self, host: &str) -> std::io::Result<Vec<SocketAddr>> {
        (host, 0_u16).to_socket_addrs().map(|addrs| addrs.collect())
    }
}

/// A [`reqwest::dns::Resolve`] implementation that filters every resolved
/// address through [`is_disallowed_ip`] AND the NetPolicy deny CIDR/IP rules.
/// If filtering removes all addresses, resolution fails with an error that
/// names ONLY the host — never the URL, query string, or any caller secret.
///
/// This is the connect-time backstop for the NetPolicy fix (#3174): reqwest
/// can only open a TCP connection to an address this resolver returns, so a
/// hostname that resolves (or rebinds) into a denied CIDR is unreachable even
/// though the URL literal carried only a hostname. Only the security boundary
/// (private-range + deny rules) is enforced here — the allowlist positive grant
/// is decided at the URL layer where the hostname is known, so it is not
/// re-applied to the pinned address (a host-allowlisted name may legitimately
/// resolve outside the allow CIDRs).
pub struct GuardedResolver {
    /// When true, drop any address the SSRF classifier rejects. Off when the
    /// resolver is installed purely for NetPolicy deny rules with
    /// `block_private:off`, so it must not start blocking private ranges.
    block_private: bool,
    allow_loopback: bool,
    /// NetPolicy deny CIDR/IP nets; any resolved address inside one is dropped.
    deny_nets: Vec<IpNet>,
    inner: Arc<dyn InnerResolver>,
}

impl std::fmt::Debug for GuardedResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardedResolver")
            .field("block_private", &self.block_private)
            .field("allow_loopback", &self.allow_loopback)
            .field("deny_nets", &self.deny_nets)
            .finish_non_exhaustive()
    }
}

impl GuardedResolver {
    /// Build a guard backed by the blocking OS resolver that enforces only the
    /// SSRF private-address block.
    pub fn new(allow_loopback: bool) -> Self {
        Self {
            block_private: true,
            allow_loopback,
            deny_nets: Vec::new(),
            inner: Arc::new(GaiInnerResolver),
        }
    }

    /// Build a guard that enforces the NetPolicy deny nets and, when
    /// `block_private`, the SSRF private-address block. `allow_loopback`, the
    /// deny nets, and `block_private` come from the effective egress policy via
    /// [`super::current_resolved_ip_rules`] and
    /// [`super::current_ssrf_client_settings`].
    pub fn with_policy(block_private: bool, allow_loopback: bool, rules: &ResolvedIpRules) -> Self {
        Self {
            block_private,
            allow_loopback,
            deny_nets: rules.deny.clone(),
            inner: Arc::new(GaiInnerResolver),
        }
    }

    #[cfg(test)]
    fn with_inner(allow_loopback: bool, inner: Arc<dyn InnerResolver>) -> Self {
        Self {
            block_private: true,
            allow_loopback,
            deny_nets: Vec::new(),
            inner,
        }
    }

    #[cfg(test)]
    fn with_inner_full(
        block_private: bool,
        allow_loopback: bool,
        deny_nets: Vec<IpNet>,
        inner: Arc<dyn InnerResolver>,
    ) -> Self {
        Self {
            block_private,
            allow_loopback,
            deny_nets,
            inner,
        }
    }
}

/// Error returned when DNS resolution yields only disallowed addresses.
///
/// The message intentionally carries only the host so it can never echo a
/// secret embedded in the request URL.
#[derive(Debug)]
struct BlockedHostError {
    host: String,
}

impl std::fmt::Display for BlockedHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "egress: host `{}` resolves only to disallowed addresses \
             (private, loopback, link-local, or metadata IP)",
            self.host
        )
    }
}

impl std::error::Error for BlockedHostError {}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let block_private = self.block_private;
        let allow_loopback = self.allow_loopback;
        let deny_nets = self.deny_nets.clone();
        let inner = self.inner.clone();
        Box::pin(async move {
            // `to_socket_addrs` is blocking; never run it on the async runtime
            // worker — hand it to the blocking pool. The trait's error slot is
            // reqwest's internal `BoxError`, i.e. `Box<dyn Error + Send + Sync>`.
            let host_for_lookup = host.clone();
            let lookup = tokio::task::spawn_blocking(move || inner.lookup(&host_for_lookup))
                .await
                .map_err(|join_err| Box::new(join_err) as BoxError)?;
            let resolved = lookup.map_err(|io_err| Box::new(io_err) as BoxError)?;
            // Resolve-once-and-pin: filter the SAME address set reqwest will
            // connect to through both the SSRF private-range block and the
            // NetPolicy deny nets. No second resolution happens between here and
            // the socket connect, so a DNS rebind cannot smuggle a denied
            // address past this point.
            let filtered: Vec<SocketAddr> = resolved
                .into_iter()
                .filter(|addr| {
                    let ip = addr.ip();
                    let ssrf_blocked = block_private && is_disallowed_ip(ip, allow_loopback);
                    let deny_blocked = deny_nets.iter().any(|net| net.contains(&ip));
                    !ssrf_blocked && !deny_blocked
                })
                .collect();
            if filtered.is_empty() {
                return Err(Box::new(BlockedHostError { host }) as BoxError);
            }
            Ok(Box::new(filtered.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ipv4_blocks_loopback_unless_allowed() {
        assert!(is_disallowed_ip(v4("127.0.0.1"), false));
        assert!(!is_disallowed_ip(v4("127.0.0.1"), true));
        assert!(is_disallowed_ip(v4("127.255.255.254"), false));
    }

    #[test]
    fn ipv4_blocks_rfc1918() {
        for ip in ["10.0.0.1", "172.16.5.5", "172.31.255.1", "192.168.1.1"] {
            assert!(is_disallowed_ip(v4(ip), false), "{ip}");
        }
    }

    #[test]
    fn ipv4_blocks_link_local_and_metadata() {
        assert!(is_disallowed_ip(v4("169.254.10.1"), false));
        assert!(is_disallowed_ip(v4("169.254.169.254"), false));
        // metadata stays blocked even with the loopback hatch on.
        assert!(is_disallowed_ip(v4("169.254.169.254"), true));
    }

    #[test]
    fn ipv4_blocks_broadcast_unspecified_multicast_documentation() {
        assert!(is_disallowed_ip(v4("255.255.255.255"), false));
        assert!(is_disallowed_ip(v4("0.0.0.0"), false));
        assert!(is_disallowed_ip(v4("224.0.0.1"), false));
        assert!(is_disallowed_ip(v4("192.0.2.1"), false)); // TEST-NET-1
        assert!(is_disallowed_ip(v4("198.51.100.1"), false)); // TEST-NET-2
        assert!(is_disallowed_ip(v4("203.0.113.1"), false)); // TEST-NET-3
    }

    #[test]
    fn ipv4_blocks_cgnat_and_benchmark() {
        assert!(is_disallowed_ip(v4("100.64.0.1"), false));
        assert!(is_disallowed_ip(v4("100.127.255.254"), false));
        assert!(!is_disallowed_ip(v4("100.63.255.255"), false));
        assert!(!is_disallowed_ip(v4("100.128.0.1"), false));
        assert!(is_disallowed_ip(v4("198.18.0.1"), false));
        assert!(is_disallowed_ip(v4("198.19.255.254"), false));
    }

    #[test]
    fn ipv4_allows_public() {
        for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "151.101.1.140"] {
            assert!(!is_disallowed_ip(v4(ip), false), "{ip}");
        }
    }

    #[test]
    fn ipv6_blocks_loopback_unless_allowed() {
        assert!(is_disallowed_ip(v4("::1"), false));
        assert!(!is_disallowed_ip(v4("::1"), true));
    }

    #[test]
    fn ipv6_blocks_unspecified_multicast_link_local_ula() {
        assert!(is_disallowed_ip(v4("::"), false));
        assert!(is_disallowed_ip(v4("ff02::1"), false)); // multicast
        assert!(is_disallowed_ip(v4("fe80::1"), false)); // link-local
        assert!(is_disallowed_ip(v4("febf::1"), false)); // top of fe80::/10
        assert!(is_disallowed_ip(v4("fc00::1"), false)); // ULA
        assert!(is_disallowed_ip(v4("fd12:3456::1"), false)); // ULA
    }

    #[test]
    fn ipv6_mapped_v4_reapplies_v4_rules() {
        assert!(is_disallowed_ip(v4("::ffff:127.0.0.1"), false));
        assert!(is_disallowed_ip(v4("::ffff:10.0.0.1"), false));
        assert!(is_disallowed_ip(v4("::ffff:169.254.169.254"), false));
        // mapped public address stays allowed.
        assert!(!is_disallowed_ip(v4("::ffff:8.8.8.8"), false));
        // mapped loopback follows the hatch.
        assert!(!is_disallowed_ip(v4("::ffff:127.0.0.1"), true));
    }

    #[test]
    fn ipv6_allows_public() {
        assert!(!is_disallowed_ip(v4("2001:4860:4860::8888"), false));
        assert!(!is_disallowed_ip(v4("2606:4700:4700::1111"), false));
    }

    // --- GuardedResolver tests using a stub inner resolver. ---

    struct StubResolver {
        addrs: Vec<SocketAddr>,
    }

    impl InnerResolver for StubResolver {
        fn lookup(&self, _host: &str) -> std::io::Result<Vec<SocketAddr>> {
            Ok(self.addrs.clone())
        }
    }

    fn stub(allow_loopback: bool, addrs: &[&str]) -> GuardedResolver {
        let addrs = addrs
            .iter()
            .map(|s| SocketAddr::new(s.parse().unwrap(), 0))
            .collect();
        GuardedResolver::with_inner(allow_loopback, Arc::new(StubResolver { addrs }))
    }

    async fn resolve_host(resolver: &GuardedResolver) -> Result<Vec<SocketAddr>, String> {
        let name: Name = "example.test".parse().unwrap();
        resolver
            .resolve(name)
            .await
            .map(|addrs| addrs.collect())
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn guarded_resolver_passes_public_addr() {
        let resolver = stub(false, &["93.184.216.34"]);
        let addrs = resolve_host(&resolver).await.expect("public allowed");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip(), v4("93.184.216.34"));
    }

    #[tokio::test]
    async fn guarded_resolver_blocks_loopback() {
        // Simulates DNS rebinding: attacker DNS hands back 127.0.0.1.
        let resolver = stub(false, &["127.0.0.1"]);
        let err = resolve_host(&resolver).await.expect_err("loopback blocked");
        assert!(err.contains("example.test"), "{err}");
    }

    #[tokio::test]
    async fn guarded_resolver_blocks_metadata() {
        let resolver = stub(false, &["169.254.169.254"]);
        let err = resolve_host(&resolver).await.expect_err("metadata blocked");
        assert!(err.contains("example.test"), "{err}");
    }

    #[tokio::test]
    async fn guarded_resolver_filters_mixed_to_public_only() {
        // A host that resolves to both a blocked and a public address keeps
        // only the public one.
        let resolver = stub(false, &["127.0.0.1", "8.8.8.8", "169.254.169.254"]);
        let addrs = resolve_host(&resolver).await.expect("public survives");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip(), v4("8.8.8.8"));
    }

    #[tokio::test]
    async fn guarded_resolver_loopback_hatch_allows_loopback() {
        let resolver = stub(true, &["127.0.0.1"]);
        let addrs = resolve_host(&resolver)
            .await
            .expect("hatch allows loopback");
        assert_eq!(addrs.len(), 1);
    }

    #[tokio::test]
    async fn guarded_resolver_error_names_host_only() {
        // The error must never carry anything but the host (no secrets).
        let resolver = stub(false, &["10.0.0.1"]);
        let err = resolve_host(&resolver).await.expect_err("private blocked");
        assert!(err.contains("example.test"), "{err}");
        assert!(
            !err.contains("10.0.0.1"),
            "must not leak the address: {err}"
        );
    }

    // --- NetPolicy deny-CIDR enforcement at the connect-time pin (#3174). ---

    fn stub_with_deny(
        block_private: bool,
        allow_loopback: bool,
        deny: &[&str],
        addrs: &[&str],
    ) -> GuardedResolver {
        let deny_nets: Vec<IpNet> = deny.iter().map(|s| s.parse().unwrap()).collect();
        let addrs = addrs
            .iter()
            .map(|s| SocketAddr::new(s.parse().unwrap(), 0))
            .collect();
        GuardedResolver::with_inner_full(
            block_private,
            allow_loopback,
            deny_nets,
            Arc::new(StubResolver { addrs }),
        )
    }

    #[tokio::test]
    async fn guarded_resolver_blocks_resolved_addr_in_deny_cidr() {
        // The connect-time backstop drops a resolved address inside a NetPolicy
        // deny CIDR even though the URL carried only a hostname — and the SAME
        // resolution is what reqwest would pin to (resolve-once-and-pin).
        let resolver = stub_with_deny(false, false, &["203.0.113.0/24"], &["203.0.113.7"]);
        let err = resolve_host(&resolver)
            .await
            .expect_err("deny CIDR blocked");
        assert!(err.contains("example.test"), "{err}");
        assert!(!err.contains("203.0.113.7"), "must not leak address: {err}");
    }

    #[tokio::test]
    async fn guarded_resolver_keeps_addr_outside_deny_cidr() {
        let resolver = stub_with_deny(false, false, &["203.0.113.0/24"], &["8.8.8.8"]);
        let addrs = resolve_host(&resolver).await.expect("outside deny CIDR");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip(), v4("8.8.8.8"));
    }

    #[tokio::test]
    async fn guarded_resolver_deny_only_does_not_block_private() {
        // With block_private off (deny-only resolver), a private address that is
        // NOT in a deny net must pass — the deny-only backstop must not silently
        // re-enable the SSRF block.
        let resolver = stub_with_deny(false, false, &["203.0.113.0/24"], &["10.0.0.1"]);
        let addrs = resolve_host(&resolver)
            .await
            .expect("private passes when block off");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip(), v4("10.0.0.1"));
    }

    #[tokio::test]
    async fn guarded_resolver_applies_both_ssrf_and_deny() {
        // With block_private on AND a deny CIDR, a mixed answer keeps only the
        // address that clears BOTH filters.
        let resolver = stub_with_deny(
            true,
            false,
            &["203.0.113.0/24"],
            &["10.0.0.1", "203.0.113.5", "8.8.8.8"],
        );
        let addrs = resolve_host(&resolver).await.expect("one survivor");
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip(), v4("8.8.8.8"));
    }
}
