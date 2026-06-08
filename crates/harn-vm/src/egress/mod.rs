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

pub mod ssrf;

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
    if let Some(blocked) = check_url(surface, url)? {
        audit_blocked(&blocked).await;
        return Err(blocked.to_vm_error());
    }
    // Host-name SSRF resolution (DNS off the runtime). The connect-time
    // GuardedResolver is the unbypassable backstop; this gives callers a clean
    // typed `EgressBlocked` instead of an opaque connect error.
    if let Some(blocked) = check_url_host_resolution(surface, url).await? {
        audit_blocked(&blocked).await;
        return Err(blocked.to_vm_error());
    }
    Ok(())
}

pub fn redirect_url_allowed(surface: &str, url: &str) -> bool {
    match check_url(surface, url) {
        Ok(Some(blocked)) => {
            audit_blocked_background(blocked);
            false
        }
        Ok(None) => true,
        Err(_) => false,
    }
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

/// Serializes egress tests that mutate `HARN_EGRESS_*` process-global env or
/// the global egress policy state. `std::env::set_var` is unsound under
/// concurrent access, so every env-mutating egress test holds this lock for its
/// whole body. Recovers from a poisoned mutex so a panicking test cannot wedge
/// the rest of the suite.
#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

fn check_url(surface: &str, raw_url: &str) -> Result<Option<EgressBlocked>, VmError> {
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
                return Ok(Some(blocked(surface, raw_url, &target, reason)));
            }
        }
        if require_explicit_policy {
            let target = EgressTarget::parse(raw_url)?;
            return Ok(Some(blocked(
                surface,
                raw_url,
                &target,
                "no egress policy configured".to_string(),
            )));
        }
        return Ok(None);
    };
    let target = EgressTarget::parse(raw_url)?;

    // Deny-private wins: the SSRF block is applied IN ADDITION to and ahead of
    // the host allow/deny rules so an `allow` entry cannot re-open a private
    // address. (Literal IPs only here; host names are resolved in the async
    // `enforce_url_allowed` path and pinned at connect time.)
    if ssrf_mode == SsrfMode::BlockPrivate {
        if let Some(reason) = private_block_for_literal(&target, allow_loopback) {
            return Ok(Some(blocked(surface, raw_url, &target, reason)));
        }
    }

    if let Some(rule) = configured
        .policy
        .deny
        .iter()
        .find(|rule| rule.matches(&target))
    {
        return Ok(Some(blocked(
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
        return Ok(None);
    }
    if configured.policy.default == DefaultAction::Allow {
        return Ok(None);
    }
    Ok(Some(blocked(
        surface,
        raw_url,
        &target,
        "no allow rule matched".to_string(),
    )))
}

/// Async host-name private-address block. Run by [`enforce_url_allowed`] after
/// the sync [`check_url`] passes: when the SSRF mode is `BlockPrivate` and the
/// host is a name (not a literal IP), resolve it off the runtime and block if
/// ANY resolved address is disallowed. The connect-time [`GuardedResolver`]
/// remains the unbypassable backstop against rebinding between this check and
/// the actual connection.
async fn check_url_host_resolution(
    surface: &str,
    raw_url: &str,
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
    let (ssrf_mode, allow_loopback) =
        effective_ssrf_settings(configured.as_ref().map(|c| &c.policy));
    if ssrf_mode != SsrfMode::BlockPrivate {
        return Ok(None);
    }
    let target = EgressTarget::parse(raw_url)?;
    if target.ip.is_some() {
        // Literal IPs are already handled synchronously in `check_url`.
        return Ok(None);
    }
    let Some(addrs) = resolve_host_addrs(&target.host).await else {
        // Unresolvable here: defer to the connect-time guard.
        return Ok(None);
    };
    if addrs.iter().any(|ip| is_disallowed_ip(*ip, allow_loopback)) {
        return Ok(Some(blocked(
            surface,
            raw_url,
            &target,
            private_block_reason(&target.host),
        )));
    }
    Ok(None)
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

    let allow = std::env::var(HARN_EGRESS_ALLOW_ENV).ok();
    let deny = std::env::var(HARN_EGRESS_DENY_ENV).ok();
    let default = std::env::var(HARN_EGRESS_DEFAULT_ENV).ok();
    let block_private = std::env::var(HARN_EGRESS_BLOCK_PRIVATE_ENV).ok();
    let allow_loopback = std::env::var(HARN_EGRESS_ALLOW_LOOPBACK_ENV).ok();
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

fn policy_from_config(config: &BTreeMap<String, VmValue>) -> Result<EgressPolicy, VmError> {
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
        dict.insert(
            "source".to_string(),
            VmValue::String(std::sync::Arc::from(configured.source)),
        );
        dict.insert(
            "default".to_string(),
            VmValue::String(std::sync::Arc::from(match configured.policy.default {
                DefaultAction::Allow => "allow",
                DefaultAction::Deny => "deny",
            })),
        );
        dict.insert(
            "allow".to_string(),
            VmValue::List(std::sync::Arc::new(
                configured
                    .policy
                    .allow
                    .iter()
                    .map(|rule| VmValue::String(std::sync::Arc::from(rule.raw.as_str())))
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
                    .map(|rule| VmValue::String(std::sync::Arc::from(rule.raw.as_str())))
                    .collect(),
            )),
        );
        let (mode, allow_loopback) = effective_ssrf_settings(Some(&configured.policy));
        dict.insert(
            "block_private".to_string(),
            VmValue::String(std::sync::Arc::from(match mode {
                SsrfMode::BlockPrivate => "private",
                SsrfMode::Off => "off",
            })),
        );
        dict.insert("allow_loopback".to_string(), VmValue::Bool(allow_loopback));
    } else {
        dict.insert("configured".to_string(), VmValue::Bool(false));
    }
    VmValue::Dict(std::sync::Arc::new(dict))
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
                target.host.len() > suffix.len()
                    && target.host.ends_with(suffix)
                    && target
                        .host
                        .as_bytes()
                        .get(target.host.len() - suffix.len() - 1)
                        == Some(&b'.')
            }
            EgressMatcher::Ip(ip) => target.ip == Some(*ip),
            EgressMatcher::Cidr(net) => target.ip.is_some_and(|ip| net.contains(&ip)),
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
    VmError::Thrown(VmValue::String(std::sync::Arc::from(message.into())))
}

impl EgressBlocked {
    pub(crate) fn to_vm_error(&self) -> VmError {
        let mut dict = BTreeMap::new();
        dict.insert(
            "type".to_string(),
            VmValue::String(std::sync::Arc::from("EgressBlocked")),
        );
        dict.insert(
            "category".to_string(),
            VmValue::String(std::sync::Arc::from("egress_blocked")),
        );
        dict.insert(
            "message".to_string(),
            VmValue::String(std::sync::Arc::from(self.to_string())),
        );
        dict.insert(
            "surface".to_string(),
            VmValue::String(std::sync::Arc::from(self.surface.as_str())),
        );
        dict.insert(
            "url".to_string(),
            VmValue::String(std::sync::Arc::from(self.url.as_str())),
        );
        dict.insert(
            "host".to_string(),
            VmValue::String(std::sync::Arc::from(self.host.as_str())),
        );
        dict.insert(
            "port".to_string(),
            self.port
                .map(|port| VmValue::Int(port as i64))
                .unwrap_or(VmValue::Nil),
        );
        dict.insert(
            "reason".to_string(),
            VmValue::String(std::sync::Arc::from(self.reason.as_str())),
        );
        VmError::Thrown(VmValue::Dict(std::sync::Arc::new(dict)))
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
mod tests {
    use super::*;

    fn install(config: &[(&str, VmValue)]) -> std::sync::MutexGuard<'static, ()> {
        let guard = test_env_lock();
        reset_egress_policy_for_tests();
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
                .map(|value| VmValue::String(std::sync::Arc::from(*value)))
                .collect(),
        ))
    }

    #[test]
    fn exact_host_and_port_restriction() {
        let _guard = install(&[
            ("allow", strings(&["api.example.com:443"])),
            ("default", VmValue::String(std::sync::Arc::from("deny"))),
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
            ("default", VmValue::String(std::sync::Arc::from("deny"))),
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
            ("default", VmValue::String(std::sync::Arc::from("deny"))),
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
            ("default", VmValue::String(std::sync::Arc::from("deny"))),
        ]);
        let blocked = check_url("http_get", "https://blocked.example.com")
            .unwrap()
            .expect("deny wins");
        assert!(blocked.reason.contains("deny rule"));
    }

    #[test]
    fn blocked_urls_redact_sensitive_query_values() {
        let _guard = install(&[("default", VmValue::String(std::sync::Arc::from("deny")))]);
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
        let _guard = test_env_lock();
        reset_egress_policy_for_tests();
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
        reset_egress_policy_for_tests();
    }

    #[test]
    fn reset_thread_local_state_clears_required_policy_scope() {
        let _guard = test_env_lock();
        reset_egress_policy_for_tests();
        let _scope = require_explicit_egress_policy_for_host();

        crate::reset_thread_local_state();

        assert!(check_url("http_get", "https://api.example.com/status")
            .unwrap()
            .is_none());
        reset_egress_policy_for_tests();
    }

    #[test]
    fn explicit_environment_policy_satisfies_required_policy_scope() {
        let _guard = test_env_lock();
        reset_egress_policy_for_tests();
        std::env::set_var(HARN_EGRESS_ALLOW_ENV, "api.example.com");
        std::env::set_var(HARN_EGRESS_DEFAULT_ENV, "deny");
        let _scope = require_explicit_egress_policy_for_host();

        assert!(check_url("http_get", "https://api.example.com/status")
            .unwrap()
            .is_none());
        assert!(check_url("http_get", "https://other.example.com/status")
            .unwrap()
            .is_some());

        std::env::remove_var(HARN_EGRESS_ALLOW_ENV);
        std::env::remove_var(HARN_EGRESS_DEFAULT_ENV);
        reset_egress_policy_for_tests();
    }

    #[test]
    fn env_seeding_is_honored() {
        let _guard = test_env_lock();
        reset_egress_policy_for_tests();
        std::env::set_var(HARN_EGRESS_ALLOW_ENV, "");
        std::env::set_var(HARN_EGRESS_DENY_ENV, "blocked-env.example.com");
        std::env::set_var(HARN_EGRESS_DEFAULT_ENV, "allow");
        assert!(check_url("http_get", "https://env.example.com")
            .unwrap()
            .is_none());
        assert!(check_url("http_get", "https://blocked-env.example.com")
            .unwrap()
            .is_some());
        std::env::remove_var(HARN_EGRESS_ALLOW_ENV);
        std::env::remove_var(HARN_EGRESS_DENY_ENV);
        std::env::remove_var(HARN_EGRESS_DEFAULT_ENV);
        reset_egress_policy_for_tests();
    }

    // --- SSRF private-address block (literal IPs; host-name DNS is covered by
    //     GuardedResolver tests in `ssrf.rs`). ---

    /// Install an explicit `block_private:"private"` policy with default allow,
    /// so the SSRF block is the only thing that can deny.
    fn install_block_private() -> std::sync::MutexGuard<'static, ()> {
        install(&[(
            "block_private",
            VmValue::String(std::sync::Arc::from("private")),
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
                VmValue::String(std::sync::Arc::from("private")),
            ),
            ("default", VmValue::String(std::sync::Arc::from("deny"))),
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
                VmValue::String(std::sync::Arc::from("private")),
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
        let _guard = test_env_lock();
        reset_egress_policy_for_tests();
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
        reset_egress_policy_for_tests();
    }

    #[test]
    fn block_private_off_opts_out_under_run_scope() {
        let _guard = test_env_lock();
        reset_egress_policy_for_tests();
        install_test_policy(&[(
            "block_private",
            VmValue::String(std::sync::Arc::from("off")),
        )]);
        let _scope = require_ssrf_guard_for_host();
        // Explicit off overrides the default-on guard.
        assert!(check_url("http_get", "http://127.0.0.1").unwrap().is_none());
        reset_egress_policy_for_tests();
    }

    #[test]
    fn env_allow_loopback_opts_out_under_run_scope() {
        let _guard = test_env_lock();
        reset_egress_policy_for_tests();
        std::env::set_var(HARN_EGRESS_ALLOW_LOOPBACK_ENV, "1");
        let _scope = require_ssrf_guard_for_host();
        // Loopback opened by env hatch...
        assert!(check_url("http_get", "http://127.0.0.1").unwrap().is_none());
        // ...but metadata stays blocked.
        assert!(check_url("http_get", "https://169.254.169.254")
            .unwrap()
            .is_some());
        std::env::remove_var(HARN_EGRESS_ALLOW_LOOPBACK_ENV);
        reset_egress_policy_for_tests();
    }

    #[test]
    fn current_ssrf_client_settings_reflects_guard_scope() {
        let _guard = test_env_lock();
        reset_egress_policy_for_tests();
        assert_eq!(current_ssrf_client_settings(), (false, false));
        let _scope = require_ssrf_guard_for_host();
        assert_eq!(current_ssrf_client_settings(), (true, false));
        drop(_scope);
        reset_egress_policy_for_tests();
        assert_eq!(current_ssrf_client_settings(), (false, false));
    }
}
