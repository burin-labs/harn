use super::*;

impl AcpServer {
    pub(super) async fn handle_session_prompt(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                self.send_prompt_protocol_error(id, "Missing sessionId");
                return;
            }
        };

        let prompt = match normalize_acp_prompt(params) {
            Ok(prompt) => prompt,
            Err(message) => {
                self.send_prompt_protocol_error(id, &message);
                return;
            }
        };
        let prompt_text = prompt.text.clone();

        let (cancellation, current_mode_id, inject_state, session_budget) =
            match self.sessions.get_mut(&session_id) {
                Some(s) => {
                    s.cancellation.begin_prompt();
                    s.host_bridge = None;
                    (
                        s.cancellation.clone(),
                        s.current_mode_id.clone(),
                        s.inject_state.clone(),
                        s.budget.clone(),
                    )
                }
                None => {
                    self.send_prompt_protocol_error(id, &format!("Unknown session: {session_id}"));
                    return;
                }
            };
        let prompt_budget = match session_budget {
            SessionBudget::Inherit => self.default_budget.clone(),
            SessionBudget::Unlimited => None,
            SessionBudget::Custom(spec) => Some(spec),
        };
        harn_vm::agent_sessions::open_or_create_with_actor_chain(
            Some(session_id.clone()),
            self.actor_chain(),
        );
        let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());
        if let Err(message) = self.sync_session_root_from_workspace_anchor(&session_id) {
            self.send_prompt_error(id, &message);
            return;
        }
        let (cwd, project_root, capability_profile) = match self.sessions.get(&session_id) {
            Some(session) => (
                session.cwd.clone(),
                session.project_root.clone(),
                session.capability_profile.clone(),
            ),
            None => {
                self.send_prompt_protocol_error(id, &format!("Unknown session: {session_id}"));
                return;
            }
        };
        #[cfg(feature = "hostlib")]
        harn_hostlib::fs::configure_session_root(&session_id, &project_root);
        let before_turn_transcript = harn_vm::agent_sessions::transcript(&session_id)
            .unwrap_or_else(|| {
                harn_vm::agent_sessions::snapshot(&session_id).unwrap_or(harn_vm::VmValue::Nil)
            });
        #[cfg(feature = "hostlib")]
        let before_turn_fs_snapshots: HashSet<String> =
            harn_hostlib::fs_snapshot::list_snapshots(&session_id)
                .map(|snapshots| {
                    snapshots
                        .into_iter()
                        .map(|snapshot| snapshot.snapshot_id)
                        .collect()
                })
                .unwrap_or_default();

        let (source, source_path) = if let Some(ref pipeline_path) = self.pipeline {
            let full_path = if Path::new(pipeline_path).is_absolute() {
                PathBuf::from(pipeline_path)
            } else {
                cwd.join(pipeline_path)
            };
            match std::fs::read_to_string(&full_path) {
                Ok(src) => (src, Some(full_path)),
                Err(e) => {
                    let message = format!("Failed to read pipeline {}: {e}", full_path.display());
                    self.send_prompt_error(id, &message);
                    return;
                }
            }
        } else {
            // Inline-prompt mode has no persistent pipeline source to host
            // `@command`-tagged decls, so a leading slash invocation can
            // only be the user expecting to invoke an advertised command
            // that doesn't exist. Surface a friendly error instead of
            // wrapping `/foo args` into `pipeline main() { /foo args }`,
            // which would fail with a generic "Compilation error" later.
            if parse_slash_invocation(&prompt_text).is_some() {
                self.send_prompt_error(
                    id,
                    "Slash commands require `--pipeline <file>`; the agent is running in inline mode.",
                );
                return;
            }
            // Wrap inline prompt source in a pipeline so the compiler has
            // an entry point.
            let wrapped = format!("pipeline main() {{\n{prompt_text}\n}}");
            (wrapped, None)
        };

        // Hot-reload: re-discover slash-commands from the just-loaded
        // source and emit `available_commands_update` if the set changed
        // since the last advertise. Only meaningful when a pipeline file
        // is configured; inline prompts have no persistent surface to
        // attach commands to.
        if source_path.is_some() {
            self.refresh_advertised_commands(&session_id, &source);
        }

        // Slash-command dispatch: if the prompt begins with `/<name>` and
        // `<name>` matches an advertised command, route to the named
        // pipeline with the post-name text as the new `prompt`. Unknown
        // slashes fall through unmodified — the default pipeline can
        // choose to treat them as text or surface its own diagnostic.
        let (effective_prompt, target_pipeline) = match parse_slash_invocation(&prompt_text) {
            Some((cmd_name, args)) => {
                let pipeline_name = self.sessions.get(&session_id).and_then(|session| {
                    session
                        .advertised_commands
                        .iter()
                        .find(|c| c.name == cmd_name)
                        .map(|c| c.pipeline_name.clone())
                });
                match pipeline_name {
                    Some(name) => (args.to_string(), Some(name)),
                    None => (prompt_text.clone(), None),
                }
            }
            None => (prompt_text.clone(), None),
        };
        let prompt_text = effective_prompt;
        let mut prompt = prompt;
        if prompt_text != prompt.text {
            retarget_prompt_text(&mut prompt, prompt_text.clone());
        }

        let output = self.output.clone();
        let pending = self.pending.clone();
        let next_id = &self.next_id;
        let sid = session_id.clone();

        // Translate AgentEvents into ACP session/update notifications so
        // the client observes tool lifecycle on the wire. The event-log
        // sink is reinstalled here because prompt teardown clears all
        // per-session transport sinks after each turn.
        clear_session_sinks(&session_id);
        harn_vm::agent_sessions::register_event_log_sink(&session_id);
        register_sink(
            session_id.clone(),
            Arc::new(AcpAgentEventSink::new(output.clone())),
        );

        let bridge = Arc::new(AcpBridge {
            session_id: sid.clone(),
            output: output.clone(),
            pending: pending.clone(),
            next_id_counter: AtomicU64::new(next_id.fetch_add(1000, Ordering::SeqCst)),
            cancellation: cancellation.clone(),
            script_name: std::sync::Mutex::new(String::new()),
            assistant_state: std::sync::Mutex::new(VisibleTextState::default()),
        });
        let bridge_output = output.clone();
        let host_bridge = Arc::new(
            harn_vm::bridge::HostBridge::from_parts_with_writer_cancel_notify_and_injection_state(
                bridge.pending.clone(),
                cancellation.cancelled.clone(),
                cancellation.notify.clone(),
                Arc::new(move |line| {
                    bridge_output.write_line(line);
                    Ok(())
                }),
                bridge.next_id_counter.fetch_add(10_000, Ordering::SeqCst),
                Some(inject_state),
            ),
        );
        host_bridge.set_session_id(&bridge.session_id);
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.host_bridge = Some(host_bridge.clone());
        }

        let compile_started = Instant::now();
        let (chunk, cache_hit) = match self.compile_pipeline_cached(
            &source,
            source_path.as_deref(),
            target_pipeline.as_deref(),
        ) {
            Ok(value) => value,
            Err(message) => {
                // Drop the error's "Compilation error: " prefix added inside
                // the helper — the caller used to format it identically.
                let mut formatted = message
                    .strip_prefix("Compilation error: ")
                    .map(|rest| format!("Compilation error: {rest}"))
                    .unwrap_or(message);
                if let Err(error) = self.clear_active_prompt_transport(&session_id).await {
                    formatted.push_str(&format!("; failed to persist agent events: {error}"));
                }
                self.send_prompt_error(id, &formatted);
                return;
            }
        };
        let compile_ms = compile_started.elapsed().as_millis() as u64;
        bridge.send_log(
            "info",
            &format!(
                "ACP_BOOT: compile_ms={compile_ms} cache={}",
                if cache_hit { "hit" } else { "miss" }
            ),
            Some(serde_json::json!({
                "compile_ms": compile_ms,
                "compile_cache": if cache_hit { "hit" } else { "miss" },
                "pipeline": source_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<inline>".to_string()),
            })),
        );
        let profile_turn = self.begin_profile_turn(&session_id);
        let _mode_guard = modes::ModePolicyGuard::enter(&current_mode_id, &self.sandbox);
        let (vm_baseline, vm_baseline_cache_hit, vm_baseline_prepare_ms) = match self
            .prepare_vm_baseline_cached(
                &source,
                source_path.as_deref(),
                target_pipeline.as_deref(),
                &cwd,
                &project_root,
                &current_mode_id,
            )
            .await
        {
            Ok(value) => value,
            Err(mut message) => {
                self.finish_profile_turn(&session_id, profile_turn);
                if let Err(error) = self.clear_active_prompt_transport(&session_id).await {
                    message.push_str(&format!("; failed to persist agent events: {error}"));
                }
                self.send_prompt_error(id, &message);
                return;
            }
        };
        let id_owned = id.clone();
        let send_output = self.output.clone();
        let host_bridge_for_response = host_bridge.clone();
        let _budget_guard = prompt_budget.as_ref().and_then(BudgetSpec::install);
        let result = execute::execute_chunk(
            chunk,
            bridge.clone(),
            host_bridge,
            execute::PromptGlobals {
                text: &prompt_text,
                content: &prompt.content,
                messages: &prompt.messages,
            },
            execute::VmSetup {
                source: &source,
                baseline: vm_baseline.as_ref(),
                baseline_cache_hit: vm_baseline_cache_hit,
                baseline_prepare_ms: vm_baseline_prepare_ms,
                source_path: source_path.as_deref(),
                cwd: &cwd,
                project_root: Some(&project_root),
                runtime_configurator: self.runtime_configurator.clone(),
                session_profile: capability_profile.clone(),
            },
        )
        .await;
        self.finish_profile_turn(&session_id, profile_turn);
        drop(_mode_guard);
        let sink_flush_error = self.clear_active_prompt_transport(&session_id).await.err();

        match result {
            Ok(output) => {
                if cancellation.cancelled.load(Ordering::SeqCst) {
                    send_json_response(
                        &send_output,
                        &id_owned,
                        cancelled_prompt_result(sink_flush_error.as_ref()),
                    );
                    return;
                }
                if let Some(error) = sink_flush_error {
                    self.send_prompt_error(
                        &id_owned,
                        &format!(
                            "Failed to persist agent events before prompt completion: {error}"
                        ),
                    );
                    return;
                }
                if !output.is_empty() {
                    bridge.send_update(&output);
                }
                let (stop_reason, terminal) =
                    host_bridge_for_response.take_prompt_outcome().map_or_else(
                        || ("end_turn".to_string(), None),
                        |(reason, terminal)| (reason, Some(terminal)),
                    );
                if stop_reason != "cancelled" {
                    #[cfg(feature = "hostlib")]
                    let fs_snapshot_ids = harn_hostlib::fs_snapshot::list_snapshots(&session_id)
                        .map(|snapshots| {
                            snapshots
                                .into_iter()
                                .filter_map(|snapshot| {
                                    (!before_turn_fs_snapshots.contains(&snapshot.snapshot_id))
                                        .then_some(snapshot.snapshot_id)
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    #[cfg(not(feature = "hostlib"))]
                    let fs_snapshot_ids = Vec::new();
                    let _ = harn_vm::agent_sessions::record_completed_turn_checkpoint(
                        &session_id,
                        before_turn_transcript,
                        fs_snapshot_ids,
                    );
                }
                send_json_response(
                    &send_output,
                    &id_owned,
                    serde_json::to_value(AcpSessionPromptResult {
                        stop_reason,
                        meta: terminal.map(AcpMeta::terminal),
                    })
                    .expect("ACP prompt result serializes"),
                );
            }
            Err(e) => {
                if cancellation.cancelled.load(Ordering::SeqCst) {
                    send_json_response(
                        &send_output,
                        &id_owned,
                        cancelled_prompt_result(sink_flush_error.as_ref()),
                    );
                    return;
                }
                let terminal_class = e.terminal_class;
                let facts = e.facts;
                let message = match sink_flush_error {
                    Some(error) => {
                        format!("{}; failed to persist agent events: {error}", e.message)
                    }
                    None => e.message,
                };
                self.send_prompt_failure(&id_owned, &message, terminal_class, facts);
            }
        }
    }
}

pub(super) fn cancelled_prompt_result(
    persistence_error: Option<&harn_vm::agent_events::AgentEventSinkError>,
) -> serde_json::Value {
    let mut result = serde_json::json!({"stopReason": "cancelled"});
    if let Some(error) = persistence_error {
        result["_meta"] = serde_json::json!({
            "harn": {"persistenceError": error.to_string()}
        });
    }
    result
}
