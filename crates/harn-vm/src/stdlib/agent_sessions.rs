//! Builtins for first-class sessions.
//!
//! Sessions are the north-star replacement for the `transcript_policy`
//! config dict. Each builtin is an explicit verb over the session
//! store in `crate::agent_sessions`. There is no policy-as-verb
//! pattern; unknown inputs are hard errors.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::agent_sessions;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub fn register_agent_session_builtins(vm: &mut Vm) {
    register_open(vm);
    register_exists(vm);
    register_length(vm);
    register_snapshot(vm);
    register_ancestry(vm);
    register_current_id(vm);
    register_reset(vm);
    register_fork(vm);
    register_fork_at(vm);
    register_close(vm);
    register_trim(vm);
    register_inject(vm);
    register_compact(vm);
}

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

fn register_open(vm: &mut Vm) {
    vm.register_builtin("agent_session_open", |args, _out| {
        let id = arg_string_opt(args, 0, "agent_session_open", "id")?;
        let resolved = agent_sessions::open_or_create(id);
        Ok(VmValue::String(Rc::from(resolved)))
    });
}

fn register_exists(vm: &mut Vm) {
    vm.register_builtin("agent_session_exists", |args, _out| {
        let id = arg_string_required(args, 0, "agent_session_exists", "id")?;
        Ok(VmValue::Bool(agent_sessions::exists(&id)))
    });
}

fn register_length(vm: &mut Vm) {
    vm.register_builtin("agent_session_length", |args, _out| {
        let id = arg_string_required(args, 0, "agent_session_length", "id")?;
        match agent_sessions::length(&id) {
            Some(n) => Ok(VmValue::Int(n as i64)),
            None => Err(err(format!(
                "agent_session_length: unknown session id '{id}'"
            ))),
        }
    });
}

fn register_snapshot(vm: &mut Vm) {
    vm.register_builtin("agent_session_snapshot", |args, _out| {
        let id = arg_string_required(args, 0, "agent_session_snapshot", "id")?;
        Ok(agent_sessions::snapshot(&id).unwrap_or(VmValue::Nil))
    });
}

fn register_ancestry(vm: &mut Vm) {
    vm.register_builtin("agent_session_ancestry", |args, _out| {
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
    });
}

/// Return the innermost active agent session id for the currently
/// executing thread, or `nil` when no session is active.
fn register_current_id(vm: &mut Vm) {
    vm.register_builtin("agent_session_current_id", |_args, _out| {
        Ok(agent_sessions::current_session_id()
            .map(|id| VmValue::String(Rc::from(id)))
            .unwrap_or(VmValue::Nil))
    });
}

fn register_reset(vm: &mut Vm) {
    vm.register_builtin("agent_session_reset", |args, _out| {
        let id = arg_string_required(args, 0, "agent_session_reset", "id")?;
        if !agent_sessions::reset_transcript(&id) {
            return Err(err(format!(
                "agent_session_reset: unknown session id '{id}'"
            )));
        }
        Ok(VmValue::Nil)
    });
}

fn register_fork(vm: &mut Vm) {
    vm.register_builtin("agent_session_fork", |args, _out| {
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
    });
}

fn register_fork_at(vm: &mut Vm) {
    vm.register_builtin("agent_session_fork_at", |args, _out| {
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
    });
}

fn register_close(vm: &mut Vm) {
    vm.register_builtin("agent_session_close", |args, _out| {
        let id = arg_string_required(args, 0, "agent_session_close", "id")?;
        if !agent_sessions::exists(&id) {
            return Err(err(format!(
                "agent_session_close: unknown session id '{id}'"
            )));
        }
        agent_sessions::close(&id);
        Ok(VmValue::Nil)
    });
}

fn register_trim(vm: &mut Vm) {
    vm.register_builtin("agent_session_trim", |args, _out| {
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
    });
}

fn register_inject(vm: &mut Vm) {
    vm.register_builtin("agent_session_inject", |args, _out| {
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
    });
}

fn register_compact(vm: &mut Vm) {
    vm.register_async_builtin("agent_session_compact", |args| async move {
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
        let original = messages.len();
        crate::orchestration::auto_compact_messages(&mut messages, &config, None).await?;
        let kept = messages.len();
        agent_sessions::replace_messages(&id, &messages);
        let _ = original;
        Ok(VmValue::Int(kept as i64))
    });
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
