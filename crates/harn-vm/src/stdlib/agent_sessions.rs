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
use crate::stdlib::options::{self, ErrorKind};
use crate::stdlib::registration::{
    async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup, SyncBuiltin,
};
use crate::value::{VmError, VmValue};

/// Sessions raise catchable errors (callers may `try`/`recover`).
const ERR_KIND: ErrorKind = ErrorKind::Thrown;
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
        "agent_session_system_prompt",
        agent_session_system_prompt_builtin,
    )
    .signature("agent_session_system_prompt(id)")
    .arity(VmBuiltinArity::Exact(1))
    .doc("Return the session-level system prompt recorded for an agent session."),
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
        .signature("agent_session_close(id, status?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Close an agent session and optionally record a close reason."),
    SyncBuiltin::new("agent_session_trim", agent_session_trim_builtin)
        .signature("agent_session_trim(id, keep_last)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Trim an agent session to the last N messages."),
    SyncBuiltin::new("agent_session_inject", agent_session_inject_builtin)
        .signature("agent_session_inject(id, message)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Inject one message into an agent session."),
    SyncBuiltin::new("agent_session_post_event", agent_session_post_event_builtin)
        .signature("agent_session_post_event(id, kind, content, source?)")
        .arity(VmBuiltinArity::Range { min: 3, max: 4 })
        .doc(
            "Post an event into a running session's agent_inbox. Triggers, \
         connectors, and external host integrations use this to nudge \
         a mid-loop session; the next turn boundary (including the \
         drain after compaction) surfaces the entry as feedback.",
        ),
    SyncBuiltin::new(
        "agent_session_drain_inbox",
        agent_session_drain_inbox_builtin,
    )
    .signature("agent_session_drain_inbox(id)")
    .arity(VmBuiltinArity::Exact(1))
    .doc(
        "Drain every pending agent_inbox entry for a session. Each entry \
         has {sequence, kind, content, source, ts_ms}.",
    ),
    SyncBuiltin::new(
        "agent_session_seed_from_jsonl",
        agent_session_seed_from_jsonl_builtin,
    )
    .signature("agent_session_seed_from_jsonl(jsonl_path, opts?)")
    .arity(VmBuiltinArity::Range { min: 1, max: 2 })
    .doc("Seed a new agent session from an LLM transcript JSONL sidecar."),
];

const AGENT_SESSION_ASYNC_PRIMITIVES: &[AsyncBuiltin] =
    &[
        async_builtin!("agent_session_compact", agent_session_compact_builtin)
            .signature("agent_session_compact(id, opts?)")
            .arity(VmBuiltinArity::Range { min: 1, max: 2 })
            .doc("Compact an agent session transcript with the host compaction runtime."),
    ];

const AGENT_SESSION_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("agent.session")
    .sync(AGENT_SESSION_SYNC_PRIMITIVES)
    .async_(AGENT_SESSION_ASYNC_PRIMITIVES);

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
    let (reason, status, metadata) = close_status_arg(args)?;
    agent_sessions::close_with_status(&id, reason, status, metadata);
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
            "provider": seeded.provider.clone(),
            "model": seeded.model.clone(),
            "tool_format": seeded.tool_format.clone(),
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
