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
//!
//! Schema follows the established tagged-dict convention (`_type`) used by
//! `tool_registry` and `skill_registry`. Values are plain Harn dicts so they
//! serde-roundtrip through `json_stringify` / `json_parse` with no special
//! handling, and the matching engine runs as a single linear sweep over
//! catalogues × rules (TH-01 acceptance criterion).

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
}
