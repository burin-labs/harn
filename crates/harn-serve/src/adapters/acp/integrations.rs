use super::*;

impl AcpServer {
    pub(super) fn handle_agent_resume(&self, params: &serde_json::Value) {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| self.sessions.keys().next().map(|s| s.as_str()));
        let Some(session_id) = session_id else {
            return;
        };
        if let Some(bridge) = self
            .sessions
            .get(session_id)
            .and_then(|session| session.host_bridge.clone())
        {
            bridge.signal_resume();
        }
    }

    pub(super) fn handle_session_list(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let sessions: Vec<serde_json::Value> = self
            .sessions
            .iter()
            .filter(|(sid, session)| self.session_matches_list_filters(sid, session, params))
            .filter_map(|(sid, _)| self.session_item_json(sid, "live", None))
            .collect();
        self.send_response(id, serde_json::json!({"sessions": sessions}));
    }

    /// `mcp/catalog`: project the persisted enable/disable allowlist (plus
    /// optional per-project overlay) onto the advertised MCP items and
    /// return the effective catalog (servers → items + `enabled`). The
    /// merge/projection is harn-owned so thin clients (the burin-code TUI /
    /// GUI) render the toggle UI without storing any toggle state. See
    /// `harn_vm::mcp_allowlist`.
    pub(super) fn handle_mcp_catalog(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let request: harn_vm::McpCatalogRequest = match serde_json::from_value(params.clone()) {
            Ok(request) => request,
            Err(error) => {
                self.send_error(id, -32602, &format!("Invalid mcp/catalog params: {error}"));
                return;
            }
        };
        let catalog = harn_vm::mcp_catalog_for_request(&request);
        match serde_json::to_value(&catalog) {
            Ok(value) => self.send_response(id, value),
            Err(error) => self.send_error(
                id,
                -32000,
                &format!("failed to encode mcp catalog: {error}"),
            ),
        }
    }

    /// `mcp/authorize`: begin an interactive OAuth authorization for an MCP
    /// server. harn does discovery + client resolution + PKCE, registers the
    /// pending flow, and returns the browser URL plus the `state` the matching
    /// `mcp/oauth_callback` must echo. The client opens `authorizeUrl`; the
    /// redirect's `code`+`state` come back via `mcp/oauth_callback`. Token
    /// exchange and storage stay in harn.
    pub(super) async fn handle_mcp_authorize(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(url) = params
            .get("url")
            .or_else(|| params.get("resource"))
            .and_then(|value| value.as_str())
        else {
            self.send_error(
                id,
                -32602,
                "mcp/authorize requires url (the MCP server URL)",
            );
            return;
        };
        let redirect_uri = params
            .get("redirectUri")
            .and_then(|value| value.as_str())
            .unwrap_or(MCP_DEFAULT_OAUTH_REDIRECT_URI)
            .to_string();
        let string_param = |key: &str| {
            params
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        // An explicit auth mode (cimd/dcr/static/byo) is optional; harn
        // auto-selects (CIMD-default) when the client omits it.
        let mode = match params.get("mode").and_then(|value| value.as_str()) {
            Some(raw) => match serde_json::from_value(serde_json::json!(raw)) {
                Ok(mode) => Some(mode),
                Err(_) => {
                    self.send_error(
                        id,
                        -32602,
                        "mcp/authorize: invalid mode (expected cimd|dcr|static|byo)",
                    );
                    return;
                }
            },
            None => None,
        };
        let request = harn_vm::mcp_oauth::BeginAuthorization {
            server_url: url.to_string(),
            redirect_uri,
            mode,
            client_id: string_param("clientId"),
            client_secret: string_param("clientSecret"),
            static_secret_id: string_param("staticSecretId"),
            scopes: string_param("scope"),
        };
        match harn_vm::mcp_oauth::begin_authorization(request).await {
            Ok(pending) => self.send_response(
                id,
                serde_json::json!({
                    "authorizeUrl": pending.authorize_url,
                    "state": pending.state,
                    "redirectUri": pending.redirect_uri,
                    "resource": pending.resource,
                    "issuer": pending.issuer,
                }),
            ),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    /// `mcp/oauth_callback`: complete an authorization begun by `mcp/authorize`.
    /// Accepts either explicit `state`+`code` (+optional `issuer`) or a full
    /// `redirectUrl` (the captured `burin://…?code=…&state=…&iss=…`) to parse
    /// them from. harn exchanges the code and stores the token.
    pub(super) async fn handle_mcp_oauth_callback(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let parsed = params
            .get("redirectUrl")
            .and_then(|value| value.as_str())
            .map(parse_oauth_redirect_url);
        let (state, code, issuer) = match parsed {
            Some(Ok(parts)) => parts,
            Some(Err(error)) => {
                self.send_error(id, -32602, &error);
                return;
            }
            None => {
                let field = |key: &str| {
                    params
                        .get(key)
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                };
                let (Some(state), Some(code)) = (field("state"), field("code")) else {
                    self.send_error(
                        id,
                        -32602,
                        "mcp/oauth_callback requires state and code (or redirectUrl)",
                    );
                    return;
                };
                (state, code, field("issuer"))
            }
        };
        match harn_vm::mcp_oauth::complete_authorization(&state, &code, issuer.as_deref()).await {
            Ok(token) => {
                // Surface the "logged in as …" identity (harn#3350) so an
                // embedding GUI can show it immediately on a successful connect.
                let display_identity =
                    harn_vm::mcp_identity::display_identity(&token.resource, &token);
                self.send_response(
                    id,
                    serde_json::json!({
                        "ok": true,
                        "resource": token.resource,
                        "issuer": token.issuer,
                        "expiresAt": token.expires_at_unix,
                        "displayIdentity": display_identity,
                    }),
                );
            }
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    /// `mcp/import_token`: migrate a token minted by an older client-specific
    /// OAuth implementation into harn's canonical MCP OAuth store. Discovery,
    /// issuer binding, resource canonicalization, and keyring layout remain
    /// harn-owned; clients only hand over the legacy token material once.
    pub(super) async fn handle_mcp_import_token(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let request: McpImportTokenParams = match serde_json::from_value(params.clone()) {
            Ok(request) => request,
            Err(error) => {
                self.send_error(
                    id,
                    -32602,
                    &format!("Invalid mcp/import_token params: {error}"),
                );
                return;
            }
        };
        let import = harn_vm::mcp_oauth::ImportStoredToken {
            server_url: request.url,
            access_token: request.access_token,
            refresh_token: request.refresh_token,
            expires_at_unix: request.expires_at,
            token_endpoint: request.token_endpoint,
            client_id: request.client_id,
            client_secret: request.client_secret,
            token_endpoint_auth_method: request.token_endpoint_auth_method,
            scopes: request.scope,
        };
        match harn_vm::mcp_oauth::import_stored_token(import).await {
            Ok(token) => self.send_response(
                id,
                serde_json::json!({
                    "ok": true,
                    "resource": token.resource,
                    "issuer": token.issuer,
                    "expiresAt": token.expires_at_unix,
                }),
            ),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    pub(super) async fn handle_hitl_respond(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let session_cwd = params
            .get("sessionId")
            .and_then(|value| value.as_str())
            .and_then(|session_id| self.sessions.get(session_id))
            .map(|session| session.cwd.as_path());
        let fallback_cwd = self
            .sessions
            .values()
            .next()
            .map(|session| session.cwd.as_path());
        let cwd = session_cwd.or(fallback_cwd);
        let response: harn_vm::HitlHostResponse = match serde_json::from_value(params.clone()) {
            Ok(response) => response,
            Err(error) => {
                self.send_error(
                    id,
                    -32602,
                    &format!("Invalid harn.hitl.respond params: {error}"),
                );
                return;
            }
        };
        match harn_vm::append_hitl_response(cwd, response).await {
            Ok(_) => self.send_response(id, serde_json::json!({"ok": true})),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    pub(super) fn workflow_base_dir_for<'a>(
        &'a self,
        params: &'a serde_json::Value,
    ) -> Option<&'a PathBuf> {
        params
            .get("sessionId")
            .and_then(|value| value.as_str())
            .and_then(|session_id| self.sessions.get(session_id))
            .map(|session| &session.cwd)
            .or_else(|| self.sessions.values().next().map(|session| &session.cwd))
    }

    pub(super) async fn handle_workflow_signal(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/signal: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/signal: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/signal: no session cwd available");
            return;
        };
        let payload = params
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match harn_vm::workflow_signal_for_base(base_dir, workflow_id, name, payload) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    pub(super) fn handle_workflow_query(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/query: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/query: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/query: no session cwd available");
            return;
        };
        match harn_vm::workflow_query_for_base(base_dir, workflow_id, name) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    pub(super) async fn handle_workflow_update(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/update: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/update: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/update: no session cwd available");
            return;
        };
        let payload = params
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let timeout_ms = params
            .get("timeoutMs")
            .and_then(|value| value.as_u64())
            .unwrap_or(30_000);
        match harn_vm::workflow_update_for_base(
            base_dir,
            workflow_id,
            name,
            payload,
            std::time::Duration::from_millis(timeout_ms),
        )
        .await
        {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    pub(super) fn handle_workflow_pause(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/pause: missing workflowId");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/pause: no session cwd available");
            return;
        };
        match harn_vm::workflow_pause_for_base(base_dir, workflow_id) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    pub(super) fn handle_workflow_resume(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/resume: missing workflowId");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/resume: no session cwd available");
            return;
        };
        match harn_vm::workflow_resume_for_base(base_dir, workflow_id) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }
}
