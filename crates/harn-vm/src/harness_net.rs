//! Per-harness `NetPolicy` rules and enforcement for `harness.net.*`.
//!
//! Issue: harn#1913 / epic #1765 — Harness explicit-capability.
//!
//! This module supplies the data model and matchers; the wiring into
//! the request path lives in `crate::vm::methods::harness`. The
//! constructor builtins (`__net_policy_create`,
//! `__net_policy_domain`, …) live in `crate::stdlib::net_policy` so
//! the Harn stdlib facade in `stdlib_net_policy.harn` can expose them
//! as a single `NetPolicy()` namespace dict, mirroring the
//! `OnBudget()` pattern.
//!
//! The earlier `crate::egress` module remains the process-global
//! egress allowlist used by the connector/HTTP builtin paths. This
//! module is intentionally narrower — it only governs the
//! `harness.net.*` surface and is bound per-harness so different
//! agents in the same process can carry different policies.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;

use ipnet::IpNet;
use serde_json::json;
use url::Url;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::value::{VmClosure, VmError, VmValue};

/// Audit topic used for both deny and audit-only allow events.
pub const NET_POLICY_AUDIT_TOPIC: &str = "harness.net.policy.audit";

/// Env var that bypasses every policy on every harness. The bypass is
/// itself audited so the trust graph still records the leak.
pub const HARN_NET_POLICY_BYPASS_ENV: &str = "HARN_NET_POLICY_BYPASS";

/// One allow / deny rule.
#[derive(Clone, Debug)]
pub struct NetPolicyRule {
    pub raw: String,
    pub matcher: NetMatcher,
    pub ports: Option<Vec<u16>>,
}

#[derive(Clone, Debug)]
pub enum NetMatcher {
    /// Exact host match (case-insensitive). IDNA-normalised via `url`'s
    /// host parser.
    Host(String),
    /// `*.suffix` — subdomain wildcard. Does NOT match the bare suffix.
    Suffix(String),
    /// Literal IP address.
    Ip(IpAddr),
    /// CIDR range; matches the resolved IP of the URL host.
    Cidr(IpNet),
}

/// Default action when no allow rule matches and no deny rule fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetPolicyDefault {
    Allow,
    Deny,
}

impl NetPolicyDefault {
    pub fn as_str(self) -> &'static str {
        match self {
            NetPolicyDefault::Allow => "allow",
            NetPolicyDefault::Deny => "deny",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, VmError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allow" => Ok(NetPolicyDefault::Allow),
            "" | "deny" => Ok(NetPolicyDefault::Deny),
            other => Err(vm_error(format!(
                "NetPolicy.create: default must be `allow` or `deny`, got `{other}`"
            ))),
        }
    }
}

/// Action taken when the request is denied (either matched a deny
/// rule or fell through to a `default: deny`).
#[derive(Clone, Debug)]
pub enum OnViolation {
    /// Throw a typed `NetPolicyViolation` error.
    Error,
    /// Allow the request but record an audit entry tagged
    /// `outcome: "audit_only"`.
    AuditOnly,
    /// Deny the request, record an audit entry tagged
    /// `outcome: "quarantine"`, and mark the agent as quarantined via
    /// an audit signal downstream consumers can pin on.
    Quarantine,
    /// Custom callback: `fn(req) -> "error" | "audit_only" |
    /// "quarantine"`. The callback closure is invoked with a request
    /// dict and the string outcome decides the next step.
    Callback(Rc<VmClosure>),
}

impl OnViolation {
    pub fn parse_str(raw: &str) -> Result<Self, VmError> {
        match raw.trim() {
            "error" => Ok(OnViolation::Error),
            "audit_only" => Ok(OnViolation::AuditOnly),
            "quarantine" => Ok(OnViolation::Quarantine),
            other => Err(vm_error(format!(
                "NetPolicy.create: on_violation must be one of `error`, `audit_only`, `quarantine`, or a callback, got `{other}`"
            ))),
        }
    }
}

/// Compiled policy attached to a `Harness`.
#[derive(Clone, Debug)]
pub struct NetPolicy {
    pub allow: Arc<Vec<NetPolicyRule>>,
    pub deny: Arc<Vec<NetPolicyRule>>,
    pub default: NetPolicyDefault,
    pub on_violation: OnViolation,
}

/// Outcome of `NetPolicy::evaluate` plus any host bookkeeping the
/// dispatcher should apply.
#[derive(Clone, Debug)]
pub enum NetPolicyDecision {
    /// Allow the request. If `audited` is true, record an
    /// `outcome: "audit_only"` entry for the trust graph.
    Allow {
        audited: bool,
        audit: Option<NetPolicyAudit>,
    },
    /// Deny the request. The caller raises the typed error and the
    /// dispatcher emits the audit.
    Deny {
        audit: NetPolicyAudit,
        quarantine: bool,
    },
}

/// Audit payload shared by both deny events and audit-only allows.
#[derive(Clone, Debug)]
pub struct NetPolicyAudit {
    pub method: String,
    pub url: String,
    pub host: String,
    pub port: Option<u16>,
    pub reason: String,
    pub outcome: &'static str,
    pub bypass: bool,
    pub matched_rule: Option<String>,
}

impl NetPolicyAudit {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "method": self.method,
            "url": self.url,
            "host": self.host,
            "port": self.port,
            "reason": self.reason,
            "outcome": self.outcome,
            "bypass": self.bypass,
            "matched_rule": self.matched_rule,
        })
    }
}

#[derive(Clone, Debug)]
pub struct NetTarget {
    pub host: String,
    pub ip: Option<IpAddr>,
    pub port: Option<u16>,
}

impl NetTarget {
    pub fn parse(raw_url: &str) -> Result<Self, VmError> {
        let parsed = Url::parse(raw_url)
            .map_err(|error| vm_error(format!("harness.net: invalid URL `{raw_url}`: {error}")))?;
        let host = parsed.host_str().ok_or_else(|| {
            vm_error(format!(
                "harness.net: URL `{raw_url}` does not include a host"
            ))
        })?;
        let host = normalize_host(host);
        let ip = IpAddr::from_str(&host).ok();
        Ok(Self {
            host,
            ip,
            port: parsed.port_or_known_default(),
        })
    }
}

impl NetPolicyRule {
    pub fn parse_host(raw: &str, ports: Option<Vec<u16>>) -> Result<Self, VmError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(vm_error("NetPolicy.host: empty host"));
        }
        let host = normalize_host(raw);
        let matcher = if let Some(suffix) = host.strip_prefix("*.") {
            if suffix.is_empty() {
                return Err(vm_error(format!(
                    "NetPolicy.domain_wildcard: invalid wildcard `{raw}`"
                )));
            }
            NetMatcher::Suffix(suffix.to_string())
        } else if let Ok(ip) = IpAddr::from_str(&host) {
            NetMatcher::Ip(ip)
        } else {
            NetMatcher::Host(host)
        };
        Ok(Self {
            raw: raw.to_string(),
            matcher,
            ports,
        })
    }

    pub fn parse_domain(raw: &str) -> Result<Self, VmError> {
        Self::parse_host(raw, None)
    }

    pub fn parse_domain_wildcard(raw: &str) -> Result<Self, VmError> {
        let trimmed = raw.trim();
        if !trimmed.starts_with("*.") {
            return Err(vm_error(format!(
                "NetPolicy.domain_wildcard: pattern must start with `*.`, got `{raw}`"
            )));
        }
        Self::parse_host(trimmed, None)
    }

    pub fn parse_cidr(raw: &str) -> Result<Self, VmError> {
        let trimmed = raw.trim();
        let net = IpNet::from_str(trimmed)
            .map_err(|error| vm_error(format!("NetPolicy.cidr: invalid CIDR `{raw}`: {error}")))?;
        Ok(Self {
            raw: trimmed.to_string(),
            matcher: NetMatcher::Cidr(net),
            ports: None,
        })
    }

    pub fn matches(&self, target: &NetTarget) -> bool {
        if let Some(ports) = &self.ports {
            match target.port {
                Some(port) if ports.contains(&port) => {}
                _ => return false,
            }
        }
        match &self.matcher {
            NetMatcher::Host(host) => target.host == *host,
            NetMatcher::Suffix(suffix) => {
                target.host.len() > suffix.len()
                    && target.host.ends_with(suffix)
                    && target
                        .host
                        .as_bytes()
                        .get(target.host.len() - suffix.len() - 1)
                        == Some(&b'.')
            }
            NetMatcher::Ip(ip) => target.ip == Some(*ip),
            NetMatcher::Cidr(net) => target.ip.is_some_and(|ip| net.contains(&ip)),
        }
    }
}

impl NetPolicy {
    /// Resolve the decision for a single request. The caller decides
    /// what to do with the audit + quarantine signal — see the
    /// dispatcher in `vm::methods::harness`.
    pub fn evaluate(&self, method: &str, raw_url: &str) -> Result<NetPolicyDecision, VmError> {
        let target = NetTarget::parse(raw_url)?;
        if let Some(rule) = self.deny.iter().find(|rule| rule.matches(&target)) {
            return Ok(self.deny_decision(
                method,
                raw_url,
                &target,
                format!("matched deny rule `{}`", rule.raw),
                Some(rule.raw.clone()),
            ));
        }
        if let Some(rule) = self.allow.iter().find(|rule| rule.matches(&target)) {
            return Ok(NetPolicyDecision::Allow {
                audited: false,
                audit: Some(NetPolicyAudit {
                    method: method.to_string(),
                    url: raw_url.to_string(),
                    host: target.host,
                    port: target.port,
                    reason: format!("matched allow rule `{}`", rule.raw),
                    outcome: "allow",
                    bypass: false,
                    matched_rule: Some(rule.raw.clone()),
                }),
            });
        }
        if self.default == NetPolicyDefault::Allow {
            return Ok(NetPolicyDecision::Allow {
                audited: false,
                audit: None,
            });
        }
        Ok(self.deny_decision(
            method,
            raw_url,
            &target,
            "no allow rule matched (default deny)".to_string(),
            None,
        ))
    }

    fn deny_decision(
        &self,
        method: &str,
        raw_url: &str,
        target: &NetTarget,
        reason: String,
        matched_rule: Option<String>,
    ) -> NetPolicyDecision {
        match &self.on_violation {
            OnViolation::Error => NetPolicyDecision::Deny {
                audit: NetPolicyAudit {
                    method: method.to_string(),
                    url: raw_url.to_string(),
                    host: target.host.clone(),
                    port: target.port,
                    reason,
                    outcome: "error",
                    bypass: false,
                    matched_rule,
                },
                quarantine: false,
            },
            OnViolation::AuditOnly => NetPolicyDecision::Allow {
                audited: true,
                audit: Some(NetPolicyAudit {
                    method: method.to_string(),
                    url: raw_url.to_string(),
                    host: target.host.clone(),
                    port: target.port,
                    reason,
                    outcome: "audit_only",
                    bypass: false,
                    matched_rule,
                }),
            },
            OnViolation::Quarantine => NetPolicyDecision::Deny {
                audit: NetPolicyAudit {
                    method: method.to_string(),
                    url: raw_url.to_string(),
                    host: target.host.clone(),
                    port: target.port,
                    reason,
                    outcome: "quarantine",
                    bypass: false,
                    matched_rule,
                },
                quarantine: true,
            },
            // Callback resolution happens in the dispatcher because it
            // needs the VM to invoke the closure. The default deny
            // shape carries the audit; the dispatcher overrides
            // `outcome` after the callback returns.
            OnViolation::Callback(_) => NetPolicyDecision::Deny {
                audit: NetPolicyAudit {
                    method: method.to_string(),
                    url: raw_url.to_string(),
                    host: target.host.clone(),
                    port: target.port,
                    reason,
                    outcome: "callback",
                    bypass: false,
                    matched_rule,
                },
                quarantine: false,
            },
        }
    }
}

/// Construct the typed VM error returned to callers when a request is
/// denied. Mirrors the shape of `crate::egress::EgressBlocked` so
/// hosts can route on either consistently.
pub fn violation_vm_error(audit: &NetPolicyAudit) -> VmError {
    let mut dict = BTreeMap::new();
    dict.insert(
        "type".to_string(),
        VmValue::String(Rc::from("NetPolicyViolation")),
    );
    dict.insert(
        "category".to_string(),
        VmValue::String(Rc::from("net_policy_violation")),
    );
    dict.insert(
        "message".to_string(),
        VmValue::String(Rc::from(format!(
            "harness.net.{} blocked {}: {}",
            audit.method, audit.url, audit.reason
        ))),
    );
    dict.insert(
        "method".to_string(),
        VmValue::String(Rc::from(audit.method.as_str())),
    );
    dict.insert(
        "url".to_string(),
        VmValue::String(Rc::from(audit.url.as_str())),
    );
    dict.insert(
        "host".to_string(),
        VmValue::String(Rc::from(audit.host.as_str())),
    );
    dict.insert(
        "port".to_string(),
        audit
            .port
            .map(|port| VmValue::Int(port as i64))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "reason".to_string(),
        VmValue::String(Rc::from(audit.reason.as_str())),
    );
    dict.insert(
        "outcome".to_string(),
        VmValue::String(Rc::from(audit.outcome)),
    );
    dict.insert(
        "matched_rule".to_string(),
        audit
            .matched_rule
            .as_deref()
            .map(|raw| VmValue::String(Rc::from(raw)))
            .unwrap_or(VmValue::Nil),
    );
    if audit.bypass {
        dict.insert("bypass".to_string(), VmValue::Bool(true));
    }
    VmError::Thrown(VmValue::Dict(Rc::new(dict)))
}

/// Build the request envelope handed to the user `on_violation`
/// callback. Plain dict so the script can index with the usual
/// optional-chaining and `?.` syntax.
pub fn violation_request_value(audit: &NetPolicyAudit) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "method".to_string(),
        VmValue::String(Rc::from(audit.method.as_str())),
    );
    dict.insert(
        "url".to_string(),
        VmValue::String(Rc::from(audit.url.as_str())),
    );
    dict.insert(
        "host".to_string(),
        VmValue::String(Rc::from(audit.host.as_str())),
    );
    dict.insert(
        "port".to_string(),
        audit
            .port
            .map(|port| VmValue::Int(port as i64))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "reason".to_string(),
        VmValue::String(Rc::from(audit.reason.as_str())),
    );
    dict.insert(
        "matched_rule".to_string(),
        audit
            .matched_rule
            .as_deref()
            .map(|raw| VmValue::String(Rc::from(raw)))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(dict))
}

/// Emit a `harness.net.policy.audit` event to the active event log,
/// if any. Returns silently when no event log is bound so unit tests
/// that bypass the full runtime still exercise the matcher.
pub async fn record_audit(audit: &NetPolicyAudit) {
    let Some(log) = active_event_log() else {
        return;
    };
    let Ok(topic) = Topic::new(NET_POLICY_AUDIT_TOPIC) else {
        return;
    };
    let _ = log
        .append(
            &topic,
            LogEvent::new("net.policy.evaluated", audit.to_json()),
        )
        .await;
}

/// Returns true when the bypass env var is set to a truthy value. The
/// dispatcher still records the bypass with `bypass: true` so the
/// audit trail keeps a record of the leak.
pub fn bypass_enabled() -> bool {
    match std::env::var(HARN_NET_POLICY_BYPASS_ENV) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
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

/// VM-side helpers used by both the constructor builtins
/// (`crate::stdlib::net_policy`) and the dispatcher when it accepts
/// a `with_net_policy({...})` shorthand dict.
pub mod parse {
    use super::*;

    /// Sentinel key used to recognise a tagged-dict policy rule.
    pub const RULE_TAG_KEY: &str = "__net_policy_rule";
    /// Sentinel key used to recognise a tagged-dict policy value.
    pub const POLICY_TAG_KEY: &str = "__net_policy";

    /// Inspect a `VmValue` and lift it into a `NetPolicyRule`. Accepts
    /// the tagged-dict shape produced by the constructor builtins as
    /// well as bare strings interpreted as `domain` (or
    /// `domain_wildcard` when they start with `*.`).
    pub fn rule_from_vm(value: &VmValue) -> Result<NetPolicyRule, VmError> {
        match value {
            VmValue::Dict(dict) => rule_from_dict(dict),
            VmValue::String(raw) => {
                let raw = raw.as_ref();
                if raw.starts_with("*.") {
                    NetPolicyRule::parse_domain_wildcard(raw)
                } else if raw.contains('/') {
                    NetPolicyRule::parse_cidr(raw)
                } else {
                    NetPolicyRule::parse_domain(raw)
                }
            }
            other => Err(vm_error(format!(
                "NetPolicy: rule must be a tagged dict or string, got {}",
                other.type_name()
            ))),
        }
    }

    fn rule_from_dict(dict: &BTreeMap<String, VmValue>) -> Result<NetPolicyRule, VmError> {
        let tag = dict
            .get(RULE_TAG_KEY)
            .and_then(|v| match v {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .ok_or_else(|| {
                vm_error(
                    "NetPolicy: rule dict is missing the `__net_policy_rule` tag; build rules via NetPolicy.domain/.domain_wildcard/.cidr/.host",
                )
            })?;
        match tag.as_str() {
            "domain" => {
                let host = require_string(dict, "host", "NetPolicy.domain")?;
                NetPolicyRule::parse_domain(&host)
            }
            "domain_wildcard" => {
                let pattern = require_string(dict, "pattern", "NetPolicy.domain_wildcard")?;
                NetPolicyRule::parse_domain_wildcard(&pattern)
            }
            "cidr" => {
                let range = require_string(dict, "range", "NetPolicy.cidr")?;
                NetPolicyRule::parse_cidr(&range)
            }
            "host" => {
                let host = require_string(dict, "host", "NetPolicy.host")?;
                let ports = match dict.get("ports") {
                    Some(VmValue::List(list)) => {
                        let mut parsed = Vec::with_capacity(list.len());
                        for value in list.iter() {
                            let port = value
                                .as_int()
                                .and_then(|n| u16::try_from(n).ok())
                                .ok_or_else(|| {
                                    vm_error("NetPolicy.host: ports must be a list of u16 integers")
                                })?;
                            parsed.push(port);
                        }
                        Some(parsed)
                    }
                    Some(VmValue::Nil) | None => None,
                    Some(_) => {
                        return Err(vm_error(
                            "NetPolicy.host: ports must be a list of u16 integers",
                        ))
                    }
                };
                NetPolicyRule::parse_host(&host, ports)
            }
            other => Err(vm_error(format!("NetPolicy: unknown rule kind `{other}`"))),
        }
    }

    fn require_string(
        dict: &BTreeMap<String, VmValue>,
        key: &str,
        callee: &str,
    ) -> Result<String, VmError> {
        match dict.get(key) {
            Some(VmValue::String(s)) => Ok(s.as_ref().to_string()),
            Some(other) => Err(vm_error(format!(
                "{callee}: `{key}` must be a string, got {}",
                other.type_name()
            ))),
            None => Err(vm_error(format!("{callee}: missing `{key}` field"))),
        }
    }

    /// Build a `NetPolicy` value from the `{allow, deny, default,
    /// on_violation}` dict produced by `NetPolicy.create(...)`.
    pub fn policy_from_dict(dict: &BTreeMap<String, VmValue>) -> Result<NetPolicy, VmError> {
        let allow = parse_rule_list(dict.get("allow"), "allow")?;
        let deny = parse_rule_list(dict.get("deny"), "deny")?;
        let default = match dict.get("default") {
            Some(VmValue::String(s)) => NetPolicyDefault::parse(s.as_ref())?,
            Some(VmValue::Nil) | None => NetPolicyDefault::Deny,
            Some(other) => {
                return Err(vm_error(format!(
                    "NetPolicy.create: default must be a string, got {}",
                    other.type_name()
                )))
            }
        };
        let on_violation = match dict.get("on_violation") {
            Some(VmValue::String(s)) => OnViolation::parse_str(s.as_ref())?,
            Some(VmValue::Closure(closure)) => OnViolation::Callback(Rc::clone(closure)),
            Some(VmValue::Nil) | None => OnViolation::Error,
            Some(other) => {
                return Err(vm_error(format!(
                    "NetPolicy.create: on_violation must be a string or callback, got {}",
                    other.type_name()
                )))
            }
        };
        Ok(NetPolicy {
            allow: Arc::new(allow),
            deny: Arc::new(deny),
            default,
            on_violation,
        })
    }

    fn parse_rule_list(value: Option<&VmValue>, side: &str) -> Result<Vec<NetPolicyRule>, VmError> {
        match value {
            None | Some(VmValue::Nil) => Ok(Vec::new()),
            Some(VmValue::List(items)) => items.iter().map(rule_from_vm).collect(),
            Some(other) => Err(vm_error(format!(
                "NetPolicy.create: `{side}` must be a list, got {}",
                other.type_name()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(raw: &str, ports: Option<Vec<u16>>) -> NetPolicyRule {
        NetPolicyRule::parse_host(raw, ports).expect("rule parses")
    }

    fn cidr(raw: &str) -> NetPolicyRule {
        NetPolicyRule::parse_cidr(raw).expect("cidr parses")
    }

    fn build(
        allow: Vec<NetPolicyRule>,
        deny: Vec<NetPolicyRule>,
        default: NetPolicyDefault,
    ) -> NetPolicy {
        NetPolicy {
            allow: Arc::new(allow),
            deny: Arc::new(deny),
            default,
            on_violation: OnViolation::Error,
        }
    }

    #[test]
    fn exact_host_match_allows() {
        let policy = build(
            vec![rule("github.com", None)],
            Vec::new(),
            NetPolicyDefault::Deny,
        );
        let decision = policy
            .evaluate("get", "https://github.com/foo")
            .expect("evaluates");
        assert!(matches!(decision, NetPolicyDecision::Allow { .. }));
    }

    #[test]
    fn wildcard_does_not_match_bare_apex() {
        let policy = build(
            vec![NetPolicyRule::parse_domain_wildcard("*.github.com").unwrap()],
            Vec::new(),
            NetPolicyDefault::Deny,
        );
        let allow = policy.evaluate("get", "https://api.github.com/x").unwrap();
        assert!(matches!(allow, NetPolicyDecision::Allow { .. }));
        let deny = policy.evaluate("get", "https://github.com/x").unwrap();
        assert!(matches!(deny, NetPolicyDecision::Deny { .. }));
    }

    #[test]
    fn cidr_matches_ip_literal() {
        let policy = build(vec![cidr("10.0.0.0/8")], Vec::new(), NetPolicyDefault::Deny);
        let allowed = policy.evaluate("get", "http://10.5.5.5/x").unwrap();
        assert!(matches!(allowed, NetPolicyDecision::Allow { .. }));
        let denied = policy.evaluate("get", "http://192.168.1.1/x").unwrap();
        assert!(matches!(denied, NetPolicyDecision::Deny { .. }));
    }

    #[test]
    fn host_port_rule_requires_matching_port() {
        let policy = build(
            vec![rule("api.anthropic.com", Some(vec![443]))],
            Vec::new(),
            NetPolicyDefault::Deny,
        );
        let allow = policy
            .evaluate("get", "https://api.anthropic.com/v1/messages")
            .unwrap();
        assert!(matches!(allow, NetPolicyDecision::Allow { .. }));
        let deny = policy
            .evaluate("get", "http://api.anthropic.com/v1/messages")
            .unwrap();
        assert!(matches!(deny, NetPolicyDecision::Deny { .. }));
    }

    #[test]
    fn deny_overrides_allow() {
        let policy = build(
            vec![NetPolicyRule::parse_domain_wildcard("*.github.com").unwrap()],
            vec![rule("evil.github.com", None)],
            NetPolicyDefault::Deny,
        );
        let decision = policy.evaluate("get", "https://evil.github.com/x").unwrap();
        match decision {
            NetPolicyDecision::Deny { audit, .. } => {
                assert!(audit.reason.contains("deny rule"));
            }
            other => panic!("expected deny, got {other:?}"),
        }
    }

    #[test]
    fn default_allow_lets_unmatched_through() {
        let policy = build(Vec::new(), Vec::new(), NetPolicyDefault::Allow);
        let allow = policy.evaluate("get", "https://example.test/x").unwrap();
        assert!(matches!(allow, NetPolicyDecision::Allow { .. }));
    }

    #[test]
    fn audit_only_allows_but_carries_audit() {
        let mut policy = build(Vec::new(), Vec::new(), NetPolicyDefault::Deny);
        policy.on_violation = OnViolation::AuditOnly;
        let decision = policy
            .evaluate("get", "https://blocked.test/x")
            .expect("evaluates");
        match decision {
            NetPolicyDecision::Allow { audited, audit } => {
                assert!(audited);
                let audit = audit.expect("audit attached");
                assert_eq!(audit.outcome, "audit_only");
                assert_eq!(audit.host, "blocked.test");
            }
            other => panic!("expected audit_only allow, got {other:?}"),
        }
    }

    #[test]
    fn quarantine_denies_with_signal() {
        let mut policy = build(Vec::new(), Vec::new(), NetPolicyDefault::Deny);
        policy.on_violation = OnViolation::Quarantine;
        match policy
            .evaluate("get", "https://blocked.test/x")
            .expect("evaluates")
        {
            NetPolicyDecision::Deny { audit, quarantine } => {
                assert!(quarantine);
                assert_eq!(audit.outcome, "quarantine");
            }
            other => panic!("expected quarantine deny, got {other:?}"),
        }
    }

    #[test]
    fn invalid_url_surfaces_typed_error() {
        let policy = build(Vec::new(), Vec::new(), NetPolicyDefault::Deny);
        let err = policy.evaluate("get", "not a url").unwrap_err();
        match err {
            VmError::Thrown(VmValue::String(s)) => {
                assert!(s.contains("invalid URL"), "unexpected error: {s}");
            }
            other => panic!("expected Thrown, got {other:?}"),
        }
    }

    #[test]
    fn parse_string_rule_branches_on_shape() {
        let domain = parse::rule_from_vm(&VmValue::String(Rc::from("github.com"))).unwrap();
        assert!(matches!(domain.matcher, NetMatcher::Host(_)));
        let wildcard = parse::rule_from_vm(&VmValue::String(Rc::from("*.github.com"))).unwrap();
        assert!(matches!(wildcard.matcher, NetMatcher::Suffix(_)));
        let cidr_rule = parse::rule_from_vm(&VmValue::String(Rc::from("10.0.0.0/8"))).unwrap();
        assert!(matches!(cidr_rule.matcher, NetMatcher::Cidr(_)));
    }

    #[test]
    fn bypass_env_recognised() {
        let original = std::env::var(HARN_NET_POLICY_BYPASS_ENV).ok();
        std::env::set_var(HARN_NET_POLICY_BYPASS_ENV, "1");
        assert!(bypass_enabled());
        std::env::set_var(HARN_NET_POLICY_BYPASS_ENV, "0");
        assert!(!bypass_enabled());
        match original {
            Some(value) => std::env::set_var(HARN_NET_POLICY_BYPASS_ENV, value),
            None => std::env::remove_var(HARN_NET_POLICY_BYPASS_ENV),
        }
    }
}
