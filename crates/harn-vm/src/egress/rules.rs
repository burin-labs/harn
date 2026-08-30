//! Egress rule and target parsing: the one place policy TEXT becomes a typed
//! rule, and a URL becomes a target those rules can be matched against.
//!
//! `mod.rs` owns policy state, installation and enforcement; it consumes the
//! parsed values here and never re-parses a rule string itself.

use super::*;

pub(crate) fn parse_ssrf_mode(raw: &str) -> Result<SsrfMode, VmError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        // `private` and `on` engage the private-address block; `off` opts out.
        "private" | "on" | "block" | "block_private" | "true" => Ok(SsrfMode::BlockPrivate),
        "off" | "false" | "none" => Ok(SsrfMode::Off),
        other => Err(vm_error(format!(
            "egress_policy: block_private must be `private`/`on` or `off`, got `{other}`"
        ))),
    }
}

pub(crate) fn parse_bool(raw: &str) -> Result<bool, VmError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" | "" => Ok(false),
        other => Err(vm_error(format!(
            "egress_policy: allow_loopback must be a boolean, got `{other}`"
        ))),
    }
}

pub(crate) fn parse_rule_values(values: &[VmValue]) -> Result<Vec<EgressRule>, VmError> {
    values
        .iter()
        .map(|value| EgressRule::parse(&value.display()))
        .collect()
}

pub(crate) fn parse_rule_list(raw: &str) -> Result<Vec<EgressRule>, VmError> {
    raw.split([',', '\n', ';'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(EgressRule::parse)
        .collect()
}

pub(crate) fn parse_default_action(raw: &str) -> Result<DefaultAction, VmError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "allow" => Ok(DefaultAction::Allow),
        "deny" => Ok(DefaultAction::Deny),
        other => Err(vm_error(format!(
            "egress_policy: default must be `allow` or `deny`, got `{other}`"
        ))),
    }
}

impl EgressRule {
    pub(crate) fn parse(raw: &str) -> Result<Self, VmError> {
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

    pub(crate) fn matches(&self, target: &EgressTarget) -> bool {
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
    pub(crate) fn is_ip_matcher(&self) -> bool {
        matches!(self.matcher, EgressMatcher::Ip(_) | EgressMatcher::Cidr(_))
    }

    /// Whether this IP/CIDR rule matches a *resolved* address for the given
    /// request port. Host/suffix rules never match here (they pin to the URL
    /// hostname, not the address it resolves to). Returns `false` for non-IP
    /// matchers so callers can blanket-iterate the rule list.
    pub(crate) fn matches_resolved_ip(&self, ip: IpAddr, request_port: Option<u16>) -> bool {
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
pub(crate) struct EgressTarget {
    pub(crate) host: String,
    pub(crate) ip: Option<IpAddr>,
    pub(crate) port: Option<u16>,
}

impl EgressTarget {
    pub(crate) fn parse(raw_url: &str) -> Result<Self, VmError> {
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

    pub(crate) fn is_loopback_host(&self) -> bool {
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

pub(crate) fn parse_rule_host_port(raw: &str) -> Result<(String, Option<u16>), VmError> {
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

pub(crate) fn split_host_port(raw: &str) -> Option<(&str, &str)> {
    let (host, port) = raw.rsplit_once(':')?;
    if host.contains(':') || port.is_empty() || !port.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((host, port))
}

pub(crate) fn parse_port(rule: &str, raw: &str) -> Result<u16, VmError> {
    raw.parse::<u16>()
        .map_err(|error| vm_error(format!("egress_policy: invalid port in `{rule}`: {error}")))
}

pub(crate) fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_end_matches('.')
        .trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase()
}
