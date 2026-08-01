fn harness_profile_name(kind: HarnessKind, method: &str) -> String {
    match kind.field_name() {
        Some(field) => format!("harness.{field}.{method}"),
        None => format!("harness.{method}"),
    }
}

impl crate::vm::Vm {
    pub(crate) async fn call_harness_method(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let clock = std::sync::Arc::clone(handle.inner().clock());
        // Harness methods can re-enter Harn through agent/tool callbacks.
        // Keep the large, capability-wide dispatch future behind one pointer
        // so each nested call does not copy its full state machine onto the
        // executor stack. This is the capability analogue of the boxed
        // builtin observation guard in `vm::dispatch` and keeps the ordinary
        // 2 MiB worker-thread stack sufficient for nested effectful agents.
        let dispatch = Box::pin(self.call_harness_method_in_scope(handle, method, args));
        crate::clock_mock::scope_capability_clock(
            clock,
            dispatch,
        )
        .await
    }

    /// Dispatch after installing receiver-owned runtime context.
    ///
    /// Keeping the public dispatcher as the single context boundary means
    /// builtin-backed and native methods share identical capability semantics.
    async fn call_harness_method_in_scope(
        &mut self,
        handle: &VmHarness,
        method: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        if let Some(capability) = handle.kind().capability_id() {
            if crate::stdlib::capability_method_manifest_entry(capability, method).is_none() {
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
            let autonomy_decision =
                crate::autonomy::enforce_capability_side_effect(handle, capability, method, args)
                    .await?;
            if let Some(crate::autonomy::AutonomyDecision::Skip(value)) = &autonomy_decision {
                return Ok(value.clone());
            }
            let autonomy_approved = matches!(
                autonomy_decision,
                Some(crate::autonomy::AutonomyDecision::AllowApproved)
            );
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
            } else if !defers_to_selected_tool && !autonomy_approved {
                crate::orchestration::enforce_current_policy_for_capability(
                    capability, method, args,
                )?;
                self.record_capability_effects(capability, method, args);
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
                .get(&capability)
                .and_then(|methods| methods.get(method))
                .cloned()
            {
                let qualified_name = format!("harness.{}.{method}", capability.field_name());
                return self
                    .call_builtin_entry(&qualified_name, dispatch, args.to_vec())
                    .await;
            }
        }
        // A same-named global builtin is not implicitly a capability method.
        // Legacy globals such as `path_metadata_set(path, namespace, data)`
        // may share a method name while accepting a different argument shape.
        // Only an explicit, matching Harness exposure may use the builtin
        // registry as the method implementation.
        let declared_builtin_method = handle.kind().capability_id().is_some_and(|capability| {
            self.builtin_metadata.get(method).is_some_and(|metadata| {
                matches!(
                    metadata.contract().exposure,
                    harn_builtin_meta::BuiltinExposure::HarnessMethod {
                        capability: declared_capability,
                        method: declared_method,
                    } if declared_capability == capability && declared_method == method
                )
            })
        });
        if declared_builtin_method
            && (self.builtins.contains_key(method) || self.async_builtins.contains_key(method))
        {
            return self.call_capability_builtin(method, args.to_vec()).await;
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
        let profile_name = crate::builtin_profile::is_enabled()
            .then(|| harness_profile_name(handle.kind(), method));
        let _profile_timer = profile_name
            .as_deref()
            .and_then(crate::builtin_profile::BuiltinTimer::start);
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
                self.call_capability_builtin(
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
                self.call_capability_builtin(&format!("__{method}"), args.to_vec())
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
                self.call_capability_builtin("__embed", args.to_vec()).await
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
                self.call_capability_builtin(builtin, args.to_vec()).await
            }
            HarnessKind::Sqlite if method == "open" => {
                self.call_capability_builtin("sqlite_open", args.to_vec()).await
            }
            HarnessKind::Postgres => {
                let builtin = match method {
                    "connect" => "pg_connect",
                    "pool" => "pg_pool",
                    _ => return Err(method_unsupported(handle, method)),
                };
                self.call_capability_builtin(builtin, args.to_vec()).await
            }
            HarnessKind::Agent => {
                let transcript_builtin = match method {
                    "transcript_inject_reminder" => Some("__transcript_inject_reminder"),
                    "transcript_clear_reminders" => Some("__transcript_clear_reminders"),
                    _ => None,
                };
                if let Some(builtin) = transcript_builtin {
                    return self.call_capability_builtin(builtin, args.to_vec()).await;
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
                    "pool_create" => Some("__pool_create"),
                    "pool_get" => Some("__pool_get"),
                    "pool_list" => Some("__pool_list"),
                    "pool_wait" => Some("__pool_wait"),
                    "pool_simulate_restart" => Some("__pool_simulate_restart"),
                    _ => None,
                };
                if let Some(builtin) = worker_builtin {
                    return self.call_capability_builtin(builtin, args.to_vec()).await;
                }
                if let Some(suffix) = method.strip_prefix("state_") {
                    return self
                        .call_capability_builtin(&format!("__agent_state_{suffix}"), args.to_vec())
                        .await;
                }
                let host_primitive = method.starts_with("session_")
                    || matches!(
                        method,
                        "emit_event"
                            | "reminder_providers_fire"
                            | "capture_events"
                            | "parse_tool_calls"
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
                self.call_capability_builtin(&builtin, args.to_vec()).await
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
        let started = crate::builtin_profile::is_enabled().then(std::time::Instant::now);
        let result =
            Self::call_harness_method_sync_fast_inner(output, executed_effects, handle, method, args);
        if result.is_some() {
            if let Some(started) = started {
                crate::builtin_profile::record(
                    &harness_profile_name(handle.kind(), method),
                    started.elapsed(),
                );
            }
        }
        result
    }

    fn call_harness_method_sync_fast_inner(
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
        if crate::stdlib::capability_method_manifest_entry(capability, method).is_none() {
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
            "now_ms" => Some(Ok(VmValue::Int(
                crate::stdlib::clock::now_wall_ms_from(clock.as_ref()),
            ))),
            "timestamp" => Some(Ok(VmValue::Float(
                crate::stdlib::clock::now_wall_ms_from(clock.as_ref()) as f64 / 1_000.0,
            ))),
            "monotonic_ms" | "elapsed" => Some(Ok(VmValue::Int(
                crate::stdlib::clock::now_monotonic_ms_from(clock.as_ref()),
            ))),
            "date_iso" => {
                let millis = crate::stdlib::clock::now_wall_ms_from(clock.as_ref());
                let timestamp = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
                    .map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                    .unwrap_or_default();
                Some(Ok(VmValue::String(arcstr::ArcStr::from(timestamp))))
            }
            "now" => {
                let millis = crate::stdlib::clock::now_wall_ms_from(clock.as_ref());
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
                    "now_ms" => Some(Ok(VmValue::Int(
                        crate::stdlib::clock::now_wall_ms_from(clock.as_ref()),
                    ))),
                    "timestamp" => {
                        let value = crate::stdlib::clock::now_wall_ms_from(clock.as_ref());
                        Some(Ok(VmValue::Float(value as f64 / 1_000.0)))
                    }
                    "monotonic_ms" | "elapsed" => Some(Ok(VmValue::Int(
                        crate::stdlib::clock::now_monotonic_ms_from(clock.as_ref()),
                    ))),
                    "date_iso" => {
                        let millis = crate::stdlib::clock::now_wall_ms_from(clock.as_ref());
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
                            crate::stdlib::clock::record_clock_sleep_from(
                                clock.as_ref(),
                                ms as u64,
                            );
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

}
