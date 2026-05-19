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

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use regex::Regex;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub const TOOL_RULE_TYPE: &str = "tool_rule";
pub const CATALOGUE_TYPE: &str = "catalogue";
pub const REGISTRY_TYPE: &str = "tool_hooks_registry";

const VALID_SEVERITIES: &[&str] = &["error", "warning", "info"];

fn err(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(message.into())))
}

fn require_dict<'a>(
    value: &'a VmValue,
    builtin: &str,
    role: &str,
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
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
) -> Result<&'a BTreeMap<String, VmValue>, VmError> {
    let dict = require_dict(value, builtin, role)?;
    match dict.get("_type") {
        Some(VmValue::String(t)) if t.as_ref() == expected => Ok(dict),
        Some(VmValue::String(t)) => Err(err(format!(
            "{builtin}: {role} must be a {expected} (created with the matching constructor), got {t}"
        ))),
        _ => Err(err(format!(
            "{builtin}: {role} must be a {expected} (created with the matching constructor)"
        ))),
    }
}

fn required_string_field(
    dict: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<String, VmError> {
    match dict.get(key) {
        Some(VmValue::String(s)) if !s.is_empty() => Ok(s.to_string()),
        Some(VmValue::String(_)) => Err(err(format!(
            "{builtin}: field `{key}` must be a non-empty string"
        ))),
        Some(other) => Err(err(format!(
            "{builtin}: field `{key}` must be a string, got {}",
            other.type_name()
        ))),
        None => Err(err(format!("{builtin}: missing required field `{key}`"))),
    }
}

fn optional_string_field(
    dict: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<String>, VmError> {
    match dict.get(key) {
        Some(VmValue::String(s)) => Ok(Some(s.to_string())),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(err(format!(
            "{builtin}: field `{key}` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn optional_int_field(
    dict: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Option<i64>, VmError> {
    match dict.get(key) {
        Some(VmValue::Int(n)) => Ok(Some(*n)),
        Some(VmValue::Nil) | None => Ok(None),
        Some(other) => Err(err(format!(
            "{builtin}: field `{key}` must be an int, got {}",
            other.type_name()
        ))),
    }
}

fn optional_string_list(
    dict: &BTreeMap<String, VmValue>,
    key: &str,
    builtin: &str,
) -> Result<Vec<String>, VmError> {
    match dict.get(key) {
        Some(VmValue::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items.iter() {
                match item {
                    VmValue::String(s) => out.push(s.to_string()),
                    other => {
                        return Err(err(format!(
                            "{builtin}: field `{key}` entries must be strings, got {}",
                            other.type_name()
                        )));
                    }
                }
            }
            Ok(out)
        }
        Some(VmValue::Nil) | None => Ok(Vec::new()),
        Some(other) => Err(err(format!(
            "{builtin}: field `{key}` must be a list of strings, got {}",
            other.type_name()
        ))),
    }
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
            if VALID_SEVERITIES.iter().any(|valid| *valid == s.as_ref()) {
                Ok(s.to_string())
            } else {
                Err(err(format!(
                    "{builtin}: `severity` must be one of {:?}, got {s:?}",
                    VALID_SEVERITIES
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
    config: &BTreeMap<String, VmValue>,
    builtin: &str,
) -> Result<BTreeMap<String, VmValue>, VmError> {
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

    let mut rule = BTreeMap::new();
    rule.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(TOOL_RULE_TYPE)),
    );
    rule.insert("id".to_string(), VmValue::String(Rc::from(id.as_str())));
    rule.insert("pattern".to_string(), pattern.clone());
    rule.insert(
        "applies_to".to_string(),
        VmValue::List(Rc::new(
            applies_to
                .into_iter()
                .map(|stack| VmValue::String(Rc::from(stack.as_str())))
                .collect(),
        )),
    );
    rule.insert(
        "severity".to_string(),
        VmValue::String(Rc::from(severity.as_str())),
    );
    rule.insert(
        "rewrite".to_string(),
        config.get("rewrite").cloned().unwrap_or(VmValue::Nil),
    );
    rule.insert(
        "explanation".to_string(),
        config
            .get("explanation")
            .cloned()
            .unwrap_or_else(|| VmValue::String(Rc::from(""))),
    );
    rule.insert(
        "references".to_string(),
        config
            .get("references")
            .cloned()
            .unwrap_or_else(|| VmValue::List(Rc::new(Vec::new()))),
    );
    rule.insert(
        "priority".to_string(),
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
            let mut seen_ids = BTreeMap::new();
            let mut rules = Vec::with_capacity(items.len());
            for (idx, entry) in items.iter().enumerate() {
                // Allow either a tagged tool_rule (already validated) or a
                // raw config dict (validate inline).
                let rule_dict = match entry {
                    VmValue::Dict(d)
                        if matches!(
                            d.get("_type"),
                            Some(VmValue::String(t)) if t.as_ref() == TOOL_RULE_TYPE
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
                rules.push(VmValue::Dict(Rc::new(rule_dict)));
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

fn build_catalogue(config: &BTreeMap<String, VmValue>) -> Result<VmValue, VmError> {
    let builtin = "catalogue";
    let id = required_string_field(config, "id", builtin)?;
    let stack = optional_string_field(config, "stack", builtin)?.unwrap_or_default();
    let version = optional_string_field(config, "version", builtin)?.unwrap_or_default();
    let source = optional_string_field(config, "source", builtin)?.unwrap_or_default();
    let priority = optional_int_field(config, "priority", builtin)?.unwrap_or(0);
    let rules = extract_rules(config.get("rules"), builtin)?;

    let mut catalogue = BTreeMap::new();
    catalogue.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(CATALOGUE_TYPE)),
    );
    catalogue.insert("id".to_string(), VmValue::String(Rc::from(id.as_str())));
    catalogue.insert(
        "stack".to_string(),
        VmValue::String(Rc::from(stack.as_str())),
    );
    catalogue.insert(
        "version".to_string(),
        VmValue::String(Rc::from(version.as_str())),
    );
    catalogue.insert(
        "source".to_string(),
        VmValue::String(Rc::from(source.as_str())),
    );
    catalogue.insert("priority".to_string(), VmValue::Int(priority));
    catalogue.insert("rules".to_string(), VmValue::List(Rc::new(rules)));
    Ok(VmValue::Dict(Rc::new(catalogue)))
}

fn registry_catalogues(registry: &BTreeMap<String, VmValue>) -> &[VmValue] {
    match registry.get("catalogues") {
        Some(VmValue::List(list)) => list,
        _ => &[],
    }
}

fn rule_id(rule: &BTreeMap<String, VmValue>) -> String {
    rule.get("id")
        .and_then(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn rule_priority(rule: &BTreeMap<String, VmValue>) -> i64 {
    match rule.get("priority") {
        Some(VmValue::Int(n)) => *n,
        _ => 0,
    }
}

fn rule_applies_to(rule: &BTreeMap<String, VmValue>) -> Vec<String> {
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

fn rule_severity(rule: &BTreeMap<String, VmValue>) -> String {
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
            let mut vm = crate::vm::clone_async_builtin_child_vm()
                .ok_or_else(|| err("tool_hooks_match: builtin requires VM execution context"))?;
            let result = vm
                .call_callable_value(
                    callable,
                    &[VmValue::String(Rc::from(command)), context.clone()],
                )
                .await?;
            Ok(result.is_truthy())
        }
        other => Err(err(format!(
            "tool_hooks_match: rule pattern must be a regex string or callable, got {}",
            other.type_name()
        ))),
    }
}

fn make_match_record(
    catalogue: &BTreeMap<String, VmValue>,
    rule: &BTreeMap<String, VmValue>,
) -> VmValue {
    let catalogue_id = catalogue
        .get("id")
        .cloned()
        .unwrap_or_else(|| VmValue::String(Rc::from("")));
    let stack = catalogue
        .get("stack")
        .cloned()
        .unwrap_or_else(|| VmValue::String(Rc::from("")));
    let mut out = BTreeMap::new();
    out.insert("catalogue_id".to_string(), catalogue_id);
    out.insert("stack".to_string(), stack);
    out.insert(
        "rule_id".to_string(),
        VmValue::String(Rc::from(rule_id(rule).as_str())),
    );
    out.insert(
        "severity".to_string(),
        VmValue::String(Rc::from(rule_severity(rule).as_str())),
    );
    out.insert(
        "explanation".to_string(),
        rule.get("explanation")
            .cloned()
            .unwrap_or_else(|| VmValue::String(Rc::from(""))),
    );
    out.insert(
        "references".to_string(),
        rule.get("references")
            .cloned()
            .unwrap_or_else(|| VmValue::List(Rc::new(Vec::new()))),
    );
    out.insert("priority".to_string(), VmValue::Int(rule_priority(rule)));
    out.insert(
        "rewrite".to_string(),
        rule.get("rewrite").cloned().unwrap_or(VmValue::Nil),
    );
    out.insert("rule".to_string(), VmValue::Dict(Rc::new(rule.clone())));
    VmValue::Dict(Rc::new(out))
}

pub(crate) fn register_tool_hooks_builtins(vm: &mut Vm) {
    vm.register_builtin("tool_rule", |args, _out| {
        let config = require_dict(
            args.first()
                .ok_or_else(|| err("tool_rule: requires a config dict"))?,
            "tool_rule",
            "config",
        )?;
        let rule = validate_rule_dict(config, "tool_rule")?;
        Ok(VmValue::Dict(Rc::new(rule)))
    });

    vm.register_builtin("catalogue", |args, _out| {
        let config = require_dict(
            args.first()
                .ok_or_else(|| err("catalogue: requires a config dict"))?,
            "catalogue",
            "config",
        )?;
        build_catalogue(config)
    });

    vm.register_builtin("tool_hooks_registry", |_args, _out| {
        let mut registry = BTreeMap::new();
        registry.insert(
            "_type".to_string(),
            VmValue::String(Rc::from(REGISTRY_TYPE)),
        );
        registry.insert("catalogues".to_string(), VmValue::List(Rc::new(Vec::new())));
        Ok(VmValue::Dict(Rc::new(registry)))
    });

    vm.register_builtin("tool_hooks_register", |args, _out| {
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
                        VmValue::String(s) => Some(s.as_ref() == new_id),
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
            "catalogues".to_string(),
            VmValue::List(Rc::new(new_catalogues)),
        );
        Ok(VmValue::Dict(Rc::new(next)))
    });

    vm.register_builtin("tool_hooks_unregister", |args, _out| {
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
                            VmValue::String(s) => Some(s.as_ref() != target_id.as_str()),
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
            "catalogues".to_string(),
            VmValue::List(Rc::new(new_catalogues)),
        );
        Ok(VmValue::Dict(Rc::new(next)))
    });

    vm.register_builtin("tool_hooks_filter", |args, _out| {
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
                            stacks.iter().any(|requested| requested == s.as_ref())
                        }
                        _ => true,
                    },
                    _ => false,
                })
                .cloned()
                .collect()
        };
        let mut next = registry;
        next.insert("catalogues".to_string(), VmValue::List(Rc::new(filtered)));
        Ok(VmValue::Dict(Rc::new(next)))
    });

    vm.register_builtin("tool_hooks_list", |args, _out| {
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
            let mut summary = BTreeMap::new();
            summary.insert(
                "id".to_string(),
                dict.get("id").cloned().unwrap_or(VmValue::Nil),
            );
            summary.insert(
                "stack".to_string(),
                dict.get("stack").cloned().unwrap_or(VmValue::Nil),
            );
            summary.insert(
                "version".to_string(),
                dict.get("version").cloned().unwrap_or(VmValue::Nil),
            );
            summary.insert(
                "source".to_string(),
                dict.get("source").cloned().unwrap_or(VmValue::Nil),
            );
            summary.insert(
                "priority".to_string(),
                dict.get("priority").cloned().unwrap_or(VmValue::Int(0)),
            );
            summary.insert("rule_count".to_string(), VmValue::Int(rule_count as i64));
            entries.push(VmValue::Dict(Rc::new(summary)));
        }
        Ok(VmValue::List(Rc::new(entries)))
    });

    vm.register_async_builtin("tool_hooks_match", |args| async move {
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
                _ => Rc::new(Vec::new()),
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
                if invoke_rule_pattern(pattern, &command, &context).await? {
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
        Ok(VmValue::List(Rc::new(
            matches
                .into_iter()
                .map(|(_, _, _, _, record)| record)
                .collect(),
        )))
    });

    // TH-03 side-effect helpers used by the `tool_hooks_mode_*` callbacks
    // in `crates/harn-stdlib/src/stdlib/stdlib_tool_hooks.harn`. Exposed as
    // top-level builtins so the mode functions don't need access to a
    // `harness` handle or transcript — they can run inside any tool
    // dispatch, including bare unit tests where no agent session exists.
    vm.register_builtin("tool_hooks_emit_audit", |args, _out| {
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
    });

    vm.register_builtin("tool_hooks_inject_reminder", |args, _out| {
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
        let reminder =
            crate::llm::helpers::reminder_from_vm_value(&VmValue::Dict(Rc::new(options.clone())));
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

        let mut out = BTreeMap::new();
        out.insert(
            "reminder_id".to_string(),
            VmValue::String(Rc::from(reminder_id.as_str())),
        );
        out.insert("deduped_count".to_string(), VmValue::Int(deduped_count));
        out.insert(
            "session_attached".to_string(),
            VmValue::Bool(session_attached),
        );
        Ok(VmValue::Dict(Rc::new(out)))
    });

    // TH-05 (#1898) classifier cache: thread-local map keyed by
    // `<scope>:<normalized_command>`. Optional TTL expires entries lazily
    // on read so we don't need a background sweeper. The Harn-side
    // `preset_run_command` wrapper builds the keys and decides what to
    // cache; the Rust helpers stay dumb on purpose so callers can swap
    // hashing / scope strategies without crossing the FFI boundary.
    vm.register_builtin("__tool_hooks_classifier_cache_get", |args, _out| {
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
    });

    vm.register_builtin("__tool_hooks_classifier_cache_put", |args, _out| {
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
    });

    vm.register_builtin("__tool_hooks_classifier_cache_clear", |_args, _out| {
        classifier_cache_clear();
        Ok(VmValue::Nil)
    });
}

#[derive(Clone)]
struct ClassifierCacheEntry {
    value: VmValue,
    /// Wall-clock ms at which the entry expires. `None` means no TTL.
    expires_at_ms: Option<i64>,
}

thread_local! {
    static CLASSIFIER_CACHE: RefCell<BTreeMap<String, ClassifierCacheEntry>> =
        const { RefCell::new(BTreeMap::new()) };
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
mod tests {
    use super::*;
    use crate::stdlib::register_vm_stdlib;

    fn vm_with_stdlib() -> Vm {
        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        vm
    }

    fn call_sync(vm: &Vm, name: &str, args: &[VmValue]) -> Result<VmValue, VmError> {
        let builtin = vm.builtins.get(name).cloned().expect("builtin registered");
        let mut out = String::new();
        builtin(args, &mut out)
    }

    fn sample_rule_config() -> VmValue {
        let mut config: BTreeMap<String, VmValue> = BTreeMap::new();
        config.insert(
            "id".to_string(),
            VmValue::String(Rc::from("rust.cargo.target_dir_conflict")),
        );
        config.insert(
            "pattern".to_string(),
            VmValue::String(Rc::from(r"^cargo (build|test)\b")),
        );
        config.insert(
            "applies_to".to_string(),
            VmValue::List(Rc::new(vec![VmValue::String(Rc::from("rust"))])),
        );
        config.insert("severity".to_string(), VmValue::String(Rc::from("warning")));
        config.insert(
            "explanation".to_string(),
            VmValue::String(Rc::from(
                "Concurrent cargo runs without --target-dir thrash the lockfile",
            )),
        );
        VmValue::Dict(Rc::new(config))
    }

    fn sample_catalogue_config(rule: VmValue) -> VmValue {
        let mut config: BTreeMap<String, VmValue> = BTreeMap::new();
        config.insert(
            "id".to_string(),
            VmValue::String(Rc::from("harn-canon/rust")),
        );
        config.insert("stack".to_string(), VmValue::String(Rc::from("rust")));
        config.insert("version".to_string(), VmValue::String(Rc::from("0.1.0")));
        config.insert(
            "source".to_string(),
            VmValue::String(Rc::from("harn-canon")),
        );
        config.insert("rules".to_string(), VmValue::List(Rc::new(vec![rule])));
        VmValue::Dict(Rc::new(config))
    }

    fn dict_string(dict: &BTreeMap<String, VmValue>, key: &str) -> String {
        dict.get(key).map(|v| v.display()).unwrap_or_default()
    }

    #[test]
    fn tool_rule_constructor_tags_dict() {
        let vm = vm_with_stdlib();
        let result = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("tool_rule ok");
        let dict = result.as_dict().expect("dict");
        assert_eq!(dict_string(dict, "_type"), TOOL_RULE_TYPE);
        assert!(dict.get("priority").is_some(), "priority defaulted");
        assert!(dict.get("references").is_some(), "references defaulted");
    }

    #[test]
    fn tool_rule_rejects_bad_severity() {
        let vm = vm_with_stdlib();
        let mut config: BTreeMap<String, VmValue> = BTreeMap::new();
        config.insert("id".to_string(), VmValue::String(Rc::from("r")));
        config.insert("pattern".to_string(), VmValue::String(Rc::from(".")));
        config.insert(
            "severity".to_string(),
            VmValue::String(Rc::from("catastrophic")),
        );
        let result = call_sync(&vm, "tool_rule", &[VmValue::Dict(Rc::new(config))]);
        assert!(matches!(result, Err(VmError::Thrown(_))));
    }

    #[test]
    fn tool_rule_rejects_invalid_regex() {
        let vm = vm_with_stdlib();
        let mut config: BTreeMap<String, VmValue> = BTreeMap::new();
        config.insert("id".to_string(), VmValue::String(Rc::from("r")));
        config.insert("pattern".to_string(), VmValue::String(Rc::from("[invalid")));
        let result = call_sync(&vm, "tool_rule", &[VmValue::Dict(Rc::new(config))]);
        let Err(VmError::Thrown(VmValue::String(message))) = result else {
            panic!("expected thrown string error, got {result:?}");
        };
        assert!(message.contains("invalid regex"));
    }

    #[test]
    fn catalogue_round_trips_via_json() {
        let vm = vm_with_stdlib();
        let rule = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("rule");
        let cat = call_sync(&vm, "catalogue", &[sample_catalogue_config(rule)]).expect("cat");
        let json = call_sync(&vm, "json_stringify", &[cat.clone()]).expect("encode");
        let decoded = call_sync(&vm, "json_parse", &[json]).expect("decode");
        let original = cat.as_dict().expect("dict");
        let decoded_dict = decoded.as_dict().expect("dict");
        assert_eq!(dict_string(decoded_dict, "id"), dict_string(original, "id"));
        assert_eq!(
            dict_string(decoded_dict, "stack"),
            dict_string(original, "stack")
        );
        assert_eq!(
            dict_string(decoded_dict, "version"),
            dict_string(original, "version")
        );
        assert_eq!(
            dict_string(decoded_dict, "_type"),
            dict_string(original, "_type")
        );
    }

    #[test]
    fn registry_register_replaces_by_id() {
        let vm = vm_with_stdlib();
        let registry = call_sync(&vm, "tool_hooks_registry", &[]).expect("registry");
        let rule = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("rule");
        let cat =
            call_sync(&vm, "catalogue", &[sample_catalogue_config(rule.clone())]).expect("cat");
        let r1 = call_sync(&vm, "tool_hooks_register", &[registry, cat.clone()]).expect("register");
        let r2 = call_sync(&vm, "tool_hooks_register", &[r1, cat]).expect("re-register replaces");
        let list = call_sync(&vm, "tool_hooks_list", &[r2]).expect("list");
        let items = match list {
            VmValue::List(items) => items,
            _ => panic!("expected list"),
        };
        assert_eq!(items.len(), 1, "duplicate id replaces, not appends");
    }

    #[test]
    fn registry_unregister_removes_catalogue() {
        let vm = vm_with_stdlib();
        let registry = call_sync(&vm, "tool_hooks_registry", &[]).expect("registry");
        let rule = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("rule");
        let cat = call_sync(&vm, "catalogue", &[sample_catalogue_config(rule)]).expect("cat");
        let registered = call_sync(&vm, "tool_hooks_register", &[registry, cat]).expect("ok");
        let pruned = call_sync(
            &vm,
            "tool_hooks_unregister",
            &[registered, VmValue::String(Rc::from("harn-canon/rust"))],
        )
        .expect("unregister");
        let list = call_sync(&vm, "tool_hooks_list", &[pruned]).expect("list");
        let items = match list {
            VmValue::List(items) => items,
            _ => panic!("expected list"),
        };
        assert!(items.is_empty(), "list empty after unregister");
    }

    #[test]
    fn context_stacks_normalizes_shapes() {
        let dict_form = VmValue::Dict(Rc::new(BTreeMap::from([(
            "stacks".to_string(),
            VmValue::List(Rc::new(vec![VmValue::String(Rc::from("rust"))])),
        )])));
        assert_eq!(context_stacks(Some(&dict_form)), vec!["rust".to_string()]);

        let dict_string = VmValue::Dict(Rc::new(BTreeMap::from([(
            "stacks".to_string(),
            VmValue::String(Rc::from("python")),
        )])));
        assert_eq!(
            context_stacks(Some(&dict_string)),
            vec!["python".to_string()]
        );

        let list_form = VmValue::List(Rc::new(vec![
            VmValue::String(Rc::from("typescript")),
            VmValue::String(Rc::from("rust")),
        ]));
        assert_eq!(
            context_stacks(Some(&list_form)),
            vec!["typescript".to_string(), "rust".to_string()]
        );

        let raw_string = VmValue::String(Rc::from("swift"));
        assert_eq!(context_stacks(Some(&raw_string)), vec!["swift".to_string()]);

        assert!(context_stacks(None).is_empty());
        assert!(context_stacks(Some(&VmValue::Nil)).is_empty());
    }

    #[test]
    fn registry_filter_keeps_matching_and_stackless_catalogues() {
        let vm = vm_with_stdlib();
        let rule = call_sync(&vm, "tool_rule", &[sample_rule_config()]).expect("rule");
        let rust_cat =
            call_sync(&vm, "catalogue", &[sample_catalogue_config(rule.clone())]).expect("cat");
        let mut shell_cfg: BTreeMap<String, VmValue> = BTreeMap::new();
        shell_cfg.insert(
            "id".to_string(),
            VmValue::String(Rc::from("harn-canon/shell")),
        );
        // Stackless catalogue: matches every requested stack — universal rules.
        shell_cfg.insert(
            "rules".to_string(),
            VmValue::List(Rc::new(vec![rule.clone()])),
        );
        let shell_cat =
            call_sync(&vm, "catalogue", &[VmValue::Dict(Rc::new(shell_cfg))]).expect("cat");
        let mut python_cfg: BTreeMap<String, VmValue> = BTreeMap::new();
        python_cfg.insert(
            "id".to_string(),
            VmValue::String(Rc::from("harn-canon/python")),
        );
        python_cfg.insert("stack".to_string(), VmValue::String(Rc::from("python")));
        python_cfg.insert("rules".to_string(), VmValue::List(Rc::new(vec![rule])));
        let py_cat =
            call_sync(&vm, "catalogue", &[VmValue::Dict(Rc::new(python_cfg))]).expect("cat");

        let registry = call_sync(&vm, "tool_hooks_registry", &[]).expect("registry");
        let r1 = call_sync(&vm, "tool_hooks_register", &[registry, rust_cat]).expect("r1");
        let r2 = call_sync(&vm, "tool_hooks_register", &[r1, shell_cat]).expect("r2");
        let r3 = call_sync(&vm, "tool_hooks_register", &[r2, py_cat]).expect("r3");

        // Empty stacks filter is a no-op — every catalogue survives.
        let unfiltered = call_sync(
            &vm,
            "tool_hooks_filter",
            &[r3.clone(), VmValue::List(Rc::new(Vec::new()))],
        )
        .expect("unfiltered");
        assert_eq!(
            match call_sync(&vm, "tool_hooks_list", &[unfiltered]).expect("list") {
                VmValue::List(items) => items.len(),
                _ => panic!("list"),
            },
            3,
        );

        let only_rust = call_sync(
            &vm,
            "tool_hooks_filter",
            &[
                r3.clone(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from("rust"))])),
            ],
        )
        .expect("filtered");
        let listed = call_sync(&vm, "tool_hooks_list", &[only_rust]).expect("list");
        let items = match listed {
            VmValue::List(items) => items,
            _ => panic!("expected list"),
        };
        // rust catalogue + stackless shell catalogue remain; python drops.
        assert_eq!(items.len(), 2);
        let ids: Vec<String> = items
            .iter()
            .filter_map(|v| match v {
                VmValue::Dict(d) => d.get("id").map(|id| id.display()),
                _ => None,
            })
            .collect();
        assert!(ids.iter().any(|id| id == "harn-canon/rust"));
        assert!(ids.iter().any(|id| id == "harn-canon/shell"));
    }

    #[test]
    fn applies_to_matching_respects_empty_lists() {
        assert!(applies_to_matches(&[], &[]));
        assert!(applies_to_matches(
            &[],
            &["python".to_string(), "rust".to_string()]
        ));
        assert!(!applies_to_matches(&["rust".to_string()], &[]));
        assert!(applies_to_matches(
            &["rust".to_string()],
            &["rust".to_string()]
        ));
        assert!(!applies_to_matches(
            &["rust".to_string()],
            &["python".to_string()]
        ));
    }

    // TH-03: emit_audit + inject_reminder side-effect helpers.

    fn audit_payload(rule_id: &str, command: &str) -> VmValue {
        let mut payload: BTreeMap<String, VmValue> = BTreeMap::new();
        payload.insert("rule_id".to_string(), VmValue::String(Rc::from(rule_id)));
        payload.insert("command".to_string(), VmValue::String(Rc::from(command)));
        VmValue::Dict(Rc::new(payload))
    }

    fn reminder_options(body: &str, tag: &str, ttl: i64) -> VmValue {
        let mut options: BTreeMap<String, VmValue> = BTreeMap::new();
        options.insert("body".to_string(), VmValue::String(Rc::from(body)));
        options.insert(
            "tags".to_string(),
            VmValue::List(Rc::new(vec![VmValue::String(Rc::from(tag))])),
        );
        options.insert("ttl_turns".to_string(), VmValue::Int(ttl));
        VmValue::Dict(Rc::new(options))
    }

    #[test]
    fn tool_hooks_emit_audit_records_lifecycle_entry() {
        let vm = vm_with_stdlib();
        // Drain anything in the log so other tests don't bleed in.
        let _ = crate::orchestration::take_lifecycle_audit_log();
        let entry = call_sync(
            &vm,
            "tool_hooks_emit_audit",
            &[
                VmValue::String(Rc::from("tool_rewrite")),
                audit_payload("rust.cargo.target_dir", "cargo build"),
            ],
        )
        .expect("emit_audit ok");
        let dict = entry.as_dict().expect("entry dict");
        assert_eq!(dict_string(dict, "kind"), "tool_rewrite");
        let drained = crate::orchestration::take_lifecycle_audit_log();
        assert_eq!(drained.len(), 1, "exactly one audit entry recorded");
        assert_eq!(drained[0].kind, "tool_rewrite");
        assert_eq!(
            drained[0].payload.get("rule_id").and_then(|v| v.as_str()),
            Some("rust.cargo.target_dir")
        );
    }

    #[test]
    fn tool_hooks_emit_audit_rejects_empty_kind() {
        let vm = vm_with_stdlib();
        let result = call_sync(
            &vm,
            "tool_hooks_emit_audit",
            &[VmValue::String(Rc::from("")), VmValue::Nil],
        );
        let Err(VmError::Thrown(VmValue::String(message))) = result else {
            panic!("expected thrown error, got {result:?}");
        };
        assert!(message.contains("non-empty"));
    }

    #[test]
    fn tool_hooks_inject_reminder_records_audit_when_no_session() {
        let vm = vm_with_stdlib();
        let _ = crate::orchestration::take_lifecycle_audit_log();
        let report = call_sync(
            &vm,
            "tool_hooks_inject_reminder",
            &[reminder_options("heads up", "tool_rewritten", 1)],
        )
        .expect("inject_reminder ok");
        let dict = report.as_dict().expect("report dict");
        // No active session in this unit test, so session_attached is false
        // but the audit entry still records the side-effect.
        assert!(
            matches!(dict.get("session_attached"), Some(VmValue::Bool(false))),
            "no live session in unit test"
        );
        assert!(
            matches!(dict.get("reminder_id"), Some(VmValue::String(s)) if !s.is_empty()),
            "reminder_id populated"
        );
        let drained = crate::orchestration::take_lifecycle_audit_log();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].kind, "tool_hooks.reminder_injected");
        assert_eq!(
            drained[0].payload.get("body").and_then(|v| v.as_str()),
            Some("heads up")
        );
        assert_eq!(
            drained[0].payload.get("ttl_turns").and_then(|v| v.as_i64()),
            Some(1)
        );
    }

    #[test]
    fn tool_hooks_inject_reminder_requires_body() {
        let vm = vm_with_stdlib();
        let mut options: BTreeMap<String, VmValue> = BTreeMap::new();
        options.insert(
            "tags".to_string(),
            VmValue::List(Rc::new(vec![VmValue::String(Rc::from("x"))])),
        );
        let result = call_sync(
            &vm,
            "tool_hooks_inject_reminder",
            &[VmValue::Dict(Rc::new(options))],
        );
        let Err(VmError::Thrown(VmValue::String(message))) = result else {
            panic!("expected thrown error, got {result:?}");
        };
        assert!(message.contains("body"));
    }

    // TH-05: classifier cache helpers.

    fn verdict_value(kind: &str) -> VmValue {
        let mut dict: BTreeMap<String, VmValue> = BTreeMap::new();
        dict.insert("kind".to_string(), VmValue::String(Rc::from(kind)));
        dict.insert("confidence".to_string(), VmValue::Float(0.9));
        VmValue::Dict(Rc::new(dict))
    }

    #[test]
    fn classifier_cache_roundtrips_value() {
        let vm = vm_with_stdlib();
        // Clean slate so prior tests don't bleed in via the thread-local map.
        call_sync(&vm, "__tool_hooks_classifier_cache_clear", &[]).expect("clear");
        let put = call_sync(
            &vm,
            "__tool_hooks_classifier_cache_put",
            &[
                VmValue::String(Rc::from("scope:hashA")),
                verdict_value("rewrite"),
                VmValue::Int(0),
                VmValue::Nil,
            ],
        )
        .expect("put");
        assert!(matches!(put, VmValue::Nil));
        let got = call_sync(
            &vm,
            "__tool_hooks_classifier_cache_get",
            &[VmValue::String(Rc::from("scope:hashA")), VmValue::Int(0)],
        )
        .expect("get");
        let dict = got.as_dict().expect("verdict dict");
        assert_eq!(dict_string(dict, "kind"), "rewrite");
    }

    #[test]
    fn classifier_cache_expires_entries_after_ttl() {
        let vm = vm_with_stdlib();
        call_sync(&vm, "__tool_hooks_classifier_cache_clear", &[]).expect("clear");
        call_sync(
            &vm,
            "__tool_hooks_classifier_cache_put",
            &[
                VmValue::String(Rc::from("scope:expiring")),
                verdict_value("deny"),
                VmValue::Int(1_000),
                VmValue::Int(500), // 500ms TTL → expires at 1_500.
            ],
        )
        .expect("put");
        // Within window → returns the verdict.
        let fresh = call_sync(
            &vm,
            "__tool_hooks_classifier_cache_get",
            &[
                VmValue::String(Rc::from("scope:expiring")),
                VmValue::Int(1_200),
            ],
        )
        .expect("get fresh");
        assert!(matches!(fresh, VmValue::Dict(_)));
        // At/after expiry → nil (and the entry is evicted on read).
        let expired = call_sync(
            &vm,
            "__tool_hooks_classifier_cache_get",
            &[
                VmValue::String(Rc::from("scope:expiring")),
                VmValue::Int(1_600),
            ],
        )
        .expect("get expired");
        assert!(matches!(expired, VmValue::Nil));
    }

    #[test]
    fn classifier_cache_rejects_empty_key() {
        let vm = vm_with_stdlib();
        let result = call_sync(
            &vm,
            "__tool_hooks_classifier_cache_get",
            &[VmValue::String(Rc::from("")), VmValue::Int(0)],
        );
        let Err(VmError::Thrown(VmValue::String(message))) = result else {
            panic!("expected thrown error, got {result:?}");
        };
        assert!(message.contains("non-empty"));
    }
}
