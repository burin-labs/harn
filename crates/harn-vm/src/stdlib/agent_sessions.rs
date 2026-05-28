//! Builtins for first-class sessions.
//!
//! Sessions are the north-star replacement for the `transcript_policy`
//! config dict. Each builtin is an explicit verb over the session
//! store in `crate::agent_sessions`. There is no policy-as-verb
//! pattern; unknown inputs are hard errors.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use crate::agent_sessions;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::stdlib::options::{self, ErrorKind};
use crate::value::{VmError, VmValue};

/// Sessions raise catchable errors (callers may `try`/`recover`).
const ERR_KIND: ErrorKind = ErrorKind::Thrown;
use crate::vm::Vm;

pub fn register_agent_session_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &AGENT_SESSION_OPEN_BUILTIN_DEF,
    &AGENT_SESSION_EXISTS_BUILTIN_DEF,
    &AGENT_SESSION_LENGTH_BUILTIN_DEF,
    &AGENT_SESSION_SNAPSHOT_BUILTIN_DEF,
    &AGENT_SESSION_ANCESTRY_BUILTIN_DEF,
    &AGENT_SESSION_CURRENT_ID_BUILTIN_DEF,
    &AGENT_SESSION_TOOL_FORMAT_BUILTIN_DEF,
    &AGENT_SESSION_SYSTEM_PROMPT_BUILTIN_DEF,
    &AGENT_SESSION_WORKSPACE_ANCHOR_BUILTIN_DEF,
    &AGENT_SESSION_SET_WORKSPACE_ANCHOR_BUILTIN_DEF,
    &AGENT_SESSION_WORKSPACE_POLICY_BUILTIN_DEF,
    &AGENT_SESSION_SET_WORKSPACE_POLICY_BUILTIN_DEF,
    &AGENT_SESSION_ADD_ROOT_BUILTIN_DEF,
    &AGENT_SESSION_REMOVE_ROOT_BUILTIN_DEF,
    &AGENT_SESSION_LIST_ROOTS_BUILTIN_DEF,
    &AGENT_SESSION_CLAIM_TOOL_FORMAT_BUILTIN_DEF,
    &AGENT_SESSION_RESET_BUILTIN_DEF,
    &AGENT_SESSION_FORK_BUILTIN_DEF,
    &AGENT_SESSION_FORK_AT_BUILTIN_DEF,
    &AGENT_SESSION_CLOSE_BUILTIN_DEF,
    &AGENT_SESSION_TRIM_BUILTIN_DEF,
    &AGENT_SESSION_INJECT_BUILTIN_DEF,
    &AGENT_SESSION_POST_EVENT_BUILTIN_DEF,
    &AGENT_SESSION_DRAIN_INBOX_BUILTIN_DEF,
    &AGENT_SESSION_SEED_FROM_JSONL_BUILTIN_DEF,
    &AGENT_SESSION_REANCHOR_BUILTIN_DEF,
    &AGENT_SESSION_COMPACT_BUILTIN_DEF,
    &CANCEL_IN_FLIGHT_TOOL_CALL_BUILTIN_DEF,
];

fn err(msg: impl Into<String>) -> VmError {
    ERR_KIND.err(msg.into())
}

/// Thin local re-exports so each builtin can keep its call sites short. The
/// shared helpers in [`crate::stdlib::options`] handle the actual logic and
/// error-kind selection.
fn arg_string_opt(
    args: &[VmValue],
    idx: usize,
    fn_name: &'static str,
    arg_name: &str,
) -> Result<Option<String>, VmError> {
    // Preserve the prior contract: do *not* trim — sessions accept whitespace
    // strings for nested-id round-trips.
    match args.get(idx) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(s)) => Ok(Some(s.to_string())),
        _ => Err(options::fn_err(
            fn_name,
            ERR_KIND,
            format_args!("`{arg_name}` must be a string or nil"),
        )),
    }
}

fn arg_string_required(
    args: &[VmValue],
    idx: usize,
    fn_name: &'static str,
    arg_name: &str,
) -> Result<String, VmError> {
    // Preserve the prior contract: accept empty strings here (sessions used
    // `arg_string_required` without trim/empty checks).
    match args.get(idx) {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        _ => Err(options::fn_err(
            fn_name,
            ERR_KIND,
            format_args!("`{arg_name}` must be a string"),
        )),
    }
}

fn arg_int_required(
    args: &[VmValue],
    idx: usize,
    fn_name: &'static str,
    arg_name: &str,
) -> Result<i64, VmError> {
    options::required_int_arg(args, idx, fn_name, arg_name, ERR_KIND)
}

fn arg_bool_opt(
    opts: &BTreeMap<String, VmValue>,
    fn_name: &str,
    arg_name: &str,
    default: bool,
) -> Result<bool, VmError> {
    match opts.get(arg_name) {
        None | Some(VmValue::Nil) => Ok(default),
        Some(VmValue::Bool(value)) => Ok(*value),
        _ => Err(err(format!("{fn_name}: `{arg_name}` must be a bool"))),
    }
}

fn opt_string(
    opts: &BTreeMap<String, VmValue>,
    fn_name: &str,
    arg_name: &str,
) -> Result<Option<String>, VmError> {
    match opts.get(arg_name) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        _ => Err(err(format!(
            "{fn_name}: `{arg_name}` must be a string or nil"
        ))),
    }
}

fn opt_usize(
    opts: &BTreeMap<String, VmValue>,
    fn_name: &str,
    arg_name: &str,
) -> Result<Option<usize>, VmError> {
    match opts.get(arg_name) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value) => {
            let Some(raw) = value.as_int() else {
                return Err(err(format!("{fn_name}: `{arg_name}` must be an int")));
            };
            if raw < 0 {
                return Err(err(format!("{fn_name}: `{arg_name}` must be >= 0")));
            }
            Ok(Some(raw as usize))
        }
    }
}

fn opts_dict_arg(
    args: &[VmValue],
    idx: usize,
    fn_name: &str,
) -> Result<BTreeMap<String, VmValue>, VmError> {
    match args.get(idx) {
        None | Some(VmValue::Nil) => Ok(BTreeMap::new()),
        Some(VmValue::Dict(opts)) => Ok(opts.as_ref().clone()),
        _ => Err(err(format!("{fn_name}: `opts` must be a dict or nil"))),
    }
}

fn seed_result_error(message: impl Into<String>) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "ok": false,
        "error": message.into(),
    }))
}

fn ok_result(fields: &[(&str, serde_json::Value)]) -> VmValue {
    let mut result =
        serde_json::Map::from_iter([("ok".to_string(), serde_json::Value::Bool(true))]);
    for (key, value) in fields {
        result.insert((*key).to_string(), value.clone());
    }
    crate::stdlib::json_to_vm_value(&serde_json::Value::Object(result))
}

fn dict_string_field(dict: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match dict.get(key) {
        Some(VmValue::String(value)) if !value.trim().is_empty() => Some(value.to_string()),
        _ => None,
    }
}

fn close_status_arg(args: &[VmValue]) -> Result<(String, String, serde_json::Value), VmError> {
    match args.get(1) {
        None | Some(VmValue::Nil) => Ok((
            "closed".to_string(),
            "closed".to_string(),
            serde_json::Value::Null,
        )),
        Some(VmValue::String(value)) => {
            let reason = value.trim();
            if reason.is_empty() {
                return Err(err(
                    "agent_session_close: `status` string must not be empty",
                ));
            }
            Ok((
                reason.to_string(),
                reason.to_string(),
                serde_json::Value::Null,
            ))
        }
        Some(VmValue::Dict(dict)) => {
            let reason = dict_string_field(dict, "reason")
                .or_else(|| dict_string_field(dict, "stop_reason"))
                .or_else(|| dict_string_field(dict, "status"))
                .unwrap_or_else(|| "closed".to_string());
            let status = dict_string_field(dict, "status").unwrap_or_else(|| reason.clone());
            Ok((
                reason,
                status,
                crate::llm::helpers::vm_value_to_json(args.get(1).expect("status arg")),
            ))
        }
        _ => Err(err(
            "agent_session_close: `status` must be a string, dict, or nil",
        )),
    }
}

const AGENT_SESSION_OPEN_OPT_KEYS: &[&str] = &["workspace_anchor", "workspace_policy"];
const AGENT_SESSION_ADD_ROOT_OPT_KEYS: &[&str] = &["mount_mode", "reason"];

#[harn_builtin(
    sig = "agent_session_open(id?: string, opts?: dict) -> any",
    category = "agent.session",
    doc = "Open or create a first-class agent session. opts may carry workspace_anchor and workspace_policy."
)]
fn agent_session_open_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_opt(args, 0, "agent_session_open", "id")?;
    let opts = match args.get(1) {
        None | Some(VmValue::Nil) => BTreeMap::new(),
        Some(VmValue::Dict(opts)) => opts.as_ref().clone(),
        _ => return Err(err("agent_session_open: `opts` must be a dict or nil")),
    };
    for key in opts.keys() {
        if !AGENT_SESSION_OPEN_OPT_KEYS.contains(&key.as_str()) {
            let expected = AGENT_SESSION_OPEN_OPT_KEYS.join(", ");
            return Err(err(format!(
                "agent_session_open: unknown option key '{key}' (expected one of: {expected})"
            )));
        }
    }
    let workspace_policy = match opts.get("workspace_policy") {
        None | Some(VmValue::Nil) => None,
        Some(value) => Some(
            crate::workspace_anchor::parse_workspace_policy_dict(value)
                .map_err(|message| err(format!("agent_session_open: {message}")))?,
        ),
    };
    let default_mount_mode = workspace_policy
        .clone()
        .or_else(|| id.as_deref().and_then(agent_sessions::workspace_policy))
        .unwrap_or_default()
        .default_mount_mode;
    let anchor = match opts.get("workspace_anchor") {
        None | Some(VmValue::Nil) => None,
        Some(value) => Some(
            crate::workspace_anchor::parse_anchor_dict_with_default_mount_mode(
                value,
                default_mount_mode,
            )
            .map_err(|message| err(format!("agent_session_open: {message}")))?,
        ),
    };
    let resolved = agent_sessions::open_or_create(id);
    if let Some(policy) = workspace_policy {
        agent_sessions::set_workspace_policy(&resolved, policy)
            .map_err(|message| err(format!("agent_session_open: {message}")))?;
    }
    if let Some(anchor) = anchor {
        agent_sessions::set_workspace_anchor(&resolved, Some(anchor))
            .map_err(|message| err(format!("agent_session_open: {message}")))?;
    }
    Ok(VmValue::String(Rc::from(resolved)))
}

#[harn_builtin(
    sig = "agent_session_workspace_anchor(id: string) -> any",
    category = "agent.session",
    doc = "Return the typed workspace anchor for an agent session, or nil when none is pinned."
)]
fn agent_session_workspace_anchor_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_workspace_anchor", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_workspace_anchor: unknown session id '{id}'"
        )));
    }
    Ok(agent_sessions::workspace_anchor(&id)
        .as_ref()
        .map(crate::workspace_anchor::WorkspaceAnchor::to_vm_value)
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig = "agent_session_set_workspace_anchor(id: string, anchor: any) -> bool",
    category = "agent.session",
    doc = "Set or clear the typed workspace anchor for an agent session."
)]
fn agent_session_set_workspace_anchor_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_set_workspace_anchor", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_set_workspace_anchor: unknown session id '{id}'"
        )));
    }
    let anchor = match args.get(1) {
        None | Some(VmValue::Nil) => None,
        Some(value) => Some(
            crate::workspace_anchor::parse_anchor_dict_with_default_mount_mode(
                value,
                agent_sessions::workspace_policy(&id)
                    .unwrap_or_default()
                    .default_mount_mode,
            )
            .map_err(|message| err(format!("agent_session_set_workspace_anchor: {message}")))?,
        ),
    };
    let changed = agent_sessions::set_workspace_anchor(&id, anchor)
        .map_err(|message| err(format!("agent_session_set_workspace_anchor: {message}")))?;
    Ok(VmValue::Bool(changed))
}

#[harn_builtin(
    sig = "agent_session_workspace_policy(id: string) -> dict",
    category = "agent.session",
    doc = "Return the session workspace policy defaults."
)]
fn agent_session_workspace_policy_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_workspace_policy", "id")?;
    agent_sessions::workspace_policy(&id)
        .map(|policy| policy.to_vm_value())
        .ok_or_else(|| {
            err(format!(
                "agent_session_workspace_policy: unknown session id '{id}'"
            ))
        })
}

#[harn_builtin(
    sig = "agent_session_set_workspace_policy(id: string, policy: dict) -> bool",
    category = "agent.session",
    doc = "Set the session workspace policy defaults. Returns true when changed."
)]
fn agent_session_set_workspace_policy_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_set_workspace_policy", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_set_workspace_policy: unknown session id '{id}'"
        )));
    }
    let value = args
        .get(1)
        .ok_or_else(|| err("agent_session_set_workspace_policy: `policy` argument is required"))?;
    let policy = crate::workspace_anchor::parse_workspace_policy_dict(value)
        .map_err(|message| err(format!("agent_session_set_workspace_policy: {message}")))?;
    let changed = agent_sessions::set_workspace_policy(&id, policy)
        .map_err(|message| err(format!("agent_session_set_workspace_policy: {message}")))?;
    Ok(VmValue::Bool(changed))
}

#[harn_builtin(
    sig = "agent_session_add_root(id: string, root: string, opts?: dict) -> dict",
    category = "agent.session",
    doc = "Mount an additional workspace root on an anchored session."
)]
fn agent_session_add_root_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_add_root", "id")?;
    let root = arg_string_required(args, 1, "agent_session_add_root", "root")?;
    let opts = opts_dict_arg(args, 2, "agent_session_add_root")?;
    for key in opts.keys() {
        if !AGENT_SESSION_ADD_ROOT_OPT_KEYS.contains(&key.as_str()) {
            let expected = AGENT_SESSION_ADD_ROOT_OPT_KEYS.join(", ");
            return Err(err(format!(
                "agent_session_add_root: unknown option key '{key}' (expected one of: {expected})"
            )));
        }
    }
    let mount_mode = opt_string(&opts, "agent_session_add_root", "mount_mode")?
        .map(|value| crate::workspace_anchor::MountMode::parse(&value))
        .transpose()
        .map_err(|message| err(format!("agent_session_add_root: {message}")))?;
    let reason = opt_string(&opts, "agent_session_add_root", "reason")?;
    Ok(
        match agent_sessions::add_workspace_root(&id, &root, mount_mode, reason) {
            Ok(mounted_at) => ok_result(&[("mounted_at", serde_json::Value::String(mounted_at))]),
            Err(message) => seed_result_error(message),
        },
    )
}

#[harn_builtin(
    sig = "agent_session_remove_root(id: string, root: string) -> dict",
    category = "agent.session",
    doc = "Remove one additional workspace root from an anchored session."
)]
fn agent_session_remove_root_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_remove_root", "id")?;
    let root = arg_string_required(args, 1, "agent_session_remove_root", "root")?;
    Ok(match agent_sessions::remove_workspace_root(&id, &root) {
        Ok(_) => ok_result(&[]),
        Err(message) => seed_result_error(message),
    })
}

#[harn_builtin(
    sig = "agent_session_list_roots(id: string) -> dict",
    category = "agent.session",
    doc = "Return {primary, additional} for the session mounted roots."
)]
fn agent_session_list_roots_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_list_roots", "id")?;
    let (primary, additional) = agent_sessions::list_workspace_roots(&id)
        .map_err(|message| err(format!("agent_session_list_roots: {message}")))?;
    Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "primary": primary,
        "additional": additional,
    })))
}

#[harn_builtin(
    sig = "agent_session_exists(id: string) -> bool",
    category = "agent.session",
    doc = "Return whether an agent session exists."
)]
fn agent_session_exists_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_exists", "id")?;
    Ok(VmValue::Bool(agent_sessions::exists(&id)))
}

#[harn_builtin(
    sig = "agent_session_length(id: string) -> int",
    category = "agent.session",
    doc = "Return the number of messages in an agent session."
)]
fn agent_session_length_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_length", "id")?;
    match agent_sessions::length(&id) {
        Some(n) => Ok(VmValue::Int(n as i64)),
        None => Err(err(format!(
            "agent_session_length: unknown session id '{id}'"
        ))),
    }
}

#[harn_builtin(
    sig = "agent_session_snapshot(id: string) -> any",
    category = "agent.session",
    doc = "Return the current transcript snapshot for an agent session."
)]
fn agent_session_snapshot_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_snapshot", "id")?;
    Ok(agent_sessions::snapshot(&id).unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig = "agent_session_ancestry(id: string) -> dict",
    category = "agent.session",
    doc = "Return parent, child, and root lineage for an agent session."
)]
fn agent_session_ancestry_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_ancestry", "id")?;
    let Some(ancestry) = agent_sessions::ancestry(&id) else {
        return Ok(VmValue::Nil);
    };
    Ok(VmValue::Dict(Rc::new(BTreeMap::from([
        (
            "parent_id".to_string(),
            ancestry
                .parent_id
                .map(|value| VmValue::String(Rc::from(value)))
                .unwrap_or(VmValue::Nil),
        ),
        (
            "child_ids".to_string(),
            VmValue::List(Rc::new(
                ancestry
                    .child_ids
                    .into_iter()
                    .map(|value| VmValue::String(Rc::from(value)))
                    .collect(),
            )),
        ),
        (
            "root_id".to_string(),
            VmValue::String(Rc::from(ancestry.root_id)),
        ),
    ]))))
}

#[harn_builtin(
    sig = "agent_session_current_id() -> string?",
    category = "agent.session",
    doc = "Return the innermost active agent session id."
)]
fn agent_session_current_id_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(agent_sessions::current_session_id()
        .map(|id| VmValue::String(Rc::from(id)))
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig = "agent_session_tool_format(id: string) -> string?",
    category = "agent.session",
    doc = "Return the claimed tool format for an agent session."
)]
fn agent_session_tool_format_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_tool_format", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_tool_format: unknown session id '{id}'"
        )));
    }
    Ok(agent_sessions::tool_format(&id)
        .map(|value| VmValue::String(Rc::from(value)))
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig = "agent_session_system_prompt(id: string) -> string?",
    category = "agent.session",
    doc = "Return the session-level system prompt recorded for an agent session."
)]
fn agent_session_system_prompt_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_system_prompt", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_system_prompt: unknown session id '{id}'"
        )));
    }
    Ok(agent_sessions::system_prompt(&id)
        .map(|value| VmValue::String(Rc::from(value)))
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig = "agent_session_claim_tool_format(id: string, tool_format: string) -> nil",
    category = "agent.session",
    doc = "Claim the tool format for an agent session."
)]
fn agent_session_claim_tool_format_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_claim_tool_format", "id")?;
    let tool_format =
        arg_string_required(args, 1, "agent_session_claim_tool_format", "tool_format")?;
    agent_sessions::claim_tool_format(&id, &tool_format)
        .map_err(|message| err(format!("agent_session_claim_tool_format: {message}")))?;
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "agent_session_reset(id: string) -> nil",
    category = "agent.session",
    doc = "Reset an agent session transcript."
)]
fn agent_session_reset_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_reset", "id")?;
    if !agent_sessions::reset_transcript(&id) {
        return Err(err(format!(
            "agent_session_reset: unknown session id '{id}'"
        )));
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "agent_session_fork(src: string, dst?: string) -> string",
    category = "agent.session",
    doc = "Fork an agent session transcript."
)]
fn agent_session_fork_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let src = arg_string_required(args, 0, "agent_session_fork", "src")?;
    let dst = arg_string_opt(args, 1, "agent_session_fork", "dst")?;
    if !agent_sessions::exists(&src) {
        return Err(err(format!(
            "agent_session_fork: unknown session id '{src}'"
        )));
    }
    match agent_sessions::fork(&src, dst) {
        Some(new_id) => Ok(VmValue::String(Rc::from(new_id))),
        None => Err(err(format!(
            "agent_session_fork: failed to fork session '{src}'"
        ))),
    }
}

#[harn_builtin(
    sig = "agent_session_fork_at(src: string, keep_first: int, dst?: string) -> string",
    category = "agent.session",
    doc = "Fork an agent session at a message boundary."
)]
fn agent_session_fork_at_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let src = arg_string_required(args, 0, "agent_session_fork_at", "src")?;
    let keep_first = arg_int_required(args, 1, "agent_session_fork_at", "keep_first")?;
    if keep_first < 0 {
        return Err(err("agent_session_fork_at: `keep_first` must be >= 0"));
    }
    let dst = arg_string_opt(args, 2, "agent_session_fork_at", "dst")?;
    if !agent_sessions::exists(&src) {
        return Err(err(format!(
            "agent_session_fork_at: unknown session id '{src}'"
        )));
    }
    match agent_sessions::fork_at(&src, keep_first as usize, dst) {
        Some(new_id) => Ok(VmValue::String(Rc::from(new_id))),
        None => Err(err(format!(
            "agent_session_fork_at: failed to fork session '{src}'"
        ))),
    }
}

#[harn_builtin(
    sig = "agent_session_close(id: string, status?: any) -> nil",
    category = "agent.session",
    doc = "Close an agent session and optionally record a close reason."
)]
fn agent_session_close_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_close", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_close: unknown session id '{id}'"
        )));
    }
    let (reason, status, metadata) = close_status_arg(args)?;
    agent_sessions::close_with_status(&id, reason, status, metadata);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "agent_session_trim(id: string, keep_last: int) -> nil",
    category = "agent.session",
    doc = "Trim an agent session to the last N messages."
)]
fn agent_session_trim_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_trim", "id")?;
    let keep_last = args
        .get(1)
        .and_then(|v| v.as_int())
        .ok_or_else(|| err("agent_session_trim: `keep_last` must be an int"))?;
    if keep_last < 0 {
        return Err(err("agent_session_trim: `keep_last` must be >= 0"));
    }
    let Some(kept) = agent_sessions::trim(&id, keep_last as usize) else {
        return Err(err(format!(
            "agent_session_trim: unknown session id '{id}'"
        )));
    };
    Ok(VmValue::Int(kept as i64))
}

#[harn_builtin(
    sig = "agent_session_inject(id: string, message: any) -> nil",
    category = "agent.session",
    doc = "Inject one message into an agent session."
)]
fn agent_session_inject_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_inject", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_inject: unknown session id '{id}'"
        )));
    }
    let message = args
        .get(1)
        .cloned()
        .ok_or_else(|| err("agent_session_inject: `message` required"))?;
    agent_sessions::inject_message(&id, message).map_err(err)?;
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "agent_session_post_event(id: string, kind: string, content: any, source?: any) -> nil",
    category = "agent.session",
    doc = "Post an event into a running session agent_inbox."
)]
fn agent_session_post_event_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_post_event", "id")?;
    let kind = arg_string_required(args, 1, "agent_session_post_event", "kind")?;
    let content = match args.get(2) {
        Some(VmValue::String(s)) => s.to_string(),
        Some(other) => {
            serde_json::to_string(&crate::llm::vm_value_to_json(other)).unwrap_or_default()
        }
        None => {
            return Err(err("agent_session_post_event: `content` required"));
        }
    };
    let source = match args.get(3) {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => "harn.post_event".to_string(),
    };
    crate::orchestration::agent_inbox::push(&id, &kind, &content, &source);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "agent_session_drain_inbox(id: string) -> list",
    category = "agent.session",
    doc = "Drain every pending agent_inbox entry for a session."
)]
fn agent_session_drain_inbox_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_drain_inbox", "id")?;
    let entries = crate::orchestration::agent_inbox::drain(&id)
        .into_iter()
        .map(|entry| {
            let mut dict = BTreeMap::new();
            dict.insert("sequence".to_string(), VmValue::Int(entry.sequence as i64));
            dict.insert("kind".to_string(), VmValue::String(Rc::from(entry.kind)));
            dict.insert(
                "content".to_string(),
                VmValue::String(Rc::from(entry.content)),
            );
            dict.insert(
                "source".to_string(),
                VmValue::String(Rc::from(entry.source)),
            );
            dict.insert("ts_ms".to_string(), VmValue::Int(entry.ts_ms));
            VmValue::Dict(Rc::new(dict))
        })
        .collect::<Vec<_>>();
    Ok(VmValue::List(Rc::new(entries)))
}

const SEED_FROM_JSONL_OPT_KEYS: &[&str] = &[
    "truncate_to_last",
    "drop_tool_calls",
    "rename_session",
    "validate",
    "provider",
    "model",
];

#[harn_builtin(
    sig = "agent_session_seed_from_jsonl(jsonl_path: string, opts?: dict) -> string",
    category = "agent.session",
    doc = "Seed a new agent session from an LLM transcript JSONL sidecar."
)]
fn agent_session_seed_from_jsonl_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let path = arg_string_required(args, 0, "agent_session_seed_from_jsonl", "jsonl_path")?;
    let opts = opts_dict_arg(args, 1, "agent_session_seed_from_jsonl")?;
    for key in opts.keys() {
        if !SEED_FROM_JSONL_OPT_KEYS.contains(&key.as_str()) {
            let expected = SEED_FROM_JSONL_OPT_KEYS.join(", ");
            return Err(err(format!(
                "agent_session_seed_from_jsonl: unknown option key '{key}' (expected one of: {expected})"
            )));
        }
    }

    let seed_options = crate::llm::transcript_seed::SeedOptions {
        truncate_to_last: opt_usize(&opts, "agent_session_seed_from_jsonl", "truncate_to_last")?,
        drop_tool_calls: arg_bool_opt(
            &opts,
            "agent_session_seed_from_jsonl",
            "drop_tool_calls",
            false,
        )?,
        validate: arg_bool_opt(&opts, "agent_session_seed_from_jsonl", "validate", true)?,
        target_provider: opt_string(&opts, "agent_session_seed_from_jsonl", "provider")?,
        target_model: opt_string(&opts, "agent_session_seed_from_jsonl", "model")?,
    };
    let rename_session = opt_string(&opts, "agent_session_seed_from_jsonl", "rename_session")?;
    let path_buf = PathBuf::from(&path);
    let seeded = match crate::llm::transcript_seed::load_seeded_transcript_from_jsonl(
        &path_buf,
        &seed_options,
    ) {
        Ok(seeded) => seeded,
        Err(message) => return Ok(seed_result_error(message)),
    };

    let metadata = serde_json::json!({
        "seeded_from_jsonl": {
            "path": path,
            "source_records": seeded.record_count,
            "source_format": seeded.source_format.as_str(),
            "partial": seeded.partial,
            "truncated": seeded.truncated,
            "provider": seeded.provider,
            "model": seeded.model,
            "tool_format": seeded.tool_format,
        }
    });
    let session_id = match agent_sessions::seed_from_messages(
        rename_session,
        &seeded.messages,
        metadata,
        seeded.system_prompt.clone(),
        seeded.tool_format.clone(),
    ) {
        Ok(session_id) => session_id,
        Err(message) => return Ok(seed_result_error(message)),
    };
    Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "ok": true,
        "session_id": session_id,
        "turns_loaded": seeded.messages.len(),
        "messages_loaded": seeded.messages.len(),
        "source_records": seeded.record_count,
        "source_format": seeded.source_format.as_str(),
        "partial": seeded.partial,
        "truncated": seeded.truncated,
        "provider": seeded.provider,
        "model": seeded.model,
        "tool_format": seeded.tool_format,
        "error": serde_json::Value::Null,
    })))
}

const REANCHOR_OPT_KEYS: &[&str] = &["carry_transcript", "compact", "reason"];

#[harn_builtin(
    sig = "agent_session_reanchor(id: string, new_anchor: any, opts?: dict) -> dict",
    kind = "async",
    category = "agent.session",
    doc = "Atomically replace a session primary workspace anchor (#2218)."
)]
async fn agent_session_reanchor_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let id = arg_string_required(&args, 0, "agent_session_reanchor", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_reanchor: unknown session id '{id}'"
        )));
    }
    let anchor_value = args
        .get(1)
        .ok_or_else(|| err("agent_session_reanchor: `new_anchor` argument is required"))?;
    let default_mount_mode = agent_sessions::workspace_policy(&id)
        .unwrap_or_default()
        .default_mount_mode;
    let new_anchor = crate::workspace_anchor::parse_anchor_dict_with_default_mount_mode(
        anchor_value,
        default_mount_mode,
    )
    .map_err(|message| err(format!("agent_session_reanchor: {message}")))?;
    let opts = match args.get(2) {
        None | Some(VmValue::Nil) => BTreeMap::new(),
        Some(VmValue::Dict(d)) => d.as_ref().clone(),
        _ => return Err(err("agent_session_reanchor: `opts` must be a dict or nil")),
    };
    for key in opts.keys() {
        if !REANCHOR_OPT_KEYS.contains(&key.as_str()) {
            let expected = REANCHOR_OPT_KEYS.join(", ");
            return Err(err(format!(
                "agent_session_reanchor: unknown option key '{key}' (expected one of: {expected})"
            )));
        }
    }
    let carry_transcript = arg_bool_opt(&opts, "agent_session_reanchor", "carry_transcript", true)?;
    let compact = arg_bool_opt(&opts, "agent_session_reanchor", "compact", false)?;
    if compact && !carry_transcript {
        return Err(err(
            "agent_session_reanchor: `compact: true` requires `carry_transcript: true`",
        ));
    }
    let reason = opt_string(&opts, "agent_session_reanchor", "reason")?;
    // `carry_transcript: false` drops the transcript by forking into a
    // fresh session id. The fork inherits system prompt + workspace
    // policy; the transcript is then cleared so the resumed turn starts
    // clean against the new anchor.
    let target_id = if carry_transcript {
        id.clone()
    } else {
        let dst = agent_sessions::fork(&id, None).ok_or_else(|| {
            err(format!(
                "agent_session_reanchor: failed to fork session '{id}' for carry_transcript=false"
            ))
        })?;
        if !agent_sessions::reset_transcript(&dst) {
            return Err(err(format!(
                "agent_session_reanchor: failed to reset forked transcript '{dst}'"
            )));
        }
        dst
    };
    let mut compacted = false;
    if compact {
        let _ = agent_session_compact_builtin(vec![VmValue::String(Rc::from(target_id.clone()))])
            .await?;
        compacted = true;
    }
    let outcome = agent_sessions::reanchor_session(
        &target_id,
        new_anchor,
        carry_transcript,
        compacted,
        reason,
    )
    .map_err(|message| err(format!("agent_session_reanchor: {message}")))?;
    Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
        "ok": true,
        "changed": outcome.changed,
        "session_id": target_id,
        "anchor": outcome.current.to_json(),
        "compacted": compacted,
        "error": serde_json::Value::Null,
    })))
}

#[harn_builtin(
    sig = "agent_session_compact(id: string, opts?: dict) -> dict",
    kind = "async",
    category = "agent.session",
    doc = "Compact an agent session transcript with the host compaction runtime."
)]
async fn agent_session_compact_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let id = arg_string_required(&args, 0, "agent_session_compact", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_compact: unknown session id '{id}'"
        )));
    }
    let opts_dict = match args.get(1) {
        Some(VmValue::Dict(d)) => (**d).clone(),
        None | Some(VmValue::Nil) => BTreeMap::new(),
        _ => return Err(err("agent_session_compact: `opts` must be a dict or nil")),
    };
    let mut config = build_compact_config(&opts_dict)?;
    let mut messages = agent_sessions::messages_json(&id);
    let original_count = messages.len();
    let reminder_events = session_compactable_events(&id);
    let provider_options = if opts_dict.is_empty() {
        serde_json::json!({})
    } else {
        crate::llm::reminder_providers::options_map_to_json(&opts_dict)
    };

    let lifecycle =
        crate::orchestration::CompactLifecycle::new(crate::orchestration::CompactMode::Host)
            .with_session_id(Some(&id))
            .with_reminder_events(reminder_events)
            .with_provider_options(provider_options);

    let Some(outcome) =
        crate::orchestration::run_compaction_lifecycle(&mut messages, &mut config, None, lifecycle)
            .await?
    else {
        return Ok(VmValue::Int(original_count as i64));
    };

    agent_sessions::replace_messages_with_summary(&id, &messages, Some(&outcome.summary))
        .map_err(err)?;
    let compaction_event = crate::llm::helpers::transcript_event(
        "compaction",
        "system",
        "internal",
        "",
        Some(outcome.event_metadata),
    );
    agent_sessions::append_event(&id, compaction_event).map_err(err)?;
    for preserved in outcome.reminder_report.preserved_events {
        agent_sessions::append_event(&id, preserved).map_err(err)?;
    }
    Ok(VmValue::Int(messages.len() as i64))
}

fn session_compactable_events(id: &str) -> Vec<VmValue> {
    let Some(transcript) = agent_sessions::transcript(id) else {
        return Vec::new();
    };
    let Some(dict) = transcript.as_dict() else {
        return Vec::new();
    };
    crate::orchestration::transcript_compactable_events(dict)
}

const COMPACT_OPT_KEYS: &[&str] = &[
    "keep_last",
    "token_threshold",
    "tool_output_max_chars",
    "compact_strategy",
    "hard_limit_tokens",
    "hard_limit_strategy",
    "custom_compactor",
    "mask_callback",
    "compress_callback",
    "policy",
    "compaction_policy",
    "compaction_request",
    "instructions",
    "mode",
    "scope",
    "preserve",
    "drop",
    "extend_default_instructions",
    "author",
];

fn build_compact_config(
    opts: &BTreeMap<String, VmValue>,
) -> Result<crate::orchestration::AutoCompactConfig, VmError> {
    for key in opts.keys() {
        if !COMPACT_OPT_KEYS.contains(&key.as_str()) {
            let expected = COMPACT_OPT_KEYS.join(", ");
            return Err(err(format!(
                "agent_session_compact: unknown option key '{key}' (expected one of: {expected})"
            )));
        }
    }
    let mut cfg = crate::orchestration::AutoCompactConfig {
        policy: crate::orchestration::parse_compaction_policy_options(
            Some(opts),
            "agent_session_compact",
        )?,
        ..Default::default()
    };
    if let Some(v) = compact_usize_opt(opts, "keep_last")? {
        cfg.keep_last = v;
    }
    if let Some(v) = compact_usize_opt(opts, "token_threshold")? {
        cfg.token_threshold = v;
    }
    if let Some(v) = compact_usize_opt(opts, "tool_output_max_chars")? {
        cfg.tool_output_max_chars = v;
    }
    if let Some(VmValue::String(s)) = opts.get("compact_strategy") {
        cfg.compact_strategy = crate::orchestration::parse_compact_strategy(s)?;
        cfg.policy_strategy =
            crate::orchestration::compact_strategy_name(&cfg.compact_strategy).to_string();
    }
    if let Some(v) = compact_usize_opt(opts, "hard_limit_tokens")? {
        cfg.hard_limit_tokens = Some(v);
    }
    if let Some(VmValue::String(s)) = opts.get("hard_limit_strategy") {
        cfg.hard_limit_strategy = crate::orchestration::parse_compact_strategy(s)?;
    }
    if let Some(v) = opts.get("custom_compactor").cloned() {
        if !matches!(v, VmValue::Closure(_)) {
            return Err(err(
                "agent_session_compact: `custom_compactor` must be a closure",
            ));
        }
        cfg.custom_compactor = Some(v);
    }
    if let Some(v) = opts.get("mask_callback").cloned() {
        if !matches!(v, VmValue::Closure(_)) {
            return Err(err(
                "agent_session_compact: `mask_callback` must be a closure",
            ));
        }
        cfg.mask_callback = Some(v);
    }
    if let Some(v) = opts.get("compress_callback").cloned() {
        if !matches!(v, VmValue::Closure(_)) {
            return Err(err(
                "agent_session_compact: `compress_callback` must be a closure",
            ));
        }
        cfg.compress_callback = Some(v);
    }
    Ok(cfg)
}

const CANCEL_TOOL_CALL_OPT_KEYS: &[&str] = &["reason", "inject_reminder", "timeout_ms"];

/// Default grace period (in milliseconds) for `cancel_in_flight_tool_call`
/// to wait for the dispatch to unwind before returning `timeout`. Matches
/// the issue spec (#2213).
const CANCEL_TOOL_CALL_DEFAULT_TIMEOUT_MS: i64 = 5_000;

#[harn_builtin(
    sig = "cancel_in_flight_tool_call(session_id: string, call_id: string, opts?: dict) -> dict",
    kind = "async",
    category = "agent.session",
    doc = "Abort a specific in-flight tool call."
)]
async fn cancel_in_flight_tool_call_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let session_id = arg_string_required(&args, 0, "cancel_in_flight_tool_call", "session_id")?;
    let call_id = arg_string_required(&args, 1, "cancel_in_flight_tool_call", "call_id")?;
    if call_id.trim().is_empty() {
        return Err(err(
            "cancel_in_flight_tool_call: `call_id` must be non-empty",
        ));
    }
    let opts = opts_dict_arg(&args, 2, "cancel_in_flight_tool_call")?;
    for key in opts.keys() {
        if !CANCEL_TOOL_CALL_OPT_KEYS.contains(&key.as_str()) {
            let expected = CANCEL_TOOL_CALL_OPT_KEYS.join(", ");
            return Err(err(format!(
                "cancel_in_flight_tool_call: unknown option key '{key}' (expected one of: {expected})"
            )));
        }
    }
    let reason = opt_string(&opts, "cancel_in_flight_tool_call", "reason")?
        .unwrap_or_else(|| "host cancelled in-flight tool call".to_string());
    let inject_reminder =
        arg_bool_opt(&opts, "cancel_in_flight_tool_call", "inject_reminder", true)?;
    let timeout_ms = match opts.get("timeout_ms") {
        None | Some(VmValue::Nil) => CANCEL_TOOL_CALL_DEFAULT_TIMEOUT_MS,
        Some(value) => value
            .as_int()
            .ok_or_else(|| err("cancel_in_flight_tool_call: `timeout_ms` must be an int"))?,
    };
    if timeout_ms < 0 {
        return Err(err("cancel_in_flight_tool_call: `timeout_ms` must be >= 0"));
    }

    let outcome = crate::tool_call_cancellations::cancel(
        &session_id,
        &call_id,
        reason.clone(),
        inject_reminder,
    );

    if matches!(
        outcome.status,
        crate::tool_call_cancellations::CancelStatus::Cancelled
    ) && inject_reminder
    {
        push_cancellation_reminder(&session_id, &call_id, outcome.tool_name.as_deref(), &reason)
            .await;
    }

    let mut final_status = outcome.status.as_str();
    if matches!(
        outcome.status,
        crate::tool_call_cancellations::CancelStatus::Cancelled
    ) {
        if let Some(handle) = outcome.handle.as_ref() {
            if timeout_ms > 0 {
                let timeout = std::time::Duration::from_millis(timeout_ms as u64);
                let wait_result = tokio::time::timeout(timeout, handle.completed()).await;
                if wait_result.is_err() && !handle.is_completed() {
                    final_status = "timeout";
                }
            }
        }
    }

    let mut result = BTreeMap::new();
    result.insert(
        "status".to_string(),
        VmValue::String(Rc::from(final_status)),
    );
    result.insert("call_id".to_string(), VmValue::String(Rc::from(call_id)));
    result.insert(
        "tool".to_string(),
        outcome
            .tool_name
            .map(|name| VmValue::String(Rc::from(name)))
            .unwrap_or(VmValue::Nil),
    );
    result.insert("reason".to_string(), VmValue::String(Rc::from(reason)));
    Ok(VmValue::Dict(Rc::new(result)))
}

async fn push_cancellation_reminder(
    session_id: &str,
    call_id: &str,
    tool_name: Option<&str>,
    reason: &str,
) {
    let Some(bridge) = crate::llm::current_host_bridge() else {
        return;
    };
    let body = match tool_name {
        Some(name) => {
            format!("Tool call `{name}` (call_id={call_id}) was cancelled by the host: {reason}")
        }
        None => format!("Tool call call_id={call_id} was cancelled by the host: {reason}"),
    };
    let params = serde_json::json!({
        "sessionId": session_id,
        "mode": "interrupt_immediate",
        "reminder": {
            "id": uuid::Uuid::now_v7().to_string(),
            "tags": ["tool_call_cancelled"],
            "dedupe_key": format!("cancel:{call_id}"),
            "preserve_on_compact": false,
            "propagate": "session",
            "role_hint": "system",
            "source": "bridge",
            "body": body,
            "fired_at_turn": 0,
        }
    });
    let _ = bridge.push_queued_session_remind_from_params(&params).await;
}

fn compact_usize_opt(
    opts: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<usize>, VmError> {
    let Some(value) = opts.get(key) else {
        return Ok(None);
    };
    let Some(raw) = value.as_int() else {
        return Err(err(format!(
            "agent_session_compact: `{key}` must be an int"
        )));
    };
    if raw < 0 {
        return Err(err(format!("agent_session_compact: `{key}` must be >= 0")));
    }
    Ok(Some(raw as usize))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::build_compact_config;
    use crate::value::VmValue;

    fn call_current_id_builtin() -> VmValue {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut vm = crate::Vm::new();
                    crate::register_vm_stdlib(&mut vm);
                    vm.call_named_builtin("agent_session_current_id", Vec::new())
                        .await
                        .expect("builtin call")
                })
                .await
        })
    }

    #[test]
    fn current_id_returns_nil_outside_active_session() {
        crate::reset_thread_local_state();
        assert!(matches!(call_current_id_builtin(), VmValue::Nil));
    }

    #[test]
    fn current_id_returns_active_session_id() {
        crate::reset_thread_local_state();
        crate::agent_sessions::push_current_session("unit-test-session".to_string());
        let current = call_current_id_builtin();
        crate::agent_sessions::pop_current_session();
        assert!(matches!(current, VmValue::String(value) if value.as_ref() == "unit-test-session"));
    }

    #[test]
    fn compact_config_rejects_negative_numeric_options() {
        for key in [
            "keep_last",
            "token_threshold",
            "tool_output_max_chars",
            "hard_limit_tokens",
        ] {
            let mut opts = BTreeMap::new();
            opts.insert(key.to_string(), VmValue::Int(-1));
            let err = build_compact_config(&opts).expect_err("negative option must fail");
            assert!(err.to_string().contains(key), "{err}");
        }
    }
}
