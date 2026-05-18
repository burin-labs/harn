//! Method dispatch for the `Harness` capability handle and its six
//! sub-handles. The stdio and clock slices are wired end-to-end; subsequent
//! tickets replace the stub bodies on the remaining sub-handles with real
//! implementations.

use std::time::Duration;

use crate::harness::{vm_string, HarnessKind, HarnessMode, VmHarness};
use crate::stdlib::io::{
    prompt_user_value, read_line_legacy_value, read_line_structured_value, write_stderr,
    write_stdout,
};
use crate::value::{ErrorCategory, VmError, VmValue};

impl crate::vm::Vm {
    pub(super) async fn call_harness_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if let HarnessMode::Null(state) = handle.inner().mode() {
            state.record_deny(handle.kind(), method, args);
            return Err(VmError::CategorizedError {
                message: format!("NullHarness denied {}::{method}", handle.kind().type_name()),
                category: ErrorCategory::ToolRejected,
            });
        }
        if matches!(handle.inner().mode(), HarnessMode::Mock(_)) {
            return self.call_mock_harness_method(handle, method, args).await;
        }
        match handle.kind() {
            HarnessKind::Root => self.call_harness_root_method(handle, method, args).await,
            HarnessKind::Stdio => self.call_harness_stdio_method(handle, method, args),
            HarnessKind::Clock => self.call_harness_clock_method(handle, method, args).await,
            HarnessKind::Fs | HarnessKind::Env | HarnessKind::Random | HarnessKind::Net => {
                Err(VmError::TypeError(format!(
                "{}::{method} is not yet implemented — wired by the E4.2-E4.4 migration tickets",
                handle.type_name(),
            )))
            }
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
                let snapshot = crate::orchestration::unsettled_state_snapshot();
                Ok(crate::stdlib::json_to_vm_value(&snapshot.to_json()))
            }
            "is_empty" => {
                let empty = match args.first() {
                    Some(state) => state_counts(state)?.is_empty(),
                    None => crate::orchestration::unsettled_state_snapshot().is_empty(),
                };
                Ok(VmValue::Bool(empty))
            }
            "counts" => match args.first() {
                Some(state) => Ok(crate::stdlib::json_to_vm_value(
                    &state_counts(state)?.to_json(),
                )),
                None => {
                    let snapshot = crate::orchestration::unsettled_state_snapshot();
                    Ok(crate::stdlib::json_to_vm_value(&snapshot.counts_json()))
                }
            },
            "summary" => match args.first() {
                Some(state) => Ok(VmValue::String(std::rc::Rc::from(
                    state_counts(state)?.summary().as_str(),
                ))),
                None => {
                    let snapshot = crate::orchestration::unsettled_state_snapshot();
                    Ok(VmValue::String(std::rc::Rc::from(snapshot.summary())))
                }
            },
            "resume_subagent" => {
                let handle_arg = args.first().cloned().ok_or_else(|| {
                    VmError::TypeError("Harness.resume_subagent expects a handle".to_string())
                })?;
                if let Some(input) = args.get(1).cloned() {
                    self.call_named_builtin("__host_worker_send_input", vec![handle_arg, input])
                        .await
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
                let snapshot = crate::orchestration::unsettled_state_snapshot();
                let status = if snapshot.is_empty() {
                    "settled"
                } else {
                    "unsettled"
                };
                Ok(crate::stdlib::json_to_vm_value(&serde_json::json!({
                    "status": status,
                    "timed_out": false,
                    "state": snapshot.to_json(),
                })))
            }
            "current_pipeline_id" => Ok(crate::orchestration::current_mutation_session()
                .and_then(|session| session.run_id.or(Some(session.session_id)))
                .map(|id| VmValue::String(std::rc::Rc::from(id)))
                .unwrap_or(VmValue::Nil)),
            "handoff_to" => Ok(record_handoff_envelope(args)),
            "emit_audit" => Ok(record_emit_audit(args)),
            "acknowledge_trigger" | "defer_trigger" => Ok(unsupported_harness_action(
                method,
                "queued trigger items do not have an acknowledgement registry on this branch",
            )),
            "acknowledge_handoff" => Ok(unsupported_harness_action(
                method,
                "partial handoff envelopes do not have an acknowledgement registry on this branch",
            )),
            "finalize" => Ok(unsupported_harness_action(
                method,
                "pipeline disposition persistence is not available on this branch",
            )),
            "spawn_settlement_agent" => Ok(unsupported_harness_action(
                method,
                "settlement-agent spawning is deferred to the drain implementation (P-03, harn#1856)",
            )),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    fn call_harness_stdio_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "println" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stdout(&mut self.output, &format!("{msg}\n"));
                Ok(VmValue::Nil)
            }
            "print" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stdout(&mut self.output, &msg);
                Ok(VmValue::Nil)
            }
            "eprintln" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stderr(&format!("{msg}\n"));
                Ok(VmValue::Nil)
            }
            "eprint" => {
                let msg = args.first().map(|a| a.display()).unwrap_or_default();
                write_stderr(&msg);
                Ok(VmValue::Nil)
            }
            "read_line" => {
                if args.is_empty() {
                    Ok(read_line_legacy_value())
                } else {
                    read_line_structured_value(args)
                }
            }
            "prompt" => prompt_user_value(args, &mut self.output),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    async fn call_harness_clock_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let clock = handle.inner().clock();
        match method {
            "now_ms" => Ok(VmValue::Int(crate::clock::now_wall_ms(clock.as_ref()))),
            "timestamp" => Ok(VmValue::Float(
                crate::clock::now_wall_ms(clock.as_ref()) as f64 / 1_000.0,
            )),
            "monotonic_ms" | "elapsed" => Ok(VmValue::Int(clock.monotonic_ms())),
            "sleep_ms" => {
                let ms = sleep_ms_arg(args)?;
                if ms > 0 {
                    clock.sleep(Duration::from_millis(ms as u64)).await;
                }
                Ok(VmValue::Nil)
            }
            _ => Err(method_unsupported(handle, method)),
        }
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
        state.record_call(handle.kind(), method, args);
        match handle.kind() {
            HarnessKind::Root => Err(method_unsupported(handle, method)),
            HarnessKind::Stdio => match method {
                "println" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stdio(&format!("{msg}\n"));
                    Ok(VmValue::Nil)
                }
                "print" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stdio(&msg);
                    Ok(VmValue::Nil)
                }
                "eprintln" | "eprint" => Ok(VmValue::Nil),
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Clock => {
                let clock = handle.inner().clock();
                match method {
                    "now_ms" => Ok(VmValue::Int(crate::clock::now_wall_ms(clock.as_ref()))),
                    "timestamp" => Ok(VmValue::Float(
                        crate::clock::now_wall_ms(clock.as_ref()) as f64 / 1_000.0,
                    )),
                    "monotonic_ms" | "elapsed" => Ok(VmValue::Int(clock.monotonic_ms())),
                    "sleep_ms" => {
                        let ms = sleep_ms_arg(args)?;
                        if ms > 0 {
                            state.advance_clock(Duration::from_millis(ms as u64));
                        }
                        Ok(VmValue::Nil)
                    }
                    _ => Err(method_unsupported(handle, method)),
                }
            }
            HarnessKind::Fs => match method {
                "read_file" | "read" => {
                    let path = string_arg(args, 0, "HarnessFs.read_file")?;
                    let bytes = state
                        .fs_read(path)
                        .ok_or_else(|| VmError::CategorizedError {
                            message: format!("MockHarness has no fs_read response for {path}"),
                            category: ErrorCategory::NotFound,
                        })?;
                    Ok(VmValue::Bytes(std::rc::Rc::new(bytes.to_vec())))
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
            HarnessKind::Env => match method {
                "get" => {
                    let key = string_arg(args, 0, "HarnessEnv.get")?;
                    Ok(state.env_get(key).map(vm_string).unwrap_or(VmValue::Nil))
                }
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Random => match method {
                "u64" | "gen_u64" => state
                    .next_random_u64()
                    .map(|value| VmValue::Int(value.min(i64::MAX as u64) as i64))
                    .ok_or_else(|| VmError::CategorizedError {
                        message: "MockHarness has no random_u64 response".to_string(),
                        category: ErrorCategory::NotFound,
                    }),
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Net => match method {
                "get" | "http_get" => {
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
        }
    }
}

#[derive(Clone, Copy)]
struct UnsettledCounts {
    suspended: usize,
    queued: usize,
    partial: usize,
    in_flight: usize,
}

impl UnsettledCounts {
    fn is_empty(self) -> bool {
        self.suspended == 0 && self.queued == 0 && self.partial == 0 && self.in_flight == 0
    }

    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "suspended": self.suspended,
            "queued": self.queued,
            "partial": self.partial,
            "in_flight": self.in_flight,
        })
    }

    fn summary(self) -> String {
        if self.is_empty() {
            "no unsettled work".to_string()
        } else {
            format!(
                "unsettled work: {} suspended subagents, {} queued triggers, {} partial handoffs, {} in-flight llm calls",
                self.suspended, self.queued, self.partial, self.in_flight
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
    })
}

fn state_bucket_len(
    dict: &std::collections::BTreeMap<String, VmValue>,
    key: &str,
) -> Result<usize, VmError> {
    match dict.get(key) {
        Some(VmValue::List(items)) => Ok(items.len()),
        Some(other) => Err(VmError::TypeError(format!(
            "unsettled-state field `{key}` must be a list, got {}",
            other.type_name()
        ))),
        None => Ok(0),
    }
}

fn unsupported_harness_action(method: &str, reason: &str) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": "unsupported",
        "method": method,
        "reason": reason,
    }))
}

fn record_emit_audit(args: &[VmValue]) -> VmValue {
    let kind = args
        .first()
        .map(|v| match v {
            VmValue::String(s) => s.as_ref().to_string(),
            other => other.display(),
        })
        .unwrap_or_default();
    let payload = args
        .get(1)
        .map(crate::llm::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let entry = crate::orchestration::record_lifecycle_audit(kind, payload);
    crate::stdlib::json_to_vm_value(&serde_json::json!({
        "status": "recorded",
        "method": "emit_audit",
        "entry": entry.to_json(),
    }))
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
        VmValue::String(s) => s.as_ref().to_string(),
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

fn method_unsupported(handle: &VmHarness, method: &str) -> VmError {
    VmError::TypeError(format!(
        "value of type {} has no method `{method}`",
        handle.type_name()
    ))
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
        Some(VmValue::String(value)) => Ok(value.as_ref()),
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
