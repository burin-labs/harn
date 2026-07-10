use super::*;

impl AcpServer {
    pub(super) fn handle_initialize(&self, id: &serde_json::Value) {
        self.send_response(
            id,
            serde_json::json!({
                "protocolVersion": 1,
                "agentCapabilities": acp_agent_capabilities(),
                "agentInfo": {
                    "name": "harn",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "authMethods": self.auth_policy.acp_auth_methods(),
            }),
        );
    }

    pub(super) fn handle_provider_catalog(&self, id: &serde_json::Value) {
        self.send_response(
            id,
            serde_json::to_value(harn_vm::provider_catalog::artifact_with_overrides(
                self.llm_config_overrides.as_ref(),
                self.llm_capability_overrides.as_ref(),
            ))
            .expect("provider catalog serializes"),
        );
    }

    pub(super) async fn handle_session_timeline_query(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let query = match parse_session_timeline_query(params) {
            Ok(query) => query,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        let log = harn_vm::event_log::active_event_log();
        match harn_vm::session_timeline::query_session_timeline(log.as_deref(), None, query).await {
            Ok(snapshot) => self.send_response(
                id,
                serde_json::to_value(snapshot).expect("session timeline snapshot serializes"),
            ),
            Err(error) => self.send_error(id, -32000, &format!("session timeline query: {error}")),
        }
    }

    pub(super) async fn handle_session_view_query(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let query = match parse_session_timeline_query(params) {
            Ok(query) => query,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        let log = harn_vm::event_log::active_event_log();
        let view = if let Some(run_path) = query.run_path.as_deref() {
            let run = match harn_vm::orchestration::load_run_record(std::path::Path::new(run_path))
            {
                Ok(run) => run,
                Err(error) => {
                    self.send_error(id, -32000, &format!("session view run load: {error}"));
                    return;
                }
            };
            let run_view = match harn_vm::orchestration::build_run_view_with_event_log(
                &run,
                Some(run_path.to_string()),
                log.as_deref(),
            )
            .await
            {
                Ok(view) => view,
                Err(error) => {
                    self.send_error(id, -32000, &format!("session view query: {error}"));
                    return;
                }
            };
            let session_id = query
                .session_id
                .clone()
                .or_else(|| run_view.run.session_id.clone());
            harn_vm::orchestration::build_session_view_from_run_views(
                vec![run_view],
                harn_vm::orchestration::SessionViewOptions {
                    session_id,
                    has_event_log: log.is_some(),
                    ..harn_vm::orchestration::SessionViewOptions::default()
                },
            )
        } else {
            match harn_vm::orchestration::build_empty_session_view(
                query.session_id.clone(),
                log.as_deref(),
            )
            .await
            {
                Ok(view) => view,
                Err(error) => {
                    self.send_error(id, -32000, &format!("session view query: {error}"));
                    return;
                }
            }
        };
        self.send_response(
            id,
            serde_json::to_value(view).expect("session view serializes"),
        );
    }

    pub(super) async fn handle_session_timeline_subscribe(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let query = match parse_session_timeline_query(params) {
            Ok(query) => query,
            Err(message) => {
                self.send_error(id, -32602, &message);
                return;
            }
        };
        let Some(log) = harn_vm::event_log::active_event_log() else {
            self.send_error(
                id,
                -32000,
                "session timeline subscribe: no active event log",
            );
            return;
        };
        let subscription_id = params
            .get("subscriptionId")
            .or_else(|| params.get("subscription_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("timeline_sub_{}", uuid::Uuid::now_v7().simple()));

        let mut stream =
            match harn_vm::session_timeline::subscribe_session_timeline(log, query.clone()).await {
                Ok(stream) => stream,
                Err(error) => {
                    self.send_error(id, -32000, &format!("session timeline subscribe: {error}"));
                    return;
                }
            };
        if let Some(existing) = self.timeline_subscriptions.remove(&subscription_id) {
            existing.handle.abort();
        }

        let output = self.output.clone();
        let notification_subscription_id = subscription_id.clone();
        let handle = tokio::spawn(async move {
            while let Some(update) = stream.next().await {
                let params = match update {
                    Ok(update) => serde_json::json!({
                        "subscriptionId": notification_subscription_id,
                        "update": update,
                    }),
                    Err(error) => serde_json::json!({
                        "subscriptionId": notification_subscription_id,
                        "error": {
                            "message": error.to_string(),
                        },
                    }),
                };
                let notification = harn_vm::jsonrpc::notification(
                    harn_vm::session_timeline::SESSION_TIMELINE_UPDATE_METHOD,
                    params,
                );
                if let Ok(line) = serde_json::to_string(&notification) {
                    output.write_line(&line);
                }
            }
        });
        self.timeline_subscriptions.insert(
            subscription_id.clone(),
            TimelineSubscription {
                session_id: query.session_id.clone(),
                handle,
            },
        );
        self.send_response(
            id,
            serde_json::json!({
                "subscriptionId": subscription_id,
                "updateMethod": harn_vm::session_timeline::SESSION_TIMELINE_UPDATE_METHOD,
            }),
        );
    }

    pub(super) fn handle_session_timeline_unsubscribe(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(subscription_id) = params
            .get("subscriptionId")
            .or_else(|| params.get("subscription_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(
                id,
                -32602,
                "session timeline unsubscribe requires subscriptionId",
            );
            return;
        };
        let removed = self
            .timeline_subscriptions
            .remove(subscription_id)
            .map(|subscription| {
                subscription.handle.abort();
                true
            })
            .unwrap_or(false);
        self.send_response(
            id,
            serde_json::json!({
                "subscriptionId": subscription_id,
                "removed": removed,
            }),
        );
    }

    pub(super) fn auth_required_data(&self) -> serde_json::Value {
        serde_json::json!({
            "authMethods": self.auth_policy.acp_auth_methods(),
        })
    }

    pub(super) fn actor_chain(&self) -> Option<harn_vm::ActorChain> {
        self.authenticated_principal
            .as_ref()
            .map(|principal| principal.subject.trim())
            .filter(|subject| !subject.is_empty())
            .map(harn_vm::ActorChain::new)
            .or_else(|| {
                self.auth_policy
                    .methods
                    .is_empty()
                    .then(|| harn_vm::ActorChain::new(crate::auth::ANONYMOUS_SUBJECT))
            })
    }

    pub(super) fn send_auth_required(&self, id: &serde_json::Value) {
        self.send_error_with_data(
            id,
            ACP_AUTH_REQUIRED_CODE,
            "auth_required",
            self.auth_required_data(),
        );
    }

    pub(super) fn requires_authentication(&self) -> bool {
        !self.auth_policy.methods.is_empty() && self.authenticated_principal.is_none()
    }

    pub(super) fn reject_unauthenticated(&self, id: &serde_json::Value) -> bool {
        if !self.requires_authentication() {
            return false;
        }
        if !id.is_null() {
            self.send_auth_required(id);
        }
        true
    }

    pub(super) async fn handle_authenticate(
        &mut self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let method_id = match params.get("methodId").and_then(|value| value.as_str()) {
            Some(method_id) => method_id,
            None => {
                self.send_error(id, -32602, "authenticate requires methodId");
                return;
            }
        };
        // A policy with no configured methods advertises the synthetic
        // local "none" flow (see `AuthPolicy::acp_auth_methods`). Honour
        // an explicit authenticate against it as a no-op success so the
        // advertised method is real: the caller is already an anonymous
        // principal.
        if self.auth_policy.methods.is_empty() && method_id == crate::ACP_LOCAL_NONE_METHOD_ID {
            let principal = AuthenticatedPrincipal {
                subject: "anonymous".to_string(),
                scheme: "none".to_string(),
                granted_scopes: std::collections::BTreeSet::new(),
                tenant_id: None,
            };
            self.authenticated_principal = Some(principal.clone());
            self.send_response(
                id,
                serde_json::json!({
                    "_meta": {
                        "harn": {
                            "authenticated": true,
                            "principal": {
                                "subject": principal.subject,
                                "scheme": principal.scheme,
                            }
                        }
                    }
                }),
            );
            return;
        }
        let Some(method) = self.auth_policy.method_by_acp_id(method_id) else {
            self.send_error_with_data(
                id,
                -32602,
                "authenticate methodId was not advertised",
                self.auth_required_data(),
            );
            return;
        };
        let auth = match acp_auth_request_for_method(method, params) {
            Ok(auth) => auth,
            Err(message) => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &message,
                    self.auth_required_data(),
                );
                return;
            }
        };
        match self.auth_policy.authorize(&auth).await {
            AuthorizationDecision::Authorized(principal) => {
                self.authenticated_principal = Some(principal.clone());
                self.send_response(
                    id,
                    serde_json::json!({
                        "_meta": {
                            "harn": {
                                "authenticated": true,
                                "principal": {
                                    "subject": principal.subject,
                                    "scheme": principal.scheme,
                                }
                            }
                        }
                    }),
                );
            }
            AuthorizationDecision::Rejected(message) => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &message,
                    self.auth_required_data(),
                );
            }
            // ACP authentication doesn't bind a route, so this branch is
            // unreachable unless a future caller threads per-route scopes
            // through this code path. Forward as auth-required so the
            // client knows to retry with a richer credential.
            AuthorizationDecision::MissingScope { required, granted } => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &crate::forbidden_message(&required, &granted),
                    self.auth_required_data(),
                );
            }
            // `authorize_mcp` is the only producer of this variant and
            // belongs to the harn-vm `harness.mcp.*` dispatch path, not
            // ACP authentication. Surfacing it here would mean policy
            // wiring leaked; forward as auth-required with the
            // policy's reason so the operator can debug.
            AuthorizationDecision::McpNotAllowlisted { reason, .. } => {
                self.send_error_with_data(
                    id,
                    ACP_AUTH_REQUIRED_CODE,
                    &reason,
                    self.auth_required_data(),
                );
            }
        }
    }

    pub fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }

    pub(super) fn insert_session(&mut self, session_id: String, cwd: PathBuf, info: SessionInfo) {
        let cancellation = self.register_session_cancellation(&session_id);
        let project_root = session_project_root_for_cwd(&cwd);
        self.sessions.insert(
            session_id.clone(),
            Session {
                cwd,
                project_root,
                cancellation,
                host_bridge: None,
                inject_state: harn_vm::bridge::HostBridgeInjectionState::default(),
                info,
                advertised_commands: Vec::new(),
                current_mode_id: modes::DEFAULT_MODE_ID.to_string(),
                budget: SessionBudget::Inherit,
                profile_turn: 0,
            },
        );
        harn_vm::agent_sessions::open_or_create_with_actor_chain(
            Some(session_id.clone()),
            self.actor_chain(),
        );
        #[cfg(feature = "hostlib")]
        if let Some(session) = self.sessions.get(&session_id) {
            harn_hostlib::fs::configure_session_root(&session_id, &session.project_root);
        }
    }
}
