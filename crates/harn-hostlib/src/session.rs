//! In-process adapter for the canonical Harn session/transcript store.
//!
//! This capability deliberately owns no persistence or ranking policy. It
//! resolves one project root to the same `.harn/session-store.sqlite` used by
//! the VM transcript journal, applies the VM redaction policy, and projects the
//! typed `harn-session-store` interface into schema-checked hostlib builtins.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_session_store::{
    AppendEvent, CreateSession, Embedder, EventId, ListFilter, ReadRange, SearchMode, SearchQuery,
    SessionStore, SessionType, SqliteSessionStore, StoreError, StoreHooks, UpdateSession,
    MAX_READ_BATCH,
};
use harn_vm::process_sandbox::FsAccess;
use harn_vm::VmValue;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::error::HostlibError;
use crate::json::vm_value_to_json;
use crate::registry::{BuiltinRegistry, HostlibCapability};
use crate::tools::permissions::enforce_path_scope;

/// Open-or-create builtin.
pub const OPEN_BUILTIN: &str = "hostlib_session_open";
/// Typed session metadata update builtin.
pub const UPDATE_BUILTIN: &str = "hostlib_session_update";
/// Canonical event append builtin.
pub const APPEND_BUILTIN: &str = "hostlib_session_append";
/// Session close builtin.
pub const CLOSE_BUILTIN: &str = "hostlib_session_close";
/// Session metadata plus events builtin.
pub const GET_BUILTIN: &str = "hostlib_session_get";
/// Scoped session-list builtin.
pub const LIST_BUILTIN: &str = "hostlib_session_list";
/// Canonical lineage fork builtin.
pub const FORK_BUILTIN: &str = "hostlib_session_fork";
/// Project-scoped FTS search builtin.
pub const SEARCH_FTS_BUILTIN: &str = "hostlib_search_fts";
/// Project-scoped semantic search builtin.
pub const SEARCH_SEMANTIC_BUILTIN: &str = "hostlib_search_semantic";
/// Project-scoped hybrid search builtin.
pub const SEARCH_HYBRID_BUILTIN: &str = "hostlib_search_hybrid";

/// Thin in-process projection over [`harn_session_store::SessionStore`].
#[derive(Clone)]
pub struct SessionCapability {
    embedder: Arc<dyn Embedder>,
}

impl SessionCapability {
    /// Use the supplied backend for both vector indexing and semantic search.
    pub fn with_embedder(embedder: Arc<dyn Embedder>) -> Self {
        Self { embedder }
    }

    fn store(
        &self,
        builtin: &'static str,
        root: &Path,
    ) -> Result<SqliteSessionStore, HostlibError> {
        enforce_path_scope(builtin, root, FsAccess::Write)?;
        let hooks = StoreHooks {
            redaction: Some(Arc::new(harn_vm::redact::current_policy())),
            embedder: self.embedder.clone(),
            ..StoreHooks::default()
        };
        SqliteSessionStore::open_with_hooks(database_path(root), hooks)
            .map_err(|error| backend_error(builtin, error))
    }

    async fn open(&self, args: Vec<VmValue>) -> Result<VmValue, HostlibError> {
        let mut request: OpenRequest = request(OPEN_BUILTIN, &args)?;
        let root = normalized_root(OPEN_BUILTIN, &request.root)?;
        if request.session.project_scope.is_none() {
            request.session.project_scope = Some(project_scope(&root));
        }
        if request.session.cwd.is_none() {
            request.session.cwd = Some(project_scope(&root));
        }
        if request.session.session_type.is_none() {
            request.session.session_type = Some(SessionType::User);
        }
        let store = self.store(OPEN_BUILTIN, &root)?;
        let meta = match request.session.id.as_deref() {
            Some(id) => match store.describe(id).await {
                Ok(meta) => meta,
                Err(StoreError::NotFound(_)) => store
                    .create(request.session)
                    .await
                    .map_err(|error| backend_error(OPEN_BUILTIN, error))?,
                Err(error) => return Err(backend_error(OPEN_BUILTIN, error)),
            },
            None => store
                .create(request.session)
                .await
                .map_err(|error| backend_error(OPEN_BUILTIN, error))?,
        };
        response(OPEN_BUILTIN, meta)
    }

    async fn append(&self, args: Vec<VmValue>) -> Result<VmValue, HostlibError> {
        let request: AppendRequest = request(APPEND_BUILTIN, &args)?;
        let root = normalized_root(APPEND_BUILTIN, &request.root)?;
        let stored = self
            .store(APPEND_BUILTIN, &root)?
            .append(&request.session_id, request.event)
            .await
            .map_err(|error| backend_error(APPEND_BUILTIN, error))?;
        response(APPEND_BUILTIN, stored)
    }

    async fn update(&self, args: Vec<VmValue>) -> Result<VmValue, HostlibError> {
        let request: UpdateRequest = request(UPDATE_BUILTIN, &args)?;
        let root = normalized_root(UPDATE_BUILTIN, &request.root)?;
        let meta = self
            .store(UPDATE_BUILTIN, &root)?
            .update(&request.session_id, request.update)
            .await
            .map_err(|error| backend_error(UPDATE_BUILTIN, error))?;
        response(UPDATE_BUILTIN, meta)
    }

    async fn close(&self, args: Vec<VmValue>) -> Result<VmValue, HostlibError> {
        let request: SessionRequest = request(CLOSE_BUILTIN, &args)?;
        let root = normalized_root(CLOSE_BUILTIN, &request.root)?;
        let meta = self
            .store(CLOSE_BUILTIN, &root)?
            .close(&request.session_id)
            .await
            .map_err(|error| backend_error(CLOSE_BUILTIN, error))?;
        response(CLOSE_BUILTIN, meta)
    }

    async fn get(&self, args: Vec<VmValue>) -> Result<VmValue, HostlibError> {
        let request: SessionRequest = request(GET_BUILTIN, &args)?;
        let root = normalized_root(GET_BUILTIN, &request.root)?;
        let store = self.store(GET_BUILTIN, &root)?;
        let session = store
            .describe(&request.session_id)
            .await
            .map_err(|error| backend_error(GET_BUILTIN, error))?;
        let mut events = Vec::new();
        let mut cursor = None;
        loop {
            let page = store
                .read(
                    &request.session_id,
                    ReadRange {
                        from_event_id: cursor,
                        limit: Some(MAX_READ_BATCH),
                        ..ReadRange::default()
                    },
                )
                .await
                .map_err(|error| backend_error(GET_BUILTIN, error))?;
            cursor = page.next_cursor;
            events.extend(page.events);
            if cursor.is_none() {
                break;
            }
        }
        response(
            GET_BUILTIN,
            serde_json::json!({"session": session, "events": events}),
        )
    }

    async fn list(&self, args: Vec<VmValue>) -> Result<VmValue, HostlibError> {
        let mut request: ListRequest = request(LIST_BUILTIN, &args)?;
        let root = normalized_root(LIST_BUILTIN, &request.root)?;
        if request.filter.project_scope.is_none() && request.filter.tenant_id.is_none() {
            request.filter.project_scope = Some(project_scope(&root));
        }
        let sessions = self
            .store(LIST_BUILTIN, &root)?
            .list(request.filter)
            .await
            .map_err(|error| backend_error(LIST_BUILTIN, error))?;
        response(LIST_BUILTIN, serde_json::json!({"sessions": sessions}))
    }

    async fn fork(&self, args: Vec<VmValue>) -> Result<VmValue, HostlibError> {
        let request: ForkRequest = request(FORK_BUILTIN, &args)?;
        let root = normalized_root(FORK_BUILTIN, &request.root)?;
        let result = self
            .store(FORK_BUILTIN, &root)?
            .fork(
                &request.session_id,
                request.at_event_id,
                request.child_session_id,
            )
            .await
            .map_err(|error| backend_error(FORK_BUILTIN, error))?;
        response(FORK_BUILTIN, result)
    }

    async fn search(
        &self,
        builtin: &'static str,
        mode: SearchMode,
        args: Vec<VmValue>,
    ) -> Result<VmValue, HostlibError> {
        let mut request: SearchRequest = request(builtin, &args)?;
        let root = normalized_root(builtin, &request.root)?;
        if request.query.filter.project_scope.is_none()
            && request.query.filter.tenant_id.is_none()
            && request.query.filter.session_id.is_none()
        {
            request.query.filter.project_scope = Some(project_scope(&root));
        }
        request.query.mode = mode;
        let result = self
            .store(builtin, &root)?
            .search(request.query)
            .await
            .map_err(|error| backend_error(builtin, error))?;
        response(builtin, result)
    }
}

impl Default for SessionCapability {
    fn default() -> Self {
        Self::with_embedder(Arc::new(harn_session_store::LexicalEmbedder::default()))
    }
}

impl HostlibCapability for SessionCapability {
    fn module_name(&self) -> &'static str {
        "session"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        let capability = self.clone();
        registry.register_async_fn("session", OPEN_BUILTIN, "open", move |args| {
            let capability = capability.clone();
            async move { capability.open(args).await }
        });
        let capability = self.clone();
        registry.register_async_fn("session", UPDATE_BUILTIN, "update", move |args| {
            let capability = capability.clone();
            async move { capability.update(args).await }
        });
        let capability = self.clone();
        registry.register_async_fn("session", APPEND_BUILTIN, "append", move |args| {
            let capability = capability.clone();
            async move { capability.append(args).await }
        });
        let capability = self.clone();
        registry.register_async_fn("session", CLOSE_BUILTIN, "close", move |args| {
            let capability = capability.clone();
            async move { capability.close(args).await }
        });
        let capability = self.clone();
        registry.register_async_fn("session", GET_BUILTIN, "get", move |args| {
            let capability = capability.clone();
            async move { capability.get(args).await }
        });
        let capability = self.clone();
        registry.register_async_fn("session", LIST_BUILTIN, "list", move |args| {
            let capability = capability.clone();
            async move { capability.list(args).await }
        });
        let capability = self.clone();
        registry.register_async_fn("session", FORK_BUILTIN, "fork", move |args| {
            let capability = capability.clone();
            async move { capability.fork(args).await }
        });
        for (builtin, method, mode) in [
            (SEARCH_FTS_BUILTIN, "search_fts", SearchMode::Fts),
            (
                SEARCH_SEMANTIC_BUILTIN,
                "search_semantic",
                SearchMode::Semantic,
            ),
            (SEARCH_HYBRID_BUILTIN, "search_hybrid", SearchMode::Hybrid),
        ] {
            let capability = self.clone();
            registry.register_async_fn("session", builtin, method, move |args| {
                let capability = capability.clone();
                async move { capability.search(builtin, mode, args).await }
            });
        }
    }
}

#[derive(Deserialize)]
struct OpenRequest {
    root: String,
    #[serde(flatten)]
    session: CreateSession,
}

#[derive(Deserialize)]
struct SessionRequest {
    root: String,
    session_id: String,
}

#[derive(Deserialize)]
struct UpdateRequest {
    root: String,
    session_id: String,
    #[serde(flatten)]
    update: UpdateSession,
}

#[derive(Deserialize)]
struct AppendRequest {
    root: String,
    session_id: String,
    event: AppendEvent,
}

#[derive(Deserialize)]
struct ListRequest {
    root: String,
    #[serde(default)]
    filter: ListFilter,
}

#[derive(Deserialize)]
struct ForkRequest {
    root: String,
    session_id: String,
    at_event_id: EventId,
    #[serde(default)]
    child_session_id: Option<String>,
}

#[derive(Deserialize)]
struct SearchRequest {
    root: String,
    #[serde(flatten)]
    query: SearchQuery,
}

fn request<T: DeserializeOwned>(
    builtin: &'static str,
    args: &[VmValue],
) -> Result<T, HostlibError> {
    let value = args.first().ok_or(HostlibError::MissingParameter {
        builtin,
        param: "params",
    })?;
    serde_json::from_value(vm_value_to_json(value)).map_err(|error| {
        HostlibError::InvalidParameter {
            builtin,
            param: "params",
            message: error.to_string(),
        }
    })
}

fn response(builtin: &'static str, value: impl serde::Serialize) -> Result<VmValue, HostlibError> {
    let value = serde_json::to_value(value).map_err(|error| HostlibError::Backend {
        builtin,
        message: format!("failed to encode response: {error}"),
    })?;
    Ok(harn_vm::json_to_vm_value(&value))
}

fn normalized_root(builtin: &'static str, root: &str) -> Result<PathBuf, HostlibError> {
    let root = root.trim();
    if root.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "root",
            message: "must be non-empty".to_string(),
        });
    }
    let path = crate::tools::args::resolve_host_path(root);
    Ok(path.canonicalize().unwrap_or(path))
}

fn project_scope(root: &Path) -> String {
    crate::tools::args::to_agent_path(root)
}

fn database_path(root: &Path) -> PathBuf {
    root.join(".harn").join("session-store.sqlite")
}

fn backend_error(builtin: &'static str, error: StoreError) -> HostlibError {
    HostlibError::Backend {
        builtin,
        message: error.to_string(),
    }
}
