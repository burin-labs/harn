//! PreToolUse hook that consults `SessionState::workspace_anchor` and
//! the configured `PathScope` mount-mode filter to gate (or remind on)
//! tool calls whose path-shaped args point outside the anchor (#2221).
//!
//! Personas / skills opt in by calling `register_path_scope_guard`;
//! `clear_path_scope_guard` removes the active guard. The guard pairs
//! with the `<scope-alert>` reminder (#2222): on Deny the guard injects
//! a typed alert body listing the three handoff options (add_root,
//! reanchor, fork to sub-agent) before rejecting the call.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::llm::helpers::{ReminderPropagate, ReminderRoleHint, ReminderSource, SystemReminder};
use crate::orchestration::{
    set_singleton_pre_tool_hook, PreToolAction, PreToolHookFn, ReminderSpec,
};
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;
use crate::workspace_anchor::MountMode;

const DEFAULT_ARG_KEYS: &[&str] = &[
    "path",
    "destination",
    "source",
    "file",
    "filepath",
    "file_path",
    "target",
];

/// Marker prefix carried on the rejection reason emitted by the guard
/// so upstream observability can distinguish a scope-driven deny from
/// an unrelated tool failure.
pub const PATH_SCOPE_VIOLATION_PREFIX: &str = "[path_scope_violation] ";

pub fn register_path_scope_guard_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] =
    &[&REGISTER_PATH_SCOPE_GUARD_DEF, &CLEAR_PATH_SCOPE_GUARD_DEF];

/// Install a PreToolUse hook that denies (or emits a `<scope-alert>`
/// reminder for) tool calls whose path-shaped args point outside the
/// session's workspace anchor (#2221). `opts` may set `arg_keys`
/// (default: path/destination/source/file/filepath/file_path/target),
/// `on_violation` ("deny" or "reminder"; default "deny"), and
/// `mount_modes` (which mounted-root modes count as in-scope; default
/// the session's writable mounts: extend + sandboxed). Returns nil.
/// Pairs with the `<scope-alert>` reminder body for the model-facing
/// handoff hint (#2222).
#[harn_builtin(
    sig = "register_path_scope_guard(opts?: dict|nil) -> nil",
    category = "agent.path_scope_guard"
)]
fn register_path_scope_guard(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let opts = match args.first() {
        None | Some(VmValue::Nil) => BTreeMap::new(),
        Some(VmValue::Dict(map)) => map.as_ref().clone(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "register_path_scope_guard: `opts` must be a dict or nil, got {}",
                other.type_name()
            )));
        }
    };
    for key in opts.keys() {
        if !matches!(
            key.as_str(),
            "arg_keys" | "on_violation" | "mount_modes" | "enabled"
        ) {
            return Err(VmError::Runtime(format!(
                "register_path_scope_guard: unknown option key '{key}' (expected one of: arg_keys, on_violation, mount_modes, enabled)"
            )));
        }
    }
    if matches!(opts.get("enabled"), Some(VmValue::Bool(false))) {
        set_singleton_pre_tool_hook(None);
        return Ok(VmValue::Nil);
    }
    let arg_keys = parse_arg_keys(&opts)?;
    let on_violation = parse_on_violation(&opts)?;
    let mount_modes = parse_mount_modes(&opts)?;

    let pre: PreToolHookFn = Arc::new(move |tool_name: &str, args: &serde_json::Value| {
        let Some(session_id) = crate::agent_sessions::current_session_id() else {
            return PreToolAction::Allow;
        };
        let Some(anchor) = crate::agent_sessions::workspace_anchor(&session_id) else {
            return PreToolAction::Allow;
        };
        let candidates = collect_path_candidates(args, &arg_keys);
        for path in candidates {
            if let Some(reason) =
                crate::llm::permissions::anchor_scope_violation(&path, &anchor, &mount_modes)
            {
                let tagged = format!("{PATH_SCOPE_VIOLATION_PREFIX}{reason}");
                let spec = scope_alert_reminder(tool_name, &path, &anchor, &reason);
                return match on_violation {
                    Violation::Deny => PreToolAction::Reminder {
                        spec,
                        then: Box::new(PreToolAction::Deny(tagged)),
                    },
                    Violation::Reminder => PreToolAction::Reminder {
                        spec,
                        then: Box::new(PreToolAction::Allow),
                    },
                };
            }
        }
        PreToolAction::Allow
    });
    set_singleton_pre_tool_hook(Some(pre));
    Ok(VmValue::Nil)
}

/// Remove the active path_scope_guard registration.
#[harn_builtin(
    sig = "clear_path_scope_guard() -> nil",
    category = "agent.path_scope_guard"
)]
fn clear_path_scope_guard(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    set_singleton_pre_tool_hook(None);
    Ok(VmValue::Nil)
}

/// Build the canonical `<scope-alert>` reminder body (#2222) that
/// describes the three handoff options to the model.
pub fn scope_alert_reminder(
    tool_name: &str,
    path: &str,
    anchor: &crate::workspace_anchor::WorkspaceAnchor,
    reason: &str,
) -> ReminderSpec {
    let mounted_roots = if anchor.additional_roots.is_empty() {
        "  (none)".to_string()
    } else {
        anchor
            .additional_roots
            .iter()
            .map(|root| {
                format!(
                    "  - {} (mount_mode: {})",
                    root.path.display(),
                    root.mount_mode.as_str(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let body = format!(
        "<scope-alert>\nTool call '{tool_name}' targeted path '{path}', which is outside the current workspace anchor ({reason}).\n\nCurrent anchor: {anchor}\nMounted roots:\n{mounted_roots}\n\nThree options:\n  - add_root: mount the path's containing repo into this session — `agent_session_add_root(session_id, root, {{mount_mode}})`\n  - reanchor: switch the session's primary anchor to that repo — `agent_session_reanchor(session_id, new_anchor)`\n  - fork: spawn a sub-agent against the target repo — `spawn_agent({{anchor: new_anchor, ...}})`\n\nPick one, or surface the choice to the user.\n</scope-alert>",
        anchor = anchor.primary.display(),
    );
    SystemReminder {
        id: format!("scope-alert:{tool_name}:{}", short_path_hash(path)),
        tags: vec!["scope_alert".to_string()],
        dedupe_key: Some(format!("scope_alert:{tool_name}:{}", short_path_hash(path))),
        ttl_turns: Some(3),
        preserve_on_compact: false,
        propagate: ReminderPropagate::Session,
        role_hint: ReminderRoleHint::System,
        source: ReminderSource::StdlibProvider,
        body,
        fired_at_turn: 0,
        originating_agent_id: None,
    }
}

fn short_path_hash(path: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(path.as_bytes());
    hex::encode(&digest[..6])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Violation {
    Deny,
    Reminder,
}

fn parse_arg_keys(opts: &BTreeMap<String, VmValue>) -> Result<Vec<String>, VmError> {
    match opts.get("arg_keys") {
        None | Some(VmValue::Nil) => {
            Ok(DEFAULT_ARG_KEYS.iter().map(|key| key.to_string()).collect())
        }
        Some(VmValue::List(items)) => items
            .iter()
            .map(|item| match item {
                VmValue::String(value) if !value.trim().is_empty() => Ok(value.to_string()),
                VmValue::String(_) => Err(VmError::Runtime(
                    "register_path_scope_guard: arg_keys entries must be non-empty strings".into(),
                )),
                other => Err(VmError::Runtime(format!(
                    "register_path_scope_guard: arg_keys entries must be strings, got {}",
                    other.type_name()
                ))),
            })
            .collect(),
        Some(other) => Err(VmError::Runtime(format!(
            "register_path_scope_guard: `arg_keys` must be a list, got {}",
            other.type_name()
        ))),
    }
}

fn parse_on_violation(opts: &BTreeMap<String, VmValue>) -> Result<Violation, VmError> {
    match opts.get("on_violation") {
        None | Some(VmValue::Nil) => Ok(Violation::Deny),
        Some(VmValue::String(value)) => match value.as_ref() {
            "deny" => Ok(Violation::Deny),
            "reminder" | "reminder_only" => Ok(Violation::Reminder),
            other => Err(VmError::Runtime(format!(
                "register_path_scope_guard: `on_violation` must be 'deny' or 'reminder', got '{other}'"
            ))),
        },
        Some(other) => Err(VmError::Runtime(format!(
            "register_path_scope_guard: `on_violation` must be a string, got {}",
            other.type_name()
        ))),
    }
}

fn parse_mount_modes(opts: &BTreeMap<String, VmValue>) -> Result<Vec<MountMode>, VmError> {
    let raw = match opts.get("mount_modes") {
        None | Some(VmValue::Nil) => {
            return Ok(vec![MountMode::Extend, MountMode::Sandboxed]);
        }
        Some(VmValue::List(items)) => items,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "register_path_scope_guard: `mount_modes` must be a list, got {}",
                other.type_name()
            )));
        }
    };
    raw.iter()
        .map(|item| match item {
            VmValue::String(value) => MountMode::parse(value).map_err(|message| {
                VmError::Runtime(format!("register_path_scope_guard: {message}"))
            }),
            other => Err(VmError::Runtime(format!(
                "register_path_scope_guard: mount_modes entries must be strings, got {}",
                other.type_name()
            ))),
        })
        .collect()
}

fn collect_path_candidates(args: &serde_json::Value, arg_keys: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    collect_inner(args, arg_keys, &mut out);
    out
}

fn collect_inner(value: &serde_json::Value, arg_keys: &[String], out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if arg_keys.iter().any(|wanted| wanted == key) {
                    collect_string_leaves(value, out);
                } else {
                    collect_inner(value, arg_keys, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_inner(item, arg_keys, out);
            }
        }
        _ => {}
    }
}

fn collect_string_leaves(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) if !text.is_empty() => out.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_string_leaves(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for value in map.values() {
                collect_string_leaves(value, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_paths_from_named_keys_only() {
        let args = serde_json::json!({
            "path": "/tmp/x",
            "metadata": {"path": "/tmp/y", "label": "ignored"},
            "other": "/tmp/z",
        });
        let keys = vec!["path".to_string()];
        let mut paths = collect_path_candidates(&args, &keys);
        paths.sort();
        assert_eq!(paths, vec!["/tmp/x".to_string(), "/tmp/y".to_string()]);
    }

    #[test]
    fn parse_mount_modes_defaults_to_writable_mounts() {
        let modes = parse_mount_modes(&BTreeMap::new()).expect("default modes");
        assert_eq!(modes, vec![MountMode::Extend, MountMode::Sandboxed]);
    }

    #[test]
    fn parse_on_violation_rejects_unknown_value() {
        let opts = BTreeMap::from([(
            "on_violation".to_string(),
            VmValue::String(std::sync::Arc::from("warn")),
        )]);
        let err = parse_on_violation(&opts).expect_err("unknown should fail");
        assert!(err.to_string().contains("on_violation"));
    }
}
