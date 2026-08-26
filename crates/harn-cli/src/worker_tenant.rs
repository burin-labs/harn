use std::path::Path;

pub(crate) fn resolve_worker_tenant_scope(
    script_path: &Path,
    tenant_id: Option<&str>,
    state_dir: Option<&Path>,
) -> Result<Option<harn_vm::TenantScope>, String> {
    let Some(tenant_id) = tenant_id else {
        return Ok(None);
    };
    let current_dir = std::env::current_dir()
        .map_err(|error| format!("failed to resolve current directory: {error}"))?;
    let configured_state_dir = state_dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HARN_ORCHESTRATOR_STATE_DIR").map(Into::into));
    let state_root = match configured_state_dir.as_deref() {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => current_dir.join(path),
        None => {
            let base_dir = script_path.parent().unwrap_or_else(|| Path::new("."));
            let base_dir = if base_dir.is_absolute() {
                base_dir.to_path_buf()
            } else {
                current_dir.join(base_dir)
            };
            base_dir.join(".harn/orchestrator")
        }
    };
    let store = harn_vm::TenantStore::load(&state_root)?;
    match store.resolve_id(tenant_id) {
        Ok(scope) => Ok(Some(scope)),
        Err(harn_vm::TenantResolutionError::Unknown) => Err(format!(
            "unknown tenant '{tenant_id}' in {}",
            state_root.display()
        )),
        Err(harn_vm::TenantResolutionError::Suspended(_)) => {
            Err(format!("tenant '{tenant_id}' is suspended"))
        }
    }
}
