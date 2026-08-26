use super::*;
use std::path::{Path, PathBuf};

const PROJECT_ROOT_NOT_AUTHORIZED_CODE: i64 = -32001;
const PROJECT_ROOT_NOT_AUTHORIZED_REASON: &str = "project_root_not_authorized";

#[derive(Clone, Debug)]
pub(in crate::commands::orchestrator::listener) struct ProjectAuthority {
    allowed_root: Option<PathBuf>,
}

impl ProjectAuthority {
    pub(in crate::commands::orchestrator::listener) fn new(
        allowed_root: Option<&Path>,
    ) -> Result<Self, OrchestratorError> {
        let allowed_root = allowed_root
            .map(std::fs::canonicalize)
            .transpose()
            .map_err(|error| format!("failed to resolve ACP project root: {error}"))?;
        Ok(Self { allowed_root })
    }

    fn resolve(&self, cwd: Option<&str>) -> Result<PathBuf, DiscoveryError> {
        let root = harn_serve::resolve_acp_session_project_root(cwd)
            .map_err(DiscoveryError::ProjectRoot)?;
        if self.allowed_root.as_ref() == Some(&root) {
            Ok(root)
        } else {
            Err(DiscoveryError::ProjectRootNotAuthorized)
        }
    }

    pub(super) fn resolve_params(&self, params: &JsonValue) -> Result<PathBuf, DiscoveryError> {
        self.resolve(project_path_param(params))
    }
}

pub(super) type DiscoveryResult = Result<Vec<JsonValue>, DiscoveryError>;

#[derive(Debug)]
pub(super) enum DiscoveryError {
    ProjectRoot(harn_serve::AcpSessionProjectRootError),
    ProjectRootNotAuthorized,
    Store(String),
}

impl DiscoveryError {
    pub(super) fn json_rpc_code(&self) -> i64 {
        match self {
            Self::ProjectRoot(_) => -32602,
            Self::ProjectRootNotAuthorized => PROJECT_ROOT_NOT_AUTHORIZED_CODE,
            Self::Store(_) => -32000,
        }
    }

    fn data(&self) -> Option<JsonValue> {
        matches!(self, Self::ProjectRootNotAuthorized)
            .then(|| json!({"reason": PROJECT_ROOT_NOT_AUTHORIZED_REASON}))
    }
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProjectRoot(error) => error.fmt(formatter),
            Self::ProjectRootNotAuthorized => {
                formatter.write_str("project root is not authorized for this listener")
            }
            Self::Store(error) => formatter.write_str(error),
        }
    }
}

pub(super) async fn answer_list_request(
    hub: &AcpWebSocketHub,
    authority: &ProjectAuthority,
    socket_tx: &mpsc::UnboundedSender<String>,
    request: &JsonValue,
) -> bool {
    if !is_session_list_request(request) {
        return false;
    }
    let request_id = request.get("id").unwrap_or(&JsonValue::Null);
    match discover_sessions(
        hub,
        authority,
        request.get("params").unwrap_or(&JsonValue::Null),
    )
    .await
    {
        Ok(sessions) => {
            send_socket_jsonrpc_result(socket_tx, request_id, json!({"sessions": sessions}));
        }
        Err(error) => send_discovery_error(socket_tx, request_id, "session/list", &error),
    }
    true
}

async fn discover_sessions(
    hub: &AcpWebSocketHub,
    authority: &ProjectAuthority,
    params: &JsonValue,
) -> DiscoveryResult {
    let project_root = authority.resolve_params(params)?;
    let mut summaries: BTreeMap<String, JsonValue> = BTreeMap::new();
    let workers: Vec<Arc<AcpWorker>> = hub
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .workers_by_session
        .values()
        .cloned()
        .collect();
    for worker in workers {
        let live_state = worker.live_state();
        let attachable_roles = worker.attachable_roles();
        for summary in worker.session_summaries() {
            if acp_session_summary_matches(&summary, live_state, params) {
                summaries.insert(
                    summary.session_id.clone(),
                    summary.to_json(live_state, &attachable_roles),
                );
            }
        }
    }

    for summary in persisted_acp_session_summaries(&hub.event_log).await {
        if summaries.contains_key(&summary.session_id) {
            continue;
        }
        if acp_session_summary_matches(&summary, "expired_replay_only", params) {
            summaries.insert(
                summary.session_id.clone(),
                summary.to_json("expired_replay_only", &[]),
            );
        }
    }

    for item in list(&project_root, params).await? {
        if let Some(session_id) = item.get("sessionId").and_then(JsonValue::as_str) {
            summaries.entry(session_id.to_string()).or_insert(item);
        }
    }
    Ok(summaries.into_values().collect())
}

pub(super) fn reject_unavailable_restore_root(
    authority: &ProjectAuthority,
    socket_tx: &mpsc::UnboundedSender<String>,
    request: &JsonValue,
) -> bool {
    let params = request.get("params").unwrap_or(&JsonValue::Null);
    let Err(error) = authority.resolve_params(params) else {
        return false;
    };
    send_discovery_error(
        socket_tx,
        request.get("id").unwrap_or(&JsonValue::Null),
        "session/load",
        &error,
    );
    true
}

pub(super) fn reject_unauthorized_requested_root(
    authority: &ProjectAuthority,
    socket_tx: &mpsc::UnboundedSender<String>,
    request: &JsonValue,
) -> bool {
    let params = request.get("params").unwrap_or(&JsonValue::Null);
    if project_path_param(params).is_none() {
        return false;
    }
    let Err(error) = authority.resolve_params(params) else {
        return false;
    };
    send_discovery_error(
        socket_tx,
        request.get("id").unwrap_or(&JsonValue::Null),
        "session/load",
        &error,
    );
    true
}

pub(super) async fn list(
    root: &Path,
    params: &JsonValue,
) -> Result<Vec<JsonValue>, DiscoveryError> {
    if !state_filter_includes_persisted(params) {
        return Ok(Vec::new());
    }
    let Some(cwd) = cwd_param(params) else {
        return Ok(Vec::new());
    };
    let sessions = harn_vm::session_timeline::list_persisted_sessions(root, 500)
        .await
        .map_err(|error| DiscoveryError::Store(error.to_string()))?;
    Ok(sessions
        .into_iter()
        .filter(|session| session.cwd.as_deref() == Some(cwd))
        .map(harn_serve::acp_persisted_session_item)
        .collect())
}

fn send_discovery_error(
    socket_tx: &mpsc::UnboundedSender<String>,
    request_id: &JsonValue,
    method: &str,
    error: &DiscoveryError,
) {
    let message = format!("{method}: {error}");
    if let Some(data) = error.data() {
        send_socket_jsonrpc_error_with_data(
            socket_tx,
            request_id,
            error.json_rpc_code(),
            &message,
            data,
        );
    } else {
        send_socket_jsonrpc_error(socket_tx, request_id, error.json_rpc_code(), &message);
    }
}

fn cwd_param(params: &JsonValue) -> Option<&str> {
    acp_session_filter_value(params, "cwd", "cwd").and_then(JsonValue::as_str)
}

fn project_path_param(params: &JsonValue) -> Option<&str> {
    cwd_param(params).or_else(|| {
        let anchor = acp_session_filter_value(params, "workspaceAnchor", "workspace_anchor")?;
        anchor
            .as_str()
            .or_else(|| anchor.get("primary").and_then(JsonValue::as_str))
    })
}

fn state_filter_includes_persisted(params: &JsonValue) -> bool {
    let filter = params
        .get("liveState")
        .or_else(|| params.get("live_state"))
        .or_else(|| params.get("state"))
        .or_else(|| {
            params.get("filter").and_then(|filter| {
                filter
                    .get("liveState")
                    .or_else(|| filter.get("live_state"))
                    .or_else(|| filter.get("state"))
            })
        });
    match filter {
        None => true,
        Some(JsonValue::String(state)) => state == "persisted",
        Some(JsonValue::Array(states)) => states.iter().any(|state| state == "persisted"),
        Some(_) => false,
    }
}
