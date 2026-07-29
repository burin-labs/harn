//! Method dispatch for the `Harness` capability handle and its
//! sub-handles. Every sub-handle (`stdio`, `clock`, `fs`, `env`,
//! `random`, `net`, `process`, `crypto`, `system`, `secrets`, `llm`,
//! `tenant`, and `obs`)
//! is wired end-to-end in real, mock, and null modes;
//! sandbox / egress rejections raised inside a sub-handle method are
//! tagged with the `HARN-CAP-201` diagnostic code so callers can
//! attribute the error to the active capability profile rather than an
//! opaque tool rejection.

use crate::value::VmDictExt;
use std::time::Duration;

use crate::harness::{vm_string, HarnessKind, HarnessMode, VmHarness};
use crate::harness_net::{
    self, record_audit, violation_request_value, violation_vm_error, NetPolicyAudit,
    NetPolicyDecision, OnViolation,
};
use crate::stdlib::io::{
    prompt_user_value, read_line_legacy_value, read_line_structured_value, write_stderr,
    write_stdout,
};
use crate::value::{ErrorCategory, VmError, VmValue};

/// Outcome of `Vm::evaluate_net_policy_for_method`. `Allow` means the
/// dispatcher should proceed with the underlying call; `Deny` carries
/// the typed error to surface to the caller.
enum NetPolicyOutcome {
    Allow,
    Deny(VmError),
}

impl crate::vm::Vm {
    pub(super) async fn call_harness_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if let Some(capability) = handle.kind().capability_id() {
            let declared = crate::stdlib::all_builtin_manifest().iter().any(|entry| {
                matches!(
                    entry.contract.exposure,
                    harn_builtin_meta::BuiltinExposure::HarnessMethod {
                        capability: candidate,
                        method: candidate_method,
                    } if candidate == capability && candidate_method == method
                )
            });
            if !declared {
                return Err(method_unsupported(handle, method));
            }
        }
        // Capability-handle methods that adjust the harness itself
        // (`with_net_policy`, `is_quarantined`) run independent of the
        // backing mode so scripts can reshape the handle even under
        // null/mock harnesses. Per issue #1913.
        if handle.kind() == HarnessKind::Root {
            match method {
                "with_net_policy" | "is_quarantined" => {
                    return self
                        .call_harness_root_capability_method(handle, method, args)
                        .await;
                }
                _ => {}
            }
        }
        if let HarnessMode::Null(state) = handle.inner().mode() {
            state.record_deny(handle.kind(), method, args);
            return Err(VmError::CategorizedError {
                message: format!("NullHarness denied {}::{method}", handle.kind().type_name()),
                category: ErrorCategory::ToolRejected,
            });
        }
        if let Some(capability) = handle.kind().capability_id() {
            let defers_to_selected_tool = capability == harn_builtin_meta::CapabilityId::Tools
                && matches!(
                    method,
                    "invoke" | "dispatch_agent_call" | "dispatch_agent_batch"
                );
            if matches!(handle.inner().mode(), HarnessMode::Mock(_)) {
                // A mock fixture is authority to return deterministic test
                // data, not authority to perform the represented real-world
                // effect. Preserve the effect receipt while deliberately
                // bypassing the real execution ceiling.
                if !defers_to_selected_tool {
                    self.record_capability_effects(capability, method, args);
                }
            } else {
                if !defers_to_selected_tool {
                    crate::orchestration::enforce_current_policy_for_capability(
                        capability, method, args,
                    )?;
                    self.record_capability_effects(capability, method, args);
                }
            }
            if capability != harn_builtin_meta::CapabilityId::Testing {
                if let Some(response) = handle.inner().fixtures().dispatch(capability, method, args)
                {
                    return response;
                }
            }
        }
        if let Some(capability) = handle.kind().capability_id() {
            if let Some(dispatch) = self
                .capability_methods
                .get(&(capability, method.to_string()))
                .cloned()
            {
                let qualified_name = format!("harness.{}.{method}", capability.field_name());
                return self
                    .call_builtin_entry(&qualified_name, dispatch, args.to_vec())
                    .await;
            }
        }
        // Macro-emitted methods whose implementation name already equals
        // their public Harness method need no handwritten dispatcher entry.
        // The contract check above proves the name belongs to this capability;
        // use the ordinary builtin registry as the single implementation
        // owner. Host-provided capability methods still win above.
        if handle.kind().capability_id().is_some()
            && (self.builtins.contains_key(method) || self.async_builtins.contains_key(method))
        {
            return self.call_named_builtin(method, args.to_vec()).await;
        }
        let sync_result = {
            let _interrupt = self.sync_builtin_interrupt_guard();
            Self::call_harness_method_sync_fast(
                &mut self.output,
                &self.executed_effects,
                handle,
                method,
                args,
            )
        };
        if let Some(result) = sync_result {
            return result;
        }
        // Enforce per-harness `NetPolicy` (issue #1913) ahead of mock
        // dispatch so audit_only / quarantine outcomes apply uniformly
        // to real and mock paths.
        if handle.kind() == HarnessKind::Net {
            if let Some(decision) = self
                .evaluate_net_policy_for_method(handle, method, args)
                .await?
            {
                match decision {
                    NetPolicyOutcome::Allow => {}
                    NetPolicyOutcome::Deny(err) => return Err(err),
                }
            }
        }
        if matches!(handle.inner().mode(), HarnessMode::Mock(_))
            && !matches!(handle.kind(), HarnessKind::Runtime | HarnessKind::Testing)
        {
            return self.call_mock_harness_method(handle, method, args).await;
        }
        match handle.kind() {
            HarnessKind::Root => self.call_harness_root_method(handle, method, args).await,
            HarnessKind::Stdio => self.call_harness_stdio_method(handle, method, args),
            HarnessKind::Term => self.call_harness_term_method(handle, method, args),
            HarnessKind::Clock => self.call_harness_clock_method(handle, method, args).await,
            HarnessKind::System if method == "host_conditions" => {
                let mut options = crate::value::DictMap::new();
                options.insert(crate::value::intern_key("schema_version"), VmValue::Int(1));
                self.call_named_builtin(
                    "hostlib_host_conditions_sample",
                    vec![VmValue::dict(options)],
                )
                .await
            }
            HarnessKind::System
                if matches!(
                    method,
                    "security_policy" | "security_stamp_directive" | "security_verify_directive"
                ) =>
            {
                self.call_named_builtin(&format!("__{method}"), args.to_vec())
                    .await
            }
            HarnessKind::System => self.call_harness_system_method(handle, method, args),
            HarnessKind::Fs => self.call_harness_fs_method(handle, method, args).await,
            HarnessKind::Env => self.call_harness_env_method(handle, method, args),
            HarnessKind::Random => self.call_harness_random_method(handle, method, args),
            HarnessKind::Net => self.call_harness_net_method(handle, method, args).await,
            HarnessKind::Process => self.call_harness_process_method(handle, method, args).await,
            HarnessKind::Channels => {
                self.call_harness_channels_method(handle, method, args)
                    .await
            }
            HarnessKind::Secrets => self.call_harness_secrets_method(handle, method, args).await,
            HarnessKind::Llm => self.call_harness_llm_method(handle, method, args).await,
            HarnessKind::Tenant => self.call_harness_tenant_method(handle, method, args),
            HarnessKind::Auth => self.call_harness_auth_method(handle, method, args).await,
            HarnessKind::Obs => self.call_harness_obs_method(handle, method, args).await,
            HarnessKind::Verdict => self.call_harness_verdict_method(handle, method, args).await,
            HarnessKind::Tools => {
                self.call_tools_capability_method(handle, method, args)
                    .await
            }
            HarnessKind::Runtime => {
                self.call_runtime_capability_method(handle, method, args)
                    .await
            }
            HarnessKind::Interaction => {
                self.call_interaction_capability_method(handle, method, args)
                    .await
            }
            HarnessKind::Project => {
                self.call_project_capability_method(handle, method, args)
                    .await
            }
            HarnessKind::Testing => {
                self.call_testing_capability_method(handle, method, args)
                    .await
            }
            HarnessKind::Embed if method == "text" => {
                self.call_named_builtin("__embed", args.to_vec()).await
            }
            HarnessKind::Memory => {
                let builtin = match method {
                    "open" => "__memory_open",
                    "store" => "__memory_store",
                    "recall" => "__memory_recall",
                    "summarize" => "__memory_summarize",
                    "forget" => "__memory_forget",
                    "update" => "__memory_update",
                    "list" => "__memory_list",
                    _ => return Err(method_unsupported(handle, method)),
                };
                self.call_named_builtin(builtin, args.to_vec()).await
            }
            HarnessKind::Sqlite if method == "open" => {
                self.call_named_builtin("sqlite_open", args.to_vec()).await
            }
            HarnessKind::Postgres => {
                let builtin = match method {
                    "connect" => "pg_connect",
                    "pool" => "pg_pool",
                    _ => return Err(method_unsupported(handle, method)),
                };
                self.call_named_builtin(builtin, args.to_vec()).await
            }
            HarnessKind::Agent => {
                let transcript_builtin = match method {
                    "transcript_inject_reminder" => Some("__transcript_inject_reminder"),
                    "transcript_clear_reminders" => Some("__transcript_clear_reminders"),
                    _ => None,
                };
                if let Some(builtin) = transcript_builtin {
                    return self.call_named_builtin(builtin, args.to_vec()).await;
                }
                let worker_builtin = match method {
                    "worker_spawn" => Some("__host_worker_spawn"),
                    "parse_resume_conditions" => Some("__host_resume_conditions_parse"),
                    "worker_send_input" => Some("__host_worker_send_input"),
                    "worker_trigger" => Some("__host_worker_trigger"),
                    "worker_wait" => Some(
                        if args
                            .first()
                            .and_then(VmValue::as_dict)
                            .and_then(|task| task.get("_type"))
                            .is_some_and(|kind| kind.display() == "pool_task")
                        {
                            "__pool_wait"
                        } else {
                            "__host_worker_wait"
                        },
                    ),
                    "worker_stop" => Some("__host_worker_stop"),
                    "worker_close" => Some("__host_worker_close"),
                    "worker_suspend" => Some("__host_worker_suspend"),
                    "worker_resume" => Some("__host_worker_resume"),
                    "worker_list" => Some("__host_worker_list"),
                    _ => None,
                };
                if let Some(builtin) = worker_builtin {
                    return self.call_named_builtin(builtin, args.to_vec()).await;
                }
                if let Some(suffix) = method.strip_prefix("state_") {
                    return self
                        .call_named_builtin(&format!("__agent_state_{suffix}"), args.to_vec())
                        .await;
                }
                let host_primitive = method.starts_with("session_")
                    || matches!(
                        method,
                        "emit_event"
                            | "reminder_providers_fire"
                            | "capture_events"
                            | "budget_pre_call_blocked"
                            | "record_native_tool_fallback"
                            | "record_compaction"
                            | "daemon_snapshot"
                            | "daemon_wait"
                    );
                let builtin = if host_primitive {
                    format!("__host_agent_{method}")
                } else {
                    format!("agent_session_{method}")
                };
                self.call_named_builtin(&builtin, args.to_vec()).await
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    pub(in crate::vm) fn call_harness_method_sync_fast(
        output: &mut String,
        executed_effects: &std::sync::Arc<
            std::sync::Mutex<std::collections::BTreeSet<crate::orchestration::EffectRecord>>,
        >,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        if handle.kind() == HarnessKind::Root {
            return None;
        }
        let capability = handle
            .kind()
            .capability_id()
            .expect("non-root harness kind has a capability id");
        Self::record_capability_effects_into(executed_effects, capability, method, args);
        if !crate::stdlib::all_builtin_manifest().iter().any(|entry| {
            matches!(
                entry.contract.exposure,
                harn_builtin_meta::BuiltinExposure::HarnessMethod {
                    capability: candidate,
                    method: candidate_method,
                } if candidate == capability && candidate_method == method
            )
        }) {
            return Some(Err(method_unsupported(handle, method)));
        }
        if let HarnessMode::Null(state) = handle.inner().mode() {
            state.record_deny(handle.kind(), method, args);
            return Some(Err(VmError::CategorizedError {
                message: format!("NullHarness denied {}::{method}", handle.kind().type_name()),
                category: ErrorCategory::ToolRejected,
            }));
        }
        if matches!(handle.inner().mode(), HarnessMode::Mock(_)) {
            return Self::call_mock_harness_method_sync_fast(handle, method, args);
        }
        match handle.kind() {
            HarnessKind::Stdio => Self::call_harness_stdio_method_sync_fast(output, method, args),
            HarnessKind::Term => Self::call_harness_term_method_sync_fast(method, args),
            HarnessKind::Clock => Self::call_harness_clock_method_sync_fast(handle, method),
            HarnessKind::Env => Self::call_harness_env_method_sync_fast(method, args),
            HarnessKind::Random => Self::call_harness_random_method_sync_fast(method, args),
            HarnessKind::Tenant => Self::call_harness_tenant_method_sync_fast(method, args),
            HarnessKind::Auth => Self::call_harness_auth_method_sync_fast(method, args),
            HarnessKind::Root
            | HarnessKind::Fs
            | HarnessKind::Net
            | HarnessKind::Process
            | HarnessKind::System
            | HarnessKind::Secrets
            | HarnessKind::Llm
            | HarnessKind::Obs
            | HarnessKind::Verdict => None,
            _ => None,
        }
    }

    fn call_harness_clock_method_sync_fast(
        handle: &VmHarness,
        method: &str,
    ) -> Option<Result<VmValue, VmError>> {
        let clock = handle.inner().clock();
        match method {
            "now_ms" => Some(Ok(VmValue::Int(crate::clock::now_wall_ms(clock.as_ref())))),
            "timestamp" => Some(Ok(VmValue::Float(
                crate::clock::now_wall_ms(clock.as_ref()) as f64 / 1_000.0,
            ))),
            "monotonic_ms" | "elapsed" => Some(Ok(VmValue::Int(clock.monotonic_ms()))),
            "date_iso" => {
                let millis = crate::clock::now_wall_ms(clock.as_ref());
                let timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
                    .map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                    .unwrap_or_default();
                Some(Ok(VmValue::String(arcstr::ArcStr::from(timestamp))))
            }
            "now" => {
                let millis = crate::clock::now_wall_ms(clock.as_ref());
                Some(crate::stdlib::date_dict_from_millis(millis))
            }
            "sleep_ms" => None,
            _ => None,
        }
    }

    fn call_harness_stdio_method_sync_fast(
        output: &mut String,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        match method {
            "println" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stdout(output, &format!("{msg}\n"));
                Some(Ok(VmValue::Nil))
            }
            "print" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stdout(output, &msg);
                Some(Ok(VmValue::Nil))
            }
            "eprintln" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stderr(&format!("{msg}\n"));
                Some(Ok(VmValue::Nil))
            }
            "eprint" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stderr(&msg);
                Some(Ok(VmValue::Nil))
            }
            "read_line" => {
                if args.is_empty() {
                    Some(Ok(read_line_legacy_value()))
                } else {
                    Some(read_line_structured_value(args))
                }
            }
            "prompt" => Some(prompt_user_value(args, output)),
            _ => None,
        }
    }

    fn call_harness_term_method_sync_fast(
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        match method {
            "width" => Some(Ok(VmValue::Int(crate::term::width() as i64))),
            "height" => Some(Ok(VmValue::Int(crate::term::height() as i64))),
            "is_tty" => {
                let stream = match string_arg(args, 0, "HarnessTerm.is_tty") {
                    Ok(stream) => stream,
                    Err(err) => return Some(Err(err)),
                };
                Some(Ok(VmValue::Bool(crate::stdlib::io::is_tty_for(stream))))
            }
            "read_password" => {
                let prompt = match optional_string_arg(args, 0, "HarnessTerm.read_password") {
                    Ok(prompt) => prompt,
                    Err(err) => return Some(Err(err)),
                };
                if args.len() > 1 {
                    return Some(Err(VmError::TypeError(
                        "HarnessTerm.read_password expects at most one prompt argument".to_string(),
                    )));
                }
                Some(crate::stdlib::io::read_password_legacy_value(prompt))
            }
            _ => None,
        }
    }

    fn call_harness_env_method_sync_fast(
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        match method {
            "get" => {
                let name = match string_arg(args, 0, "HarnessEnv.get") {
                    Ok(name) => name,
                    Err(err) => return Some(Err(err)),
                };
                Some(Ok(crate::stdlib::process::read_env_value(name)
                    .map(vm_string)
                    .unwrap_or(VmValue::Nil)))
            }
            "get_or" => {
                let name = match string_arg(args, 0, "HarnessEnv.get_or") {
                    Ok(name) => name,
                    Err(err) => return Some(Err(err)),
                };
                let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
                Some(Ok(crate::stdlib::process::read_env_value(name)
                    .map(vm_string)
                    .unwrap_or(default)))
            }
            _ => None,
        }
    }

    fn call_harness_random_method_sync_fast(
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        use rand::seq::SliceRandom;
        use rand::{Rng, RngExt};
        match method {
            "f64" => Some(Ok(VmValue::Float(rand::rng().random()))),
            "u64" => {
                let value: u64 = rand::rng().random();
                Some(Ok(VmValue::Int(value.min(i64::MAX as u64) as i64)))
            }
            "range" => {
                let min = match args.first().and_then(|v| v.as_int()) {
                    Some(min) => min,
                    None => {
                        return Some(Err(VmError::TypeError(
                            "HarnessRandom.range expects an integer min argument".to_string(),
                        )))
                    }
                };
                let max = match args.get(1).and_then(|v| v.as_int()) {
                    Some(max) => max,
                    None => {
                        return Some(Err(VmError::TypeError(
                            "HarnessRandom.range expects an integer max argument".to_string(),
                        )))
                    }
                };
                if min > max {
                    return Some(Ok(VmValue::Nil));
                }
                Some(Ok(VmValue::Int(rand::rng().random_range(min..=max))))
            }
            "choice" => {
                let Some(VmValue::List(items)) = args.first() else {
                    return Some(Ok(VmValue::Nil));
                };
                if items.is_empty() {
                    return Some(Ok(VmValue::Nil));
                }
                let idx = rand::rng().random_range(0..items.len());
                Some(Ok(items[idx].clone()))
            }
            "shuffle" => {
                let Some(VmValue::List(items)) = args.first() else {
                    return Some(Ok(VmValue::Nil));
                };
                let mut shuffled = items.as_ref().clone();
                shuffled.shuffle(&mut rand::rng());
                Some(Ok(VmValue::List(std::sync::Arc::new(shuffled))))
            }
            "uuid" => Some(Ok(VmValue::String(arcstr::ArcStr::from(
                uuid::Uuid::new_v4().to_string(),
            )))),
            "uuid_v7" => Some(Ok(VmValue::String(arcstr::ArcStr::from(
                uuid::Uuid::now_v7().to_string(),
            )))),
            "bytes" => {
                let length = match args.first().and_then(VmValue::as_int) {
                    Some(length) if (1..=1024).contains(&length) => length as usize,
                    _ => {
                        return Some(Err(VmError::TypeError(
                            "HarnessRandom.bytes expects a length from 1 through 1024".to_string(),
                        )))
                    }
                };
                let mut bytes = vec![0u8; length];
                rand::rng().fill_bytes(&mut bytes);
                Some(Ok(VmValue::Bytes(std::sync::Arc::new(bytes))))
            }
            _ => None,
        }
    }

    fn call_harness_tenant_method_sync_fast(
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        if !args.is_empty() {
            return Some(Err(VmError::TypeError(format!(
                "HarnessTenant.{method} takes no arguments"
            ))));
        }
        match method {
            "id" => Some(match crate::harness_tenant::current_tenant_id() {
                Some(tenant) => Ok(vm_string(tenant.0)),
                None => Err(VmError::CategorizedError {
                    message: crate::harness_tenant::MISSING_TENANT_MESSAGE.to_string(),
                    category: ErrorCategory::Auth,
                }),
            }),
            "try_id" => Some(Ok(crate::harness_tenant::current_tenant_id()
                .map(|tenant| vm_string(tenant.0))
                .unwrap_or(VmValue::Nil))),
            _ => None,
        }
    }

    fn call_mock_harness_method_sync_fast(
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        let HarnessMode::Mock(state) = handle.inner().mode() else {
            unreachable!("mock sync fast path is only called for mock harnesses");
        };
        let result = match handle.kind() {
            HarnessKind::Stdio => match method {
                "println" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stdio(&format!("{msg}\n"));
                    Some(Ok(VmValue::Nil))
                }
                "print" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stdio(&msg);
                    Some(Ok(VmValue::Nil))
                }
                "eprintln" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stderr(&format!("{msg}\n"));
                    Some(Ok(VmValue::Nil))
                }
                "eprint" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stderr(&msg);
                    Some(Ok(VmValue::Nil))
                }
                "read_line" => Some(Ok(mock_read_line_value(state, args))),
                "prompt" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stdio(&msg);
                    Some(Ok(mock_read_line_value(state, &[])))
                }
                _ => None,
            },
            HarnessKind::Term => match method {
                "width" => Some(Ok(VmValue::Int(
                    mock_term_dimension(state.env_get("COLUMNS"), 80) as i64,
                ))),
                "height" => Some(Ok(VmValue::Int(
                    mock_term_dimension(state.env_get("LINES"), 24) as i64,
                ))),
                "is_tty" => Some(Ok(VmValue::Bool(false))),
                "read_password" => {
                    let prompt = match optional_string_arg(args, 0, "HarnessTerm.read_password") {
                        Ok(prompt) => prompt,
                        Err(err) => return Some(Err(err)),
                    };
                    if args.len() > 1 {
                        return Some(Err(VmError::TypeError(
                            "HarnessTerm.read_password expects at most one prompt argument"
                                .to_string(),
                        )));
                    }
                    if !prompt.is_empty() {
                        state.push_stderr(prompt);
                    }
                    Some(state.pop_stdin_line().map(vm_string).ok_or_else(|| {
                        VmError::Runtime("HarnessTerm.read_password: stdin reached EOF".to_string())
                    }))
                }
                _ => None,
            },
            HarnessKind::Clock => {
                let clock = handle.inner().clock();
                match method {
                    "now_ms" => Some(Ok(VmValue::Int(crate::clock::now_wall_ms(clock.as_ref())))),
                    "timestamp" => Some(Ok(VmValue::Float(
                        crate::clock::now_wall_ms(clock.as_ref()) as f64 / 1_000.0,
                    ))),
                    "monotonic_ms" | "elapsed" => Some(Ok(VmValue::Int(clock.monotonic_ms()))),
                    "date_iso" => {
                        let millis = crate::clock::now_wall_ms(clock.as_ref());
                        let timestamp =
                            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
                                .map(|value| {
                                    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                                })
                                .unwrap_or_default();
                        Some(Ok(VmValue::String(arcstr::ArcStr::from(timestamp))))
                    }
                    "sleep_ms" => {
                        let ms = match sleep_ms_arg(args) {
                            Ok(ms) => ms,
                            Err(err) => return Some(Err(err)),
                        };
                        if ms > 0 {
                            state.advance_clock(Duration::from_millis(ms as u64));
                        }
                        Some(Ok(VmValue::Nil))
                    }
                    _ => None,
                }
            }
            HarnessKind::Env => match method {
                "get" => {
                    let key = match string_arg(args, 0, "HarnessEnv.get") {
                        Ok(key) => key,
                        Err(err) => return Some(Err(err)),
                    };
                    Some(Ok(state
                        .env_get(key)
                        .map(vm_string)
                        .unwrap_or(VmValue::Nil)))
                }
                "get_or" => {
                    let key = match string_arg(args, 0, "HarnessEnv.get_or") {
                        Ok(key) => key,
                        Err(err) => return Some(Err(err)),
                    };
                    let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
                    Some(Ok(state.env_get(key).map(vm_string).unwrap_or(default)))
                }
                _ => None,
            },
            HarnessKind::Random => match method {
                "u64" => Some(
                    state
                        .next_random_u64()
                        .map(|value| VmValue::Int(value.min(i64::MAX as u64) as i64))
                        .ok_or_else(|| VmError::CategorizedError {
                            message: "MockHarness has no random_u64 response".to_string(),
                            category: ErrorCategory::NotFound,
                        }),
                ),
                "uuid" | "uuid_v7" => Some(
                    state
                        .next_random_u64()
                        .map(|value| {
                            let value = (u128::from(value) << 64) | u128::from(value);
                            VmValue::String(arcstr::ArcStr::from(
                                uuid::Uuid::from_u128(value).to_string(),
                            ))
                        })
                        .ok_or_else(|| VmError::CategorizedError {
                            message: format!("MockHarness has no {method} response"),
                            category: ErrorCategory::NotFound,
                        }),
                ),
                "bytes" => {
                    let length = match args.first().and_then(VmValue::as_int) {
                        Some(length) if (1..=1024).contains(&length) => length as usize,
                        _ => {
                            return Some(Err(VmError::TypeError(
                                "HarnessRandom.bytes expects a length from 1 through 1024"
                                    .to_string(),
                            )))
                        }
                    };
                    Some(
                        state
                            .next_random_u64()
                            .map(|value| {
                                let seed = value.to_le_bytes();
                                let bytes =
                                    (0..length).map(|index| seed[index % seed.len()]).collect();
                                VmValue::Bytes(std::sync::Arc::new(bytes))
                            })
                            .ok_or_else(|| VmError::CategorizedError {
                                message: "MockHarness has no random_u64 response".to_string(),
                                category: ErrorCategory::NotFound,
                            }),
                    )
                }
                _ => None,
            },
            HarnessKind::System => {
                let json = match method {
                    "cpu" => serde_json::json!({
                        "count": 1,
                        "physical_count": 1,
                        "model": "mock-cpu",
                        "frequency_mhz": 0u64,
                        "usage_pct": 0.0,
                    }),
                    "memory" => serde_json::json!({
                        "total_bytes": 0u64,
                        "used_bytes": 0u64,
                        "available_bytes": 0u64,
                        "total_gb": 0.0,
                        "used_gb": 0.0,
                        "available_gb": 0.0,
                        "pressure": "unknown",
                    }),
                    "gpus" | "gpu" => serde_json::Value::Array(Vec::new()),
                    "temperature" => serde_json::json!({"components": []}),
                    "platform" => serde_json::json!({
                        "os": "mock",
                        "arch": "mock",
                        "version": "mock",
                        "kernel": "mock",
                        "long_os_version": "mock",
                        "hostname": "mock",
                    }),
                    "identity" => serde_json::json!({
                        "username": "mock",
                        "hostname": "mock",
                        "pid": 1,
                    }),
                    "processes" => serde_json::Value::Array(vec![serde_json::json!({
                        "pid": 1,
                        "parent_pid": serde_json::Value::Null,
                        "name": "harn",
                        "cpu_pct": 0.0,
                        "mem_bytes": 0u64,
                        "is_harn_owned": true,
                        "is_self": true,
                    })]),
                    _ => return None,
                };
                Some(Ok(crate::stdlib::json_to_vm_value(&json)))
            }
            HarnessKind::Tenant => Self::call_harness_tenant_method_sync_fast(method, args),
            HarnessKind::Auth => Self::call_harness_auth_method_sync_fast(method, args),
            HarnessKind::Root
            | HarnessKind::Fs
            | HarnessKind::Net
            | HarnessKind::Process
            | HarnessKind::Secrets
            | HarnessKind::Llm
            | HarnessKind::Obs
            | HarnessKind::Verdict => None,
            _ => None,
        };
        if result.is_some() {
            state.record_call(handle.kind(), method, args);
        }
        result
    }

    /// Root-handle capability methods that bypass the null/mock-mode
    /// gating because they reshape the `Harness` value itself rather
    /// than touch the OS. Currently `with_net_policy` (issue #1913)
    /// and `is_quarantined`. Adding new entries here requires the
    /// caller-side allowlist in `call_harness_method` to be updated
    /// at the same time.
    async fn call_harness_root_capability_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "with_net_policy" => {
                let policy_value = args.first().ok_or_else(|| {
                    VmError::TypeError(
                        "Harness.with_net_policy expects a NetPolicy.create({...}) result"
                            .to_string(),
                    )
                })?;
                let dict = policy_value.as_dict().ok_or_else(|| {
                    VmError::TypeError(format!(
                        "Harness.with_net_policy: expected a NetPolicy dict, got {}",
                        policy_value.type_name()
                    ))
                })?;
                let policy = crate::harness_net::parse::policy_from_dict(dict)?;
                let source = crate::harness::Harness::from_inner(handle.inner().clone());
                Ok(source.with_net_policy(policy).into_vm_value())
            }
            "is_quarantined" => Ok(VmValue::Bool(handle.inner().is_quarantined())),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    /// Apply the per-harness `NetPolicy` (issue #1913) to a
    /// `harness.net.*` method call. Returns `Ok(None)` when no policy
    /// is bound; `Ok(Some(Allow))` for an allowed (possibly audited)
    /// request; `Ok(Some(Deny))` carrying the typed VmError when the
    /// request must be blocked.
    async fn evaluate_net_policy_for_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<Option<NetPolicyOutcome>, VmError> {
        let Some(policy) = handle.inner().net_policy().cloned() else {
            return Ok(None);
        };
        // Bypass is honoured *after* the policy is bound so that
        // configuring a policy and forgetting to clear the env var
        // still leaves an audit trail.
        if harness_net::bypass_enabled() {
            let bypass_audit = NetPolicyAudit {
                method: method.to_string(),
                url: args
                    .first()
                    .map(|v| v.display())
                    .unwrap_or_else(|| "<missing>".to_string()),
                host: String::new(),
                port: None,
                reason: "HARN_NET_POLICY_BYPASS set; policy not enforced".to_string(),
                outcome: "bypass",
                bypass: true,
                matched_rule: None,
            };
            record_audit(&bypass_audit).await;
            return Ok(Some(NetPolicyOutcome::Allow));
        }
        let Some(url) = args.first().and_then(|v| match v {
            VmValue::String(s) => Some(s.as_str().to_string()),
            _ => None,
        }) else {
            // No URL argument — let the underlying method surface its
            // own type error.
            return Ok(None);
        };
        let decision = policy.evaluate(method, &url)?;
        match decision {
            NetPolicyDecision::Allow { audited, audit } => {
                if audited {
                    if let Some(audit) = &audit {
                        record_audit(audit).await;
                    }
                }
                Ok(Some(NetPolicyOutcome::Allow))
            }
            NetPolicyDecision::Deny { audit, quarantine } => {
                if quarantine {
                    handle.inner().mark_quarantined();
                }
                // Custom callback resolution: invoke the user closure
                // and respect its returned outcome string.
                if let OnViolation::Callback(closure) = policy.on_violation.clone() {
                    let request = violation_request_value(&audit);
                    match self.call_closure_pub(&closure, &[request]).await {
                        Ok(value) => match value {
                            VmValue::String(s) => {
                                let outcome = OnViolation::parse_str(s.as_str())?;
                                return self.apply_callback_outcome(handle, audit, outcome).await;
                            }
                            other => {
                                return Err(VmError::TypeError(format!(
                                    "NetPolicy.on_violation callback must return one of `error`, `audit_only`, `quarantine`, got {}",
                                    other.type_name()
                                )));
                            }
                        },
                        Err(err) => return Err(err),
                    }
                }
                record_audit(&audit).await;
                Ok(Some(NetPolicyOutcome::Deny(violation_vm_error(&audit))))
            }
        }
    }

    async fn apply_callback_outcome(
        &mut self,
        handle: &VmHarness,
        mut audit: NetPolicyAudit,
        outcome: OnViolation,
    ) -> Result<Option<NetPolicyOutcome>, VmError> {
        match outcome {
            OnViolation::Error => {
                audit.outcome = "error";
                record_audit(&audit).await;
                Ok(Some(NetPolicyOutcome::Deny(violation_vm_error(&audit))))
            }
            OnViolation::AuditOnly => {
                audit.outcome = "audit_only";
                record_audit(&audit).await;
                Ok(Some(NetPolicyOutcome::Allow))
            }
            OnViolation::Quarantine => {
                audit.outcome = "quarantine";
                handle.inner().mark_quarantined();
                record_audit(&audit).await;
                Ok(Some(NetPolicyOutcome::Deny(violation_vm_error(&audit))))
            }
            OnViolation::Callback(_) => Err(VmError::TypeError(
                "NetPolicy.on_violation callback may not return another callback".to_string(),
            )),
        }
    }

    fn call_harness_system_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        _args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        use crate::harness_system as sys;
        let json = match method {
            "cpu" => sys::cpu_snapshot(),
            "memory" => sys::memory_snapshot(),
            "gpus" | "gpu" => sys::gpus_snapshot(),
            "temperature" => sys::temperature_snapshot(),
            "platform" => sys::platform_snapshot(),
            "identity" => sys::identity_snapshot(),
            "processes" => sys::processes_snapshot(),
            _ => return Err(method_unsupported(handle, method)),
        };
        Ok(crate::stdlib::json_to_vm_value(&json))
    }

    async fn call_harness_secrets_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let provider = crate::connectors::harn_module::active_harn_connector_ctx()
            .map(|ctx| ctx.secrets)
            .or_else(|| handle.inner().secret_provider().cloned())
            .ok_or_else(|| VmError::CategorizedError {
                message: "HarnessSecrets: no secret provider bound to this harness".to_string(),
                category: ErrorCategory::NotFound,
            })?;
        match method {
            "read" | "read_bytes" => {
                if args.len() > 2 {
                    return Err(VmError::TypeError(format!(
                        "{}.{method} expects name and optional scope",
                        handle.type_name()
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let scope = secret_scope_arg(args.get(1))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                let secret = provider
                    .read_scoped(crate::secrets::SecretReadRequest {
                        id,
                        scope,
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                if method == "read_bytes" {
                    return Ok(VmValue::Bytes(std::sync::Arc::new(
                        secret.with_exposed(|bytes| bytes.to_vec()),
                    )));
                }
                let text = secret.with_exposed(|bytes| {
                    std::str::from_utf8(bytes)
                        .map(str::to_string)
                        .map_err(|error| {
                            VmError::TypeError(format!(
                                "{}.read secret `{name}` was not UTF-8: {error}",
                                handle.type_name()
                            ))
                        })
                })?;
                Ok(vm_string(text))
            }
            "write" => {
                if args.len() > 4 {
                    return Err(VmError::TypeError(format!(
                        "{}.write expects name, value, optional scope, and optional ttl",
                        handle.type_name()
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let value = secret_value_arg(handle, method, args.get(1), "value")?;
                let scope = secret_scope_arg(args.get(2))?;
                let ttl =
                    optional_duration_arg(args.get(3), &format!("{}.write", handle.type_name()))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                let receipt = provider
                    .write_scoped(crate::secrets::SecretWriteRequest {
                        id,
                        scope,
                        value,
                        options: crate::secrets::SecretWriteOptions { ttl },
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                Ok(secret_write_receipt_value(receipt))
            }
            "rotate" => {
                if args.len() > 4 {
                    return Err(VmError::TypeError(format!(
                        "{}.rotate expects name, generator/value, optional scope, and optional options",
                        handle.type_name()
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let value = match args.get(1) {
                    Some(VmValue::Closure(closure)) => {
                        let generated = self.call_closure_pub(closure, &[]).await?;
                        secret_value_from_vm(handle, method, &generated, "generator result")?
                    }
                    other => secret_value_arg(handle, method, other, "value")?,
                };
                let scope = secret_scope_arg(args.get(2))?;
                let options = secret_rotation_options_arg(args.get(3))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                let receipt = provider
                    .rotate_scoped(crate::secrets::SecretRotateRequest {
                        id,
                        scope,
                        value,
                        options,
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                Ok(secret_rotation_receipt_value(receipt))
            }
            "delete" => {
                if args.len() > 2 {
                    return Err(VmError::TypeError(format!(
                        "{}.delete expects name and optional scope",
                        handle.type_name()
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let scope = secret_scope_arg(args.get(1))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                provider
                    .delete_scoped(crate::secrets::SecretDeleteRequest {
                        id,
                        scope,
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                Ok(VmValue::Nil)
            }
            "lease" | "lease_bytes" => {
                if args.len() > 3 {
                    return Err(VmError::TypeError(format!(
                        "{}.{method} expects name, duration, and optional scope",
                        handle.type_name(),
                    )));
                }
                let name = secret_name_arg(handle, method, args.first())?;
                let duration = required_duration_arg(
                    args.get(1),
                    &format!("{}.{}", handle.type_name(), method),
                )?;
                let scope = secret_scope_arg(args.get(2))?;
                let id = secret_id_for_scope(&name, &scope)?;
                crate::secrets::ensure_scoped_secret_access_allowed(method, &id)
                    .map_err(secret_error_to_vm)?;
                let grant = provider
                    .lease_scoped(crate::secrets::SecretLeaseRequest {
                        id,
                        scope,
                        duration,
                        audit: secret_audit_context(),
                    })
                    .await
                    .map_err(secret_error_to_vm)?;
                Ok(secret_lease_grant_value(method == "lease_bytes", grant)?)
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    async fn call_harness_llm_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "call" => self.call_named_builtin("llm_call", args.to_vec()).await,
            "call_safe" => {
                self.call_named_builtin("llm_call_safe", args.to_vec())
                    .await
            }
            "call_structured" => {
                self.call_named_builtin("llm_call_structured", args.to_vec())
                    .await
            }
            "call_structured_safe" => {
                self.call_named_builtin("llm_call_structured_safe", args.to_vec())
                    .await
            }
            "call_structured_result" => {
                self.call_named_builtin("llm_call_structured_result", args.to_vec())
                    .await
            }
            "recover_schema" => {
                self.call_named_builtin("schema_recover", args.to_vec())
                    .await
            }
            "completion" => {
                self.call_named_builtin("llm_completion", args.to_vec())
                    .await
            }
            "stream" => self.call_named_builtin("llm_stream", args.to_vec()).await,
            "with_rate_limit" => {
                self.call_named_builtin("with_rate_limit", args.to_vec())
                    .await
            }
            "stream_call" => {
                self.call_named_builtin("llm_stream_call", args.to_vec())
                    .await
            }
            "mock_clear" => {
                self.call_named_builtin("llm_mock_clear", args.to_vec())
                    .await
            }
            "mock_enqueue" => self.call_named_builtin("llm_mock", args.to_vec()).await,
            "mock_calls" => {
                self.call_named_builtin("llm_mock_calls", args.to_vec())
                    .await
            }
            "mock_snapshot" => {
                self.call_named_builtin("llm_mock_snapshot", args.to_vec())
                    .await
            }
            "mock_push_scope" => {
                self.call_named_builtin("llm_mock_push_scope", args.to_vec())
                    .await
            }
            "mock_pop_scope" => {
                self.call_named_builtin("llm_mock_pop_scope", args.to_vec())
                    .await
            }
            "upload_file" => {
                self.call_named_builtin("__files_upload", args.to_vec())
                    .await
            }
            "session_cost" | "budget" | "budget_remaining" => {
                self.call_named_builtin(&format!("__llm_{method}"), args.to_vec())
                    .await
            }
            "catalog" => Ok(crate::llm::config_builtins::llm_catalog_value()),
            "catalog_refresh" => {
                if args.len() > 1 {
                    return Err(VmError::TypeError(
                        "HarnessLlm.catalog_refresh expects at most one options dict".to_string(),
                    ));
                }
                let options = crate::llm::config_builtins::parse_catalog_refresh_options(
                    args.first(),
                    "HarnessLlm.catalog_refresh",
                )?;
                let report = crate::provider_catalog::refresh_runtime_catalog(options).await;
                let json = serde_json::to_value(report).map_err(|error| {
                    VmError::Runtime(format!(
                        "HarnessLlm.catalog_refresh: serialize result: {error}"
                    ))
                })?;
                Ok(crate::stdlib::json_to_vm_value(&json))
            }
            "providers" => Ok(crate::llm::config_builtins::llm_provider_status_value()),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    fn call_harness_tenant_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        Self::call_harness_tenant_method_sync_fast(method, args)
            .unwrap_or_else(|| Err(method_unsupported(handle, method)))
    }

    async fn call_harness_auth_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let oauth_builtin = match method {
            "oauth_storage_memory" => Some("__oauth_storage_memory_handle"),
            "oauth_storage_file" => Some("__oauth_storage_file_handle"),
            "oauth_storage_cloud" => Some("__oauth_storage_cloud_handle"),
            "oauth_storage_get" => Some("__oauth_storage_get"),
            "oauth_storage_set" => Some("__oauth_storage_set"),
            "oauth_storage_delete" => Some("__oauth_storage_delete"),
            "oauth_storage_with_refresh_lock" => Some("__oauth_storage_with_refresh_lock"),
            "oauth_registration_store" => Some("__oauth_dynreg_store_handle"),
            "oauth_register_client" => Some("__oauth_dynreg_register"),
            "oauth_registered_client" => Some("__oauth_dynreg_get"),
            "oauth_registered_clients" => Some("__oauth_dynreg_list"),
            _ => None,
        };
        if let Some(builtin) = oauth_builtin {
            return self.call_named_builtin(builtin, args.to_vec()).await;
        }
        Self::call_harness_auth_method_sync_fast(method, args)
            .unwrap_or_else(|| Err(method_unsupported(handle, method)))
    }

    /// Read-only getters over the ambient authenticated principal (see
    /// [`crate::harness_auth`]). Pure thread-local reads — no host state —
    /// so the whole surface rides the sync-fast path. `subject`/`scheme`
    /// raise a typed [`ErrorCategory::Auth`] error when no principal is
    /// bound (mirroring `harness.tenant.id()`); the `try_*` and `kind`
    /// getters return `nil`, and `scopes`/`has_scope`/`is_authenticated`
    /// degrade to empty/false so an unauthenticated route can branch
    /// without try/catch.
    fn call_harness_auth_method_sync_fast(
        method: &str,
        args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        use crate::harness_auth::{current_auth_principal, MISSING_PRINCIPAL_MESSAGE};

        // `has_scope` is the one arity-1 method; everything else is nullary.
        if method == "has_scope" {
            let scope = match args {
                [VmValue::String(scope)] => scope.to_string(),
                [other] => {
                    return Some(Err(VmError::TypeError(format!(
                        "HarnessAuth.has_scope expects a string scope, got {}",
                        other.type_name()
                    ))));
                }
                _ => {
                    return Some(Err(VmError::TypeError(
                        "HarnessAuth.has_scope expects exactly one string argument".to_string(),
                    )));
                }
            };
            let granted = current_auth_principal()
                .map(|principal| principal.scopes.contains(&scope))
                .unwrap_or(false);
            return Some(Ok(VmValue::Bool(granted)));
        }

        let is_nullary_getter = matches!(
            method,
            "is_authenticated"
                | "scopes"
                | "subject"
                | "try_subject"
                | "scheme"
                | "try_scheme"
                | "kind"
        );
        if !is_nullary_getter {
            return None;
        }
        if !args.is_empty() {
            return Some(Err(VmError::TypeError(format!(
                "HarnessAuth.{method} takes no arguments"
            ))));
        }
        let principal = current_auth_principal();
        let auth_error = || VmError::CategorizedError {
            message: MISSING_PRINCIPAL_MESSAGE.to_string(),
            category: ErrorCategory::Auth,
        };
        match method {
            "is_authenticated" => Some(Ok(VmValue::Bool(principal.is_some()))),
            "scopes" => {
                let scopes = principal
                    .map(|principal| {
                        principal
                            .scopes
                            .iter()
                            .map(|scope| vm_string(scope.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Some(Ok(VmValue::List(std::sync::Arc::new(scopes))))
            }
            "subject" => Some(match principal {
                Some(principal) if !principal.subject.is_empty() => {
                    Ok(vm_string(principal.subject.clone()))
                }
                _ => Err(auth_error()),
            }),
            "try_subject" => Some(Ok(principal
                .filter(|principal| !principal.subject.is_empty())
                .map(|principal| vm_string(principal.subject.clone()))
                .unwrap_or(VmValue::Nil))),
            "scheme" => Some(match principal {
                Some(principal) if !principal.scheme.is_empty() => {
                    Ok(vm_string(principal.scheme.clone()))
                }
                _ => Err(auth_error()),
            }),
            "try_scheme" => Some(Ok(principal
                .filter(|principal| !principal.scheme.is_empty())
                .map(|principal| vm_string(principal.scheme.clone()))
                .unwrap_or(VmValue::Nil))),
            "kind" => Some(Ok(principal
                .and_then(|principal| principal.kind.clone())
                .map(vm_string)
                .unwrap_or(VmValue::Nil))),
            _ => None,
        }
    }

    async fn call_harness_obs_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        use crate::stdlib::observability::{
            emit_instrument, end_span_typed, log_typed, start_span_typed, MetricInstrument,
        };

        match method {
            "configure" => {
                self.call_named_builtin("__obs_configure", args.to_vec())
                    .await
            }
            "auto_backend" => {
                self.call_named_builtin("__obs_auto_backend", args.to_vec())
                    .await
            }
            "emit" => self.call_named_builtin("__obs_emit", args.to_vec()).await,
            "events" => self.call_named_builtin("__obs_events", args.to_vec()).await,
            "events_take" => {
                self.call_named_builtin("__obs_events_take", args.to_vec())
                    .await
            }
            "reset" => self.call_named_builtin("__obs_reset", args.to_vec()).await,
            "request_id" => {
                require_no_args(handle, method, args)?;
                Ok(crate::observability::request_id::current_request_id()
                    .map(vm_string)
                    .unwrap_or(VmValue::Nil))
            }
            "start_span" => {
                let name = obs_string_arg(handle, method, args.first(), "name")?;
                let attrs = obs_attrs_arg(handle, method, args.get(1))?;
                start_span_typed(name, attrs)
            }
            "end_span" => {
                let handle_arg = args.first().cloned().unwrap_or(VmValue::Nil);
                end_span_typed(handle_arg);
                Ok(VmValue::Nil)
            }
            "span" => {
                let name = obs_string_arg(handle, method, args.first(), "name")?;
                let attrs = obs_attrs_arg(handle, method, args.get(1))?;
                let callback = args.get(2).cloned().unwrap_or(VmValue::Nil);
                let span_handle = start_span_typed(name, attrs)?;
                // No callback → imperative mode: hand the span handle
                // back so the caller can close it with `end_span` later.
                // The other branches always close before returning so
                // an erroring closure can't leak a span.
                let closure = match callback {
                    VmValue::Nil => return Ok(span_handle),
                    VmValue::Closure(closure) => closure,
                    other => {
                        end_span_typed(span_handle);
                        return Err(VmError::TypeError(format!(
                            "{}.span callback must be a closure or nil, got {}",
                            handle.type_name(),
                            other.type_name()
                        )));
                    }
                };
                let result = self.call_closure_pub(&closure, &[]).await;
                end_span_typed(span_handle);
                result
            }
            "log" => {
                // Argument order matches `std/observability::log`:
                // `(message, level = "info", fields = {})`. Keeping the
                // two surfaces in lockstep means a script that imports
                // either one round-trips identically.
                let message = obs_string_arg(handle, method, args.first(), "message")?;
                let level = obs_optional_string_arg(handle, method, args.get(1), "level")?
                    .unwrap_or_else(|| "info".to_string());
                let fields = obs_attrs_arg(handle, method, args.get(2))?;
                let emitted = log_typed(message, level, fields)?;
                Ok(crate::stdlib::json_to_vm_value(&emitted))
            }
            "counter" | "histogram" | "gauge" => {
                let instrument = match method {
                    "counter" => MetricInstrument::Counter,
                    "histogram" => MetricInstrument::Histogram,
                    "gauge" => MetricInstrument::Gauge,
                    _ => unreachable!("outer match restricts the arm"),
                };
                let name = obs_string_arg(handle, method, args.first(), "name")?;
                let value_arg = args.get(1).cloned().unwrap_or(VmValue::Nil);
                let value_json = obs_number_arg(handle, method, &value_arg)?;
                let attrs = obs_attrs_arg(handle, method, args.get(2))?;
                let emitted = emit_instrument(instrument, name, value_json, attrs)?;
                Ok(crate::stdlib::json_to_vm_value(&emitted))
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    async fn call_harness_root_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "unsettled_state" => {
                let snapshot = crate::orchestration::unsettled_state_snapshot_async().await;
                Ok(crate::stdlib::json_to_vm_value(&snapshot.to_json()))
            }
            "is_empty" => {
                let empty = match args.first() {
                    Some(state) => state_counts(state)?.is_empty(),
                    None => crate::orchestration::unsettled_state_snapshot_async()
                        .await
                        .is_empty(),
                };
                Ok(VmValue::Bool(empty))
            }
            "counts" => match args.first() {
                Some(state) => Ok(crate::stdlib::json_to_vm_value(
                    &state_counts(state)?.to_json(),
                )),
                None => {
                    let snapshot = crate::orchestration::unsettled_state_snapshot_async().await;
                    Ok(crate::stdlib::json_to_vm_value(&snapshot.counts_json()))
                }
            },
            "summary" => match args.first() {
                Some(state) => Ok(VmValue::String(arcstr::ArcStr::from(
                    state_counts(state)?.summary().as_str(),
                ))),
                None => {
                    let snapshot = crate::orchestration::unsettled_state_snapshot_async().await;
                    Ok(VmValue::String(arcstr::ArcStr::from(snapshot.summary())))
                }
            },
            "resume_subagent" => {
                let handle_arg = args.first().cloned().ok_or_else(|| {
                    VmError::TypeError("Harness.resume_subagent expects a handle".to_string())
                })?;
                if let Some(input) = args.get(1).cloned() {
                    match self
                        .call_named_builtin(
                            "__host_worker_resume",
                            vec![handle_arg.clone(), input.clone()],
                        )
                        .await
                    {
                        Ok(value) => Ok(value),
                        Err(error) if error.to_string().contains("not suspended") => {
                            self.call_named_builtin(
                                "__host_worker_send_input",
                                vec![handle_arg, input],
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    }
                } else {
                    self.call_named_builtin("__host_worker_resume", vec![handle_arg])
                        .await
                }
            }
            "cancel_subagent" => {
                let handle_arg = args.first().cloned().ok_or_else(|| {
                    VmError::TypeError("Harness.cancel_subagent expects a handle".to_string())
                })?;
                self.call_named_builtin("__host_worker_close", vec![handle_arg])
                    .await
            }
            "wait_for_any_settlement" => {
                let snapshot = crate::orchestration::unsettled_state_snapshot_async().await;
                let status = if snapshot.is_empty() {
                    "settled"
                } else {
                    "unsettled"
                };
                Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
                    "status": status,
                    "timed_out": !snapshot.is_empty(),
                    "state": snapshot.to_json(),
                })))
            }
            "current_pipeline_id" => Ok(crate::orchestration::current_mutation_session()
                .and_then(|session| session.run_id.or(Some(session.session_id)))
                .map(|id| VmValue::String(arcstr::ArcStr::from(id)))
                .unwrap_or(VmValue::Nil)),
            "handoff_to" => Ok(record_handoff_envelope(args)),
            "emit_audit" => {
                let ctx = crate::vm::AsyncBuiltinCtx::from_vm(self.child_vm());
                Ok(record_emit_audit_with_hooks(&ctx, args).await)
            }
            "acknowledge_trigger" => Ok(acknowledge_trigger(args).await),
            "defer_trigger" => Ok(defer_trigger(args).await),
            "acknowledge_handoff" => Ok(acknowledge_handoff(args)),
            "finalize" => Ok(finalize_pipeline(args)),
            "spawn_settlement_agent" => {
                let ctx = crate::vm::AsyncBuiltinCtx::from_vm(self.child_vm());
                Ok(record_spawn_settlement_agent_with_hooks(&ctx, args).await)
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    fn call_harness_stdio_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        Self::call_harness_stdio_method_sync_fast(&mut self.output, method, args)
            .unwrap_or_else(|| Err(method_unsupported(handle, method)))
    }

    fn call_harness_term_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        Self::call_harness_term_method_sync_fast(method, args)
            .unwrap_or_else(|| Err(method_unsupported(handle, method)))
    }

    async fn call_harness_clock_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if let Some(result) = Self::call_harness_clock_method_sync_fast(handle, method) {
            return result;
        }
        let clock = handle.inner().clock();
        match method {
            "sleep_ms" => {
                let ms = sleep_ms_arg(args)?;
                if ms > 0 {
                    let sleep = clock.sleep(Duration::from_millis(ms as u64));
                    tokio::pin!(sleep);
                    let mut poll = tokio::time::interval(Duration::from_millis(10));
                    loop {
                        tokio::select! {
                            _ = &mut sleep => break,
                            _ = poll.tick() => {
                                if self.is_cancel_requested() {
                                    return Err(
                                        crate::stdlib::cancelled_vm_error(),
                                    );
                                }
                            }
                        }
                    }
                }
                Ok(VmValue::Nil)
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    /// Dispatch `harness.fs.*` in real mode by delegating to the existing
    /// fs builtin with the same name surface. Method names are normalized
    /// (e.g. `harness.fs.read_text` → `read_file`, `harness.fs.delete`
    /// → `delete_file`) so the migration target reads like the new
    /// language API while the actual implementation — sandbox path
    /// enforcement, overlay handling, transcript tagging — remains the
    /// single canonical copy in `stdlib::fs`.
    async fn call_harness_fs_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let path_value = |path: &std::path::Path| {
            VmValue::String(arcstr::ArcStr::from(path.to_string_lossy().into_owned()))
        };
        match method {
            "cwd" => {
                let path = crate::stdlib::process::current_execution_context()
                    .and_then(|context| context.cwd.map(std::path::PathBuf::from))
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                return Ok(path_value(&path));
            }
            "source_dir" => {
                let path = self
                    .source_dir
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                return Ok(path_value(&path));
            }
            "project_root" => {
                return Ok(self.project_root().map(&path_value).unwrap_or(VmValue::Nil));
            }
            "workspace_root" => {
                let path = std::env::current_dir()
                    .ok()
                    .or_else(|| self.project_root().map(std::path::Path::to_path_buf))
                    .or_else(|| {
                        crate::stdlib::process::current_execution_context()
                            .and_then(|context| context.cwd.map(std::path::PathBuf::from))
                    })
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                return Ok(path_value(&path));
            }
            "home_dir" => {
                let path = crate::user_dirs::home_dir().unwrap_or_default();
                return Ok(path_value(&path));
            }
            "runtime_paths" => {
                return self.call_named_builtin("runtime_paths", vec![]).await;
            }
            _ => {}
        }
        let builtin = match method {
            "read_text" => "read_file",
            "read_text_result" => "read_file_result",
            "read_bytes" => "read_file_bytes",
            "write_text" => "write_file",
            "write_bytes" => "write_file_bytes",
            "replace_text" => "replace_file",
            "replace_text_result" => "replace_file_result",
            "replace_bytes" => "replace_file_bytes",
            "replace_bytes_result" => "replace_file_bytes_result",
            "exists" => "file_exists",
            "status" => "path_status",
            "delete" => "delete_file",
            "append" => "append_file",
            "append_locked" => "append_file_locked",
            "list_dir" => "list_dir",
            "mkdir" => "mkdir",
            "copy" => "copy_file",
            "temp_dir" => "temp_dir",
            "workspace_temp_dir" => "workspace_temp_dir",
            "mkdtemp" => "mkdtemp",
            "mkdtemp_in_workspace" => "mkdtemp_in_workspace",
            "stat" => "stat",
            "rename" => "move_file",
            "read_lines" => "read_lines",
            "read_lines_page_result" => "read_lines_page_result",
            "walk" => "walk_dir",
            "glob" => "glob",
            "find_text" => "find_text",
            "find_evidence" => "find_evidence",
            "package_snapshot_open" => "package_snapshot_open",
            "package_snapshot_close" => "package_snapshot_close",
            "render_prompt" => "render",
            "render_prompt_with_provenance" => "render_with_provenance",
            "render_template" => "render_string",
            _ => return Err(method_unsupported(handle, method)),
        };
        self.call_named_builtin(builtin, args.to_vec())
            .await
            .map_err(tag_sandbox_denied)
    }

    /// Dispatch `harness.env.*` in real mode against the same execution
    /// context overlay (HARN_REPLAY etc.) the ambient `env` builtin
    /// reads from. Read-only by design — process env mutation belongs
    /// in policy, not language surface, so there is intentionally no
    /// `set` method.
    fn call_harness_env_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        Self::call_harness_env_method_sync_fast(method, args)
            .unwrap_or_else(|| Err(method_unsupported(handle, method)))
    }

    /// Dispatch `harness.random.*` in real mode. The handle is intentionally
    /// stateless (process-wide `rand::rng()`); explicit `Rng` handles
    /// remain available via the ambient `Rng.*` surface for replay
    /// tests that need a seeded stream.
    fn call_harness_random_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        Self::call_harness_random_method_sync_fast(method, args)
            .unwrap_or_else(|| Err(method_unsupported(handle, method)))
    }

    /// Dispatch `harness.net.*` in real mode through the canonical HTTP
    /// implementation, preserving its egress allowlist and retry policy.
    async fn call_harness_net_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let verb = match method {
            "get" => Some(("GET", false)),
            "post" => Some(("POST", true)),
            "put" => Some(("PUT", true)),
            "patch" => Some(("PATCH", true)),
            "delete" => Some(("DELETE", false)),
            _ => None,
        };
        if let Some((http_method, has_body)) = verb {
            return crate::http::execute_http_verb(http_method, has_body, args.to_vec())
                .await
                .map_err(tag_sandbox_denied);
        }
        let server_builtin = match method {
            "server" => Some("__http_server"),
            "server_route" => Some("__http_server_route"),
            "server_before" => Some("__http_server_before"),
            "server_after" => Some("__http_server_after"),
            "server_request" => Some("__http_server_request"),
            "server_test" => Some("__http_server_test"),
            "server_set_ready" => Some("__http_server_set_ready"),
            "server_readiness" => Some("__http_server_readiness"),
            "server_ready" => Some("__http_server_ready"),
            "server_on_shutdown" => Some("__http_server_on_shutdown"),
            "server_shutdown" => Some("__http_server_shutdown"),
            "server_tls_plain" => Some("__http_server_tls_plain"),
            "server_tls_edge" => Some("__http_server_tls_edge"),
            "server_tls_pem" => Some("__http_server_tls_pem"),
            "server_tls_self_signed_dev" => Some("__http_server_tls_self_signed_dev"),
            "server_security_headers" => Some("__http_server_security_headers"),
            _ => None,
        };
        if let Some(builtin) = server_builtin {
            return self.call_named_builtin(builtin, args.to_vec()).await;
        }
        match method {
            "request" => {
                let http_method = string_arg(args, 0, "HarnessNet.request")?.to_string();
                let url = string_arg(args, 1, "HarnessNet.request")?.to_string();
                let options = match args.get(2) {
                    Some(VmValue::Dict(d)) => (**d).clone(),
                    _ => crate::value::DictMap::new(),
                };
                crate::http::execute_http_request(&http_method.to_uppercase(), &url, &options)
                    .await
                    .map_err(tag_sandbox_denied)
            }
            "download" => self
                .call_named_builtin("__http_download", args.to_vec())
                .await
                .map_err(tag_sandbox_denied),
            "stream_open" => {
                self.call_named_builtin("__http_stream_open", args.to_vec())
                    .await
            }
            "stream_read" => {
                self.call_named_builtin("__http_stream_read", args.to_vec())
                    .await
            }
            "stream_info" => {
                self.call_named_builtin("__http_stream_info", args.to_vec())
                    .await
            }
            "stream_close" => {
                self.call_named_builtin("__http_stream_close", args.to_vec())
                    .await
            }
            "session" => {
                self.call_named_builtin("__http_session", args.to_vec())
                    .await
            }
            "session_request" => {
                self.call_named_builtin("__http_session_request", args.to_vec())
                    .await
            }
            "session_close" => {
                self.call_named_builtin("__http_session_close", args.to_vec())
                    .await
            }
            "unix_socket_json_request" => self
                .call_named_builtin("__net_unix_socket_json_request", args.to_vec())
                .await
                .map_err(tag_sandbox_denied),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    async fn call_harness_process_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "exec" | "shell" | "exec_at" | "shell_at" => {
                return self.call_named_builtin(method, args.to_vec()).await;
            }
            "run" => {
                let params = required_dict_arg(args, 0, "HarnessProcess.run")?;
                let program = params
                    .get("program")
                    .and_then(|value| match value {
                        VmValue::String(program) => Some(program.as_str()),
                        _ => None,
                    })
                    .filter(|program| !program.is_empty())
                    .ok_or_else(|| {
                        VmError::TypeError(
                            "HarnessProcess.run: command.program must be a non-empty string"
                                .to_string(),
                        )
                    })?;
                let command_args = match params.get("args") {
                    None | Some(VmValue::Nil) => Vec::new(),
                    Some(VmValue::List(items)) => items
                        .iter()
                        .map(|value| match value {
                            VmValue::String(value) => Ok(VmValue::String(value.clone())),
                            other => Err(VmError::TypeError(format!(
                                "HarnessProcess.run: command.args entries must be strings, got {}",
                                other.type_name()
                            ))),
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    Some(other) => {
                        return Err(VmError::TypeError(format!(
                            "HarnessProcess.run: command.args must be a list, got {}",
                            other.type_name()
                        )))
                    }
                };
                let mut wire = params.clone();
                wire.remove("program");
                wire.remove("args");
                wire.put_str("mode", "argv");
                wire.insert(
                    crate::value::intern_key("argv"),
                    VmValue::List(std::sync::Arc::new(
                        std::iter::once(VmValue::String(arcstr::ArcStr::from(program)))
                            .chain(command_args)
                            .collect(),
                    )),
                );
                let ctx = crate::vm::AsyncBuiltinCtx::from_vm(self.child_vm());
                crate::stdlib::host::dispatch_host_operation_with_ctx(
                    Some(&ctx),
                    "process",
                    "exec",
                    &wire,
                )
                .await
            }
            "default_shell" => {
                crate::stdlib::host::dispatch_host_operation(
                    "process",
                    "get_default_shell",
                    &crate::value::DictMap::new(),
                )
                .await
            }
            "list_shells" => {
                crate::stdlib::host::dispatch_host_operation(
                    "process",
                    "list_shells",
                    &crate::value::DictMap::new(),
                )
                .await
            }
            "shell_invocation" => {
                let params = required_dict_arg(args, 0, "HarnessProcess.shell_invocation")?;
                crate::stdlib::host::dispatch_host_operation("process", "shell_invocation", params)
                    .await
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    async fn call_runtime_capability_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "context" => return Ok(crate::runtime_context::runtime_context_value(self)),
            "context_values" => {
                return Ok(VmValue::dict(self.runtime_context.values.clone()));
            }
            "context_get" => return crate::runtime_context::runtime_context_get(self, args),
            "context_set" => return crate::runtime_context::runtime_context_set(self, args),
            "context_clear" => return crate::runtime_context::runtime_context_clear(self, args),
            _ => {}
        }
        if method == "exit" {
            if args.len() > 1 {
                return Err(VmError::TypeError(
                    "HarnessRuntime.exit expects at most one integer argument".to_string(),
                ));
            }
            let code = args.first().and_then(VmValue::as_int).unwrap_or(0);
            return Err(VmError::ProcessExit(code as i32));
        }
        if method == "host_capabilities" {
            return self.call_named_builtin("host_capabilities", vec![]).await;
        }
        if method == "host_has" {
            return self.call_named_builtin("host_has", args.to_vec()).await;
        }
        if method == "sync_mutex_acquire" {
            return self
                .call_named_builtin("sync_mutex_acquire", args.to_vec())
                .await;
        }
        if method == "introspection" {
            return self
                .call_named_builtin("runtime_introspection", args.to_vec())
                .await;
        }
        let params = if method == "record_run" {
            required_dict_arg(args, 0, "HarnessRuntime.record_run")?.clone()
        } else {
            if !args.is_empty() {
                return Err(VmError::TypeError(format!(
                    "{}.{method} takes no arguments",
                    handle.type_name()
                )));
            }
            crate::value::DictMap::new()
        };
        crate::stdlib::host::dispatch_host_operation("runtime", method, &params).await
    }

    async fn call_interaction_capability_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let builtin = match method {
            "ask_user" => Some("ask_user"),
            "request_approval" => Some("request_approval"),
            "dual_control" => Some("dual_control"),
            "escalate_to" => Some("escalate_to"),
            _ => None,
        };
        if let Some(builtin) = builtin {
            return self.call_named_builtin(builtin, args.to_vec()).await;
        }
        if method != "ask" {
            return Err(method_unsupported(handle, method));
        }
        let question = args
            .first()
            .ok_or_else(|| VmError::TypeError("HarnessInteraction.ask expects a question".into()))?
            .clone();
        let mut params = crate::value::DictMap::new();
        params.insert(crate::value::intern_key("question"), question);
        if let Some(kind) = args.get(1) {
            params.insert(crate::value::intern_key("type"), kind.clone());
        }
        crate::stdlib::host::dispatch_host_operation("interaction", "ask", &params).await
    }

    async fn call_tools_capability_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let builtin = match method {
            "list_registered" => "host_tool_list",
            "invoke" => "host_tool_call",
            "dispatch_agent_call" => "__host_agent_dispatch_tool_call",
            "dispatch_agent_batch" => "__host_agent_dispatch_tool_batch",
            _ => return Err(method_unsupported(handle, method)),
        };
        self.call_named_builtin(builtin, args.to_vec()).await
    }

    async fn call_dict_host_capability_method(
        &mut self,
        handle: &VmHarness,
        capability: &str,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let params = required_dict_arg(args, 0, handle.type_name())?;
        crate::stdlib::host::dispatch_host_operation(capability, method, params).await
    }

    async fn call_project_capability_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let optional_request = || -> Result<crate::value::DictMap, VmError> {
            match args.first() {
                None | Some(VmValue::Nil) => Ok(crate::value::DictMap::new()),
                Some(value) => value.as_dict().cloned().ok_or_else(|| {
                    VmError::TypeError(format!(
                        "{}.{method} expects a request record",
                        handle.type_name()
                    ))
                }),
            }
        };
        let request_value = |request: &crate::value::DictMap, key: &str| {
            request.get(key).cloned().unwrap_or(VmValue::Nil)
        };
        let builtin_args = match method {
            "metadata_entries" => {
                let request = optional_request()?;
                Some((
                    "metadata_entries",
                    vec![request_value(&request, "namespace")],
                ))
            }
            "metadata_status" => {
                let request = optional_request()?;
                Some((
                    "metadata_status",
                    vec![request_value(&request, "namespace")],
                ))
            }
            "content_hash" => Some(("compute_content_hash", args.to_vec())),
            "path_metadata_get" => {
                let request = required_dict_arg(args, 0, "HarnessProject.path_metadata_get")?;
                Some((
                    "path_metadata_get",
                    vec![
                        request_value(request, "path"),
                        request_value(request, "namespace"),
                        request_value(request, "options"),
                    ],
                ))
            }
            "path_metadata_set" => {
                let request = required_dict_arg(args, 0, "HarnessProject.path_metadata_set")?;
                Some((
                    "path_metadata_set",
                    vec![
                        request_value(request, "path"),
                        request_value(request, "namespace"),
                        request
                            .get("value")
                            .or_else(|| request.get("data"))
                            .cloned()
                            .unwrap_or(VmValue::Nil),
                        request_value(request, "options"),
                    ],
                ))
            }
            "path_metadata_entries" => {
                let request = optional_request()?;
                Some((
                    "path_metadata_entries",
                    vec![
                        request_value(&request, "namespace"),
                        request_value(&request, "options"),
                    ],
                ))
            }
            "scan_directory" => Some(("scan_directory", args.to_vec())),
            _ => None,
        };
        if let Some((builtin, builtin_args)) = builtin_args {
            return self.call_named_builtin(builtin, builtin_args).await;
        }
        let builtin = match method {
            "scan" => Some("project_scan_native"),
            "fingerprint" => Some("project_fingerprint"),
            "context_profile" => Some("project_context_profile_native"),
            "scan_tree" => Some("project_scan_tree_native"),
            "walk_tree" => Some("project_walk_tree_native"),
            "catalog" => Some("project_catalog_native"),
            _ => None,
        };
        if let Some(builtin) = builtin {
            return self.call_named_builtin(builtin, args.to_vec()).await;
        }
        self.call_dict_host_capability_method(handle, "project", method, args)
            .await
    }

    async fn call_testing_capability_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let fixtures = handle.inner().fixtures();
        match method {
            "clock_set" => {
                let unix_ms = args.first().and_then(VmValue::as_int).ok_or_else(|| {
                    VmError::TypeError(
                        "HarnessTesting.clock_set expects Unix milliseconds as an int".to_string(),
                    )
                })?;
                handle.inner().set_test_clock(unix_ms)?;
                Ok(VmValue::Nil)
            }
            "clock_advance" => {
                let milliseconds = args.first().and_then(VmValue::as_int).ok_or_else(|| {
                    VmError::TypeError(
                        "HarnessTesting.clock_advance expects milliseconds as an int".to_string(),
                    )
                })?;
                Ok(VmValue::Int(
                    handle.inner().advance_test_clock(milliseconds)?,
                ))
            }
            "clock_reset" => {
                require_no_args(handle, method, args)?;
                handle.inner().clear_test_clock();
                Ok(VmValue::Nil)
            }
            "transport_mock_clear" | "transport_mock_calls" => {
                self.call_named_builtin(&format!("__{method}"), args.to_vec())
                    .await
            }
            "stdin_set" => self.call_named_builtin("mock_stdin", args.to_vec()).await,
            "stdin_reset" => self.call_named_builtin("unmock_stdin", args.to_vec()).await,
            "tty_set" => self.call_named_builtin("mock_tty", args.to_vec()).await,
            "tty_reset" => self.call_named_builtin("unmock_tty", args.to_vec()).await,
            "capture_stderr_start" => {
                self.call_named_builtin("capture_stderr_start", args.to_vec())
                    .await
            }
            "capture_stderr_take" => {
                self.call_named_builtin("capture_stderr_take", args.to_vec())
                    .await
            }
            "clear" | "push_scope" | "pop_scope" => {
                require_no_args(handle, method, args)?;
                match method {
                    "clear" => fixtures.clear(),
                    "push_scope" => fixtures.push_scope(),
                    "pop_scope" => fixtures.pop_scope()?,
                    _ => unreachable!(),
                }
                Ok(VmValue::Nil)
            }
            "respond" | "respond_error" => {
                let capability_name = string_arg(args, 0, &format!("HarnessTesting.{method}"))?;
                let capability = harn_builtin_meta::CapabilityId::from_field_name(capability_name)
                    .ok_or_else(|| {
                        VmError::TypeError(format!(
                            "HarnessTesting.{method}: unknown capability `{capability_name}`"
                        ))
                    })?;
                if capability == harn_builtin_meta::CapabilityId::Testing {
                    return Err(VmError::TypeError(
                        "HarnessTesting cannot fixture its own control methods".to_string(),
                    ));
                }
                let target_method = string_arg(args, 1, &format!("HarnessTesting.{method}"))?;
                let declared = crate::stdlib::all_builtin_manifest().iter().any(|entry| {
                    matches!(
                        entry.contract.exposure,
                        harn_builtin_meta::BuiltinExposure::HarnessMethod {
                            capability: candidate,
                            method: candidate_method,
                        } if candidate == capability && candidate_method == target_method
                    )
                });
                if !declared
                    && !crate::harness::is_capability_driver_fixture(capability, target_method)
                {
                    return Err(VmError::TypeError(format!(
                        "HarnessTesting.{method}: undeclared method `harness.{}.{target_method}`",
                        capability.field_name()
                    )));
                }
                let response = if method == "respond" {
                    Ok(args.get(2).cloned().unwrap_or(VmValue::Nil))
                } else {
                    Err(string_arg(args, 2, "HarnessTesting.respond_error")?.to_string())
                };
                let when = match args.get(3) {
                    None | Some(VmValue::Nil) => None,
                    Some(VmValue::Dict(selector)) => Some((**selector).clone()),
                    Some(other) => {
                        return Err(VmError::TypeError(format!(
                            "HarnessTesting.{method}: selector must be a dict, got {}",
                            other.type_name()
                        )))
                    }
                };
                let repeat = match args.get(4) {
                    None | Some(VmValue::Nil) => false,
                    Some(VmValue::Bool(repeat)) => *repeat,
                    Some(other) => {
                        return Err(VmError::TypeError(format!(
                            "HarnessTesting.{method}: repeat must be a bool, got {}",
                            other.type_name()
                        )))
                    }
                };
                fixtures.respond(capability, target_method, response, when, repeat);
                Ok(VmValue::Nil)
            }
            "calls" => {
                require_no_args(handle, method, args)?;
                let calls = fixtures
                    .calls()
                    .into_iter()
                    .map(|call| {
                        let mut record = crate::value::DictMap::new();
                        record.put_str("capability", call.capability.field_name());
                        record.put_str("method", call.method);
                        record.insert(
                            crate::value::intern_key("args"),
                            VmValue::List(std::sync::Arc::new(call.args)),
                        );
                        VmValue::dict(record)
                    })
                    .collect();
                Ok(VmValue::List(std::sync::Arc::new(calls)))
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    async fn call_harness_channels_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let builtin = match method {
            "append" => "emit_channel",
            "events" => "channel_events",
            "subscribe" => "channel_subscribe",
            "consumer_cursor" => "channel_consumer_cursor",
            "ack" => "channel_ack",
            "flush_aggregations" => "flush_trigger_aggregations",
            _ => return Err(method_unsupported(handle, method)),
        };
        self.call_named_builtin(builtin, args.to_vec()).await
    }

    async fn call_mock_harness_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let HarnessMode::Mock(state) = handle.inner().mode() else {
            unreachable!("mock dispatch is only called for mock harnesses");
        };
        if let Some(result) = Self::call_mock_harness_method_sync_fast(handle, method, args) {
            return result;
        }
        state.record_call(handle.kind(), method, args);
        if let Some(capability) = handle.kind().capability_id() {
            if let Some(response) = state.capability_response(capability, method) {
                return Ok(response);
            }
        }
        match handle.kind() {
            HarnessKind::Root
            | HarnessKind::Stdio
            | HarnessKind::Term
            | HarnessKind::Clock
            | HarnessKind::Env
            | HarnessKind::Random
            | HarnessKind::Channels
            | HarnessKind::System
            | HarnessKind::Secrets
            | HarnessKind::Tenant
            | HarnessKind::Auth => Err(method_unsupported(handle, method)),
            HarnessKind::Fs => match method {
                "read_file" | "read" => {
                    let path = string_arg(args, 0, "HarnessFs.read_file")?;
                    let bytes = state
                        .fs_read(path)
                        .ok_or_else(|| VmError::CategorizedError {
                            message: format!("MockHarness has no fs_read response for {path}"),
                            category: ErrorCategory::NotFound,
                        })?;
                    Ok(VmValue::Bytes(std::sync::Arc::new(bytes.to_vec())))
                }
                "read_text" => {
                    let path = string_arg(args, 0, "HarnessFs.read_text")?;
                    let bytes = state
                        .fs_read(path)
                        .ok_or_else(|| VmError::CategorizedError {
                            message: format!("MockHarness has no fs_read response for {path}"),
                            category: ErrorCategory::NotFound,
                        })?;
                    let text = std::str::from_utf8(bytes).map_err(|error| {
                        VmError::TypeError(format!("HarnessFs.read_text: {error}"))
                    })?;
                    Ok(vm_string(text))
                }
                "exists" => {
                    let path = string_arg(args, 0, "HarnessFs.exists")?;
                    Ok(VmValue::Bool(state.fs_read(path).is_some()))
                }
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Net => match method {
                "get" => {
                    let url = string_arg(args, 0, "HarnessNet.get")?;
                    Ok(state.net_get(url).map(vm_string).ok_or_else(|| {
                        VmError::CategorizedError {
                            message: format!("MockHarness has no net_get response for {url}"),
                            category: ErrorCategory::NotFound,
                        }
                    })?)
                }
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Process => match method {
                "run" => Err(VmError::CategorizedError {
                    message: "MockHarness has no process response".to_string(),
                    category: ErrorCategory::NotFound,
                }),
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Llm => self.call_harness_llm_method(handle, method, args).await,
            HarnessKind::Obs => {
                // Mock mode shares the same OBS_STATE thread-local as
                // real mode — fixtures already drive `__obs_*` via
                // `std/observability` and expect the same emissions to
                // surface through `harness.obs.*` calls.
                self.call_harness_obs_method(handle, method, args).await
            }
            HarnessKind::Verdict => self.call_harness_verdict_method(handle, method, args).await,
            _ => Err(method_unsupported(handle, method)),
        }
    }
}

#[derive(Clone, Copy)]
struct UnsettledCounts {
    suspended: usize,
    queued: usize,
    partial: usize,
    in_flight: usize,
    pool_pending: usize,
}

impl UnsettledCounts {
    fn is_empty(self) -> bool {
        self.suspended == 0
            && self.queued == 0
            && self.partial == 0
            && self.in_flight == 0
            && self.pool_pending == 0
    }

    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "suspended": self.suspended,
            "queued": self.queued,
            "partial": self.partial,
            "in_flight": self.in_flight,
            "pool_pending": self.pool_pending,
        })
    }

    fn summary(self) -> String {
        if self.is_empty() {
            "no unsettled work".to_string()
        } else {
            format!(
                "unsettled work: {} suspended subagents, {} queued triggers, {} partial handoffs, {} in-flight llm calls, {} pool pending tasks",
                self.suspended, self.queued, self.partial, self.in_flight, self.pool_pending
            )
        }
    }
}

fn state_counts(state: &VmValue) -> Result<UnsettledCounts, VmError> {
    let Some(dict) = state.as_dict() else {
        return Err(VmError::TypeError(
            "Harness unsettled-state helpers expect a state dict".to_string(),
        ));
    };
    Ok(UnsettledCounts {
        suspended: state_bucket_len(dict, "suspended_subagents")?,
        queued: state_bucket_len(dict, "queued_triggers")?,
        partial: state_bucket_len(dict, "partial_handoffs")?,
        in_flight: state_bucket_len(dict, "in_flight_llm_calls")?,
        pool_pending: state_bucket_len(dict, "pool_pending_tasks")?,
    })
}

fn state_bucket_len(dict: &crate::value::DictMap, key: &str) -> Result<usize, VmError> {
    match dict.get(key) {
        Some(VmValue::List(items)) => Ok(items.len()),
        Some(other) => Err(VmError::TypeError(format!(
            "unsettled-state field `{key}` must be a list, got {}",
            other.type_name()
        ))),
        None => Ok(0),
    }
}

async fn acknowledge_trigger(args: &[VmValue]) -> VmValue {
    let Some(id) = args
        .first()
        .map(vm_value_string)
        .filter(|id| !id.is_empty())
    else {
        return json_receipt("rejected", "acknowledge_trigger", "missing trigger id");
    };
    let receipt = acknowledge_trigger_id(&id).await;
    crate::stdlib::json_to_vm_value(&receipt)
}

async fn defer_trigger(args: &[VmValue]) -> VmValue {
    let Some(id) = args
        .first()
        .map(vm_value_string)
        .filter(|id| !id.is_empty())
    else {
        return json_receipt("rejected", "defer_trigger", "missing trigger id");
    };
    let target = args
        .get(1)
        .map(vm_value_string)
        .filter(|target| !target.trim().is_empty())
        .unwrap_or_else(|| "deferred-triggers".to_string());
    let acknowledgement = acknowledge_trigger_id(&id).await;
    if acknowledgement
        .get("status")
        .and_then(serde_json::Value::as_str)
        != Some("acknowledged")
    {
        return crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": acknowledgement
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("rejected"),
            "method": "defer_trigger",
            "trigger_id": id,
            "acknowledgement": acknowledgement,
        }));
    }
    let envelope = crate::orchestration::record_partial_handoff(
        target,
        serde_json::json!({
            "deferred_trigger": acknowledgement.get("item").cloned().unwrap_or(serde_json::Value::Null),
            "acknowledgement": acknowledgement,
        }),
    );
    crate::orchestration::record_lifecycle_audit(
        "trigger_deferred",
        serde_json::json!({
            "trigger_id": id,
            "envelope_id": envelope.envelope_id,
        }),
    );
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": "deferred",
        "method": "defer_trigger",
        "trigger_id": id,
        "acknowledgement": acknowledgement,
        "envelope": envelope.to_json(),
    }))
}

async fn acknowledge_trigger_id(id: &str) -> serde_json::Value {
    let snapshot = crate::orchestration::unsettled_state_snapshot_async().await;
    // HARN-DRN-001 ordering enforcement (#1856 P-03): the drain loop
    // must finalize earlier categories before later ones. Queued
    // triggers come AFTER suspended subagents, so a non-empty
    // suspended_subagents bucket blocks trigger acknowledgement.
    //
    // The conformance fixture
    // `pipeline_drain_ordering_enforcement.harn` seeds a partial-handoff
    // envelope as a stand-in for a suspended subagent (the test author
    // comment calls this out explicitly: real subagent snapshot wiring
    // is heavier than a single fixture warrants). To honor that
    // intent we also reject when `partial_handoffs` is non-empty,
    // surfacing the "suspended subagents remain" wording the fixture
    // expects.
    if !snapshot.suspended_subagents.is_empty() || !snapshot.partial_handoffs.is_empty() {
        return serde_json::json!({
            "status": "rejected",
            "method": "acknowledge_trigger",
            "trigger_id": id,
            "reason": "HARN-DRN-001: cannot acknowledge trigger while suspended subagents remain",
        });
    }
    let Some(item) = snapshot
        .queued_triggers
        .iter()
        .find(|item| item.get("id").and_then(serde_json::Value::as_str) == Some(id))
        .cloned()
    else {
        return serde_json::json!({
            "status": "not_found",
            "method": "acknowledge_trigger",
            "trigger_id": id,
        });
    };
    let Some(log) = crate::event_log::active_event_log() else {
        return serde_json::json!({
            "status": "rejected",
            "method": "acknowledge_trigger",
            "trigger_id": id,
            "reason": "no active event log is installed",
            "item": item,
        });
    };
    let result = match item.get("source").and_then(serde_json::Value::as_str) {
        Some("worker_queue") => {
            let queue = item
                .get("queue")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let job_event_id = item
                .get("job_event_id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default();
            match crate::triggers::WorkerQueue::new(log)
                .ack_job(queue, job_event_id, "pipeline_lifecycle")
                .await
            {
                Ok(true) => serde_json::json!({"status": "acknowledged"}),
                Ok(false) => serde_json::json!({"status": "not_found"}),
                Err(error) => serde_json::json!({
                    "status": "rejected",
                    "reason": error.to_string(),
                }),
            }
        }
        Some("trigger_inbox") => {
            let Some(binding_key) = item
                .get("binding_key")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return serde_json::json!({
                    "status": "rejected",
                    "method": "acknowledge_trigger",
                    "trigger_id": id,
                    "reason": "queued trigger is missing binding_key",
                    "item": item,
                });
            };
            let Some(event_id) = item
                .get("event_id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                return serde_json::json!({
                    "status": "rejected",
                    "method": "acknowledge_trigger",
                    "trigger_id": id,
                    "reason": "queued trigger is missing event_id",
                    "item": item,
                });
            };
            let request = crate::triggers::DispatchCancelRequest {
                binding_key: binding_key.to_string(),
                event_id: event_id.to_string(),
                requested_at: crate::clock_mock::now_utc(),
                requested_by: Some("pipeline_lifecycle".to_string()),
                audit_id: None,
            };
            match crate::triggers::append_dispatch_cancel_request(&log, &request).await {
                Ok(_) => serde_json::json!({"status": "acknowledged"}),
                Err(error) => serde_json::json!({
                    "status": "rejected",
                    "reason": error.to_string(),
                }),
            }
        }
        Some(source) => serde_json::json!({
            "status": "rejected",
            "reason": format!("unknown queued trigger source `{source}`"),
        }),
        None => serde_json::json!({
            "status": "rejected",
            "reason": "queued trigger is missing source",
        }),
    };
    if result.get("status").and_then(serde_json::Value::as_str) == Some("acknowledged") {
        crate::orchestration::record_lifecycle_audit(
            "trigger_acknowledged",
            serde_json::json!({
                "trigger_id": id,
                "item": item.clone(),
            }),
        );
    }
    let mut receipt = serde_json::Map::new();
    receipt.insert(
        "status".to_string(),
        result
            .get("status")
            .cloned()
            .unwrap_or_else(|| serde_json::json!("rejected")),
    );
    receipt.insert(
        "method".to_string(),
        serde_json::json!("acknowledge_trigger"),
    );
    receipt.insert("trigger_id".to_string(), serde_json::json!(id));
    receipt.insert("item".to_string(), item);
    if let Some(reason) = result.get("reason").cloned() {
        receipt.insert("reason".to_string(), reason);
    }
    serde_json::Value::Object(receipt)
}

fn acknowledge_handoff(args: &[VmValue]) -> VmValue {
    let Some(envelope_id) = args
        .first()
        .map(vm_value_string)
        .filter(|id| !id.is_empty())
    else {
        return json_receipt("rejected", "acknowledge_handoff", "missing envelope id");
    };
    let decision = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    // HARN-DRN-001 ordering enforcement (#1856 P-03): handoffs come
    // third in the drain order (after subagents, after triggers). A
    // non-empty earlier bucket blocks handoff acknowledgement. This
    // uses the sync snapshot deliberately — the in-memory subagent
    // registry is sufficient for the ordering check; the async snapshot
    // would also include event-log-backed triggers but `acknowledge_handoff`
    // is itself sync today, and only the in-memory check matters here.
    let snapshot = crate::orchestration::unsettled_state_snapshot();
    if !snapshot.suspended_subagents.is_empty() {
        return crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "rejected",
            "method": "acknowledge_handoff",
            "envelope_id": envelope_id,
            "reason": "HARN-DRN-001: cannot acknowledge handoff while suspended subagents remain",
        }));
    }
    match crate::orchestration::acknowledge_partial_handoff(&envelope_id, decision) {
        Some(envelope) => crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "acknowledged",
            "method": "acknowledge_handoff",
            "envelope": envelope.to_json(),
        })),
        None => crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "not_found",
            "method": "acknowledge_handoff",
            "envelope_id": envelope_id,
        })),
    }
}

fn finalize_pipeline(args: &[VmValue]) -> VmValue {
    let disposition = args
        .first()
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let receipt = crate::orchestration::finalize_pipeline_disposition(disposition);
    crate::stdlib::json_to_vm_value(&receipt)
}

fn json_receipt(status: &str, method: &str, reason: &str) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": status,
        "method": method,
        "reason": reason,
    }))
}

fn vm_value_string(value: &VmValue) -> String {
    match value {
        VmValue::String(text) => text.as_str().to_string(),
        other => other.display(),
    }
}

/// Persist a `harness.emit_audit` call. When the audit kind is
/// `drain_decision`, fires the `OnDrainDecision` lifecycle hook
/// (harn#1859) first: Allow proceeds, Block returns a `blocked` receipt
/// so the drain agent can short-circuit the tool call, Modify rewrites
/// the audit payload before persisting.
async fn record_emit_audit_with_hooks(
    ctx: &crate::vm::AsyncBuiltinCtx,
    args: &[VmValue],
) -> VmValue {
    let kind = args
        .first()
        .map(|v| match v {
            VmValue::String(s) => s.as_str().to_string(),
            other => other.display(),
        })
        .unwrap_or_default();
    let mut payload = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    if kind == "drain_decision" {
        let hook_payload = serde_json::json!({
            "event": crate::orchestration::HookEvent::OnDrainDecision.as_str(),
            "action": payload.get("action").cloned().unwrap_or(serde_json::Value::Null),
            "item": payload.get("item").cloned().unwrap_or(serde_json::Value::Null),
            "payload": payload.clone(),
        });
        match crate::orchestration::run_lifecycle_hooks_with_control_with_ctx(
            Some(ctx),
            crate::orchestration::HookEvent::OnDrainDecision,
            &hook_payload,
        )
        .await
        {
            Ok(crate::orchestration::HookControl::Allow) => {}
            Ok(crate::orchestration::HookControl::Block { reason }) => {
                return crate::stdlib::json_to_vm_value(&serde_json::json!({
                    "status": "blocked",
                    "method": "emit_audit",
                    "kind": kind,
                    "reason": reason,
                }));
            }
            Ok(crate::orchestration::HookControl::Modify { payload: modified }) => {
                if let Some(p) = modified.get("payload") {
                    payload = p.clone();
                }
            }
            Ok(crate::orchestration::HookControl::Decision { .. }) => {}
            Err(err) => {
                return crate::stdlib::json_to_vm_value(&serde_json::json!({
                    "status": "error",
                    "method": "emit_audit",
                    "kind": kind,
                    "error": err.to_string(),
                }));
            }
        }
        record_drain_decision_span(&payload);
    }
    let entry = crate::orchestration::record_lifecycle_audit(kind, payload);
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": "recorded",
        "method": "emit_audit",
        "entry": entry.to_json(),
    }))
}

/// Async wrapper that runs the settlement-agent drain loop (#1856 P-03)
/// sandwiched by `PreDrain` (Allow/Deny/Modify) and `PostDrain`
/// (advisory). The loop body lives in
/// `crate::orchestration::run_settlement_agent_loop` — it walks the
/// unsettled snapshot in deterministic order (subagents → triggers →
/// handoffs → in-flight LLM calls → pool pending), records a
/// `drain_decision` audit per disposition (firing `OnDrainDecision`
/// hooks via the standard route), and terminates when the snapshot is
/// empty or the configurable budget (default 5, hard cap 20) is
/// exhausted.
async fn record_spawn_settlement_agent_with_hooks(
    ctx: &crate::vm::AsyncBuiltinCtx,
    args: &[VmValue],
) -> VmValue {
    let mut unsettled = args
        .first()
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let return_value = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let options = args
        .get(2)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let pre_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PreDrain.as_str(),
        "unsettled": unsettled.clone(),
        "return_value": return_value.clone(),
        "options": options.clone(),
    });
    match crate::orchestration::run_lifecycle_hooks_with_control_with_ctx(
        Some(ctx),
        crate::orchestration::HookEvent::PreDrain,
        &pre_payload,
    )
    .await
    {
        Ok(crate::orchestration::HookControl::Allow) => {}
        Ok(crate::orchestration::HookControl::Block { reason }) => {
            return crate::stdlib::json_to_vm_value(&serde_json::json!({
                "status": "skipped",
                "method": "spawn_settlement_agent",
                "reason": reason,
            }));
        }
        Ok(crate::orchestration::HookControl::Modify { payload }) => {
            if let Some(new_unsettled) = payload.get("unsettled") {
                unsettled = new_unsettled.clone();
            }
        }
        Ok(crate::orchestration::HookControl::Decision { .. }) => {}
        Err(err) => {
            return crate::stdlib::json_to_vm_value(&serde_json::json!({
                "status": "error",
                "method": "spawn_settlement_agent",
                "error": err.to_string(),
            }));
        }
    }
    let span_links = crate::tracing::current_span_link()
        .map(|link| {
            link.with_attributes(std::collections::BTreeMap::from([(
                "harn.link.kind".to_string(),
                "pipeline".to_string(),
            )]))
        })
        .into_iter()
        .collect();
    let span_id = crate::tracing::span_start_detached_with_links(
        crate::tracing::SpanKind::Drain,
        "settlement_agent".to_string(),
        span_links,
    );
    if span_id != 0 {
        if let Ok(counts) = state_counts(&crate::stdlib::json_to_vm_value(&unsettled)) {
            crate::tracing::span_set_metadata(span_id, "counts", counts.to_json());
        }
    }
    let outcome_json = crate::orchestration::run_settlement_agent_loop_with_ctx(
        Some(ctx),
        unsettled.clone(),
        return_value,
        options,
    )
    .await;
    if span_id != 0 {
        if let Some(status) = outcome_json.get("status").cloned() {
            crate::tracing::span_set_metadata(span_id, "status", status);
        }
        if let Some(iterations) = outcome_json.get("iterations").cloned() {
            crate::tracing::span_set_metadata(span_id, "iterations", iterations);
        }
        crate::tracing::span_end(span_id);
    }
    let outcome = crate::stdlib::json_to_vm_value(&outcome_json);
    let post_payload = serde_json::json!({
        "event": crate::orchestration::HookEvent::PostDrain.as_str(),
        "unsettled": unsettled,
        "outcome": outcome_json,
    });
    if let Err(err) = crate::orchestration::run_lifecycle_hooks_with_ctx(
        Some(ctx),
        crate::orchestration::HookEvent::PostDrain,
        &post_payload,
    )
    .await
    {
        return crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "error",
            "method": "spawn_settlement_agent",
            "error": err.to_string(),
        }));
    }
    outcome
}

fn record_drain_decision_span(payload: &serde_json::Value) {
    let links = crate::tracing::current_span_link()
        .map(|link| {
            link.with_attributes(std::collections::BTreeMap::from([(
                "harn.link.kind".to_string(),
                "drain".to_string(),
            )]))
        })
        .into_iter()
        .collect();
    let span_id = crate::tracing::span_start_detached_with_links(
        crate::tracing::SpanKind::DrainDecision,
        payload
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("drain_decision")
            .to_string(),
        links,
    );
    if span_id != 0 {
        if let Some(action) = payload.get("action").and_then(|value| value.as_str()) {
            crate::tracing::span_set_metadata(span_id, "action", serde_json::json!(action));
        }
        if let Some(item) = payload.pointer("/item/id").and_then(|value| value.as_str()) {
            crate::tracing::span_set_metadata(span_id, "item_id", serde_json::json!(item));
        }
        crate::tracing::span_end(span_id);
    }
}

fn record_handoff_envelope(args: &[VmValue]) -> VmValue {
    let Some(target_value) = args.first() else {
        return crate::stdlib::json_to_vm_value(&serde_json::json!({
            "status": "rejected",
            "method": "handoff_to",
            "reason": "missing target pipeline argument",
        }));
    };
    let target = match target_value {
        VmValue::String(s) => s.as_str().to_string(),
        other => other.display(),
    };
    let payload = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let envelope = crate::orchestration::record_partial_handoff(target, payload);
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": "queued",
        "method": "handoff_to",
        "envelope": envelope.to_json(),
    }))
}

/// Decorate sandbox/egress rejections raised inside a harness sub-handle
/// call with the `HARN-CAP-201` diagnostic code so callers (and the
/// portal) can attribute the error to the harness capability profile
/// instead of an opaque tool rejection.
///
/// Two error shapes need decoration:
///   * `CategorizedError { ToolRejected, "sandbox violation: ..." }` —
///     filesystem path enforcement, process cwd enforcement.
///   * `Thrown(Dict { type: "EgressBlocked", ... })` — net allowlist
///     denial raised by `crate::egress::enforce_url_allowed`.
///
/// Errors unrelated to sandbox enforcement (`TypeError`, plain
/// `Thrown`, `Runtime`) pass through untouched so the harness method
/// surface keeps the original diagnostic. Already-tagged errors are
/// idempotent — the check avoids double prefixing under nested
/// dispatch.
pub(crate) fn tag_sandbox_denied(error: VmError) -> VmError {
    match error {
        VmError::CategorizedError { message, category }
            if matches!(category, ErrorCategory::ToolRejected)
                && message.contains("sandbox violation")
                && !message.contains(HARN_CAP_201_CODE) =>
        {
            VmError::CategorizedError {
                message: format!("{HARN_CAP_201_CODE}: {message}"),
                category,
            }
        }
        VmError::Thrown(VmValue::Dict(dict)) if is_egress_blocked_dict(&dict) => {
            VmError::Thrown(VmValue::Dict(tag_egress_dict(dict)))
        }
        other => other,
    }
}

const HARN_CAP_201_CODE: &str = "HARN-CAP-201";

fn is_egress_blocked_dict(dict: &crate::value::DictMap) -> bool {
    matches!(
        dict.get("type"),
        Some(VmValue::String(value)) if value.as_str() == "EgressBlocked"
    )
}

fn tag_egress_dict(
    dict: std::sync::Arc<crate::value::DictMap>,
) -> std::sync::Arc<crate::value::DictMap> {
    let mut next = (*dict).clone();
    if matches!(
        next.get("code"),
        Some(VmValue::String(value)) if value.as_str() == HARN_CAP_201_CODE
    ) {
        return std::sync::Arc::new(next);
    }
    next.put_str("code", HARN_CAP_201_CODE);
    std::sync::Arc::new(next)
}

pub(crate) fn method_unsupported(handle: &VmHarness, method: &str) -> VmError {
    VmError::TypeError(format!(
        "value of type {} has no method `{method}`",
        handle.type_name()
    ))
}

fn require_no_args(handle: &VmHarness, method: &str, args: &[VmValue]) -> Result<(), VmError> {
    if args.is_empty() {
        return Ok(());
    }
    Err(VmError::TypeError(format!(
        "{}.{method} takes no arguments",
        handle.type_name()
    )))
}

fn obs_string_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
    field: &str,
) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(text)) => Ok(text.to_string()),
        Some(other) => Err(VmError::TypeError(format!(
            "{}.{method} expects {field}: string, got {}",
            handle.type_name(),
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!(
            "{}.{method} missing required {field}",
            handle.type_name()
        ))),
    }
}

/// Like [`obs_string_arg`] but treats a missing slot or explicit `nil`
/// as `None` so the caller can apply a default — used for `level` on
/// `harness.obs.log`, where the user-facing surface defaults to
/// `"info"`.
fn obs_optional_string_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
    field: &str,
) -> Result<Option<String>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::String(text)) => Ok(Some(text.to_string())),
        Some(other) => Err(VmError::TypeError(format!(
            "{}.{method} expects {field}: string, got {}",
            handle.type_name(),
            other.type_name()
        ))),
    }
}

fn obs_attrs_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
) -> Result<serde_json::Map<String, serde_json::Value>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(serde_json::Map::new()),
        // `VmValue::Dict` always lowers to `serde_json::Value::Object`,
        // so the conversion never returns another variant here — the
        // explicit Object pattern keeps the call site total without
        // adding a dead arm.
        Some(value @ VmValue::Dict(_)) => {
            let serde_json::Value::Object(map) =
                crate::stdlib::observability::vm_value_to_json(value)
            else {
                unreachable!("Dict lowers to Object");
            };
            Ok(map)
        }
        Some(other) => Err(VmError::TypeError(format!(
            "{}.{method} expects attrs: dict, got {}",
            handle.type_name(),
            other.type_name()
        ))),
    }
}

fn obs_number_arg(
    handle: &VmHarness,
    method: &str,
    value: &VmValue,
) -> Result<serde_json::Value, VmError> {
    match value {
        VmValue::Int(n) => Ok(serde_json::json!(*n)),
        VmValue::Float(n) => Ok(serde_json::json!(*n)),
        VmValue::Duration(ms) => Ok(serde_json::json!(*ms)),
        other => Err(VmError::TypeError(format!(
            "{}.{method} expects value: number, got {}",
            handle.type_name(),
            other.type_name()
        ))),
    }
}

fn sleep_ms_arg(args: &[VmValue]) -> Result<i64, VmError> {
    args.first()
        .and_then(|v| match v {
            VmValue::Int(n) => Some(*n),
            VmValue::Duration(ms) => Some(*ms),
            _ => None,
        })
        .ok_or_else(|| {
            VmError::TypeError("HarnessClock.sleep_ms expects an int or duration argument".into())
        })
}

fn string_arg<'a>(args: &'a [VmValue], index: usize, callee: &str) -> Result<&'a str, VmError> {
    match args.get(index) {
        Some(VmValue::String(value)) => Ok(value.as_str()),
        Some(other) => Err(VmError::TypeError(format!(
            "{callee} expects string argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!(
            "{callee} expects string argument {}",
            index + 1
        ))),
    }
}

fn required_dict_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    callee: &str,
) -> Result<&'a crate::value::DictMap, VmError> {
    args.get(index)
        .and_then(VmValue::as_dict)
        .ok_or_else(|| VmError::TypeError(format!("{callee} expects a dict argument")))
}

fn optional_string_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    callee: &str,
) -> Result<&'a str, VmError> {
    match args.get(index) {
        None | Some(VmValue::Nil) => Ok(""),
        Some(VmValue::String(value)) => Ok(value.as_str()),
        Some(other) => Err(VmError::TypeError(format!(
            "{callee} expects string argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
    }
}

fn secret_name_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(name)) if !name.trim().is_empty() => Ok(name.to_string()),
        Some(VmValue::String(_)) => Err(VmError::TypeError(format!(
            "{}.{method} expects a non-empty secret name",
            handle.type_name()
        ))),
        Some(other) => Err(VmError::TypeError(format!(
            "{}.{method} expects name: string, got {}",
            handle.type_name(),
            other.type_name()
        ))),
        None => Err(VmError::TypeError(format!(
            "{}.{method} missing required name",
            handle.type_name()
        ))),
    }
}

fn secret_value_arg(
    handle: &VmHarness,
    method: &str,
    value: Option<&VmValue>,
    field: &str,
) -> Result<crate::secrets::SecretBytes, VmError> {
    let value = value.ok_or_else(|| {
        VmError::TypeError(format!(
            "{}.{method} missing required {field}",
            handle.type_name()
        ))
    })?;
    secret_value_from_vm(handle, method, value, field)
}

fn secret_value_from_vm(
    handle: &VmHarness,
    method: &str,
    value: &VmValue,
    field: &str,
) -> Result<crate::secrets::SecretBytes, VmError> {
    match value {
        VmValue::String(text) => Ok(crate::secrets::SecretBytes::from(text.as_str())),
        VmValue::Bytes(bytes) => Ok(crate::secrets::SecretBytes::from(bytes.as_slice())),
        other => Err(VmError::TypeError(format!(
            "{}.{method} expects {field}: string or bytes, got {}",
            handle.type_name(),
            other.type_name()
        ))),
    }
}

fn secret_scope_arg(value: Option<&VmValue>) -> Result<crate::secrets::SecretScope, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(crate::secrets::SecretScope::tenant(
            crate::harness_tenant::current_tenant_id().map(|tenant| tenant.0),
        )),
        Some(VmValue::String(scope)) => parse_secret_scope_string(scope.as_str()),
        Some(VmValue::Dict(dict)) => {
            let kind = dict
                .get("kind")
                .and_then(|value| match value {
                    VmValue::String(kind) => Some(kind.as_str()),
                    _ => None,
                })
                .ok_or_else(|| {
                    VmError::TypeError(
                        "HarnessSecrets scope dict requires string `kind`".to_string(),
                    )
                })?
                .trim();
            let id = match dict.get("id") {
                None | Some(VmValue::Nil) => None,
                Some(VmValue::String(id)) if !id.is_empty() => Some(id.to_string()),
                Some(VmValue::String(_)) => None,
                Some(other) => {
                    return Err(VmError::TypeError(format!(
                        "HarnessSecrets scope `id` must be a string or nil, got {}",
                        other.type_name()
                    )))
                }
            };
            match kind {
                "tenant" => Ok(crate::secrets::SecretScope::tenant(id.or_else(|| {
                    crate::harness_tenant::current_tenant_id().map(|tenant| tenant.0)
                }))),
                "workspace" => id
                    .map(crate::secrets::SecretScope::workspace)
                    .ok_or_else(|| {
                        VmError::TypeError(
                            "HarnessSecrets workspace scope requires non-empty `id`".to_string(),
                        )
                    }),
                "system" if id.is_none() => Ok(crate::secrets::SecretScope::system()),
                "system" => Err(VmError::TypeError(
                    "HarnessSecrets system scope does not take an `id`".to_string(),
                )),
                other if !other.trim().is_empty() => {
                    Ok(crate::secrets::SecretScope::custom(other, id))
                }
                _ => Err(VmError::TypeError(
                    "HarnessSecrets scope `kind` must not be empty".to_string(),
                )),
            }
        }
        Some(other) => Err(VmError::TypeError(format!(
            "HarnessSecrets scope must be nil, string, or dict, got {}",
            other.type_name()
        ))),
    }
}

fn parse_secret_scope_string(raw: &str) -> Result<crate::secrets::SecretScope, VmError> {
    let value = raw.trim();
    if value.is_empty() || value == "tenant" {
        return Ok(crate::secrets::SecretScope::tenant(
            crate::harness_tenant::current_tenant_id().map(|tenant| tenant.0),
        ));
    }
    if value == "system" {
        return Ok(crate::secrets::SecretScope::system());
    }
    if let Some(id) = value.strip_prefix("tenant:") {
        return Ok(crate::secrets::SecretScope::tenant(
            (!id.is_empty())
                .then(|| id.to_string())
                .or_else(|| crate::harness_tenant::current_tenant_id().map(|tenant| tenant.0)),
        ));
    }
    if let Some(id) = value.strip_prefix("workspace:") {
        if id.is_empty() {
            return Err(VmError::TypeError(
                "HarnessSecrets workspace scope requires an id".to_string(),
            ));
        }
        return Ok(crate::secrets::SecretScope::workspace(id));
    }
    if let Some((kind, id)) = value.split_once(':') {
        if kind.is_empty() {
            return Err(VmError::TypeError(
                "HarnessSecrets custom scope kind must not be empty".to_string(),
            ));
        }
        return Ok(crate::secrets::SecretScope::custom(
            kind,
            (!id.is_empty()).then(|| id.to_string()),
        ));
    }
    Ok(crate::secrets::SecretScope::custom(value, None))
}

fn secret_id_for_scope(
    name: &str,
    scope: &crate::secrets::SecretScope,
) -> Result<crate::secrets::SecretId, VmError> {
    if name.trim().starts_with(crate::secrets::SECRET_REF_SCHEME) || name.contains('/') {
        return crate::secrets::parse_secret_id(name).map_err(secret_error_to_vm);
    }
    Ok(crate::secrets::SecretId::new(scope.namespace(), name))
}

fn optional_duration_arg(
    value: Option<&VmValue>,
    callee: &str,
) -> Result<Option<std::time::Duration>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(value) => required_duration_arg(Some(value), callee).map(Some),
    }
}

fn required_duration_arg(
    value: Option<&VmValue>,
    callee: &str,
) -> Result<std::time::Duration, VmError> {
    let millis = match value {
        Some(VmValue::Int(ms)) | Some(VmValue::Duration(ms)) => *ms,
        Some(other) => {
            return Err(VmError::TypeError(format!(
                "{callee} expects duration as int milliseconds or duration, got {}",
                other.type_name()
            )))
        }
        None => {
            return Err(VmError::TypeError(format!(
                "{callee} expects duration as int milliseconds or duration"
            )))
        }
    };
    let millis = u64::try_from(millis)
        .map_err(|_| VmError::TypeError(format!("{callee} duration must be non-negative")))?;
    Ok(std::time::Duration::from_millis(millis))
}

fn secret_rotation_options_arg(
    value: Option<&VmValue>,
) -> Result<crate::secrets::SecretRotationOptions, VmError> {
    let Some(value) = value else {
        return Ok(crate::secrets::SecretRotationOptions::default());
    };
    match value {
        VmValue::Nil => Ok(crate::secrets::SecretRotationOptions::default()),
        VmValue::Dict(dict) => Ok(crate::secrets::SecretRotationOptions {
            grace: optional_duration_arg(dict.get("grace_ms"), "HarnessSecrets.rotate grace_ms")?,
            ttl: optional_duration_arg(dict.get("ttl_ms"), "HarnessSecrets.rotate ttl_ms")?,
        }),
        other => Err(VmError::TypeError(format!(
            "HarnessSecrets.rotate options must be a dict or nil, got {}",
            other.type_name()
        ))),
    }
}

fn secret_audit_context() -> crate::secrets::SecretAuditContext {
    let principal = crate::harness_auth::current_auth_principal();
    crate::secrets::SecretAuditContext {
        request_id: crate::observability::request_id::current_request_id(),
        actor_subject: principal
            .as_ref()
            .filter(|principal| !principal.subject.is_empty())
            .map(|principal| principal.subject.clone()),
        actor_kind: principal.and_then(|principal| principal.kind.clone()),
    }
}

fn secret_scope_value(scope: &crate::secrets::SecretScope) -> VmValue {
    let mut out = std::collections::BTreeMap::new();
    out.put_str("kind", scope.kind());
    out.put_opt_str("id", scope.id());
    VmValue::dict(out)
}

fn secret_id_value(id: &crate::secrets::SecretId) -> VmValue {
    let mut out = std::collections::BTreeMap::new();
    out.put_str("namespace", &id.namespace);
    out.put_str("name", &id.name);
    match id.version {
        crate::secrets::SecretVersion::Latest => out.put_str("version", "latest"),
        crate::secrets::SecretVersion::Exact(version) => {
            out.put_int("version", version.min(i64::MAX as u64) as i64);
        }
    }
    VmValue::dict(out)
}

fn secret_write_receipt_value(receipt: crate::secrets::SecretWriteReceipt) -> VmValue {
    let mut out = std::collections::BTreeMap::new();
    out.put_str("provider", receipt.provider);
    out.put("id", secret_id_value(&receipt.id));
    out.put("scope", secret_scope_value(&receipt.scope));
    out.put_opt(
        "version",
        receipt
            .version
            .map(|version| VmValue::Int(version.min(i64::MAX as u64) as i64)),
    );
    out.put_opt(
        "expires_at_ms",
        receipt.expires_at_unix_ms.map(VmValue::Int),
    );
    VmValue::dict(out)
}

fn secret_rotation_receipt_value(receipt: crate::secrets::SecretRotationReceipt) -> VmValue {
    let mut out = std::collections::BTreeMap::new();
    out.put_str("provider", receipt.provider);
    out.put("id", secret_id_value(&receipt.id));
    out.put("scope", secret_scope_value(&receipt.scope));
    out.put_opt(
        "from_version",
        receipt
            .from_version
            .map(|version| VmValue::Int(version.min(i64::MAX as u64) as i64)),
    );
    out.put_opt(
        "to_version",
        receipt
            .to_version
            .map(|version| VmValue::Int(version.min(i64::MAX as u64) as i64)),
    );
    out.put_opt(
        "grace_until_ms",
        receipt.grace_until_unix_ms.map(VmValue::Int),
    );
    out.put_opt(
        "expires_at_ms",
        receipt.expires_at_unix_ms.map(VmValue::Int),
    );
    VmValue::dict(out)
}

fn secret_lease_grant_value(
    bytes_value: bool,
    grant: crate::secrets::SecretLeaseGrant,
) -> Result<VmValue, VmError> {
    let value = grant.value.with_exposed(|bytes| {
        if bytes_value {
            return Ok(VmValue::Bytes(std::sync::Arc::new(bytes.to_vec())));
        }
        std::str::from_utf8(bytes)
            .map(str::to_string)
            .map(vm_string)
            .map_err(|error| {
                VmError::TypeError(format!(
                    "HarnessSecrets.lease secret `{}` was not UTF-8: {error}",
                    grant.id.name
                ))
            })
    })?;
    let mut out = std::collections::BTreeMap::new();
    out.put_str("provider", grant.provider);
    out.put("id", secret_id_value(&grant.id));
    out.put("scope", secret_scope_value(&grant.scope));
    out.put_str("lease_id", grant.lease_id);
    out.put("value", value);
    out.put_int("expires_at_ms", grant.expires_at_unix_ms);
    Ok(VmValue::dict(out))
}

fn secret_error_to_vm(error: crate::secrets::SecretError) -> VmError {
    use crate::secrets::SecretError;
    match error {
        SecretError::NotFound { .. } | SecretError::NoProviders { .. } => {
            VmError::CategorizedError {
                message: error.to_string(),
                category: ErrorCategory::NotFound,
            }
        }
        SecretError::Unsupported { .. } | SecretError::InvalidInput(_) => {
            VmError::TypeError(error.to_string())
        }
        SecretError::AccessDenied { .. } => VmError::CategorizedError {
            message: error.to_string(),
            category: ErrorCategory::Auth,
        },
        SecretError::Backend { .. } | SecretError::InvalidConfig(_) | SecretError::All(_) => {
            VmError::CategorizedError {
                message: error.to_string(),
                category: ErrorCategory::ToolError,
            }
        }
    }
}

fn mock_term_dimension(raw: Option<&str>, fallback: usize) -> usize {
    crate::term::dimension_from_env(raw).unwrap_or(fallback)
}

/// Mock variant of `harness.stdio.read_line` / `prompt`. When called with
/// no options dict, returns a plain string (or nil at EOF); when called
/// with an options dict, returns the structured `{ok, status, value?}`
/// dict that mirrors the real surface so tests can assert on either
/// shape without re-mocking.
fn mock_read_line_value(state: &crate::harness::MockHarnessState, args: &[VmValue]) -> VmValue {
    let structured = matches!(args.first(), Some(VmValue::Dict(_)));
    match state.pop_stdin_line() {
        Some(line) => {
            if structured {
                let mut out = std::collections::BTreeMap::new();
                out.insert("ok".to_string(), VmValue::Bool(true));
                out.put_str("status", "ok");
                out.put_str("value", line);
                VmValue::dict(out)
            } else {
                VmValue::String(arcstr::ArcStr::from(line))
            }
        }
        None => {
            if structured {
                let mut out = std::collections::BTreeMap::new();
                out.insert("ok".to_string(), VmValue::Bool(false));
                out.put_str("status", "eof");
                VmValue::dict(out)
            } else {
                VmValue::Nil
            }
        }
    }
}
