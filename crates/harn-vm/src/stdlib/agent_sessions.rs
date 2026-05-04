//! Builtins for first-class sessions.
//!
//! Sessions are the north-star replacement for the `transcript_policy`
//! config dict. Each builtin is an explicit verb over the session
//! store in `crate::agent_sessions`. There is no policy-as-verb
//! pattern; unknown inputs are hard errors.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::agent_sessions;
use crate::stdlib::registration::{
    boxed_async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup, SyncBuiltin,
};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

pub fn register_agent_session_builtins(vm: &mut Vm) {
    register_builtin_group(vm, AGENT_SESSION_PRIMITIVES);
}

const AGENT_SESSION_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("agent_session_open", agent_session_open_builtin)
        .signature("agent_session_open(id?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Open or create a first-class agent session."),
    SyncBuiltin::new("agent_session_exists", agent_session_exists_builtin)
        .signature("agent_session_exists(id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Return whether an agent session exists."),
    SyncBuiltin::new("agent_session_length", agent_session_length_builtin)
        .signature("agent_session_length(id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Return the number of messages in an agent session."),
    SyncBuiltin::new("agent_session_snapshot", agent_session_snapshot_builtin)
        .signature("agent_session_snapshot(id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Return the current transcript snapshot for an agent session."),
    SyncBuiltin::new("agent_session_ancestry", agent_session_ancestry_builtin)
        .signature("agent_session_ancestry(id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Return parent, child, and root lineage for an agent session."),
    SyncBuiltin::new("agent_session_current_id", agent_session_current_id_builtin)
        .signature("agent_session_current_id()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Return the innermost active agent session id."),
    SyncBuiltin::new(
        "agent_session_tool_format",
        agent_session_tool_format_builtin,
    )
    .signature("agent_session_tool_format(id)")
    .arity(VmBuiltinArity::Exact(1))
    .doc("Return the claimed tool format for an agent session."),
    SyncBuiltin::new(
        "agent_session_claim_tool_format",
        agent_session_claim_tool_format_builtin,
    )
    .signature("agent_session_claim_tool_format(id, tool_format)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Claim the tool format for an agent session."),
    SyncBuiltin::new("agent_session_reset", agent_session_reset_builtin)
        .signature("agent_session_reset(id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Reset an agent session transcript."),
    SyncBuiltin::new("agent_session_fork", agent_session_fork_builtin)
        .signature("agent_session_fork(src, dst?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Fork an agent session transcript."),
    SyncBuiltin::new("agent_session_fork_at", agent_session_fork_at_builtin)
        .signature("agent_session_fork_at(src, keep_first, dst?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .doc("Fork an agent session at a message boundary."),
    SyncBuiltin::new("agent_session_close", agent_session_close_builtin)
        .signature("agent_session_close(id)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Close an agent session."),
    SyncBuiltin::new("agent_session_trim", agent_session_trim_builtin)
        .signature("agent_session_trim(id, keep_last)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Trim an agent session to the last N messages."),
    SyncBuiltin::new("agent_session_inject", agent_session_inject_builtin)
        .signature("agent_session_inject(id, message)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Inject one message into an agent session."),
];

const AGENT_SESSION_ASYNC_PRIMITIVES: &[AsyncBuiltin] =
    &[AsyncBuiltin::new("agent_session_compact", |args| {
        boxed_async_builtin(agent_session_compact_builtin(args))
    })
    .signature("agent_session_compact(id, opts?)")
    .arity(VmBuiltinArity::Range { min: 1, max: 2 })
    .doc("Compact an agent session transcript with the host compaction runtime.")];

const AGENT_SESSION_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("agent.session")
    .sync(AGENT_SESSION_SYNC_PRIMITIVES)
    .async_(AGENT_SESSION_ASYNC_PRIMITIVES);

fn err(msg: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(msg.into())))
}

fn arg_string_opt(
    args: &[VmValue],
    idx: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<Option<String>, VmError> {
    match args.get(idx) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(s)) => Ok(Some(s.to_string())),
        _ => Err(err(format!(
            "{fn_name}: `{arg_name}` must be a string or nil"
        ))),
    }
}

fn arg_string_required(
    args: &[VmValue],
    idx: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<String, VmError> {
    match args.get(idx) {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        _ => Err(err(format!("{fn_name}: `{arg_name}` must be a string"))),
    }
}

fn arg_int_required(
    args: &[VmValue],
    idx: usize,
    fn_name: &str,
    arg_name: &str,
) -> Result<i64, VmError> {
    args.get(idx)
        .and_then(VmValue::as_int)
        .ok_or_else(|| err(format!("{fn_name}: `{arg_name}` must be an int")))
}

fn agent_session_open_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_opt(args, 0, "agent_session_open", "id")?;
    let resolved = agent_sessions::open_or_create(id);
    Ok(VmValue::String(Rc::from(resolved)))
}

fn agent_session_exists_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_exists", "id")?;
    Ok(VmValue::Bool(agent_sessions::exists(&id)))
}

fn agent_session_length_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_length", "id")?;
    match agent_sessions::length(&id) {
        Some(n) => Ok(VmValue::Int(n as i64)),
        None => Err(err(format!(
            "agent_session_length: unknown session id '{id}'"
        ))),
    }
}

fn agent_session_snapshot_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_snapshot", "id")?;
    Ok(agent_sessions::snapshot(&id).unwrap_or(VmValue::Nil))
}

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

fn agent_session_current_id_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(agent_sessions::current_session_id()
        .map(|id| VmValue::String(Rc::from(id)))
        .unwrap_or(VmValue::Nil))
}

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

fn agent_session_reset_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_reset", "id")?;
    if !agent_sessions::reset_transcript(&id) {
        return Err(err(format!(
            "agent_session_reset: unknown session id '{id}'"
        )));
    }
    Ok(VmValue::Nil)
}

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

fn agent_session_close_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_close", "id")?;
    if !agent_sessions::exists(&id) {
        return Err(err(format!(
            "agent_session_close: unknown session id '{id}'"
        )));
    }
    agent_sessions::close(&id);
    Ok(VmValue::Nil)
}

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
    let config = build_compact_config(&opts_dict)?;
    let mut messages = agent_sessions::messages_json(&id);
    crate::orchestration::auto_compact_messages(&mut messages, &config, None).await?;
    let kept = messages.len();
    agent_sessions::replace_messages(&id, &messages);
    Ok(VmValue::Int(kept as i64))
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
    let mut cfg = crate::orchestration::AutoCompactConfig::default();
    if let Some(v) = opts.get("keep_last").and_then(|v| v.as_int()) {
        if v < 0 {
            return Err(err("agent_session_compact: `keep_last` must be >= 0"));
        }
        cfg.keep_last = v as usize;
    }
    if let Some(v) = opts.get("token_threshold").and_then(|v| v.as_int()) {
        cfg.token_threshold = v as usize;
    }
    if let Some(v) = opts.get("tool_output_max_chars").and_then(|v| v.as_int()) {
        cfg.tool_output_max_chars = v as usize;
    }
    if let Some(VmValue::String(s)) = opts.get("compact_strategy") {
        cfg.compact_strategy = crate::orchestration::parse_compact_strategy(s)?;
    }
    if let Some(v) = opts.get("hard_limit_tokens").and_then(|v| v.as_int()) {
        cfg.hard_limit_tokens = Some(v as usize);
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

#[cfg(test)]
mod tests {
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
}
