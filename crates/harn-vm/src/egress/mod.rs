use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{OnceLock, RwLock};

use ipnet::IpNet;
use serde_json::json;
use url::Url;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

mod provider_allow;
pub mod ssrf;
#[cfg(test)]
pub(crate) mod test_support;

pub use provider_allow::{
    configured_provider_private_allow_host, install_ssrf_guard_with_private_host_allowlist,
    ssrf_client_cache_key,
};
pub use ssrf::{is_disallowed_ip, GuardedResolver};

pub const HARN_EGRESS_ALLOW_ENV: &str = "HARN_EGRESS_ALLOW";
pub const HARN_EGRESS_DENY_ENV: &str = "HARN_EGRESS_DENY";
pub const HARN_EGRESS_DEFAULT_ENV: &str = "HARN_EGRESS_DEFAULT";
pub const HARN_EGRESS_BLOCK_PRIVATE_ENV: &str = "HARN_EGRESS_BLOCK_PRIVATE";
pub const HARN_EGRESS_ALLOW_LOOPBACK_ENV: &str = "HARN_EGRESS_ALLOW_LOOPBACK";
pub const EGRESS_AUDIT_TOPIC: &str = "connectors.egress.audit";

thread_local! {
    static REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH: RefCell<usize> = const { RefCell::new(0) };
    static REQUIRE_SSRF_GUARD_DEPTH: RefCell<usize> = const { RefCell::new(0) };
}

/// Whether the private-address (SSRF) block is engaged for a policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SsrfMode {
    /// No private-address blocking; only host allow/deny rules apply.
    Off,
    /// Block any URL whose host is, or resolves to, a private / loopback /
    /// link-local / metadata / reserved address.
    #[default]
    BlockPrivate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DefaultAction {
    Allow,
    Deny,
}

#[derive(Clone, Debug)]
struct EgressPolicy {
    allow: Vec<EgressRule>,
    deny: Vec<EgressRule>,
    default: DefaultAction,
    /// `None` means "caller did not set the axis": the effective mode then
    /// falls back to [`SsrfMode::BlockPrivate`] when the SSRF guard scope is
    /// active (e.g. under `harn run`) and [`SsrfMode::Off`] otherwise.
    block_private: Option<SsrfMode>,
    allow_loopback: bool,
}

#[derive(Clone, Debug)]
struct EgressRule {
    raw: String,
    matcher: EgressMatcher,
    port: Option<u16>,
}

#[derive(Clone, Debug)]
enum EgressMatcher {
    Host(String),
    Suffix(String),
    Ip(IpAddr),
    Cidr(IpNet),
}

#[derive(Clone, Debug)]
struct EgressState {
    #[cfg(not(test))]
    env_checked: bool,
    #[cfg(not(test))]
    policy: Option<ConfiguredPolicy>,
    #[cfg(test)]
    test_env_checked: HashSet<std::thread::ThreadId>,
    #[cfg(test)]
    test_policies: HashMap<std::thread::ThreadId, ConfiguredPolicy>,
}

#[derive(Clone, Debug)]
struct ConfiguredPolicy {
    source: &'static str,
    policy: EgressPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EgressBlocked {
    pub surface: String,
    pub url: String,
    pub host: String,
    pub port: Option<u16>,
    pub reason: String,
}

static EGRESS_STATE: OnceLock<RwLock<EgressState>> = OnceLock::new();

fn state() -> &'static RwLock<EgressState> {
    EGRESS_STATE.get_or_init(|| {
        RwLock::new(EgressState {
            #[cfg(not(test))]
            env_checked: false,
            #[cfg(not(test))]
            policy: None,
            #[cfg(test)]
            test_env_checked: HashSet::new(),
            #[cfg(test)]
            test_policies: HashMap::new(),
        })
    })
}

pub fn register_egress_builtins(vm: &mut Vm) {
    vm.register_builtin("egress_policy", |args, _out| {
        let Some(VmValue::Dict(config)) = args.first() else {
            return Err(vm_error("egress_policy: requires a config dict"));
        };
        let policy = policy_from_config(config)?;
        install_policy(policy, "stdlib")?;
        Ok(policy_summary())
    });
}

pub async fn enforce_url_allowed(surface: &str, url: &str) -> Result<(), VmError> {
    // `resolution_required` carries forward a deferred default-deny hostname:
    // the URL-literal rules did not grant it, but a resolved-IP allow rule
    // might. The resolution step must then either find a granting address or
    // block (an unresolvable host has no other path to approval).
    let resolution_required = match check_url_decision(surface, url)? {
        SyncCheck::Allowed => false,
        SyncCheck::DeferToResolution(_) => true,
        SyncCheck::Blocked(blocked) => {
            audit_blocked(&blocked).await;
            return Err(blocked.to_vm_error());
        }
    };
    // Host-name resolution (DNS off the runtime): applies the SSRF private
    // block AND the NetPolicy IP/CIDR allow & deny rules to the resolved
    // addresses. The connect-time GuardedResolver is the unbypassable backstop
    // that re-checks the same predicates on the pinned address; this step gives
    // callers a clean typed `EgressBlocked` instead of an opaque connect error.
    if let Some(blocked) = check_url_host_resolution(surface, url, resolution_required).await? {
        audit_blocked(&blocked).await;
        return Err(blocked.to_vm_error());
    }
    Ok(())
}

pub fn redirect_url_allowed(surface: &str, previous_url: Option<&str>, url: &str) -> bool {
    match check_redirect_url(surface, previous_url, url) {
        Ok(Some(blocked)) => {
            audit_blocked_background(blocked);
            false
        }
        Ok(None) => true,
        Err(_) => false,
    }
}

pub fn redirect_policy(surface: &'static str, max_redirects: usize) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= max_redirects {
            attempt.error("too many redirects")
        } else if redirect_url_allowed(
            surface,
            attempt.previous().last().map(|url| url.as_str()),
            attempt.url().as_str(),
        ) {
            attempt.follow()
        } else {
            attempt.error("egress policy blocked redirect target")
        }
    })
}

pub fn redact_reqwest_error(error: &reqwest::Error) -> String {
    redact_diagnostic_text(&error.to_string())
}

pub fn redact_diagnostic_text(raw: &str) -> String {
    let policy = crate::redact::current_policy();
    let urls_redacted = policy.redact_urls_in_text(raw);
    policy.redact_string(urls_redacted.as_ref()).into_owned()
}

fn check_redirect_url(
    surface: &str,
    previous_url: Option<&str>,
    raw_url: &str,
) -> Result<Option<EgressBlocked>, VmError> {
    if let Some(blocked) = insecure_redirect_downgrade_block(surface, previous_url, raw_url)? {
        return Ok(Some(blocked));
    }
    check_url(surface, raw_url)
}

fn insecure_redirect_downgrade_block(
    surface: &str,
    previous_url: Option<&str>,
    raw_url: &str,
) -> Result<Option<EgressBlocked>, VmError> {
    let Some(previous_url) = previous_url else {
        return Ok(None);
    };
    let previous = Url::parse(previous_url).map_err(|error| {
        vm_error(format!(
            "egress: invalid redirect source `{previous_url}`: {error}"
        ))
    })?;
    if previous.scheme() != "https" {
        return Ok(None);
    }
    let target_url = Url::parse(raw_url)
        .map_err(|error| vm_error(format!("egress: invalid URL `{raw_url}`: {error}")))?;
    if target_url.scheme() != "http" {
        return Ok(None);
    }

    let target = EgressTarget::parse(raw_url)?;
    if insecure_redirect_loopback_exempt(&target) {
        return Ok(None);
    }
    Ok(Some(blocked(
        surface,
        raw_url,
        &target,
        "https redirect target downgrades to insecure http".to_string(),
    )))
}

fn insecure_redirect_loopback_exempt(target: &EgressTarget) -> bool {
    let (_, allow_loopback) = current_ssrf_client_settings();
    allow_loopback && target.is_loopback_host()
}

pub fn client_error_for_url(surface: &str, url: &str) -> Option<crate::connectors::ClientError> {
    match check_url(surface, url) {
        Ok(Some(blocked)) => {
            audit_blocked_background(blocked.clone());
            Some(crate::connectors::ClientError::EgressBlocked(blocked))
        }
        Ok(None) => None,
        Err(error) => Some(crate::connectors::ClientError::InvalidArgs(
            error.to_string(),
        )),
    }
}

pub fn connector_error_for_url(
    surface: &str,
    url: &str,
) -> Option<crate::connectors::ConnectorError> {
    match check_url(surface, url) {
        Ok(Some(blocked)) => {
            audit_blocked_background(blocked.clone());
            Some(crate::connectors::ConnectorError::Activation(
                blocked.to_string(),
            ))
        }
        Ok(None) => None,
        Err(error) => Some(crate::connectors::ConnectorError::Activation(
            error.to_string(),
        )),
    }
}

pub fn reset_egress_policy_for_host() {
    let mut guard = state().write().expect("egress policy state poisoned");
    #[cfg(test)]
    {
        let thread_id = std::thread::current().id();
        guard.test_env_checked.remove(&thread_id);
        guard.test_policies.remove(&thread_id);
    }
    #[cfg(not(test))]
    {
        guard.env_checked = false;
        guard.policy = None;
    }
    clear_explicit_egress_policy_requirement_for_host();
    clear_ssrf_guard_requirement_for_host();
}

pub(crate) fn clear_explicit_egress_policy_requirement_for_host() {
    REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH.with(|depth| *depth.borrow_mut() = 0);
}

#[cfg(test)]
pub fn reset_egress_policy_for_tests() {
    reset_egress_policy_for_host();
}

/// Reads one `HARN_EGRESS_*` variable for policy construction.
///
/// Under `cfg(test)` the process environment is structurally invisible: reads
/// come from a thread-local override map instead, so ambient shell
/// configuration (a developer's `HARN_EGRESS_BLOCK_PRIVATE=off`, a CI
/// wrapper's allowlist) can never seed a policy a test did not ask for.
/// Env-derived-policy tests inject values through
/// [`EgressTestEnvGuard::set`], which matches the per-thread keying of the
/// rest of the `cfg(test)` egress state and keeps tests parallel-safe without
/// any cross-test lock.
fn egress_env_var(key: &str) -> Option<String> {
    #[cfg(test)]
    {
        test_env_overrides::get(key)
    }
    #[cfg(not(test))]
    {
        std::env::var(key).ok()
    }
}

#[cfg(test)]
mod test_env_overrides {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    thread_local! {
        static OVERRIDES: RefCell<BTreeMap<String, String>> =
            const { RefCell::new(BTreeMap::new()) };
    }

    pub(super) fn get(key: &str) -> Option<String> {
        OVERRIDES.with(|overrides| overrides.borrow().get(key).cloned())
    }

    pub(super) fn set(key: &str, value: &str) {
        OVERRIDES.with(|overrides| {
            overrides
                .borrow_mut()
                .insert(key.to_owned(), value.to_owned());
        });
    }

    pub(super) fn clear() {
        OVERRIDES.with(|overrides| overrides.borrow_mut().clear());
    }
}

/// Gives a test a clean egress universe with hermetic edges: on both creation
/// and drop it clears this thread's env overrides (see [`egress_env_var`])
/// and resets this thread's policy state, so neither ambient configuration
/// nor a sibling test's leftovers can leak in, and nothing leaks out. All
/// `cfg(test)` egress state is thread-keyed, so no cross-test serialization
/// is needed and the guard is safe to hold across `await` points.
///
/// This governs only the *inputs* to policy installation;
/// `egress_policy(...)`'s deliberate refuse-to-override behavior is
/// unchanged.
#[cfg(test)]
#[must_use]
pub(crate) fn test_env_guard() -> EgressTestEnvGuard {
    test_env_overrides::clear();
    reset_egress_policy_for_host();
    EgressTestEnvGuard {}
}

/// Guard returned by [`test_env_guard`]. Injects `HARN_EGRESS_*` values for
/// this thread via [`EgressTestEnvGuard::set`] and clears all egress test
/// state on drop.
#[cfg(test)]
pub(crate) struct EgressTestEnvGuard {}

#[cfg(test)]
impl EgressTestEnvGuard {
    /// Sets a `HARN_EGRESS_*` variable for this thread only, visible to
    /// [`egress_env_var`] readers on the same thread.
    pub(crate) fn set(&self, key: &str, value: &str) {
        test_env_overrides::set(key, value);
    }
}

#[cfg(test)]
impl Drop for EgressTestEnvGuard {
    fn drop(&mut self) {
        test_env_overrides::clear();
        reset_egress_policy_for_host();
    }
}

/// A clean egress configuration scope for constructing a test client.
#[cfg(test)]
pub(crate) struct EgressTestConfigGuard {
    _env: EgressTestEnvGuard,
}

#[cfg(test)]
impl EgressTestConfigGuard {
    pub(crate) fn new() -> Self {
        Self {
            _env: test_env_guard(),
        }
    }
}

/// Install a thread-local egress policy from `(key, value)` config pairs for
/// tests that need to drive the real HTTP client path without touching
/// process-global `HARN_EGRESS_*` env (which is unsound under concurrency).
#[cfg(test)]
pub(crate) fn install_test_policy(config: &[(&str, VmValue)]) {
    let map = config
        .iter()
        .cloned()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    let policy = policy_from_config(&map).expect("test egress policy parses");
    install_policy(policy, "test").expect("test egress policy installs");
}

/// Scope outbound network to explicit `egress_policy(...)` /
/// `HARN_EGRESS_*` configuration. Without a configured policy, URL
/// checks return [`EgressBlocked`] before opening a socket.
pub fn require_explicit_egress_policy_for_host() -> ExplicitEgressPolicyGuard {
    REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH.with(|depth| {
        *depth.borrow_mut() += 1;
    });
    ExplicitEgressPolicyGuard
}

#[derive(Debug)]
pub struct ExplicitEgressPolicyGuard;

impl Drop for ExplicitEgressPolicyGuard {
    fn drop(&mut self) {
        REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH.with(|depth| {
            let mut depth = depth.borrow_mut();
            *depth = depth.saturating_sub(1);
        });
    }
}

/// Default-on the SSRF private-address guard for outbound HTTP.
///
/// Mirrors [`require_explicit_egress_policy_for_host`]. While in scope, any
/// URL whose host is, or resolves to, a private/loopback/link-local/metadata
/// address is blocked by [`check_url`] unless the caller explicitly opts out
/// via `egress_policy({block_private:"off"})` / `HARN_EGRESS_BLOCK_PRIVATE=off`.
pub fn require_ssrf_guard_for_host() -> SsrfGuardScope {
    REQUIRE_SSRF_GUARD_DEPTH.with(|depth| {
        *depth.borrow_mut() += 1;
    });
    SsrfGuardScope
}

#[derive(Debug)]
pub struct SsrfGuardScope;

impl Drop for SsrfGuardScope {
    fn drop(&mut self) {
        REQUIRE_SSRF_GUARD_DEPTH.with(|depth| {
            let mut depth = depth.borrow_mut();
            *depth = depth.saturating_sub(1);
        });
    }
}

pub(crate) fn clear_ssrf_guard_requirement_for_host() {
    REQUIRE_SSRF_GUARD_DEPTH.with(|depth| *depth.borrow_mut() = 0);
}

fn ssrf_guard_scope_active() -> bool {
    REQUIRE_SSRF_GUARD_DEPTH.with(|depth| *depth.borrow() > 0)
}

/// The effective SSRF settings the HTTP client builder should apply, taking
/// the configured policy (if any), env seeding, and the default-on guard scope
/// into account. Returns `(block_private_active, allow_loopback)`.
///
/// Used by `crate::http::client::build_http_client` to decide whether to
/// install a [`GuardedResolver`] (the connect-time backstop) and to key the
/// pooled-client cache so clients with different SSRF settings never alias.
pub fn current_ssrf_client_settings() -> (bool, bool) {
    // Best-effort: seed env so a process configured purely via env is honored.
    let _ = ensure_env_seeded();
    let configured = {
        let guard = state().read().expect("egress policy state poisoned");
        #[cfg(test)]
        {
            guard
                .test_policies
                .get(&std::thread::current().id())
                .cloned()
        }
        #[cfg(not(test))]
        {
            guard.policy.clone()
        }
    };
    let (mode, allow_loopback) = effective_ssrf_settings(configured.as_ref().map(|c| &c.policy));
    (mode == SsrfMode::BlockPrivate, allow_loopback)
}

/// The NetPolicy IP/CIDR rules that must be enforced against a host's
/// *resolved* addresses at connect time.
///
/// This is the connect-time analogue of the URL-literal rule matching done by
/// [`check_url`]: it carries only the IP-literal and CIDR rules (host/suffix
/// rules pin to the URL hostname and cannot be evaluated against an address).
/// `deny` rules block any resolved address they contain; when `allow_active`
/// is set the allowlist is "only these", so a resolved address that matches no
/// `allow` rule is rejected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedIpRules {
    /// Resolved addresses inside any of these nets are blocked (deny wins).
    pub deny: Vec<IpNet>,
    /// When `allow_active`, a resolved address must fall inside one of these
    /// nets (or be host-allowed at the URL layer) to be permitted.
    pub allow: Vec<IpNet>,
    /// True when an allowlist with a default-deny posture is in effect, i.e.
    /// the resolved address must be positively allowed.
    pub allow_active: bool,
}

impl ResolvedIpRules {
    /// True when no resolved-IP rule would ever change a decision, so callers
    /// can skip installing the connect-time resolver / re-resolving.
    pub fn is_empty(&self) -> bool {
        self.deny.is_empty() && !self.allow_active
    }
}

/// Collapse an IP-literal rule to a host-sized net so it can be matched the
/// same way as a CIDR rule.
fn ip_to_net(ip: IpAddr) -> IpNet {
    match ip {
        IpAddr::V4(v4) => IpNet::from(ipnet::Ipv4Net::new(v4, 32).expect("/32 is valid")),
        IpAddr::V6(v6) => IpNet::from(ipnet::Ipv6Net::new(v6, 128).expect("/128 is valid")),
    }
}

/// Extract the IP/CIDR allow & deny rules from a policy into a
/// [`ResolvedIpRules`] for the connect-time [`GuardedResolver`]. Host/suffix
/// rules are intentionally dropped — they pin to the URL hostname, never the
/// resolved IP.
///
/// Only PORT-UNSCOPED rules are carried here: the resolver sees addresses
/// stripped of the destination port (reqwest substitutes the URL port after
/// resolution), so it cannot honor a `:port` qualifier. Port-scoped IP/CIDR
/// rules are enforced by the port-aware pre-check in
/// [`check_url_host_resolution`] instead; dropping them from the backstop avoids
/// over-blocking other ports to the same address.
///
/// `allow_active` reflects a default-deny posture, but it is informational for
/// the resolver: the backstop enforces only the deny side (a security
/// boundary), since the allow grant depends on hostname context the resolver
/// does not have.
fn resolved_ip_rules_for(policy: &EgressPolicy) -> ResolvedIpRules {
    let net_of = |rule: &EgressRule| match &rule.matcher {
        EgressMatcher::Cidr(net) => Some(*net),
        EgressMatcher::Ip(ip) => Some(ip_to_net(*ip)),
        _ => None,
    };
    let deny: Vec<IpNet> = policy
        .deny
        .iter()
        .filter(|rule| rule.port.is_none())
        .filter_map(net_of)
        .collect();
    let allow: Vec<IpNet> = policy
        .allow
        .iter()
        .filter(|rule| rule.port.is_none())
        .filter_map(net_of)
        .collect();
    ResolvedIpRules {
        deny,
        allow,
        allow_active: policy.default == DefaultAction::Deny,
    }
}

/// The NetPolicy IP/CIDR rules the HTTP client builder should enforce at
/// connect time against resolved addresses, reflecting the configured policy
/// (if any) plus env seeding.
///
/// Used by `crate::http::client::build_http_client` so the connect-time
/// [`GuardedResolver`] pins the connection to an address that has been checked
/// against the operator's CIDR/IP allow & deny rules — closing the gap where a
/// hostname resolving into a denied CIDR (or out of an allowed CIDR) was only
/// caught for URL-literal IPs.
pub fn current_resolved_ip_rules() -> ResolvedIpRules {
    let _ = ensure_env_seeded();
    let configured = {
        let guard = state().read().expect("egress policy state poisoned");
        #[cfg(test)]
        {
            guard
                .test_policies
                .get(&std::thread::current().id())
                .cloned()
        }
        #[cfg(not(test))]
        {
            guard.policy.clone()
        }
    };
    configured
        .as_ref()
        .map(|c| resolved_ip_rules_for(&c.policy))
        .unwrap_or_default()
}

/// Install the connect-time SSRF [`GuardedResolver`] on an arbitrary reqwest
/// client builder, using the same effective egress settings
/// `crate::http::client::build_http_client` applies to the `http_*` builtins.
///
/// Outbound clients built OUTSIDE the `http_*` path — the shared LLM clients,
/// connector clients, MCP discovery/OAuth/card/transport clients, the provider
/// healthcheck and remote catalog fetchers — route through here so a hostname
/// that resolves (or DNS-rebinds) to a private / loopback / link-local /
/// metadata (`169.254.169.254`) / denied-CIDR address is unreachable at connect
/// time. The resolver is the unbypassable backstop: reqwest can only open a TCP
/// connection to an address this resolver returns.
///
/// Like the `http_*` path, the resolver is installed only when the private
/// block is engaged OR NetPolicy deny rules exist; otherwise the builder is
/// returned untouched (e.g. `block_private:off`). The effective loopback hatch
/// is honored, so a caller that has opted into loopback (or a local model whose
/// URL is a literal `127.0.0.1`, which never hits the resolver) keeps working.
pub fn install_ssrf_guard(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    install_ssrf_guard_with_private_host_allowlist(builder, &[])
}

/// Resolve the effective SSRF mode + loopback hatch given the (optional)
/// configured policy. An unset `block_private` axis defaults to
/// [`SsrfMode::BlockPrivate`] iff the guard scope is active.
fn effective_ssrf_settings(configured: Option<&EgressPolicy>) -> (SsrfMode, bool) {
    let scope_active = ssrf_guard_scope_active();
    match configured {
        Some(policy) => {
            let mode = policy.block_private.unwrap_or(if scope_active {
                SsrfMode::BlockPrivate
            } else {
                SsrfMode::Off
            });
            (mode, policy.allow_loopback)
        }
        None => {
            let mode = if scope_active {
                SsrfMode::BlockPrivate
            } else {
                SsrfMode::Off
            };
            (mode, false)
        }
    }
}

/// Reason text for an SSRF private-address block. Carries only the host, never
/// the resolved address or any secret embedded in the URL.
fn private_block_reason(host: &str) -> String {
    format!(
        "host `{host}` is a disallowed address \
         (private, loopback, link-local, or metadata IP)"
    )
}

/// Synchronous private-address block for IP-literal hosts. Host *names* are
/// handled by the async [`enforce_url_allowed`] path (DNS off the runtime) and
/// ultimately by the connect-time [`GuardedResolver`]; this only classifies a
/// literal address so the sync callers (redirects, connectors) still block the
/// most direct SSRF attempt.
fn private_block_for_literal(target: &EgressTarget, allow_loopback: bool) -> Option<String> {
    target.ip.and_then(|ip| {
        is_disallowed_ip(ip, allow_loopback).then(|| private_block_reason(&target.host))
    })
}

/// Resolve a host to its IP addresses using the blocking OS resolver on the
/// blocking pool. Returns `None` on resolution failure or when no addresses
/// are produced (deferred to the connect-time guard).
async fn resolve_host_addrs(host: &str) -> Option<Vec<IpAddr>> {
    use std::net::ToSocketAddrs;
    let host = host.to_string();
    tokio::task::spawn_blocking(move || {
        (host.as_str(), 0_u16)
            .to_socket_addrs()
            .ok()
            .map(|addrs| addrs.map(|addr| addr.ip()).collect::<Vec<_>>())
            .filter(|addrs: &Vec<IpAddr>| !addrs.is_empty())
    })
    .await
    .ok()
    .flatten()
}

/// Outcome of the synchronous URL-literal egress check.
enum SyncCheck {
    /// The URL is permitted by the URL-literal rules. Resolved-IP rules may
    /// still apply downstream (see `enforce_url_allowed`).
    Allowed,
    /// The URL is blocked outright; no later resolution can grant it.
    Blocked(EgressBlocked),
    /// A default-deny allowlist matched no host/suffix rule for this hostname,
    /// but at least one IP/CIDR allow rule exists. The hostname might still be
    /// granted if it RESOLVES into an allow net, so the decision is deferred to
    /// [`check_url_host_resolution`]. Sync-only callers treat this as blocked.
    DeferToResolution(EgressBlocked),
}

/// Thin wrapper for the synchronous callers (redirects, connectors,
/// client-error mapping) that cannot resolve hostnames. A `DeferToResolution`
/// outcome is conservatively treated as blocked: the URL-literal rules did not
/// grant it and these paths have no resolution step to consult.
pub(crate) fn check_url(surface: &str, raw_url: &str) -> Result<Option<EgressBlocked>, VmError> {
    match check_url_decision(surface, raw_url)? {
        SyncCheck::Allowed => Ok(None),
        SyncCheck::Blocked(blocked) | SyncCheck::DeferToResolution(blocked) => Ok(Some(blocked)),
    }
}

fn check_url_decision(surface: &str, raw_url: &str) -> Result<SyncCheck, VmError> {
    ensure_env_seeded()?;
    let configured = {
        let guard = state().read().expect("egress policy state poisoned");
        #[cfg(test)]
        {
            let thread_id = std::thread::current().id();
            guard.test_policies.get(&thread_id).cloned()
        }
        #[cfg(not(test))]
        {
            guard.policy.clone()
        }
    };
    let (ssrf_mode, allow_loopback) =
        effective_ssrf_settings(configured.as_ref().map(|c| &c.policy));
    let require_explicit_policy =
        REQUIRE_EXPLICIT_EGRESS_POLICY_DEPTH.with(|depth| *depth.borrow() > 0);

    let Some(configured) = configured else {
        // No host allow/deny policy is configured. The private-block still
        // applies in addition when the SSRF guard is active (default-on).
        if ssrf_mode == SsrfMode::BlockPrivate {
            let target = EgressTarget::parse(raw_url)?;
            if let Some(reason) = private_block_for_literal(&target, allow_loopback) {
                return Ok(SyncCheck::Blocked(blocked(
                    surface, raw_url, &target, reason,
                )));
            }
        }
        if require_explicit_policy {
            let target = EgressTarget::parse(raw_url)?;
            return Ok(SyncCheck::Blocked(blocked(
                surface,
                raw_url,
                &target,
                "no egress policy configured".to_string(),
            )));
        }
        return Ok(SyncCheck::Allowed);
    };
    let target = EgressTarget::parse(raw_url)?;

    // Deny-private wins: the SSRF block is applied IN ADDITION to and ahead of
    // the host allow/deny rules so an `allow` entry cannot re-open a private
    // address. (Literal IPs only here; host names are resolved in the async
    // `enforce_url_allowed` path and pinned at connect time.)
    if ssrf_mode == SsrfMode::BlockPrivate {
        if let Some(reason) = private_block_for_literal(&target, allow_loopback) {
            return Ok(SyncCheck::Blocked(blocked(
                surface, raw_url, &target, reason,
            )));
        }
    }

    if let Some(rule) = configured
        .policy
        .deny
        .iter()
        .find(|rule| rule.matches(&target))
    {
        return Ok(SyncCheck::Blocked(blocked(
            surface,
            raw_url,
            &target,
            format!("matched deny rule `{}`", rule.raw),
        )));
    }
    if configured
        .policy
        .allow
        .iter()
        .any(|rule| rule.matches(&target))
    {
        return Ok(SyncCheck::Allowed);
    }
    if configured.policy.default == DefaultAction::Allow {
        return Ok(SyncCheck::Allowed);
    }

    // Default-deny and no host/suffix allow matched. For a literal-IP target
    // this is final. For a HOSTNAME, an IP/CIDR allow rule could still grant it
    // once resolved (the gap #3174 closes for the allow direction), so defer.
    let could_allow_on_resolution = target.ip.is_none()
        && configured
            .policy
            .allow
            .iter()
            .any(EgressRule::is_ip_matcher);
    let blocked = blocked(
        surface,
        raw_url,
        &target,
        "no allow rule matched".to_string(),
    );
    if could_allow_on_resolution {
        Ok(SyncCheck::DeferToResolution(blocked))
    } else {
        Ok(SyncCheck::Blocked(blocked))
    }
}

/// Async host-name resolution check. Run by [`enforce_url_allowed`] after the
/// sync [`check_url`] passes (or after `check_url` deferred a hostname whose
/// only possible allow is a CIDR/IP rule): resolve the host ONCE off the
/// runtime and evaluate the resolved addresses against
///
///   1. the SSRF private-address block (when `block_private` is active), and
///   2. the NetPolicy IP/CIDR allow & deny rules ([`ResolvedIpRules`]).
///
/// This is the typed-error analogue of the connect-time [`GuardedResolver`]:
/// it gives callers a clean `EgressBlocked` instead of an opaque connect
/// error. The GuardedResolver remains the unbypassable backstop — it
/// re-applies the SAME checks to whatever address reqwest actually pins the
/// connection to, so a DNS rebind between this pre-check and the connection
/// cannot defeat a deny CIDR or smuggle a connection out of an allow CIDR.
///
/// `resolution_required` is set when the sync layer could not grant the
/// hostname (default-deny with no host/suffix allow match) and the only way it
/// can be allowed is a resolved-IP allow rule; in that case an unresolvable
/// host must be blocked rather than deferred, since nothing downstream can
/// grant it.
async fn check_url_host_resolution(
    surface: &str,
    raw_url: &str,
    resolution_required: bool,
) -> Result<Option<EgressBlocked>, VmError> {
    let configured = {
        let guard = state().read().expect("egress policy state poisoned");
        #[cfg(test)]
        {
            let thread_id = std::thread::current().id();
            guard.test_policies.get(&thread_id).cloned()
        }
        #[cfg(not(test))]
        {
            guard.policy.clone()
        }
    };
    let (ssrf_mode, _allow_loopback) =
        effective_ssrf_settings(configured.as_ref().map(|c| &c.policy));
    let block_private = ssrf_mode == SsrfMode::BlockPrivate;
    // Does the policy carry any IP/CIDR deny rule (port-scoped or not)? Such a
    // rule can only ever change the outcome via the resolved-IP layer.
    let has_ip_deny = configured
        .as_ref()
        .is_some_and(|c| c.policy.deny.iter().any(EgressRule::is_ip_matcher));

    // Nothing on the resolved-IP axis can change the outcome: defer entirely.
    // (`resolution_required` already implies a deferred allow decision.)
    if !block_private && !has_ip_deny && !resolution_required {
        return Ok(None);
    }

    let target = EgressTarget::parse(raw_url)?;
    if target.ip.is_some() {
        // Literal IPs are already fully handled synchronously in `check_url`
        // (both the SSRF block and the IP/CIDR rules).
        return Ok(None);
    }

    let Some(addrs) = resolve_host_addrs(&target.host).await else {
        // Unresolvable here. If a resolved-IP allow rule was the host's only
        // path to approval we must block; otherwise defer to the connect-time
        // guard (which will itself fail to connect to a disallowed address).
        if resolution_required {
            return Ok(Some(blocked(
                surface,
                raw_url,
                &target,
                "no allow rule matched".to_string(),
            )));
        }
        return Ok(None);
    };

    Ok(evaluate_resolved_addrs(
        configured.as_ref(),
        surface,
        raw_url,
        &target,
        &addrs,
        resolution_required,
    ))
}

/// Pure resolved-address policy evaluation, factored out of
/// [`check_url_host_resolution`] so it can be unit-tested with synthetic
/// addresses (no real DNS). `configured` is the effective policy; `addrs` are
/// the host's resolved addresses.
///
/// Resolve-once-and-pin: callers pass a SINGLE resolution and every decision
/// below uses exactly those addresses. The connect-time [`GuardedResolver`]
/// re-applies the same SSRF + deny predicates to whatever address reqwest pins
/// the socket to, so the two layers cannot disagree under DNS rebinding.
fn evaluate_resolved_addrs(
    configured: Option<&ConfiguredPolicy>,
    surface: &str,
    raw_url: &str,
    target: &EgressTarget,
    addrs: &[IpAddr],
    resolution_required: bool,
) -> Option<EgressBlocked> {
    let (ssrf_mode, allow_loopback) = effective_ssrf_settings(configured.map(|c| &c.policy));
    let block_private = ssrf_mode == SsrfMode::BlockPrivate;
    let allow_active = configured.is_some_and(|c| c.policy.default == DefaultAction::Deny);

    // (1) SSRF private-address block: if ANY resolved address is disallowed,
    //     block (a private address among the answers is a rebinding vector).
    if block_private && addrs.iter().any(|ip| is_disallowed_ip(*ip, allow_loopback)) {
        return Some(blocked(
            surface,
            raw_url,
            target,
            private_block_reason(&target.host),
        ));
    }

    // (2) NetPolicy deny CIDR/IP wins over allow: if ANY resolved address
    //     matches a deny IP/CIDR rule (port-aware), block. This is the gap
    //     #3174 covers: a hostname that resolves into a denied CIDR must be
    //     blocked. Matching against the rule list (rather than pre-reduced
    //     nets) preserves per-rule `:port` scoping.
    if let Some(rule) = configured.and_then(|c| {
        c.policy.deny.iter().find(|rule| {
            rule.is_ip_matcher()
                && addrs
                    .iter()
                    .any(|ip| rule.matches_resolved_ip(*ip, target.port))
        })
    }) {
        return Some(blocked(
            surface,
            raw_url,
            target,
            format!("resolved address matched deny rule `{}`", rule.raw),
        ));
    }

    // (3) Allowlist gating: when a default-deny allowlist is in effect and the
    //     host was NOT already granted at the URL layer (`resolution_required`),
    //     the host is allowed only if a resolved address matches an allow
    //     IP/CIDR rule (port-aware). Without one, reject.
    if resolution_required && allow_active {
        let permitted = configured.is_some_and(|c| {
            c.policy.allow.iter().any(|rule| {
                rule.is_ip_matcher()
                    && addrs
                        .iter()
                        .any(|ip| rule.matches_resolved_ip(*ip, target.port))
            })
        });
        if !permitted {
            return Some(blocked(
                surface,
                raw_url,
                target,
                "no allow rule matched".to_string(),
            ));
        }
    }

    None
}

fn blocked(surface: &str, url: &str, target: &EgressTarget, reason: String) -> EgressBlocked {
    EgressBlocked {
        surface: surface.to_string(),
        url: redact_sensitive_url(url),
        host: target.host.clone(),
        port: target.port,
        reason,
    }
}

async fn audit_blocked(blocked: &EgressBlocked) {
    let Some(log) = active_event_log() else {
        return;
    };
    let Ok(topic) = Topic::new(EGRESS_AUDIT_TOPIC) else {
        return;
    };
    let payload = json!({
        "surface": blocked.surface,
        "url": blocked.url,
        "host": blocked.host,
        "port": blocked.port,
        "reason": blocked.reason,
        "error_type": "EgressBlocked",
    });
    let _ = log
        .append(&topic, LogEvent::new("egress.blocked", payload))
        .await;
}

fn audit_blocked_background(blocked: EgressBlocked) {
    let Some(log) = active_event_log() else {
        return;
    };
    let Ok(topic) = Topic::new(EGRESS_AUDIT_TOPIC) else {
        return;
    };
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let payload = json!({
                "surface": blocked.surface,
                "url": blocked.url,
                "host": blocked.host,
                "port": blocked.port,
                "reason": blocked.reason,
                "error_type": "EgressBlocked",
            });
            let _ = log
                .append(&topic, LogEvent::new("egress.blocked", payload))
                .await;
        });
    }
}

fn install_policy(policy: EgressPolicy, source: &'static str) -> Result<(), VmError> {
    ensure_env_seeded()?;
    let mut guard = state().write().expect("egress policy state poisoned");
    #[cfg(test)]
    {
        let thread_id = std::thread::current().id();
        if let Some(existing) = guard.test_policies.get(&thread_id) {
            return Err(vm_error(format!(
                "egress_policy: policy already configured from {}",
                existing.source
            )));
        }
        guard
            .test_policies
            .insert(thread_id, ConfiguredPolicy { source, policy });
        Ok(())
    }
    #[cfg(not(test))]
    {
        if let Some(existing) = &guard.policy {
            return Err(vm_error(format!(
                "egress_policy: policy already configured from {}",
                existing.source
            )));
        }
        guard.policy = Some(ConfiguredPolicy { source, policy });
        Ok(())
    }
}

/// Installs a deny-by-default egress policy with the given allow rules for
/// the testbench's `NetworkConfig::DenyByDefault`, giving network confinement
/// a typed entry point instead of riding on mutated process-global
/// `HARN_EGRESS_*` variables. A declared deny-by-default bench must be exactly
/// that, so this deterministically replaces any prior policy — including one
/// seeded from ambient environment, which the refuse-to-override `egress_policy`
/// path would otherwise mix with or trip over. It also marks the environment
/// as checked so a later lazy seed cannot clobber the bench policy.
pub(crate) fn install_deny_by_default_policy(allow: &[String]) -> Result<(), VmError> {
    let policy = EgressPolicy {
        allow: parse_rule_list(&allow.join(","))?,
        deny: Vec::new(),
        default: DefaultAction::Deny,
        block_private: None,
        allow_loopback: false,
    };
    reset_egress_policy_for_host();
    let configured = ConfiguredPolicy {
        source: "testbench",
        policy,
    };
    let mut guard = state().write().expect("egress policy state poisoned");
    #[cfg(test)]
    {
        let thread_id = std::thread::current().id();
        guard.test_env_checked.insert(thread_id);
        guard.test_policies.insert(thread_id, configured);
    }
    #[cfg(not(test))]
    {
        guard.env_checked = true;
        guard.policy = Some(configured);
    }
    Ok(())
}

fn ensure_env_seeded() -> Result<(), VmError> {
    {
        let guard = state().read().expect("egress policy state poisoned");
        #[cfg(test)]
        {
            if guard
                .test_env_checked
                .contains(&std::thread::current().id())
            {
                return Ok(());
            }
        }
        #[cfg(not(test))]
        {
            if guard.env_checked {
                return Ok(());
            }
        }
    }

    let allow = egress_env_var(HARN_EGRESS_ALLOW_ENV);
    let deny = egress_env_var(HARN_EGRESS_DENY_ENV);
    let default = egress_env_var(HARN_EGRESS_DEFAULT_ENV);
    let block_private = egress_env_var(HARN_EGRESS_BLOCK_PRIVATE_ENV);
    let allow_loopback = egress_env_var(HARN_EGRESS_ALLOW_LOOPBACK_ENV);
    let any_set = allow.is_some()
        || deny.is_some()
        || default.is_some()
        || block_private.is_some()
        || allow_loopback.is_some();
    let build_policy = || -> Result<EgressPolicy, VmError> {
        Ok(EgressPolicy {
            allow: parse_rule_list(allow.as_deref().unwrap_or(""))?,
            deny: parse_rule_list(deny.as_deref().unwrap_or(""))?,
            default: parse_default_action(default.as_deref().unwrap_or("allow"))?,
            block_private: block_private.as_deref().map(parse_ssrf_mode).transpose()?,
            allow_loopback: allow_loopback
                .as_deref()
                .map(parse_bool)
                .transpose()?
                .unwrap_or(false),
        })
    };
    let mut guard = state().write().expect("egress policy state poisoned");
    #[cfg(test)]
    {
        let thread_id = std::thread::current().id();
        if guard.test_env_checked.contains(&thread_id) {
            return Ok(());
        }
        guard.test_env_checked.insert(thread_id);
        if !any_set {
            return Ok(());
        }
        guard.test_policies.insert(
            thread_id,
            ConfiguredPolicy {
                source: "environment",
                policy: build_policy()?,
            },
        );
        Ok(())
    }
    #[cfg(not(test))]
    {
        if guard.env_checked {
            return Ok(());
        }
        guard.env_checked = true;
        if !any_set {
            return Ok(());
        }
        guard.policy = Some(ConfiguredPolicy {
            source: "environment",
            policy: build_policy()?,
        });
        Ok(())
    }
}

fn policy_from_config(config: &crate::value::DictMap) -> Result<EgressPolicy, VmError> {
    let allow = match config.get("allow") {
        Some(VmValue::List(items)) => parse_rule_values(items)?,
        Some(VmValue::Nil) => Vec::new(),
        Some(_) => return Err(vm_error("egress_policy: allow must be a list")),
        None => Vec::new(),
    };
    let deny = match config.get("deny") {
        Some(VmValue::List(items)) => parse_rule_values(items)?,
        Some(VmValue::Nil) => Vec::new(),
        Some(_) => return Err(vm_error("egress_policy: deny must be a list")),
        None => Vec::new(),
    };
    let default = config
        .get("default")
        .map(|value| parse_default_action(&value.display()))
        .transpose()?
        .unwrap_or(DefaultAction::Allow);
    let block_private = config
        .get("block_private")
        .map(|value| parse_ssrf_mode(&value.display()))
        .transpose()?;
    let allow_loopback = match config.get("allow_loopback") {
        Some(value) => parse_bool(&value.display())?,
        None => false,
    };
    Ok(EgressPolicy {
        allow,
        deny,
        default,
        block_private,
        allow_loopback,
    })
}

fn parse_ssrf_mode(raw: &str) -> Result<SsrfMode, VmError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        // `private` and `on` engage the private-address block; `off` opts out.
        "private" | "on" | "block" | "block_private" | "true" => Ok(SsrfMode::BlockPrivate),
        "off" | "false" | "none" => Ok(SsrfMode::Off),
        other => Err(vm_error(format!(
            "egress_policy: block_private must be `private`/`on` or `off`, got `{other}`"
        ))),
    }
}

fn parse_bool(raw: &str) -> Result<bool, VmError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" | "" => Ok(false),
        other => Err(vm_error(format!(
            "egress_policy: allow_loopback must be a boolean, got `{other}`"
        ))),
    }
}

fn parse_rule_values(values: &[VmValue]) -> Result<Vec<EgressRule>, VmError> {
    values
        .iter()
        .map(|value| EgressRule::parse(&value.display()))
        .collect()
}

fn parse_rule_list(raw: &str) -> Result<Vec<EgressRule>, VmError> {
    raw.split([',', '\n', ';'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(EgressRule::parse)
        .collect()
}

fn parse_default_action(raw: &str) -> Result<DefaultAction, VmError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "allow" => Ok(DefaultAction::Allow),
        "deny" => Ok(DefaultAction::Deny),
        other => Err(vm_error(format!(
            "egress_policy: default must be `allow` or `deny`, got `{other}`"
        ))),
    }
}

fn policy_summary() -> VmValue {
    let configured = {
        let guard = state().read().expect("egress policy state poisoned");
        #[cfg(test)]
        {
            guard
                .test_policies
                .get(&std::thread::current().id())
                .cloned()
        }
        #[cfg(not(test))]
        {
            guard.policy.clone()
        }
    };
    let mut dict = BTreeMap::new();
    if let Some(configured) = configured {
        dict.insert("configured".to_string(), VmValue::Bool(true));
        dict.put_str("source", configured.source);
        dict.put_str(
            "default",
            match configured.policy.default {
                DefaultAction::Allow => "allow",
                DefaultAction::Deny => "deny",
            },
        );
        dict.insert(
            "allow".to_string(),
            VmValue::List(std::sync::Arc::new(
                configured
                    .policy
                    .allow
                    .iter()
                    .map(|rule| VmValue::String(arcstr::ArcStr::from(rule.raw.as_str())))
                    .collect(),
            )),
        );
        dict.insert(
            "deny".to_string(),
            VmValue::List(std::sync::Arc::new(
                configured
                    .policy
                    .deny
                    .iter()
                    .map(|rule| VmValue::String(arcstr::ArcStr::from(rule.raw.as_str())))
                    .collect(),
            )),
        );
        let (mode, allow_loopback) = effective_ssrf_settings(Some(&configured.policy));
        dict.put_str(
            "block_private",
            match mode {
                SsrfMode::BlockPrivate => "private",
                SsrfMode::Off => "off",
            },
        );
        dict.insert("allow_loopback".to_string(), VmValue::Bool(allow_loopback));
    } else {
        dict.insert("configured".to_string(), VmValue::Bool(false));
    }
    VmValue::dict(dict)
}

impl EgressRule {
    fn parse(raw: &str) -> Result<Self, VmError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(vm_error("egress_policy: empty egress rule"));
        }
        let (host, port) = parse_rule_host_port(raw)?;
        let host = normalize_host(&host);
        let matcher = if let Some(suffix) = host.strip_prefix("*.") {
            if suffix.is_empty() {
                return Err(vm_error(format!(
                    "egress_policy: invalid wildcard rule `{raw}`"
                )));
            }
            EgressMatcher::Suffix(suffix.to_string())
        } else if host.contains('/') {
            EgressMatcher::Cidr(IpNet::from_str(&host).map_err(|error| {
                vm_error(format!("egress_policy: invalid CIDR rule `{raw}`: {error}"))
            })?)
        } else if let Ok(ip) = IpAddr::from_str(&host) {
            EgressMatcher::Ip(ip)
        } else {
            EgressMatcher::Host(host)
        };
        Ok(Self {
            raw: raw.to_string(),
            matcher,
            port,
        })
    }

    fn matches(&self, target: &EgressTarget) -> bool {
        if let Some(port) = self.port {
            if target.port != Some(port) {
                return false;
            }
        }
        match &self.matcher {
            EgressMatcher::Host(host) => target.host == *host,
            EgressMatcher::Suffix(suffix) => {
                crate::harness_net::host_has_dns_suffix(&target.host, suffix)
            }
            EgressMatcher::Ip(ip) => target.ip == Some(*ip),
            EgressMatcher::Cidr(net) => target.ip.is_some_and(|ip| net.contains(&ip)),
        }
    }

    /// True when this rule is an IP-literal or CIDR matcher (the only rule
    /// kinds that can be evaluated against a *resolved* address). Host and
    /// suffix matchers can only ever match the URL hostname, so they are out
    /// of scope for the resolved-IP layer.
    fn is_ip_matcher(&self) -> bool {
        matches!(self.matcher, EgressMatcher::Ip(_) | EgressMatcher::Cidr(_))
    }

    /// Whether this IP/CIDR rule matches a *resolved* address for the given
    /// request port. Host/suffix rules never match here (they pin to the URL
    /// hostname, not the address it resolves to). Returns `false` for non-IP
    /// matchers so callers can blanket-iterate the rule list.
    fn matches_resolved_ip(&self, ip: IpAddr, request_port: Option<u16>) -> bool {
        if let Some(port) = self.port {
            if request_port != Some(port) {
                return false;
            }
        }
        match &self.matcher {
            EgressMatcher::Ip(rule_ip) => *rule_ip == ip,
            EgressMatcher::Cidr(net) => net.contains(&ip),
            EgressMatcher::Host(_) | EgressMatcher::Suffix(_) => false,
        }
    }
}

#[derive(Clone, Debug)]
struct EgressTarget {
    host: String,
    ip: Option<IpAddr>,
    port: Option<u16>,
}

impl EgressTarget {
    fn parse(raw_url: &str) -> Result<Self, VmError> {
        let parsed = Url::parse(raw_url)
            .map_err(|error| vm_error(format!("egress: invalid URL `{raw_url}`: {error}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| vm_error(format!("egress: URL `{raw_url}` does not include a host")))?;
        let host = normalize_host(host);
        let ip = IpAddr::from_str(&host).ok();
        Ok(Self {
            host,
            ip,
            port: parsed.port_or_known_default(),
        })
    }

    fn is_loopback_host(&self) -> bool {
        self.host == "localhost"
            || self.ip.is_some_and(|ip| match ip {
                IpAddr::V4(v4) => v4.is_loopback(),
                IpAddr::V6(v6) => {
                    v6.is_loopback()
                        || v6
                            .to_ipv4_mapped()
                            .is_some_and(|mapped| mapped.is_loopback())
                }
            })
    }
}

fn parse_rule_host_port(raw: &str) -> Result<(String, Option<u16>), VmError> {
    if let Ok(url) = Url::parse(raw) {
        if let Some(host) = url.host_str() {
            return Ok((host.to_string(), url.port_or_known_default()));
        }
    }
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('[') {
        let Some((host, suffix)) = rest.split_once(']') else {
            return Err(vm_error(format!(
                "egress_policy: invalid bracketed host rule `{raw}`"
            )));
        };
        let port = if let Some(port) = suffix.strip_prefix(':') {
            Some(parse_port(raw, port)?)
        } else if suffix.is_empty() {
            None
        } else {
            return Err(vm_error(format!(
                "egress_policy: invalid bracketed host rule `{raw}`"
            )));
        };
        return Ok((host.to_string(), port));
    }
    if let Some((host, port)) = split_host_port(raw) {
        return Ok((host.to_string(), Some(parse_port(raw, port)?)));
    }
    Ok((raw.to_string(), None))
}

fn split_host_port(raw: &str) -> Option<(&str, &str)> {
    let (host, port) = raw.rsplit_once(':')?;
    if host.contains(':') || port.is_empty() || !port.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((host, port))
}

fn parse_port(rule: &str, raw: &str) -> Result<u16, VmError> {
    raw.parse::<u16>()
        .map_err(|error| vm_error(format!("egress_policy: invalid port in `{rule}`: {error}")))
}

fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_end_matches('.')
        .trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase()
}

fn redact_sensitive_url(url: &str) -> String {
    crate::redact::current_policy().redact_url(url)
}

fn vm_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message.into())))
}

impl EgressBlocked {
    pub(crate) fn to_vm_error(&self) -> VmError {
        let mut dict = BTreeMap::new();
        dict.put_str("type", "EgressBlocked");
        dict.put_str("category", "egress_blocked");
        dict.put_str("message", self.to_string());
        dict.put_str("surface", self.surface.as_str());
        dict.put_str("url", self.url.as_str());
        dict.put_str("host", self.host.as_str());
        dict.insert(
            "port".to_string(),
            self.port
                .map(|port| VmValue::Int(port as i64))
                .unwrap_or(VmValue::Nil),
        );
        dict.put_str("reason", self.reason.as_str());
        VmError::Thrown(VmValue::dict(dict))
    }
}

impl std::fmt::Display for EgressBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.port {
            Some(port) => write!(
                f,
                "EgressBlocked: {} blocked {}:{} for {} ({})",
                self.surface, self.host, port, self.url, self.reason
            ),
            None => write!(
                f,
                "EgressBlocked: {} blocked {} for {} ({})",
                self.surface, self.host, self.url, self.reason
            ),
        }
    }
}

#[cfg(test)]
mod tests;
