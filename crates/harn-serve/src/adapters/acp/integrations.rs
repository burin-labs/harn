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

    /// `mcp/authorize_batch` (harn#3357): begin OAuth for many MCP servers at
    /// once over the [`harn_vm::mcp_bulk_auth`] driver. The request carries the
    /// explicit `servers` to consider plus a batch selection `mode`
    /// (`missing` = first-auth servers without a valid bearer, the default;
    /// `expired` = re-auth stale stored tokens; `all` = force a fresh flow for
    /// every server). harn begins all selected flows concurrently and returns
    /// `{ flows, skipped, failed }`; the client opens each `flows[].authorizeUrl`
    /// (serialized, to avoid a popup storm) and posts each captured callback
    /// back via `mcp/oauth_callback` — whose `state` routes to this batch's
    /// driver automatically. Per-server progress streams as `mcp/authorize_status`
    /// notifications so a GUI updates each row live. Pure adapter: all flow
    /// logic lives in the driver; this serializes its outcomes and events.
    pub(super) async fn handle_mcp_authorize_batch(
        &self,
        id: &serde_json::Value,
        params: &serde_json::Value,
    ) {
        let servers = match parse_bulk_auth_servers(params) {
            Ok(servers) => servers,
            Err(error) => {
                self.send_error(id, -32602, &error);
                return;
            }
        };
        let mode = match parse_bulk_auth_mode(params) {
            Ok(mode) => mode,
            Err(error) => {
                self.send_error(id, -32602, &error);
                return;
            }
        };
        let redirect_uri = params
            .get("redirectUri")
            .and_then(|value| value.as_str())
            .unwrap_or(MCP_DEFAULT_OAUTH_REDIRECT_URI)
            .to_string();

        // Fresh driver per batch. Subscribe *before* prepare so no phase event
        // is missed, and forward each as an `mcp/authorize_status` notification
        // on a background task (the transport sink is Clone + Send + 'static).
        let driver = Arc::new(harn_vm::mcp_bulk_auth::McpBulkAuth::new());
        let mut status_rx = driver.subscribe();
        let output = self.output.clone();
        tokio::spawn(async move {
            loop {
                match status_rx.recv().await {
                    Ok(status) => {
                        let notification = harn_vm::jsonrpc::notification(
                            MCP_AUTHORIZE_STATUS_METHOD,
                            authorize_status_params(&status),
                        );
                        if let Ok(line) = serde_json::to_string(&notification) {
                            output.write_line(&line);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Store as the active batch so the matching `mcp/oauth_callback`s route
        // through it. Replacing any prior driver drops its sender, which ends
        // that batch's forwarder task on the next `recv`.
        *self
            .active_bulk_auth
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(driver.clone());

        let outcomes = driver.prepare(servers, mode, &redirect_uri).await;
        self.send_response(id, authorize_batch_response(&outcomes));
    }

    /// `mcp/oauth_callback`: complete an authorization begun by `mcp/authorize`.
    /// Accepts either explicit `state`+`code` (+optional `issuer`) or a full
    /// `redirectUrl` (the captured `burin://…?code=…&state=…&iss=…`) to parse
    /// them from. harn exchanges the code and stores the token.
    ///
    /// When `state` belongs to an active `mcp/authorize_batch` (harn#3357), the
    /// completion is routed through that driver so it streams `Exchanging`/
    /// `Connected` status notifications; otherwise the single-URL path is taken
    /// unchanged. The response is identical either way.
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
        // Route through the active batch driver when this state is one of its
        // flows (so completion streams status); else complete directly.
        let batch_driver = {
            let guard = self
                .active_bulk_auth
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            match guard.as_ref() {
                Some(driver) if driver.knows_state(&state) => Some(driver.clone()),
                _ => None,
            }
        };
        let result = match &batch_driver {
            Some(driver) => driver.complete(&state, &code, issuer.as_deref()).await,
            None => {
                harn_vm::mcp_oauth::complete_authorization(&state, &code, issuer.as_deref()).await
            }
        };
        match result {
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

/// JSON-RPC notification method that streams per-server bulk-auth progress
/// (harn#3357). One frame per [`harn_vm::mcp_bulk_auth::McpAuthStatus`].
const MCP_AUTHORIZE_STATUS_METHOD: &str = "mcp/authorize_status";

/// Parse the `servers` array of an `mcp/authorize_batch` request into driver
/// inputs. Each item needs a `url` (or `serverUrl`); `name` defaults to the URL.
/// An empty/missing array is an error — there is nothing to authorize. The
/// optional per-server `mode` is the OAuth *client* auth mode (cimd/dcr/static/
/// byo), distinct from the batch-level selection mode parsed separately.
fn parse_bulk_auth_servers(
    params: &serde_json::Value,
) -> Result<Vec<harn_vm::mcp_bulk_auth::BulkAuthServer>, String> {
    let Some(items) = params.get("servers").and_then(|value| value.as_array()) else {
        return Err("mcp/authorize_batch requires servers (an array of { name, url })".to_string());
    };
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let url = item
            .get("url")
            .or_else(|| item.get("serverUrl"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("mcp/authorize_batch servers[{index}] requires url"))?;
        let name = item
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or(url)
            .to_string();
        let string_field = |key: &str| {
            item.get(key)
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        let client_mode = match item.get("mode").and_then(|value| value.as_str()) {
            Some(raw) => Some(serde_json::from_value(serde_json::json!(raw)).map_err(|_| {
                format!(
                    "mcp/authorize_batch servers[{index}]: invalid mode (expected cimd|dcr|static|byo)"
                )
            })?),
            None => None,
        };
        out.push(harn_vm::mcp_bulk_auth::BulkAuthServer {
            name,
            server_url: url.to_string(),
            mode: client_mode,
            client_id: string_field("clientId"),
            client_secret: string_field("clientSecret"),
            static_secret_id: string_field("staticSecretId"),
            scopes: string_field("scope"),
        });
    }
    Ok(out)
}

/// Parse the batch selection `mode` (which servers to authenticate). Defaults to
/// `missing` (first-auth) when omitted.
fn parse_bulk_auth_mode(
    params: &serde_json::Value,
) -> Result<harn_vm::mcp_bulk_auth::BulkAuthMode, String> {
    use harn_vm::mcp_bulk_auth::BulkAuthMode;
    match params.get("mode").and_then(|value| value.as_str()) {
        None | Some("missing") => Ok(BulkAuthMode::Missing),
        Some("expired") => Ok(BulkAuthMode::Expired),
        Some("all") => Ok(BulkAuthMode::All),
        Some(other) => Err(format!(
            "mcp/authorize_batch: invalid mode '{other}' (expected missing|expired|all)"
        )),
    }
}

/// Group the driver's prepare outcomes into the `{ flows, skipped, failed }`
/// response. `flows` carries the camelCase [`harn_vm::mcp_bulk_auth::PreparedFlow`]
/// (authorizeUrl/state/…) the client opens; each `state` is distinct.
fn authorize_batch_response(
    outcomes: &[harn_vm::mcp_bulk_auth::PrepareOutcome],
) -> serde_json::Value {
    use harn_vm::mcp_bulk_auth::PrepareOutcome;
    let mut flows = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();
    for outcome in outcomes {
        match outcome {
            PrepareOutcome::Pending(flow) => {
                flows.push(serde_json::to_value(flow).unwrap_or(serde_json::Value::Null));
            }
            PrepareOutcome::Skipped {
                name,
                server_url,
                reason,
            } => skipped.push(serde_json::json!({
                "name": name,
                "serverUrl": server_url,
                "reason": reason,
            })),
            PrepareOutcome::Failed {
                name,
                server_url,
                error,
            } => failed.push(serde_json::json!({
                "name": name,
                "serverUrl": server_url,
                "error": error,
            })),
        }
    }
    serde_json::json!({ "flows": flows, "skipped": skipped, "failed": failed })
}

/// Project a driver status event into the camelCase params of an
/// `mcp/authorize_status` notification (kept camelCase for ACP wire consistency
/// even though the shared type serializes snake_case for the CLI `--json`).
fn authorize_status_params(status: &harn_vm::mcp_bulk_auth::McpAuthStatus) -> serde_json::Value {
    let mut params = serde_json::json!({
        "server": status.server,
        "serverUrl": status.server_url,
        "phase": status.phase,
    });
    if let Some(detail) = &status.detail {
        params["detail"] = serde_json::json!(detail);
    }
    params
}

#[cfg(test)]
mod authorize_batch_tests {
    use super::*;
    use harn_vm::mcp_bulk_auth::{
        BulkAuthMode, McpAuthPhase, McpAuthStatus, PrepareOutcome, PreparedFlow,
    };
    use tokio::sync::mpsc;

    #[test]
    fn parse_servers_reads_name_url_and_client_fields() {
        let params = serde_json::json!({
            "servers": [
                { "name": "Notion", "url": "https://mcp.notion.com/mcp" },
                { "url": "https://mcp.linear.app/mcp", "scope": "read", "clientId": "abc" },
            ]
        });
        let servers = parse_bulk_auth_servers(&params).expect("valid");
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].name, "Notion");
        assert_eq!(servers[0].server_url, "https://mcp.notion.com/mcp");
        // name defaults to the URL when omitted.
        assert_eq!(servers[1].name, "https://mcp.linear.app/mcp");
        assert_eq!(servers[1].scopes.as_deref(), Some("read"));
        assert_eq!(servers[1].client_id.as_deref(), Some("abc"));
    }

    #[test]
    fn parse_servers_requires_array_and_url() {
        assert!(parse_bulk_auth_servers(&serde_json::json!({})).is_err());
        let no_url = serde_json::json!({ "servers": [ { "name": "x" } ] });
        assert!(parse_bulk_auth_servers(&no_url).is_err());
    }

    #[test]
    fn parse_servers_rejects_bad_client_mode() {
        let params =
            serde_json::json!({ "servers": [ { "url": "https://x/mcp", "mode": "bogus" } ] });
        assert!(parse_bulk_auth_servers(&params).is_err());
    }

    #[test]
    fn parse_servers_empty_array_is_ok() {
        let servers = parse_bulk_auth_servers(&serde_json::json!({ "servers": [] })).unwrap();
        assert!(servers.is_empty());
    }

    #[test]
    fn parse_mode_defaults_to_missing() {
        assert_eq!(
            parse_bulk_auth_mode(&serde_json::json!({})).unwrap(),
            BulkAuthMode::Missing
        );
        assert_eq!(
            parse_bulk_auth_mode(&serde_json::json!({ "mode": "expired" })).unwrap(),
            BulkAuthMode::Expired
        );
        assert_eq!(
            parse_bulk_auth_mode(&serde_json::json!({ "mode": "all" })).unwrap(),
            BulkAuthMode::All
        );
        assert!(parse_bulk_auth_mode(&serde_json::json!({ "mode": "nope" })).is_err());
    }

    #[test]
    fn response_groups_outcomes_with_distinct_flow_states() {
        let outcomes = vec![
            PrepareOutcome::Pending(PreparedFlow {
                name: "a".to_string(),
                server_url: "https://a/mcp".to_string(),
                authorize_url: "https://auth/a?state=s1".to_string(),
                state: "s1".to_string(),
                redirect_uri: "http://127.0.0.1/cb".to_string(),
            }),
            PrepareOutcome::Pending(PreparedFlow {
                name: "b".to_string(),
                server_url: "https://b/mcp".to_string(),
                authorize_url: "https://auth/b?state=s2".to_string(),
                state: "s2".to_string(),
                redirect_uri: "http://127.0.0.1/cb".to_string(),
            }),
            PrepareOutcome::Skipped {
                name: "c".to_string(),
                server_url: "https://c/mcp".to_string(),
                reason: "already connected".to_string(),
            },
            PrepareOutcome::Failed {
                name: "d".to_string(),
                server_url: "https://d/mcp".to_string(),
                error: "discovery failed".to_string(),
            },
        ];
        let response = authorize_batch_response(&outcomes);
        let flows = response["flows"].as_array().unwrap();
        assert_eq!(flows.len(), 2);
        assert_eq!(flows[0]["authorizeUrl"], "https://auth/a?state=s1");
        let s1 = flows[0]["state"].as_str().unwrap();
        let s2 = flows[1]["state"].as_str().unwrap();
        assert_ne!(s1, s2, "each prepared flow carries a distinct state");
        assert_eq!(response["skipped"].as_array().unwrap().len(), 1);
        assert_eq!(response["skipped"][0]["reason"], "already connected");
        assert_eq!(response["failed"].as_array().unwrap().len(), 1);
        assert_eq!(response["failed"][0]["error"], "discovery failed");
    }

    #[test]
    fn status_params_are_camelcase() {
        let params = authorize_status_params(&McpAuthStatus {
            server: "Notion".to_string(),
            server_url: "https://mcp.notion.com/mcp".to_string(),
            phase: McpAuthPhase::AwaitingConsent,
            detail: None,
        });
        assert_eq!(params["server"], "Notion");
        assert_eq!(params["serverUrl"], "https://mcp.notion.com/mcp");
        assert_eq!(params["phase"], "awaiting_consent");
        assert!(params.get("detail").is_none());

        let with_detail = authorize_status_params(&McpAuthStatus {
            server: "Linear".to_string(),
            server_url: "https://mcp.linear.app/mcp".to_string(),
            phase: McpAuthPhase::Failed,
            detail: Some("discovery failed".to_string()),
        });
        assert_eq!(with_detail["phase"], "failed");
        assert_eq!(with_detail["detail"], "discovery failed");
    }

    async fn recv_value(rx: &mut mpsc::UnboundedReceiver<String>) -> serde_json::Value {
        let line = rx.recv().await.expect("a frame");
        serde_json::from_str(&line).expect("valid json frame")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authorize_batch_empty_servers_returns_empty_groups() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
        server
            .handle_incoming_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "mcp/authorize_batch",
                "params": { "servers": [] }
            }))
            .await;
        let frame = recv_value(&mut rx).await;
        assert_eq!(frame["id"], 1);
        assert_eq!(frame["result"]["flows"].as_array().unwrap().len(), 0);
        assert_eq!(frame["result"]["skipped"].as_array().unwrap().len(), 0);
        assert_eq!(frame["result"]["failed"].as_array().unwrap().len(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authorize_batch_missing_servers_is_invalid_params() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
        server
            .handle_incoming_message(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "mcp/authorize_batch",
                "params": {}
            }))
            .await;
        let frame = recv_value(&mut rx).await;
        assert_eq!(frame["id"], 7);
        assert_eq!(frame["error"]["code"], -32602);
    }
}
