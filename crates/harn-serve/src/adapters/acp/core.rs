use super::*;

impl AcpServer {
    pub fn new(config: AcpServerConfig) -> Self {
        Self::new_with_output(config, AcpOutput::stdout())
    }

    /// Create an ACP server that writes responses and notifications to a
    /// caller-provided output sink.
    ///
    /// Prefer [`crate::EmbeddedAgent`] or [`run_acp_channel_server_with_handle`]
    /// unless the host already owns the compatible current-thread runtime and
    /// wants to drive incoming JSON-RPC messages directly.
    pub fn new_with_output(config: AcpServerConfig, output: AcpOutput) -> Self {
        harn_vm::llm_config::set_user_overrides(config.llm_config_overrides.clone());
        harn_vm::llm::capabilities::set_user_overrides(config.llm_capability_overrides.clone());
        let llm_config_overrides = config.llm_config_overrides.clone();
        let llm_capability_overrides = config.llm_capability_overrides.clone();

        Self {
            descriptor: AdapterDescriptor {
                id: "acp".to_string(),
                caller_shape: "agent-session".to_string(),
                supports_streaming: true,
                supports_cancel: true,
            },
            pipeline: config.pipeline,
            auth_policy: config.auth_policy,
            authenticated_principal: config.authenticated_principal,
            runtime_configurator: config.runtime_configurator,
            sessions: HashMap::new(),
            inject_controls: HashMap::new(),
            timeline_subscriptions: HashMap::new(),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            session_cancellations: Arc::new(std::sync::Mutex::new(HashMap::new())),
            output,
            compile_cache: None,
            vm_baseline_cache: None,
            profile: config.profile,
            llm_config_overrides,
            llm_capability_overrides,
            default_budget: config.budget,
            sandbox: config.sandbox,
            active_bulk_auth: std::sync::Mutex::new(None),
        }
    }

    /// Compile `source` for `target_pipeline` (or the default entry point
    /// when `target_pipeline` is None), reusing the cached chunk when the
    /// file at `source_path` has the same mtime as the last cache fill and
    /// the target hasn't changed.
    ///
    /// Returns `(chunk, hit)` so the caller can keep its existing compile-
    /// time telemetry meaningful (hits report ~0 ms).
    ///
    /// Inline-mode prompts pass `source_path: None` and never hit cache —
    /// the source is freshly generated per turn so there's nothing to reuse.
    pub(super) fn compile_pipeline_cached(
        &mut self,
        source: &str,
        source_path: Option<&Path>,
        target_pipeline: Option<&str>,
    ) -> Result<(harn_vm::Chunk, bool), String> {
        let target_owned = target_pipeline.map(|s| s.to_string());
        let cache_key = source_path.and_then(|path| {
            std::fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .map(|mtime| (path.to_path_buf(), mtime))
        });
        if let Some((ref path, mtime)) = cache_key {
            if let Some(entry) = self.compile_cache.as_ref() {
                if entry.path == *path
                    && entry.mtime == mtime
                    && entry.target_pipeline == target_owned
                    && entry.source == source
                {
                    return Ok((entry.chunk.clone(), true));
                }
            }
        }
        let chunk = match target_pipeline {
            Some(name) => harn_vm::compile_source_named(source, name),
            None => harn_vm::compile_source(source),
        }
        .map_err(|e| format!("Compilation error: {e}"))?;
        if let Some((path, mtime)) = cache_key {
            self.compile_cache = Some(CompileCacheEntry {
                path,
                mtime,
                target_pipeline: target_owned,
                source: source.to_string(),
                chunk: chunk.clone(),
            });
        }
        Ok((chunk, false))
    }

    pub(super) async fn prepare_vm_baseline_cached(
        &mut self,
        source: &str,
        source_path: Option<&Path>,
        target_pipeline: Option<&str>,
        cwd: &Path,
        project_root: &Path,
        mode_id: &str,
    ) -> Result<(Option<harn_vm::VmBaseline>, Option<bool>, u64), String> {
        let Some(source_path) = source_path else {
            return Ok((None, None, 0));
        };

        let prepare_started = Instant::now();
        let target_owned = target_pipeline.map(str::to_string);
        let cache_key = std::fs::metadata(source_path)
            .and_then(|m| m.modified())
            .ok()
            .map(|mtime| (source_path.to_path_buf(), mtime));
        let project_root = Some(project_root.to_path_buf());

        if let Some((ref path, mtime)) = cache_key {
            if let Some(entry) = self.vm_baseline_cache.as_ref() {
                if entry.path == *path
                    && entry.mtime == mtime
                    && entry.target_pipeline == target_owned
                    && entry.source == source
                    && entry.cwd == cwd
                    && entry.project_root == project_root
                    && entry.mode_id == mode_id
                {
                    return Ok((
                        Some(entry.baseline.clone()),
                        Some(true),
                        prepare_started.elapsed().as_millis() as u64,
                    ));
                }
            }
        }

        let baseline = execute::prepare_vm_baseline(
            source,
            source_path,
            cwd,
            project_root.as_deref(),
            self.runtime_configurator.clone(),
        )
        .await?;
        if let Some((path, mtime)) = cache_key {
            self.vm_baseline_cache = Some(VmBaselineCacheEntry {
                path,
                mtime,
                target_pipeline: target_owned,
                source: source.to_string(),
                cwd: cwd.to_path_buf(),
                project_root,
                mode_id: mode_id.to_string(),
                baseline: baseline.clone(),
            });
        } else {
            self.vm_baseline_cache = None;
        }

        Ok((
            Some(baseline),
            Some(false),
            prepare_started.elapsed().as_millis() as u64,
        ))
    }

    /// Write a complete JSON-RPC message to the current transport.
    pub(super) fn write_line(&self, line: &str) {
        self.output.write_line(line);
    }

    /// Send a JSON-RPC success response.
    pub(super) fn send_response(&self, id: &serde_json::Value, result: serde_json::Value) {
        let response = harn_vm::jsonrpc::response(id.clone(), result);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    /// Send a JSON-RPC error response.
    pub(super) fn send_error(&self, id: &serde_json::Value, code: i64, message: &str) {
        let response = harn_vm::jsonrpc::error_response(id.clone(), code, message);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    pub(super) fn send_error_with_data(
        &self,
        id: &serde_json::Value,
        code: i64,
        message: &str,
        data: serde_json::Value,
    ) {
        let response = harn_vm::jsonrpc::error_response_with_data(id.clone(), code, message, data);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    pub(super) fn emit_control_outcome(
        &self,
        session_id: &str,
        method: &str,
        outcome: &str,
        status: &str,
        actor: serde_json::Value,
        target: serde_json::Value,
        reason: Option<&str>,
    ) {
        harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::ControlOutcome {
            session_id: session_id.to_string(),
            control_id: control_id(),
            method: method.to_string(),
            outcome: outcome.to_string(),
            status: status.to_string(),
            actor,
            target,
            reason: reason.map(str::to_string),
            metadata: serde_json::Value::Null,
        });
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    pub(super) fn send_notification(&self, method: &str, params: serde_json::Value) {
        let notification = harn_vm::jsonrpc::notification(method, params);
        if let Ok(line) = serde_json::to_string(&notification) {
            self.write_line(&line);
        }
    }

    /// Send a `session/update` notification with an agent message chunk.
    pub(super) fn send_update(&self, session_id: &str, text: &str) {
        let visible_text = sanitize_visible_assistant_text(text, true);
        let mut content = serde_json::json!({
            "type": "text",
            "text": text,
        });
        let mut content_meta = serde_json::Map::new();
        content_meta.insert(
            "visible_text".to_string(),
            serde_json::Value::String(visible_text.clone()),
        );
        content_meta.insert(
            "visible_delta".to_string(),
            serde_json::Value::String(visible_text),
        );
        events::merge_harn_meta(&mut content, content_meta);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": content,
                },
            }),
        );
    }

    pub(super) fn send_prompt_error(
        &self,
        session_id: &str,
        id: &serde_json::Value,
        message: &str,
    ) {
        self.send_update(session_id, &format!("Error: {message}\n"));
        self.send_error(id, -32000, message);
        eprintln!("{message}");
    }

    /// Generate a unique session ID.
    pub(super) fn next_session_id(&mut self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    pub(super) fn register_session_cancellation(
        &mut self,
        session_id: &str,
    ) -> SessionCancellation {
        let cancellation = SessionCancellation::default();
        self.session_cancellations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), cancellation.clone());
        cancellation
    }
}
