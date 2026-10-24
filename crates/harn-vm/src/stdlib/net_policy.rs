//! Constructor builtins for `harness.net.*` access policies
//! (issue harn#1913).
//!
//! Each `__net_policy_*` builtin produces a tagged dict that the
//! dispatcher in `crate::harness_methods` recognises via
//! [`crate::harness_net::parse`]. The Harn-side surface lives in
//! `std/net_policy` (`stdlib_net_policy.harn`) and assembles them into
//! the `NetPolicy()` namespace dict.
//!
//! The constructors only validate input shape — `NetPolicy.create(...)`
//! is what actually compiles the policy into matchers. That keeps
//! authoring deterministic (constructors never touch the network or
//! DNS) and lets the dispatcher resolve rules lazily on each
//! `harness.net.*` call.

use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::harness_net::parse::{POLICY_TAG_KEY, RULE_TAG_KEY};
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

harn_vm_core::stdlib_module_installer!(Pure, pub fn register_net_policy_builtins);

#[harn_builtin(
    exposure = "pure",
    sig = "__net_policy_domain(host: string) -> dict",
    category = "net_policy"
)]
fn net_policy_domain_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let host = require_string(args, 0, "NetPolicy.domain")?;
    Ok(rule_dict("domain", &[("host", VmValue::string(host))]))
}

#[harn_builtin(
    exposure = "pure",
    sig = "__net_policy_domain_wildcard(pattern: string) -> dict",
    category = "net_policy"
)]
fn net_policy_domain_wildcard_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let pattern = require_string(args, 0, "NetPolicy.domain_wildcard")?;
    if !pattern.starts_with("*.") {
        return Err(thrown(format!(
            "NetPolicy.domain_wildcard: pattern must start with `*.`, got `{pattern}`"
        )));
    }
    Ok(rule_dict(
        "domain_wildcard",
        &[("pattern", VmValue::string(pattern))],
    ))
}

#[harn_builtin(
    exposure = "pure",
    sig = "__net_policy_cidr(range: string) -> dict",
    category = "net_policy"
)]
fn net_policy_cidr_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let range = require_string(args, 0, "NetPolicy.cidr")?;
    // Validate eagerly so authoring errors surface at construction
    // rather than on the first denied request.
    crate::harness_net::NetPolicyRule::parse_cidr(&range)?;
    Ok(rule_dict("cidr", &[("range", VmValue::string(range))]))
}

#[harn_builtin(
    exposure = "pure",
    sig = "__net_policy_host(host: string, ports?: list) -> dict",
    category = "net_policy"
)]
fn net_policy_host_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let host = require_string(args, 0, "NetPolicy.host")?;
    let ports_value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    match &ports_value {
        VmValue::Nil => {}
        VmValue::List(list) => {
            for value in list.iter() {
                let _ = value
                    .as_int()
                    .and_then(|n| u16::try_from(n).ok())
                    .ok_or_else(|| {
                        thrown("NetPolicy.host: ports must be a list of u16 integers")
                    })?;
            }
        }
        other => {
            return Err(thrown(format!(
                "NetPolicy.host: ports must be a list of u16 integers, got {}",
                other.type_name()
            )))
        }
    }
    Ok(rule_dict(
        "host",
        &[("host", VmValue::string(host)), ("ports", ports_value)],
    ))
}

#[harn_builtin(
    exposure = "pure",
    sig = "__net_policy_create(config: dict) -> dict",
    category = "net_policy"
)]
fn net_policy_create_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config = args
        .first()
        .and_then(|v| v.as_dict())
        .ok_or_else(|| thrown("NetPolicy.create: expected a config dict"))?
        .clone();
    // Validate the shape eagerly so authoring errors surface at
    // construction time instead of on the first denied request.
    crate::harness_net::parse::policy_from_dict(&config)?;
    let mut policy = BTreeMap::new();
    policy.insert(POLICY_TAG_KEY.to_string(), VmValue::Bool(true));
    if let Some(allow) = config.get("allow") {
        policy.insert("allow".to_string(), allow.clone());
    } else {
        policy.put("allow", VmValue::list(Vec::new()));
    }
    if let Some(deny) = config.get("deny") {
        policy.insert("deny".to_string(), deny.clone());
    } else {
        policy.put("deny", VmValue::list(Vec::new()));
    }
    let default = config
        .get("default")
        .cloned()
        .unwrap_or_else(|| VmValue::string("deny"));
    policy.insert("default".to_string(), default);
    let on_violation = config
        .get("on_violation")
        .cloned()
        .unwrap_or_else(|| VmValue::string("error"));
    policy.insert("on_violation".to_string(), on_violation);
    Ok(VmValue::dict(policy))
}

fn rule_dict(kind: &'static str, fields: &[(&str, VmValue)]) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(RULE_TAG_KEY.to_string(), VmValue::string(kind));
    for (key, value) in fields {
        dict.insert((*key).to_string(), value.clone());
    }
    VmValue::dict(dict)
}

fn require_string(args: &[VmValue], index: usize, callee: &str) -> Result<String, VmError> {
    match args.get(index) {
        Some(VmValue::String(s)) => Ok(s.as_str().to_string()),
        Some(other) => Err(thrown(format!(
            "{callee}: expected string argument, got {}",
            other.type_name()
        ))),
        None => Err(thrown(format!("{callee}: expected a string argument"))),
    }
}

fn thrown(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::string(message.into()))
}
