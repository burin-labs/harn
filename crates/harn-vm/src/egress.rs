use std::collections::BTreeMap;
use std::net::IpAddr;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::{OnceLock, RwLock};

use ipnet::IpNet;
use serde_json::json;
use url::Url;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub const HARN_EGRESS_ALLOW_ENV: &str = "HARN_EGRESS_ALLOW";
pub const HARN_EGRESS_DENY_ENV: &str = "HARN_EGRESS_DENY";
pub const HARN_EGRESS_DEFAULT_ENV: &str = "HARN_EGRESS_DEFAULT";
pub const EGRESS_AUDIT_TOPIC: &str = "connectors.egress.audit";

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
    env_checked: bool,
    policy: Option<ConfiguredPolicy>,
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
            env_checked: false,
            policy: None,
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
    let Some(blocked) = check_url(surface, url)? else {
        return Ok(());
    };
    audit_blocked(&blocked).await;
    Err(blocked.to_vm_error())
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
    guard.env_checked = false;
    guard.policy = None;
}

#[cfg(test)]
pub fn reset_egress_policy_for_tests() {
    reset_egress_policy_for_host();
}

fn check_url(surface: &str, raw_url: &str) -> Result<Option<EgressBlocked>, VmError> {
    ensure_env_seeded()?;
    let configured = {
        let guard = state().read().expect("egress policy state poisoned");
        guard.policy.clone()
    };
    let Some(configured) = configured else {
        return Ok(None);
    };
    let target = EgressTarget::parse(raw_url)?;
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

fn blocked(surface: &str, url: &str, target: &EgressTarget, reason: String) -> EgressBlocked {
    EgressBlocked {
        surface: surface.to_string(),
        url: url.to_string(),
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
    if let Some(existing) = &guard.policy {
        return Err(vm_error(format!(
            "egress_policy: policy already configured from {}",
            existing.source
        )));
    }
    guard.policy = Some(ConfiguredPolicy { source, policy });
    Ok(())
}

fn ensure_env_seeded() -> Result<(), VmError> {
    {
        let guard = state().read().expect("egress policy state poisoned");
        if guard.env_checked {
            return Ok(());
        }
    }

    let allow = std::env::var(HARN_EGRESS_ALLOW_ENV).ok();
    let deny = std::env::var(HARN_EGRESS_DENY_ENV).ok();
    let default = std::env::var(HARN_EGRESS_DEFAULT_ENV).ok();
    let mut guard = state().write().expect("egress policy state poisoned");
    if guard.env_checked {
        return Ok(());
    }
    guard.env_checked = true;
    if allow.is_none() && deny.is_none() && default.is_none() {
        return Ok(());
    }
    let policy = EgressPolicy {
        allow: parse_rule_list(allow.as_deref().unwrap_or(""))?,
        deny: parse_rule_list(deny.as_deref().unwrap_or(""))?,
        default: parse_default_action(default.as_deref().unwrap_or("allow"))?,
    };
    guard.policy = Some(ConfiguredPolicy {
        source: "environment",
        policy,
    });
    Ok(())
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
    Ok(EgressPolicy {
        allow,
        deny,
        default,
    })
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
        guard.policy.clone()
    };
    let mut dict = BTreeMap::new();
    if let Some(configured) = configured {
        dict.insert("configured".to_string(), VmValue::Bool(true));
        dict.insert(
            "source".to_string(),
            VmValue::String(Rc::from(configured.source)),
        );
        dict.insert(
            "default".to_string(),
            VmValue::String(Rc::from(match configured.policy.default {
                DefaultAction::Allow => "allow",
                DefaultAction::Deny => "deny",
            })),
        );
        dict.insert(
            "allow".to_string(),
            VmValue::List(Rc::new(
                configured
                    .policy
                    .allow
                    .iter()
                    .map(|rule| VmValue::String(Rc::from(rule.raw.as_str())))
                    .collect(),
            )),
        );
        dict.insert(
            "deny".to_string(),
            VmValue::List(Rc::new(
                configured
                    .policy
                    .deny
                    .iter()
                    .map(|rule| VmValue::String(Rc::from(rule.raw.as_str())))
                    .collect(),
            )),
        );
    } else {
        dict.insert("configured".to_string(), VmValue::Bool(false));
    }
    VmValue::Dict(Rc::new(dict))
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

fn vm_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(message.into())))
}

impl EgressBlocked {
    pub(crate) fn to_vm_error(&self) -> VmError {
        let mut dict = BTreeMap::new();
        dict.insert(
            "type".to_string(),
            VmValue::String(Rc::from("EgressBlocked")),
        );
        dict.insert(
            "category".to_string(),
            VmValue::String(Rc::from("egress_blocked")),
        );
        dict.insert(
            "message".to_string(),
            VmValue::String(Rc::from(self.to_string())),
        );
        dict.insert(
            "surface".to_string(),
            VmValue::String(Rc::from(self.surface.as_str())),
        );
        dict.insert(
            "url".to_string(),
            VmValue::String(Rc::from(self.url.as_str())),
        );
        dict.insert(
            "host".to_string(),
            VmValue::String(Rc::from(self.host.as_str())),
        );
        dict.insert(
            "port".to_string(),
            self.port
                .map(|port| VmValue::Int(port as i64))
                .unwrap_or(VmValue::Nil),
        );
        dict.insert(
            "reason".to_string(),
            VmValue::String(Rc::from(self.reason.as_str())),
        );
        VmError::Thrown(VmValue::Dict(Rc::new(dict)))
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

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn install(config: &[(&str, VmValue)]) -> std::sync::MutexGuard<'static, ()> {
        let guard = ENV_LOCK.lock().unwrap();
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
        VmValue::List(Rc::new(
            values
                .iter()
                .map(|value| VmValue::String(Rc::from(*value)))
                .collect(),
        ))
    }

    #[test]
    fn exact_host_and_port_restriction() {
        let _guard = install(&[
            ("allow", strings(&["api.example.com:443"])),
            ("default", VmValue::String(Rc::from("deny"))),
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
            ("default", VmValue::String(Rc::from("deny"))),
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
            ("default", VmValue::String(Rc::from("deny"))),
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
            ("default", VmValue::String(Rc::from("deny"))),
        ]);
        let blocked = check_url("http_get", "https://blocked.example.com")
            .unwrap()
            .expect("deny wins");
        assert!(blocked.reason.contains("deny rule"));
    }

    #[test]
    fn env_seeding_is_honored() {
        let _guard = ENV_LOCK.lock().unwrap();
        reset_egress_policy_for_tests();
        std::env::set_var(HARN_EGRESS_ALLOW_ENV, "env.example.com");
        std::env::set_var(HARN_EGRESS_DENY_ENV, "");
        std::env::set_var(HARN_EGRESS_DEFAULT_ENV, "deny");
        assert!(check_url("http_get", "https://env.example.com")
            .unwrap()
            .is_none());
        assert!(check_url("http_get", "https://other.example.com")
            .unwrap()
            .is_some());
        std::env::remove_var(HARN_EGRESS_ALLOW_ENV);
        std::env::remove_var(HARN_EGRESS_DENY_ENV);
        std::env::remove_var(HARN_EGRESS_DEFAULT_ENV);
        reset_egress_policy_for_tests();
    }
}
