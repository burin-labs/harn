//! Tool-hook catalogue primitive (epic #1884).
//!
//! Exposes the foundational schema for "command faux-pas" rules: a
//! [`ToolRule`] declares a single matchable pattern + rewrite/explanation, a
//! [`Catalogue`] bundles rules with provenance metadata, and a
//! [`ToolHooksRegistry`] composes catalogues so downstream tickets
//! (TH-04 seed catalogues, TH-05 LLM classifier, etc.) can layer on
//! without re-defining the data model.
//!
//! Ticket coverage:
//! * TH-01 (#1894): [`ToolRule`] + [`Catalogue`] schema, registry
//!   register/unregister/list, linear `tool_hooks_match` sweep.
//! * TH-02 (#1895): `tool_hooks_filter` catalogue-stack pre-filter helper
//!   consumed by the `preset_run_command` Harn facade in
//!   `crates/harn-stdlib/src/stdlib/stdlib_tool_hooks.harn`.
//! * TH-03 (#1896): `tool_hooks_emit_audit` + `tool_hooks_inject_reminder`
//!   side-effect helpers used by the `tool_hooks_mode_*` callbacks. The
//!   audit helper routes through `record_lifecycle_audit` so existing
//!   `lifecycle_audit_log_take` / `pipeline_lifecycle_audit_log_*`
//!   surfaces observe the entry; the reminder helper builds a typed
//!   [`SystemReminder`], injects it into the active agent session when
//!   one exists, and records a `tool_hooks.reminder_injected` audit
//!   entry regardless so conformance and replay can verify the
//!   side-effect even from headless pipelines.
//! * TH-05 (#1898): `__tool_hooks_classifier_cache_get` /
//!   `__tool_hooks_classifier_cache_put` thread-local cache helpers
//!   backing the opt-in LLM classifier in `preset_run_command`. Keyed by
//!   normalized-command hash + classifier scope id so independent
//!   wrappers don't share verdicts; entries optionally expire after
//!   `ttl_seconds` so callers can refresh long-running daemons without
//!   restarting the process.
//!
//! Schema follows the established tagged-dict convention (`_type`) used by
//! `tool_registry` and `skill_registry`. Values are plain Harn dicts so they
//! serde-roundtrip through `json_stringify` / `json_parse` with no special
//! handling, and the matching engine runs as a single linear sweep over
//! catalogues × rules (TH-01 acceptance criterion).

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;

use regex::Regex;

use crate::stdlib::args::{ErrorKind, Options};
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub const TOOL_RULE_TYPE: &str = "tool_rule";
pub const CATALOGUE_TYPE: &str = "catalogue";
pub const REGISTRY_TYPE: &str = "tool_hooks_registry";

const VALID_SEVERITIES: &[&str] = &["error", "warning", "info"];

fn err(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message.into())))
}

fn require_dict<'a>(
    value: &'a VmValue,
    builtin: &str,
    role: &str,
) -> Result<&'a crate::value::DictMap, VmError> {
    match value {
        VmValue::Dict(d) => Ok(d.as_ref()),
        other => Err(err(format!(
            "{builtin}: {role} must be a dict, got {}",
            other.type_name()
        ))),
    }
}

fn require_tagged<'a>(
    value: &'a VmValue,
    expected: &str,
    builtin: &str,
    role: &str,
) -> Result<&'a crate::value::DictMap, VmError> {
    let dict = require_dict(value, builtin, role)?;
    match dict.get("_type") {
        Some(VmValue::String(t)) if t.as_str() == expected => Ok(dict),
        Some(VmValue::String(t)) => Err(err(format!(
            "{builtin}: {role} must be a {expected} (created with the matching constructor), got {t}"
        ))),
        _ => Err(err(format!(
            "{builtin}: {role} must be a {expected} (created with the matching constructor)"
        ))),
    }
}

/// Hook payloads arrive as tagged dicts, so their fields are read with the
/// shared option contract rather than a per-field match.
fn fields<'a>(dict: &'a crate::value::DictMap, builtin: &'a str) -> Options<'a, 'a> {
    Options::new(builtin, ErrorKind::Thrown, Some(dict))
}

fn required_string_field(
    dict: &crate::value::DictMap,
    key: &'static str,
    builtin: &str,
) -> Result<String, VmError> {
    fields(dict, builtin)
        .non_empty_string(key)
        .map(str::to_string)
}

fn optional_string_field(
    dict: &crate::value::DictMap,
    key: &'static str,
    builtin: &str,
) -> Result<Option<String>, VmError> {
    Ok(fields(dict, builtin).opt_string(key)?.map(str::to_string))
}

fn optional_int_field(
    dict: &crate::value::DictMap,
    key: &'static str,
    builtin: &str,
) -> Result<Option<i64>, VmError> {
    fields(dict, builtin).opt_int(key)
}

fn optional_string_list(
    dict: &crate::value::DictMap,
    key: &'static str,
    builtin: &str,
) -> Result<Vec<String>, VmError> {
    Ok(fields(dict, builtin)
        .opt_string_list(key)?
        .unwrap_or_default()
        .into_iter()
        .map(str::to_string)
        .collect())
}

fn validate_pattern(value: &VmValue, builtin: &str) -> Result<(), VmError> {
    match value {
        VmValue::String(s) => {
            Regex::new(s)
                .map_err(|e| err(format!("{builtin}: invalid regex in `pattern`: {e}")))?;
            Ok(())
        }
        _ if Vm::is_callable_value(value) => Ok(()),
        other => Err(err(format!(
            "{builtin}: `pattern` must be a regex string or a callable predicate, got {}",
            other.type_name()
        ))),
    }
}

fn validate_severity(value: Option<&VmValue>, builtin: &str) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(s)) => {
            if VALID_SEVERITIES.iter().any(|valid| *valid == s.as_str()) {
                Ok(s.to_string())
            } else {
                Err(err(format!(
                    "{builtin}: `severity` must be one of {VALID_SEVERITIES:?}, got {s:?}"
                )))
            }
        }
        Some(VmValue::Nil) | None => Ok("warning".to_string()),
        Some(other) => Err(err(format!(
            "{builtin}: `severity` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn validate_rule_dict(
    config: &crate::value::DictMap,
    builtin: &str,
) -> Result<crate::value::DictMap, VmError> {
    let id = required_string_field(config, "id", builtin)?;
    let pattern = config
        .get("pattern")
        .ok_or_else(|| err(format!("{builtin}: missing required field `pattern`")))?;
    validate_pattern(pattern, builtin)?;

    // `applies_to` defaults to an empty list (matches every stack).
    let applies_to = optional_string_list(config, "applies_to", builtin)?;
    let severity = validate_severity(config.get("severity"), builtin)?;

    if let Some(value) = config.get("rewrite") {
        if !matches!(value, VmValue::Nil) && !Vm::is_callable_value(value) {
            return Err(err(format!(
                "{builtin}: `rewrite` must be a callable or nil, got {}",
                value.type_name()
            )));
        }
    }

    // Light-touch shape checks for the remaining fields; they pass through
    // unchanged so downstream tickets can attach extra metadata without
    // schema churn.
    let _ = optional_string_field(config, "explanation", builtin)?;
    let _ = optional_string_list(config, "references", builtin)?;
    let _ = optional_int_field(config, "priority", builtin)?;

    let mut rule = crate::value::DictMap::new();
    rule.put_str("_type", TOOL_RULE_TYPE);
    rule.put_str("id", id.as_str());
    rule.insert(crate::value::intern_key("pattern"), pattern.clone());
    rule.insert(
        crate::value::intern_key("applies_to"),
        VmValue::List(std::sync::Arc::new(
            applies_to
                .into_iter()
                .map(|stack| VmValue::String(arcstr::ArcStr::from(stack.as_str())))
                .collect(),
        )),
    );
    rule.put_str("severity", severity.as_str());
    rule.insert(
        crate::value::intern_key("rewrite"),
        config.get("rewrite").cloned().unwrap_or(VmValue::Nil),
    );
    rule.insert(
        crate::value::intern_key("explanation"),
        config
            .get("explanation")
            .cloned()
            .unwrap_or_else(|| VmValue::String(arcstr::ArcStr::from(""))),
    );
    rule.insert(
        crate::value::intern_key("references"),
        config
            .get("references")
            .cloned()
            .unwrap_or_else(|| VmValue::List(std::sync::Arc::new(Vec::new()))),
    );
    rule.insert(
        crate::value::intern_key("priority"),
        config.get("priority").cloned().unwrap_or(VmValue::Int(0)),
    );
    Ok(rule)
}

fn extract_rules(raw_rules: Option<&VmValue>, builtin: &str) -> Result<Vec<VmValue>, VmError> {
    let Some(value) = raw_rules else {
        return Ok(Vec::new());
    };
    match value {
        VmValue::List(items) => {
            let mut seen_ids = std::collections::BTreeMap::new();
            let mut rules = Vec::with_capacity(items.len());
            for (idx, entry) in items.iter().enumerate() {
                // Allow either a tagged tool_rule (already validated) or a
                // raw config dict (validate inline).
                let rule_dict = match entry {
                    VmValue::Dict(d)
                        if matches!(
                            d.get("_type"),
                            Some(VmValue::String(t)) if t.as_str() == TOOL_RULE_TYPE
                        ) =>
                    {
                        d.as_ref().clone()
                    }
                    VmValue::Dict(d) => validate_rule_dict(d.as_ref(), builtin)?,
                    other => {
                        return Err(err(format!(
                            "{builtin}: rule at index {idx} must be a tool_rule dict, got {}",
                            other.type_name()
                        )));
                    }
                };
                let id = rule_dict
                    .get("id")
                    .and_then(|v| match v {
                        VmValue::String(s) => Some(s.to_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                if let Some(prev) = seen_ids.insert(id.clone(), idx) {
                    return Err(err(format!(
                        "{builtin}: duplicate rule id `{id}` at indices {prev} and {idx}"
                    )));
                }
                rules.push(VmValue::dict(rule_dict));
            }
            Ok(rules)
        }
        VmValue::Nil => Ok(Vec::new()),
        other => Err(err(format!(
            "{builtin}: `rules` must be a list, got {}",
            other.type_name()
        ))),
    }
}

fn build_catalogue(config: &crate::value::DictMap) -> Result<VmValue, VmError> {
    let builtin = "catalogue";
    let id = required_string_field(config, "id", builtin)?;
    let stack = optional_string_field(config, "stack", builtin)?.unwrap_or_default();
    let version = optional_string_field(config, "version", builtin)?.unwrap_or_default();
    let source = optional_string_field(config, "source", builtin)?.unwrap_or_default();
    let priority = optional_int_field(config, "priority", builtin)?.unwrap_or(0);
    let rules = extract_rules(config.get("rules"), builtin)?;

    let mut catalogue = crate::value::DictMap::new();
    catalogue.put_str("_type", CATALOGUE_TYPE);
    catalogue.put_str("id", id.as_str());
    catalogue.put_str("stack", stack.as_str());
    catalogue.put_str("version", version.as_str());
    catalogue.put_str("source", source.as_str());
    catalogue.insert(crate::value::intern_key("priority"), VmValue::Int(priority));
    catalogue.insert(
        crate::value::intern_key("rules"),
        VmValue::List(std::sync::Arc::new(rules)),
    );
    Ok(VmValue::dict(catalogue))
}

fn registry_catalogues(registry: &crate::value::DictMap) -> &[VmValue] {
    match registry.get("catalogues") {
        Some(VmValue::List(list)) => list,
        _ => &[],
    }
}

fn rule_id(rule: &crate::value::DictMap) -> String {
    rule.get("id")
        .and_then(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn rule_priority(rule: &crate::value::DictMap) -> i64 {
    match rule.get("priority") {
        Some(VmValue::Int(n)) => *n,
        _ => 0,
    }
}

fn rule_applies_to(rule: &crate::value::DictMap) -> Vec<String> {
    match rule.get("applies_to") {
        Some(VmValue::List(items)) => items
            .iter()
            .filter_map(|v| match v {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn rule_severity(rule: &crate::value::DictMap) -> String {
    match rule.get("severity") {
        Some(VmValue::String(s)) => s.to_string(),
        _ => "warning".to_string(),
    }
}

fn context_stacks(context: Option<&VmValue>) -> Vec<String> {
    let Some(value) = context else {
        return Vec::new();
    };
    match value {
        VmValue::Dict(d) => match d.get("stacks") {
            Some(VmValue::List(items)) => items
                .iter()
                .filter_map(|v| match v {
                    VmValue::String(s) => Some(s.to_string()),
                    _ => None,
                })
                .collect(),
            Some(VmValue::String(s)) => vec![s.to_string()],
            _ => Vec::new(),
        },
        VmValue::List(items) => items
            .iter()
            .filter_map(|v| match v {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        VmValue::String(s) => vec![s.to_string()],
        VmValue::Nil => Vec::new(),
        _ => Vec::new(),
    }
}

/// Returns true when the rule's `applies_to` filter accepts the requested
/// stacks. An empty `applies_to` matches every stack; otherwise the call
/// must list at least one overlapping stack.
fn applies_to_matches(rule_stacks: &[String], requested: &[String]) -> bool {
    if rule_stacks.is_empty() {
        return true;
    }
    if requested.is_empty() {
        return false;
    }
    rule_stacks
        .iter()
        .any(|stack| requested.iter().any(|r| r == stack))
}

async fn invoke_rule_pattern(
    ctx: &crate::vm::AsyncBuiltinCtx,
    pattern: &VmValue,
    command: &str,
    context: &VmValue,
) -> Result<bool, VmError> {
    match pattern {
        VmValue::String(regex_src) => {
            let regex = Regex::new(regex_src).map_err(|e| {
                err(format!(
                    "tool_hooks_match: invalid regex in rule pattern: {e}"
                ))
            })?;
            Ok(regex.is_match(command))
        }
        callable if Vm::is_callable_value(callable) => {
            let mut vm = ctx.child_vm();
            let command = VmValue::String(arcstr::ArcStr::from(command));
            let result = vm.call_callable_two(callable, &command, context).await?;
            ctx.forward_output(&vm.take_output());
            Ok(result.is_truthy())
        }
        other => Err(err(format!(
            "tool_hooks_match: rule pattern must be a regex string or callable, got {}",
            other.type_name()
        ))),
    }
}

fn make_match_record(catalogue: &crate::value::DictMap, rule: &crate::value::DictMap) -> VmValue {
    let catalogue_id = catalogue
        .get("id")
        .cloned()
        .unwrap_or_else(|| VmValue::String(arcstr::ArcStr::from("")));
    let stack = catalogue
        .get("stack")
        .cloned()
        .unwrap_or_else(|| VmValue::String(arcstr::ArcStr::from("")));
    let mut out = crate::value::DictMap::new();
    out.insert(crate::value::intern_key("catalogue_id"), catalogue_id);
    out.insert(crate::value::intern_key("stack"), stack);
    out.put_str("rule_id", rule_id(rule).as_str());
    out.put_str("severity", rule_severity(rule).as_str());
    out.insert(
        crate::value::intern_key("explanation"),
        rule.get("explanation")
            .cloned()
            .unwrap_or_else(|| VmValue::String(arcstr::ArcStr::from(""))),
    );
    out.insert(
        crate::value::intern_key("references"),
        rule.get("references")
            .cloned()
            .unwrap_or_else(|| VmValue::List(std::sync::Arc::new(Vec::new()))),
    );
    out.insert(
        crate::value::intern_key("priority"),
        VmValue::Int(rule_priority(rule)),
    );
    out.insert(
        crate::value::intern_key("rewrite"),
        rule.get("rewrite").cloned().unwrap_or(VmValue::Nil),
    );
    out.insert(
        crate::value::intern_key("rule"),
        VmValue::dict(rule.clone()),
    );
    VmValue::dict(out)
}

pub(crate) fn register_tool_hooks_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "tool_rule(config: dict) -> dict", category = "tool_hooks"
)]
fn tool_rule_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config = require_dict(
        args.first()
            .ok_or_else(|| err("tool_rule: requires a config dict"))?,
        "tool_rule",
        "config",
    )?;
    let rule = validate_rule_dict(config, "tool_rule")?;
    Ok(VmValue::dict(rule))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "catalogue(config: dict) -> dict", category = "tool_hooks"
)]
fn catalogue_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config = require_dict(
        args.first()
            .ok_or_else(|| err("catalogue: requires a config dict"))?,
        "catalogue",
        "config",
    )?;
    build_catalogue(config)
}

#[harn_builtin(
    exposure = "harness.tools.hooks_registry",
    effects = ["state.read@const=tool-hooks"],
    sig = "tool_hooks_registry() -> dict", category = "tool_hooks"
)]
fn tool_hooks_registry_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut registry = crate::value::DictMap::new();
    registry.put_str("_type", REGISTRY_TYPE);
    registry.insert(
        crate::value::intern_key("catalogues"),
        VmValue::List(std::sync::Arc::new(Vec::new())),
    );
    Ok(VmValue::dict(registry))
}

#[harn_builtin(
    exposure = "harness.tools.hooks_register",
    effects = ["state.mutate@const=tool-hooks"],
    sig = "tool_hooks_register(registry: dict, catalogue: dict) -> dict",
    category = "tool_hooks"
)]
fn tool_hooks_register_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = require_tagged(
        args.first()
            .ok_or_else(|| err("tool_hooks_register: requires a registry"))?,
        REGISTRY_TYPE,
        "tool_hooks_register",
        "first argument",
    )?
    .clone();
    let catalogue_value = args
        .get(1)
        .ok_or_else(|| err("tool_hooks_register: requires a catalogue"))?;
    let catalogue = require_tagged(
        catalogue_value,
        CATALOGUE_TYPE,
        "tool_hooks_register",
        "second argument",
    )?;
    let new_id = catalogue
        .get("id")
        .and_then(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .unwrap_or_default();
    if new_id.is_empty() {
        return Err(err("tool_hooks_register: catalogue is missing an id"));
    }

    let existing = registry_catalogues(&registry);
    let mut new_catalogues: Vec<VmValue> = Vec::with_capacity(existing.len() + 1);
    let mut replaced = false;
    for entry in existing {
        if let VmValue::Dict(dict) = entry {
            let id_match = dict
                .get("id")
                .and_then(|v| match v {
                    VmValue::String(s) => Some(s.as_str() == new_id),
                    _ => None,
                })
                .unwrap_or(false);
            if id_match {
                new_catalogues.push(catalogue_value.clone());
                replaced = true;
                continue;
            }
        }
        new_catalogues.push(entry.clone());
    }
    if !replaced {
        new_catalogues.push(catalogue_value.clone());
    }
    let mut next = registry;
    next.insert(
        crate::value::intern_key("catalogues"),
        VmValue::List(std::sync::Arc::new(new_catalogues)),
    );
    Ok(VmValue::dict(next))
}

#[harn_builtin(
    exposure = "harness.tools.hooks_unregister",
    effects = ["state.mutate@const=tool-hooks"],
    sig = "tool_hooks_unregister(registry: dict, catalogue_id: string) -> dict",
    category = "tool_hooks"
)]
fn tool_hooks_unregister_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = require_tagged(
        args.first()
            .ok_or_else(|| err("tool_hooks_unregister: requires a registry"))?,
        REGISTRY_TYPE,
        "tool_hooks_unregister",
        "first argument",
    )?
    .clone();
    let target_id = match args.get(1) {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            return Err(err(format!(
                "tool_hooks_unregister: catalogue id must be a string, got {}",
                other.type_name()
            )));
        }
        None => return Err(err("tool_hooks_unregister: requires a catalogue id")),
    };
    let existing = registry_catalogues(&registry);
    let new_catalogues: Vec<VmValue> = existing
        .iter()
        .filter(|entry| {
            if let VmValue::Dict(dict) = entry {
                dict.get("id")
                    .and_then(|v| match v {
                        VmValue::String(s) => Some(s.as_str() != target_id.as_str()),
                        _ => None,
                    })
                    .unwrap_or(true)
            } else {
                true
            }
        })
        .cloned()
        .collect();
    let mut next = registry;
    next.insert(
        crate::value::intern_key("catalogues"),
        VmValue::List(std::sync::Arc::new(new_catalogues)),
    );
    Ok(VmValue::dict(next))
}

#[harn_builtin(
    exposure = "harness.tools.hooks_filter",
    effects = ["state.read@const=tool-hooks"],
    sig = "tool_hooks_filter(registry: dict, stacks?: any) -> dict",
    category = "tool_hooks"
)]
fn tool_hooks_filter_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = require_tagged(
        args.first()
            .ok_or_else(|| err("tool_hooks_filter: requires a registry"))?,
        REGISTRY_TYPE,
        "tool_hooks_filter",
        "first argument",
    )?
    .clone();
    let stacks = match args.get(1) {
        Some(VmValue::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                match item {
                    VmValue::String(s) => out.push(s.to_string()),
                    other => {
                        return Err(err(format!(
                            "tool_hooks_filter: stacks entries must be strings, got {}",
                            other.type_name()
                        )));
                    }
                }
            }
            out
        }
        Some(VmValue::String(s)) => vec![s.to_string()],
        Some(VmValue::Nil) | None => Vec::new(),
        Some(other) => {
            return Err(err(format!(
                "tool_hooks_filter: stacks must be a list of strings or string, got {}",
                other.type_name()
            )));
        }
    };
    // Empty stacks list is a no-op so callers can pass through an
    // unfiltered registry without branching on the call-site.
    let filtered: Vec<VmValue> = if stacks.is_empty() {
        registry_catalogues(&registry).to_vec()
    } else {
        registry_catalogues(&registry)
            .iter()
            .filter(|entry| match entry {
                VmValue::Dict(dict) => match dict.get("stack") {
                    // Catalogues with no declared stack act as "match any" —
                    // they ship rules that aren't language-specific.
                    Some(VmValue::String(s)) if !s.is_empty() => {
                        stacks.iter().any(|requested| requested == s.as_str())
                    }
                    _ => true,
                },
                _ => false,
            })
            .cloned()
            .collect()
    };
    let mut next = registry;
    next.insert(
        crate::value::intern_key("catalogues"),
        VmValue::List(std::sync::Arc::new(filtered)),
    );
    Ok(VmValue::dict(next))
}

#[harn_builtin(
    exposure = "harness.tools.hooks_list",
    effects = ["state.read@const=tool-hooks"],
    sig = "tool_hooks_list(registry: dict) -> list",
    category = "tool_hooks"
)]
fn tool_hooks_list_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = require_tagged(
        args.first()
            .ok_or_else(|| err("tool_hooks_list: requires a registry"))?,
        REGISTRY_TYPE,
        "tool_hooks_list",
        "first argument",
    )?;
    let mut entries = Vec::new();
    for catalogue in registry_catalogues(registry) {
        let VmValue::Dict(dict) = catalogue else {
            continue;
        };
        let rule_count = match dict.get("rules") {
            Some(VmValue::List(rules)) => rules.len(),
            _ => 0,
        };
        let mut summary = crate::value::DictMap::new();
        summary.insert(
            crate::value::intern_key("id"),
            dict.get("id").cloned().unwrap_or(VmValue::Nil),
        );
        summary.insert(
            crate::value::intern_key("stack"),
            dict.get("stack").cloned().unwrap_or(VmValue::Nil),
        );
        summary.insert(
            crate::value::intern_key("version"),
            dict.get("version").cloned().unwrap_or(VmValue::Nil),
        );
        summary.insert(
            crate::value::intern_key("source"),
            dict.get("source").cloned().unwrap_or(VmValue::Nil),
        );
        summary.insert(
            crate::value::intern_key("priority"),
            dict.get("priority").cloned().unwrap_or(VmValue::Int(0)),
        );
        summary.insert(
            crate::value::intern_key("rule_count"),
            VmValue::Int(rule_count as i64),
        );
        entries.push(VmValue::dict(summary));
    }
    Ok(VmValue::List(std::sync::Arc::new(entries)))
}

#[harn_builtin(
    exposure = "harness.tools.hooks_match",
    effects = ["state.read@const=tool-hooks"],
    sig = "tool_hooks_match(registry: dict, command: string, context?: any) -> list",
    category = "tool_hooks",
    kind = "async"
)]
async fn tool_hooks_match_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let registry = require_tagged(
        args.first()
            .ok_or_else(|| err("tool_hooks_match: requires a registry"))?,
        REGISTRY_TYPE,
        "tool_hooks_match",
        "first argument",
    )?
    .clone();
    let command = match args.get(1) {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            return Err(err(format!(
                "tool_hooks_match: command must be a string, got {}",
                other.type_name()
            )));
        }
        None => return Err(err("tool_hooks_match: requires a command string")),
    };
    let context = args.get(2).cloned().unwrap_or(VmValue::Nil);
    let requested_stacks = context_stacks(Some(&context));

    // Linear sweep: catalogues × rules. Per the acceptance criterion we
    // intentionally do not pre-index — keep ordering predictable, optimize
    // later if profiling shows it matters.
    let mut matches: Vec<(usize, usize, i64, i64, VmValue)> = Vec::new();
    for (cat_idx, catalogue) in registry_catalogues(&registry).iter().enumerate() {
        let VmValue::Dict(catalogue_dict) = catalogue else {
            continue;
        };
        let catalogue_priority = match catalogue_dict.get("priority") {
            Some(VmValue::Int(n)) => *n,
            _ => 0,
        };
        let rules = match catalogue_dict.get("rules") {
            Some(VmValue::List(rules)) => rules.clone(),
            _ => std::sync::Arc::new(Vec::new()),
        };
        for (rule_idx, rule) in rules.iter().enumerate() {
            let VmValue::Dict(rule_dict) = rule else {
                continue;
            };
            let rule_stacks = rule_applies_to(rule_dict);
            if !applies_to_matches(&rule_stacks, &requested_stacks) {
                continue;
            }
            let Some(pattern) = rule_dict.get("pattern") else {
                continue;
            };
            if invoke_rule_pattern(&ctx, pattern, &command, &context).await? {
                let record = make_match_record(catalogue_dict, rule_dict);
                matches.push((
                    cat_idx,
                    rule_idx,
                    catalogue_priority,
                    rule_priority(rule_dict),
                    record,
                ));
            }
        }
    }

    // Sort: rule priority desc, then catalogue priority desc, then
    // declaration order (catalogue index, then rule index).
    matches.sort_by(|a, b| {
        b.3.cmp(&a.3)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    Ok(VmValue::List(std::sync::Arc::new(
        matches
            .into_iter()
            .map(|(_, _, _, _, record)| record)
            .collect(),
    )))
}

// TH-03 side-effect helpers used by the `tool_hooks_mode_*` callbacks
// in `crates/harn-stdlib/src/stdlib/stdlib_tool_hooks.harn`. Exposed as
// top-level builtins so the mode functions don't need access to a
// `harness` handle or transcript — they can run inside any tool
// dispatch, including bare unit tests where no agent session exists.
#[harn_builtin(
    exposure = "harness.tools.hooks_emit_audit",
    effects = ["observability.write@dynamic"],
    sig = "tool_hooks_emit_audit(kind: string, payload?: any) -> dict",
    category = "tool_hooks"
)]
fn tool_hooks_emit_audit_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let kind = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        Some(VmValue::String(_)) => {
            return Err(err(
                "tool_hooks_emit_audit: kind must be a non-empty string",
            ));
        }
        Some(other) => {
            return Err(err(format!(
                "tool_hooks_emit_audit: kind must be a string, got {}",
                other.type_name()
            )));
        }
        None => return Err(err("tool_hooks_emit_audit: requires a kind string")),
    };
    let payload = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let entry = crate::orchestration::record_lifecycle_audit(kind, payload);
    Ok(crate::stdlib::json_to_vm_value(&entry.to_json()))
}

#[harn_builtin(
    exposure = "harness.tools.hooks_inject_reminder",
    effects = ["state.write@dynamic"],
    sig = "tool_hooks_inject_reminder(options: dict) -> dict",
    category = "tool_hooks"
)]
fn tool_hooks_inject_reminder_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let options = require_dict(
        args.first()
            .ok_or_else(|| err("tool_hooks_inject_reminder: requires an options dict"))?,
        "tool_hooks_inject_reminder",
        "options",
    )?;
    let body = match options.get("body") {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(err(
                "tool_hooks_inject_reminder: options.body must be a non-empty string",
            ));
        }
    };
    // Build the typed reminder via the canonical helper so dedupe/
    // ttl/propagate/role_hint semantics stay in lockstep with
    // `transcript.inject_reminder`. We pass the dict directly; the
    // helper applies the same protocol defaults.
    let reminder = crate::llm::helpers::reminder_from_vm_value(&VmValue::Dict(
        std::sync::Arc::new(options.clone()),
    ));
    // Body cannot be empty from the helper either, since we already
    // validated above. Replace whatever the helper produced with the
    // validated body to keep one source of truth.
    let reminder = crate::llm::helpers::SystemReminder { body, ..reminder };
    let reminder_id = reminder.id.clone();

    // Attach to the active agent session when one exists so the
    // reminder shows up in the next turn's transcript. Headless
    // pipelines (no session) silently skip the session write but
    // still record the audit entry below, so conformance can
    // observe the side-effect either way.
    let mut session_attached = false;
    let mut deduped_count: i64 = 0;
    if let Some(session_id) = crate::agent_sessions::current_session_id() {
        match crate::agent_sessions::inject_reminder(&session_id, reminder.clone()) {
            Ok(report) => {
                session_attached = true;
                deduped_count = report.deduped_count as i64;
            }
            Err(_) => {
                // Session no longer exists — fall through to the
                // audit-only path rather than throwing, since the
                // tool-hook callback is best-effort.
            }
        }
    }

    // Always record an audit entry so the side-effect is observable
    // via `lifecycle_audit_log_take()` / event-log replay even when
    // no live session is attached.
    let audit_payload = serde_json::json!({
        "reminder_id": &reminder_id,
        "tags": &reminder.tags,
        "body": &reminder.body,
        "ttl_turns": reminder.ttl_turns,
        "dedupe_key": &reminder.dedupe_key,
        "session_attached": session_attached,
        "deduped_count": deduped_count,
    });
    crate::orchestration::record_lifecycle_audit("tool_hooks.reminder_injected", audit_payload);

    let mut out = crate::value::DictMap::new();
    out.put_str("reminder_id", reminder_id.as_str());
    out.insert(
        crate::value::intern_key("deduped_count"),
        VmValue::Int(deduped_count),
    );
    out.insert(
        crate::value::intern_key("session_attached"),
        VmValue::Bool(session_attached),
    );
    Ok(VmValue::dict(out))
}

// TH-05 (#1898) classifier cache: thread-local map keyed by
// `<scope>:<normalized_command>`. Optional TTL expires entries lazily
// on read so we don't need a background sweeper. The Harn-side
// `preset_run_command` wrapper builds the keys and decides what to
// cache; the Rust helpers stay dumb on purpose so callers can swap
// hashing / scope strategies without crossing the FFI boundary.
#[harn_builtin(
    exposure = "harness.tools.classifier_cache_get",
    effects = ["state.read@arg0"],
    sig = "__tool_hooks_classifier_cache_get(key: string, now_ms?: int) -> any",
    category = "tool_hooks"
)]
fn tool_hooks_classifier_cache_get_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let key = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(err(
                "__tool_hooks_classifier_cache_get: key must be a non-empty string",
            ));
        }
    };
    let now_ms = match args.get(1) {
        Some(VmValue::Int(n)) => *n,
        Some(VmValue::Nil) | None => 0,
        Some(other) => {
            return Err(err(format!(
                "__tool_hooks_classifier_cache_get: now_ms must be an int, got {}",
                other.type_name()
            )));
        }
    };
    Ok(classifier_cache_get(&key, now_ms))
}

#[harn_builtin(
    exposure = "harness.tools.classifier_cache_put",
    effects = ["state.write@arg0"],
    sig = "__tool_hooks_classifier_cache_put(key: string, value: any, now_ms?: int, ttl_ms?: int) -> nil",
    category = "tool_hooks"
)]
fn tool_hooks_classifier_cache_put_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let key = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(err(
                "__tool_hooks_classifier_cache_put: key must be a non-empty string",
            ));
        }
    };
    let value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let now_ms = match args.get(2) {
        Some(VmValue::Int(n)) => *n,
        Some(VmValue::Nil) | None => 0,
        Some(other) => {
            return Err(err(format!(
                "__tool_hooks_classifier_cache_put: now_ms must be an int, got {}",
                other.type_name()
            )));
        }
    };
    let ttl_ms = match args.get(3) {
        Some(VmValue::Int(n)) if *n > 0 => Some(*n),
        Some(VmValue::Nil) | None => None,
        Some(VmValue::Int(_)) => None,
        Some(other) => {
            return Err(err(format!(
                "__tool_hooks_classifier_cache_put: ttl_ms must be an int or nil, got {}",
                other.type_name()
            )));
        }
    };
    classifier_cache_put(key, value, now_ms, ttl_ms);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    exposure = "harness.tools.classifier_cache_clear",
    effects = ["state.mutate@const=tool-hook-classifier-cache"],
    sig = "__tool_hooks_classifier_cache_clear() -> nil",
    category = "tool_hooks"
)]
fn tool_hooks_classifier_cache_clear_impl(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    classifier_cache_clear();
    Ok(VmValue::Nil)
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &TOOL_RULE_IMPL_DEF,
    &CATALOGUE_IMPL_DEF,
    &TOOL_HOOKS_REGISTRY_IMPL_DEF,
    &TOOL_HOOKS_REGISTER_IMPL_DEF,
    &TOOL_HOOKS_UNREGISTER_IMPL_DEF,
    &TOOL_HOOKS_FILTER_IMPL_DEF,
    &TOOL_HOOKS_LIST_IMPL_DEF,
    &TOOL_HOOKS_MATCH_IMPL_DEF,
    &TOOL_HOOKS_EMIT_AUDIT_IMPL_DEF,
    &TOOL_HOOKS_INJECT_REMINDER_IMPL_DEF,
    &TOOL_HOOKS_CLASSIFIER_CACHE_GET_IMPL_DEF,
    &TOOL_HOOKS_CLASSIFIER_CACHE_PUT_IMPL_DEF,
    &TOOL_HOOKS_CLASSIFIER_CACHE_CLEAR_IMPL_DEF,
];

#[derive(Clone)]
struct ClassifierCacheEntry {
    value: VmValue,
    /// Wall-clock ms at which the entry expires. `None` means no TTL.
    expires_at_ms: Option<i64>,
}

thread_local! {
    static CLASSIFIER_CACHE: RefCell<BTreeMap<String, ClassifierCacheEntry>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

fn classifier_cache_get(key: &str, now_ms: i64) -> VmValue {
    CLASSIFIER_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let expired = matches!(
            cache.get(key),
            Some(entry) if entry.expires_at_ms.is_some_and(|exp| now_ms >= exp)
        );
        if expired {
            cache.remove(key);
            return VmValue::Nil;
        }
        cache
            .get(key)
            .map(|entry| entry.value.clone())
            .unwrap_or(VmValue::Nil)
    })
}

fn classifier_cache_put(key: String, value: VmValue, now_ms: i64, ttl_ms: Option<i64>) {
    let expires_at_ms = ttl_ms.map(|t| now_ms.saturating_add(t));
    CLASSIFIER_CACHE.with(|cell| {
        cell.borrow_mut().insert(
            key,
            ClassifierCacheEntry {
                value,
                expires_at_ms,
            },
        );
    });
}

fn classifier_cache_clear() {
    CLASSIFIER_CACHE.with(|cell| cell.borrow_mut().clear());
}

#[cfg(test)]
mod tests;
