//! One owner for session-store location and physical workspace spelling.

use super::*;

/// Explicit trees use their own `.harn`; defaults honor the caller's scoped
/// state root or `HARN_STATE_DIR`. Normalize named trees before deriving the
/// directory and persisted workspace identity, including not-yet-created roots.
pub(crate) fn canonical_store_state_dir(
    options: Option<&DictMap>,
) -> Result<SessionStoreDir, VmError> {
    let explicit_root = option_string(options, "root")?
        .map(|root| crate::stdlib::process::resolve_source_relative_path(&root));
    let dir = store_state_dir(explicit_root)?;
    Ok(SessionStoreDir(crate::run_tape::durable_path(
        &dir.0,
        "session store",
    )?))
}

fn store_state_dir(explicit_root: Option<PathBuf>) -> Result<SessionStoreDir, VmError> {
    if explicit_root.is_none() && crate::persistent_state::current_session_workspace().is_some() {
        return Ok(SessionStoreDir(
            crate::persistent_state::current_persistent_state_root()
                .expect("session scope has a state root"),
        ));
    }
    let named_root = explicit_root.or_else(|| {
        crate::stdlib::process::read_env_value(STORE_ROOT_ENV)
            .filter(|value| !value.trim().is_empty())
            .map(|root| crate::stdlib::process::resolve_source_relative_path(&root))
    });
    Ok(match named_root {
        Some(root) => {
            let root =
                crate::workspace_path::canonicalize_existing_ancestor(&root).map_err(|error| {
                    VmError::Runtime(format!("session_store: cannot resolve root: {error}"))
                })?;
            SessionStoreDir::under_root(&root)
        }
        None => SessionStoreDir(crate::runtime_paths::state_root(
            &crate::stdlib::process::runtime_root_base(),
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_state_root_remains_authoritative_unless_a_tree_is_named() {
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let launcher = crate::security::LauncherEnvironment::from_snapshot(
            std::iter::once((
                crate::runtime_paths::HARN_STATE_DIR_ENV.to_string(),
                state.path().to_string_lossy().into_owned(),
            ))
            .collect(),
        );
        let scope =
            crate::persistent_state::SessionStoreScope::resolve(workspace.path(), &launcher)
                .unwrap();
        let _guard = scope.enter();
        assert_eq!(
            canonical_store_state_dir(None).unwrap().as_path(),
            scope.state_root()
        );
        let options = json_to_vm_value(&json!({"root": workspace.path().to_string_lossy()}));
        assert_eq!(
            canonical_store_state_dir(options.as_dict()).unwrap(),
            SessionStoreDir::under_root(scope.workspace())
        );
    }

    #[test]
    #[cfg(unix)]
    fn named_root_resolves_aliases_and_missing_suffix_but_refuses_dangling_links() {
        let temp = tempfile::tempdir().unwrap();
        let canonical = temp.path().canonicalize().unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&canonical, &alias).unwrap();
        for (input, expected) in [
            (alias.clone(), canonical.clone()),
            (alias.join("new/project"), canonical.join("new/project")),
        ] {
            let options = json_to_vm_value(&json!({"root": input.to_string_lossy()}));
            assert_eq!(
                canonical_store_state_dir(options.as_dict()).unwrap(),
                SessionStoreDir::under_root(&expected),
            );
        }
        let dangling = temp.path().join("dangling");
        std::os::unix::fs::symlink(temp.path().join("absent"), &dangling).unwrap();
        let options = json_to_vm_value(&json!({"root": dangling.to_string_lossy()}));
        assert!(canonical_store_state_dir(options.as_dict()).is_err());
    }
}
