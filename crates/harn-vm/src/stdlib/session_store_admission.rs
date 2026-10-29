//! Canonical session creation, import and workspace validation.

use super::*;

pub(super) async fn ensure_session(
    store: &SqliteSessionStore,
    state_dir: &SessionStoreDir,
    session_id: &str,
    create_if_missing: bool,
    tenant_id: Option<String>,
) -> Result<bool, VmError> {
    let project_scope = state_dir
        .as_path()
        .parent()
        .map(|path| path.to_string_lossy().into_owned());
    ensure_session_with_create(
        store,
        state_dir,
        session_id,
        create_if_missing,
        tenant_id.clone(),
        CreateSession {
            id: Some(session_id.to_string()),
            tenant_id,
            cwd: project_scope.clone(),
            session_type: Some(SessionType::User),
            project_scope,
            ..CreateSession::default()
        },
    )
    .await
}

async fn ensure_session_with_create(
    store: &SqliteSessionStore,
    state_dir: &SessionStoreDir,
    session_id: &str,
    create_if_missing: bool,
    tenant_id: Option<String>,
    mut create: CreateSession,
) -> Result<bool, VmError> {
    if let Some(workspace) =
        crate::persistent_state::session_workspace_for_state(state_dir.as_path())
    {
        let workspace = workspace.to_string_lossy().into_owned();
        create.cwd = Some(workspace.clone());
        create.project_scope = Some(workspace);
    }
    validate_session_id(session_id)?;
    let legacy_path = legacy_session_path(state_dir, session_id)?;
    if legacy_path.is_file() {
        let source = read_legacy_events(state_dir, session_id)?;
        if !source.events.is_empty() {
            import_legacy_events(store, session_id, source, tenant_id.as_deref()).await?;
            return match store.describe(session_id).await {
                Ok(meta) => {
                    validate_tenant(session_id, &meta.tenant_id, tenant_id.as_deref())?;
                    crate::persistent_state::validate_session_store_workspace(state_dir.as_path(), &meta)?;
                    Ok(true)
                }
                Err(StoreError::NotFound(_)) => Err(VmError::Runtime(format!(
                    "session_store: legacy source for '{session_id}' was already imported and the canonical session was deleted"
                ))),
                Err(error) => Err(store_error(error)),
            };
        }
    }
    match store.describe(session_id).await {
        Ok(meta) => {
            validate_tenant(session_id, &meta.tenant_id, tenant_id.as_deref())?;
            crate::persistent_state::validate_session_store_workspace(state_dir.as_path(), &meta)?;
            Ok(true)
        }
        Err(StoreError::NotFound(_)) => {
            if !create_if_missing {
                return Ok(false);
            }
            match store.create(create).await {
                Ok(_) => {}
                Err(StoreError::AlreadyExists(_)) => {}
                Err(error) => return Err(store_error(error)),
            }
            let meta = store.describe(session_id).await.map_err(store_error)?;
            validate_tenant(session_id, &meta.tenant_id, tenant_id.as_deref())?;
            crate::persistent_state::validate_session_store_workspace(state_dir.as_path(), &meta)?;
            Ok(true)
        }
        Err(error) => Err(store_error(error)),
    }
}

pub(crate) async fn open_canonical_agent_session(
    state_dir: &SessionStoreDir,
    session_id: &str,
    parent_session_id: Option<String>,
    session_type: SessionType,
) -> Result<CanonicalStore, VmError> {
    let directories = crate::agent_sessions::workspace_anchor(session_id)
        .map(|anchor| anchor.additional_directories())
        .transpose()
        .map_err(VmError::Runtime)?
        .unwrap_or_default();
    open_canonical_agent_session_with_directories(
        state_dir,
        session_id,
        parent_session_id,
        session_type,
        directories,
    )
    .await
}

pub(crate) async fn open_canonical_agent_session_with_directories(
    state_dir: &SessionStoreDir,
    session_id: &str,
    parent_session_id: Option<String>,
    session_type: SessionType,
    additional_directories: Vec<String>,
) -> Result<CanonicalStore, VmError> {
    let store = open_store(state_dir)?;
    let project_scope = state_dir
        .as_path()
        .parent()
        .map(|path| path.to_string_lossy().into_owned());
    ensure_session_with_create(
        &store,
        state_dir,
        session_id,
        true,
        None,
        CreateSession {
            id: Some(session_id.to_string()),
            parent_session_id,
            cwd: project_scope.clone(),
            session_type: Some(session_type),
            additional_directories,
            project_scope,
            ..CreateSession::default()
        },
    )
    .await?;
    Ok(store)
}
