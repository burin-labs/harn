impl crate::vm::Vm {
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
                    crate::stdlib::clock::record_clock_sleep_from(clock.as_ref(), ms as u64);
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
                // Source-relative paths are lexical: imported helpers resolve
                // them against the module owning the current closure. The VM
                // updates this context at each call-frame transition,
                // including workflow child-VM crossings.
                let path = crate::stdlib::process::VM_SOURCE_DIR
                    .with(|source_dir| source_dir.borrow().clone())
                    .or_else(|| self.source_dir.clone())
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
                return self.call_capability_builtin("runtime_paths", vec![]).await;
            }
            "package_snapshot_open" | "package_snapshot_close" => {
                return self
                    .call_capability_builtin(method, args.to_vec())
                    .await
                    .map_err(tag_sandbox_denied);
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
            "render_prompt" => "render",
            "render_prompt_with_provenance" => "render_with_provenance",
            "render_template" => "render_string",
            _ => return Err(method_unsupported(handle, method)),
        };
        self.call_capability_sync_builtin(builtin, args)
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
            return crate::http::execute_harness_http_verb(
                handle.inner().fixtures().http_mocks(),
                http_method,
                has_body,
                args.to_vec(),
            )
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
            return self.call_capability_builtin(builtin, args.to_vec()).await;
        }
        match method {
            "request" => {
                let http_method = string_arg(args, 0, "HarnessNet.request")?.to_string();
                let url = string_arg(args, 1, "HarnessNet.request")?.to_string();
                let options = match args.get(2) {
                    Some(VmValue::Dict(d)) => (**d).clone(),
                    _ => crate::value::DictMap::new(),
                };
                crate::http::execute_harness_http_request(
                    handle.inner().fixtures().http_mocks(),
                    &http_method.to_uppercase(),
                    &url,
                    &options,
                )
                .await
                .map_err(tag_sandbox_denied)
            }
            "download" => crate::http::execute_harness_http_download(
                handle.inner().fixtures().http_mocks(),
                args.to_vec(),
            )
            .await
            .map_err(tag_sandbox_denied),
            "stream_open" => crate::http::execute_harness_http_stream_open(
                handle.inner().fixtures().http_mocks(),
                args.to_vec(),
            )
            .await,
            "stream_read" => {
                self.call_capability_builtin("__http_stream_read", args.to_vec())
                    .await
            }
            "stream_info" => {
                self.call_capability_builtin("__http_stream_info", args.to_vec())
                    .await
            }
            "stream_close" => {
                self.call_capability_builtin("__http_stream_close", args.to_vec())
                    .await
            }
            "session" => {
                self.call_capability_builtin("__http_session", args.to_vec())
                    .await
            }
            "session_request" => crate::http::execute_harness_http_session_request(
                handle.inner().fixtures().http_mocks(),
                args.to_vec(),
            )
            .await,
            "session_close" => {
                self.call_capability_builtin("__http_session_close", args.to_vec())
                    .await
            }
            "unix_socket_json_request" => self
                .call_capability_builtin("__net_unix_socket_json_request", args.to_vec())
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
            "exec" | "exec_at" => {
                let (cwd, command) = if method == "exec_at" {
                    if args.len() < 2 {
                        return Err(VmError::Thrown(vm_string(
                            "exec_at: directory and command are required",
                        )));
                    }
                    (
                        Some(string_arg(args, 0, "HarnessProcess.exec_at")?),
                        &args[1..],
                    )
                } else {
                    if args.is_empty() {
                        return Err(VmError::Thrown(vm_string("exec: command is required")));
                    }
                    (None, args)
                };
                let argv = command
                    .iter()
                    .map(|value| match value {
                        VmValue::String(value) => Ok(VmValue::String(value.clone())),
                        other => Err(VmError::TypeError(format!(
                            "HarnessProcess.{method}: command entries must be strings, got {}",
                            other.type_name()
                        ))),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut wire = crate::value::DictMap::new();
                wire.put_str("mode", "argv");
                wire.insert(
                    crate::value::intern_key("argv"),
                    VmValue::List(std::sync::Arc::new(argv)),
                );
                if let Some(cwd) = cwd {
                    wire.put_str("cwd", cwd);
                }
                let ctx = crate::vm::AsyncBuiltinCtx::from_vm(self.child_vm());
                return crate::stdlib::host::dispatch_host_operation_with_ctx(
                    Some(&ctx),
                    "process",
                    "exec",
                    &wire,
                )
                .await;
            }
            "shell" | "shell_at" => {
                let (cwd, command_index) = if method == "shell_at" {
                    if args.len() < 2 {
                        return Err(VmError::Thrown(vm_string(
                            "shell_at: directory and command string are required",
                        )));
                    }
                    (Some(string_arg(args, 0, "HarnessProcess.shell_at")?), 1)
                } else {
                    (None, 0)
                };
                let command = args
                    .get(command_index)
                    .and_then(|value| match value {
                        VmValue::String(value) if !value.is_empty() => Some(value.as_str()),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        VmError::Thrown(vm_string(if method == "shell_at" {
                            "shell_at: command string is required"
                        } else {
                            "shell: command string is required"
                        }))
                    })?;
                let mut wire = crate::value::DictMap::new();
                wire.put_str("mode", "shell");
                wire.put_str("command", command);
                if let Some(cwd) = cwd {
                    wire.put_str("cwd", cwd);
                }
                let ctx = crate::vm::AsyncBuiltinCtx::from_vm(self.child_vm());
                return crate::stdlib::host::dispatch_host_operation_with_ctx(
                    Some(&ctx),
                    "process",
                    "exec",
                    &wire,
                )
                .await;
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
            return self.call_capability_builtin("host_capabilities", vec![]).await;
        }
        if method == "host_has" {
            return self.call_capability_builtin("host_has", args.to_vec()).await;
        }
        if method == "sync_mutex_acquire" {
            return self
                .call_capability_builtin("sync_mutex_acquire", args.to_vec())
                .await;
        }
        if method == "introspection" {
            return self
                .call_capability_builtin("runtime_introspection", args.to_vec())
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
            return self.call_capability_builtin(builtin, args.to_vec()).await;
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
            "mcp_bootstrap" => "__host_mcp_bootstrap",
            _ => return Err(method_unsupported(handle, method)),
        };
        if method == "list_registered" || method == "mcp_bootstrap" {
            self.call_capability_builtin(builtin, args.to_vec()).await
        } else {
            // Invocation authority is enforced by the selected tool's own
            // contract, which can be narrower than the registry handle.
            self.call_named_builtin(builtin, args.to_vec()).await
        }
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
            return self.call_capability_builtin(builtin, builtin_args).await;
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
            return self.call_capability_builtin(builtin, args.to_vec()).await;
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
            "http_mock" => {
                let method = string_arg(args, 0, "HarnessTesting.http_mock")?.to_string();
                let url_pattern =
                    string_arg(args, 1, "HarnessTesting.http_mock")?.to_string();
                let response = args
                    .get(2)
                    .and_then(VmValue::as_dict)
                    .cloned()
                    .unwrap_or_default();
                crate::http::register_harness_http_mock(
                    fixtures.http_mocks(),
                    method,
                    url_pattern,
                    &response,
                );
                Ok(VmValue::Nil)
            }
            "http_mock_clear" => {
                require_no_args(handle, method, args)?;
                fixtures.http_mocks().clear();
                Ok(VmValue::Nil)
            }
            "http_mock_calls" => {
                let options = args.first().and_then(VmValue::as_dict);
                let include_sensitive = options.is_some_and(|options| {
                    options
                        .get("include_sensitive")
                        .and_then(|value| match value {
                            VmValue::Bool(value) => Some(*value),
                            _ => None,
                        })
                        .unwrap_or(false)
                        || options
                            .get("include_sensitive_headers")
                            .and_then(|value| match value {
                                VmValue::Bool(value) => Some(*value),
                                _ => None,
                            })
                            .unwrap_or(false)
                });
                let redact_sensitive = !include_sensitive
                    && options.is_none_or(|options| {
                        options
                            .get("redact_sensitive")
                            .or_else(|| options.get("redact_headers"))
                            .and_then(|value| match value {
                                VmValue::Bool(value) => Some(*value),
                                _ => None,
                            })
                            .unwrap_or(true)
                    });
                Ok(VmValue::List(std::sync::Arc::new(
                    fixtures.http_mocks().calls_value(redact_sensitive),
                )))
            }
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
                let advanced = handle.inner().advance_test_clock(milliseconds)?;
                if milliseconds > 0 {
                    crate::stdlib::clock::record_clock_sleep_from(
                        handle.inner().clock().as_ref(),
                        milliseconds as u64,
                    );
                }
                Ok(VmValue::Int(advanced))
            }
            "clock_reset" => {
                require_no_args(handle, method, args)?;
                handle.inner().clear_test_clock();
                Ok(VmValue::Nil)
            }
            "transport_mock_clear" | "transport_mock_calls" => {
                self.call_capability_builtin(&format!("__{method}"), args.to_vec())
                    .await
            }
            "stdin_set" => self.call_capability_builtin("mock_stdin", args.to_vec()).await,
            "stdin_reset" => {
                self.call_capability_builtin("unmock_stdin", args.to_vec())
                    .await
            }
            "tty_set" => self.call_capability_builtin("mock_tty", args.to_vec()).await,
            "tty_reset" => {
                self.call_capability_builtin("unmock_tty", args.to_vec())
                    .await
            }
            "capture_stderr_start" => {
                self.call_capability_builtin("capture_stderr_start", args.to_vec())
                    .await
            }
            "capture_stderr_take" => {
                self.call_capability_builtin("capture_stderr_take", args.to_vec())
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
                let target_method = string_arg(args, 1, &format!("HarnessTesting.{method}"))?;
                let unregistered_ok = match args.get(5) {
                    None | Some(VmValue::Nil) => false,
                    Some(VmValue::Bool(value)) => *value,
                    Some(other) => {
                        return Err(VmError::TypeError(format!(
                            "HarnessTesting.{method}: unregistered_ok must be a bool, got {}",
                            other.type_name()
                        )))
                    }
                };
                if let Some(capability) =
                    harn_builtin_meta::CapabilityId::from_field_name(capability_name)
                {
                    if capability == harn_builtin_meta::CapabilityId::Testing {
                        return Err(VmError::TypeError(
                            "HarnessTesting cannot fixture its own control methods".to_string(),
                        ));
                    }
                    let declared = crate::stdlib::capability_method_manifest_entry(
                        capability,
                        target_method,
                    )
                    .is_some();
                    let host_method =
                        harn_builtin_meta::host_capabilities::is_host_capability_method(
                            capability,
                            target_method,
                        );
                    if !declared
                        && !host_method
                        && !unregistered_ok
                        && !crate::harness::is_capability_driver_fixture(capability, target_method)
                    {
                        return Err(VmError::TypeError(format!(
                            "HarnessTesting.{method}: undeclared method `harness.{}.{target_method}`",
                            capability.field_name()
                        )));
                    }
                } else if !crate::stdlib::host::host_operation_is_registered(
                    capability_name,
                    target_method,
                ) && !unregistered_ok
                {
                    return Err(VmError::TypeError(format!(
                        "HarnessTesting.{method}: unknown capability or host operation \
                         `{capability_name}.{target_method}`; pass unregistered_ok=true only for \
                         an operation the embedding host registers at runtime"
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
                fixtures.respond(capability_name, target_method, response, when, repeat);
                Ok(VmValue::Nil)
            }
            "calls" => {
                require_no_args(handle, method, args)?;
                let calls = fixtures
                    .calls()
                    .into_iter()
                    .map(|call| {
                        let mut record = crate::value::DictMap::new();
                        record.put_str("capability", call.capability);
                        if call.host_operation {
                            record.put_str("operation", call.member.clone());
                            record.put_str("method", call.member);
                            record.insert(
                                crate::value::intern_key("params"),
                                call.args.first().cloned().unwrap_or(VmValue::Nil),
                            );
                            record.insert(
                                crate::value::intern_key("args"),
                                VmValue::List(std::sync::Arc::new(call.args)),
                            );
                        } else {
                            record.put_str("method", call.member.clone());
                            record.put_str("operation", call.member);
                            record.insert(
                                crate::value::intern_key("params"),
                                call.args.first().cloned().unwrap_or(VmValue::Nil),
                            );
                            record.insert(
                                crate::value::intern_key("args"),
                                VmValue::List(std::sync::Arc::new(call.args)),
                            );
                        }
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
        self.call_capability_builtin(builtin, args.to_vec()).await
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
                // Inline template rendering is deterministic over explicit
                // arguments and performs no filesystem access. Mock Harnesses
                // therefore execute the real parser/evaluator after recording
                // the call instead of requiring a canned response for every
                // bindings record.
                "render_template" => {
                    self.call_harness_fs_method(handle, method, args).await
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
