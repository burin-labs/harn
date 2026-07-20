use super::*;
use harn_modules::package_snapshot::PackageSnapshot;

#[cfg(test)]
mod preparation_test_probe {
    use std::cell::Cell;

    use super::PackageError;

    thread_local! {
        static FAIL_AFTER_UPSERT: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn fail_after_upsert() {
        FAIL_AFTER_UPSERT.with(|fail| fail.set(true));
    }

    pub(super) fn after_upsert() -> Result<(), PackageError> {
        FAIL_AFTER_UPSERT.with(|fail| {
            if fail.replace(false) {
                Err(PackageError::Ops(
                    "injected post-upsert preparation failure".to_string(),
                ))
            } else {
                Ok(())
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LocalDependencyInstallReceipt {
    pub alias: String,
    pub path: String,
    pub installed_packages: usize,
    pub manifest_changed: bool,
    pub generation_changed: bool,
    pub generation: String,
}

#[must_use = "the local dependency install must be committed or rolled back"]
pub(crate) struct LocalDependencyInstall {
    receipt: LocalDependencyInstallReceipt,
    rollback: Option<LocalDependencyRollback>,
}

impl LocalDependencyInstall {
    pub(crate) fn receipt(&self) -> &LocalDependencyInstallReceipt {
        &self.receipt
    }

    pub(crate) fn commit(mut self) -> LocalDependencyInstallReceipt {
        self.rollback = None;
        self.receipt
    }

    pub(crate) fn rollback(mut self) -> Result<LocalDependencyInstallReceipt, PackageError> {
        if let Some(rollback) = self.rollback.take() {
            rollback.restore_owned_dependency()?;
        }
        Ok(self.receipt)
    }
}

struct LocalDependencyRollback {
    workspace: PackageWorkspace,
    package_root: PathBuf,
    alias: String,
    owned_edge_added: bool,
    _mutation_lock: Option<ProjectMutationLock>,
}

impl LocalDependencyRollback {
    fn capture(
        workspace: &PackageWorkspace,
        ctx: &ManifestContext,
        package_root: &Path,
        alias: &str,
        mutation_lock: Option<ProjectMutationLock>,
    ) -> Self {
        Self {
            workspace: workspace.clone(),
            package_root: package_root.to_path_buf(),
            alias: alias.to_string(),
            owned_edge_added: !ctx.manifest.dependencies.contains_key(alias),
            _mutation_lock: mutation_lock,
        }
    }

    fn restore_after_failed_install(self) -> Result<(), PackageError> {
        self.restore_owned_dependency_from_manifest()
    }

    fn restore_owned_dependency(self) -> Result<(), PackageError> {
        self.restore_owned_dependency_from_manifest()
    }

    fn restore_owned_dependency_from_manifest(self) -> Result<(), PackageError> {
        let manifest_path = self.workspace.manifest_dir().join(MANIFEST);
        with_manifest_write_lock(&manifest_path, || {
            self.restore_owned_dependency_under_manifest_lock(&manifest_path)
        })
    }

    fn restore_owned_dependency_under_manifest_lock(
        &self,
        manifest_path: &Path,
    ) -> Result<(), PackageError> {
        if !self.owned_edge_added {
            return Ok(());
        }
        let ctx = self.workspace.load_manifest_context()?;
        let removed = match ctx.manifest.dependencies.get(&self.alias) {
            Some(dependency) if dependency_targets(&ctx.dir, dependency, &self.package_root) => {
                remove_dependency_from_manifest_locked(manifest_path, &self.alias)?
            }
            Some(_) => {
                return Err(PackageError::Ops(format!(
                    "refusing to roll back local dependency '{}' because its target changed",
                    self.alias
                )))
            }
            None => false,
        };
        if !removed {
            return Ok(());
        }
        // Never restore stale lock or pointer bytes. Re-resolve while the
        // manifest mutation remains locked, so the next manifest writer
        // observes both the rollback edit and its published generation.
        install_packages_in_locked(&self.workspace, false, None, false).map(|_| ())
    }
}

fn preparation_failure_with_rollback(
    rollback: &LocalDependencyRollback,
    manifest_path: &Path,
    error: PackageError,
) -> PackageError {
    match rollback.restore_owned_dependency_under_manifest_lock(manifest_path) {
        Ok(()) => error,
        Err(rollback_error) => PackageError::Ops(format!(
            "local dependency preparation failed: {error}; rollback failed: {rollback_error}"
        )),
    }
}

fn failed_install_with_rollback(
    rollback: LocalDependencyRollback,
    error: PackageError,
) -> Result<LocalDependencyInstall, PackageError> {
    match rollback.restore_after_failed_install() {
        Ok(()) => Err(error),
        Err(rollback_error) => Err(PackageError::Ops(format!(
            "local dependency install failed: {error}; rollback failed: {rollback_error}"
        ))),
    }
}

#[cfg(test)]
pub(crate) fn install_local_package(
    workspace: &PackageWorkspace,
    package_root: &Path,
) -> Result<LocalDependencyInstall, PackageError> {
    let mutation_lock = acquire_project_mutation_lock(workspace.manifest_dir())
        .map_err(|error| PackageError::Ops(error.to_string()))?;
    install_local_package_inner(workspace, package_root, Some(mutation_lock))
}

pub(crate) fn install_local_package_locked(
    workspace: &PackageWorkspace,
    package_root: &Path,
    _mutation_lock: &ProjectMutationLock,
) -> Result<LocalDependencyInstall, PackageError> {
    install_local_package_inner(workspace, package_root, None)
}

fn install_local_package_inner(
    workspace: &PackageWorkspace,
    package_root: &Path,
    mut mutation_lock: Option<ProjectMutationLock>,
) -> Result<LocalDependencyInstall, PackageError> {
    let package_root = package_root.canonicalize().map_err(|error| {
        PackageError::Manifest(format!(
            "failed to canonicalize local package {}: {error}",
            package_root.display()
        ))
    })?;
    if !package_root.join(MANIFEST).is_file() {
        return Err(PackageError::Manifest(format!(
            "local package {} has no {MANIFEST}",
            package_root.display()
        )));
    }

    let manifest_path = workspace.manifest_dir().join(MANIFEST);
    let (alias, dependency_path, manifest_changed, generation_before, rollback) =
        with_manifest_write_lock(&manifest_path, || {
            let ctx = workspace.load_manifest_context()?;
            let dependency_path = local_dependency_path(&ctx.dir, &package_root)?;
            let preferred = derive_package_alias_from_path(&package_root)?;
            let alias = local_dependency_alias(&ctx, &package_root, &preferred, &dependency_path)?;
            let rollback = LocalDependencyRollback::capture(
                workspace,
                &ctx,
                &package_root,
                &alias,
                mutation_lock.take(),
            );
            let manifest_before = fs::read_to_string(&manifest_path).map_err(|error| {
                PackageError::Manifest(format!(
                    "failed to read {}: {error}",
                    manifest_path.display()
                ))
            })?;
            let generation_before = current_generation(&ctx.dir)?;
            let dependency = Dependency::Table(Box::new(DepTable {
                path: Some(dependency_path.clone()),
                ..DepTable::default()
            }));
            if let Err(error) =
                upsert_dependency_in_manifest_locked(&manifest_path, &alias, &dependency)
            {
                return Err(preparation_failure_with_rollback(
                    &rollback,
                    &manifest_path,
                    error,
                ));
            }
            #[cfg(test)]
            if let Err(error) = preparation_test_probe::after_upsert() {
                return Err(preparation_failure_with_rollback(
                    &rollback,
                    &manifest_path,
                    error,
                ));
            }
            let manifest_after = match fs::read_to_string(&manifest_path) {
                Ok(content) => content,
                Err(error) => {
                    return Err(preparation_failure_with_rollback(
                        &rollback,
                        &manifest_path,
                        PackageError::Manifest(format!(
                            "failed to read {}: {error}",
                            manifest_path.display()
                        )),
                    ));
                }
            };
            Ok((
                alias,
                dependency_path,
                manifest_before != manifest_after,
                generation_before,
                rollback,
            ))
        })?;

    let installed_packages = match install_packages_in_locked(workspace, false, None, false) {
        Ok(installed) => installed,
        Err(error) => return failed_install_with_rollback(rollback, error),
    };
    let generation = match current_generation(workspace.manifest_dir()) {
        Ok(Some(generation)) => generation,
        Ok(None) => {
            return failed_install_with_rollback(
                rollback,
                PackageError::Lockfile(
                    "local package install published no package generation".to_string(),
                ),
            )
        }
        Err(error) => return failed_install_with_rollback(rollback, error),
    };
    Ok(LocalDependencyInstall {
        receipt: LocalDependencyInstallReceipt {
            alias,
            path: dependency_path,
            installed_packages,
            manifest_changed,
            generation_changed: generation_before.as_deref() != Some(generation.as_str()),
            generation,
        },
        rollback: Some(rollback),
    })
}

fn local_dependency_path(project_root: &Path, package_root: &Path) -> Result<String, PackageError> {
    let project_root = project_root.canonicalize().map_err(|error| {
        PackageError::Manifest(format!(
            "failed to canonicalize project root {}: {error}",
            project_root.display()
        ))
    })?;
    let selected = package_root
        .strip_prefix(&project_root)
        .unwrap_or(package_root);
    if selected.as_os_str().is_empty() {
        return Err(PackageError::Manifest(
            "a project cannot install itself as a local dependency".to_string(),
        ));
    }
    // The result is written into `harn.toml` as a dependency `path = "..."`.
    // Manifests are portable artifacts, so the stored path always uses POSIX
    // separators; otherwise a manifest authored on Windows would carry
    // backslashes and fail to resolve when the project is checked out on a
    // Unix host. Forward slashes resolve correctly on every platform.
    selected
        .to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| PackageError::Manifest("local package path is not UTF-8".to_string()))
}

fn local_dependency_alias(
    ctx: &ManifestContext,
    package_root: &Path,
    preferred: &str,
    dependency_path: &str,
) -> Result<String, PackageError> {
    for (alias, dependency) in &ctx.manifest.dependencies {
        if dependency_targets(&ctx.dir, dependency, package_root) {
            return Ok(alias.clone());
        }
    }
    if !ctx.manifest.dependencies.contains_key(preferred) {
        validate_package_alias(preferred)?;
        return Ok(preferred.to_string());
    }

    let digest = sha256_hex(dependency_path.as_bytes());
    for length in (8..=digest.len()).step_by(4) {
        let candidate = format!("{preferred}-{}", &digest[..length]);
        validate_package_alias(&candidate)?;
        if !ctx.manifest.dependencies.contains_key(&candidate) {
            return Ok(candidate);
        }
    }
    Err(PackageError::Manifest(format!(
        "could not derive an unused dependency alias for {}",
        package_root.display()
    )))
}

fn dependency_targets(project_root: &Path, dependency: &Dependency, package_root: &Path) -> bool {
    let Some(path) = dependency.local_path() else {
        return false;
    };
    let path = Path::new(path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    resolved
        .canonicalize()
        .is_ok_and(|resolved| resolved == package_root)
}

fn current_generation(project_root: &Path) -> Result<Option<String>, PackageError> {
    PackageSnapshot::acquire(project_root)
        .map(|snapshot| snapshot.map(|snapshot| snapshot.generation().to_string()))
        .map_err(|error| PackageError::Lockfile(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn failed_generation_removes_owned_dependency_and_republishes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let manifest = root.join(MANIFEST);
        let manifest_before = b"[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n";
        fs::write(&manifest, manifest_before).unwrap();

        let package = root.join("broken-package");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join(MANIFEST),
            "[package]\nname = \"broken-package\"\nversion = \"0.1.0\"\n\n[dependencies]\nmissing = { path = \"missing\" }\n",
        )
        .unwrap();

        let error = install_local_package(&PackageWorkspace::from_manifest_dir(root), &package)
            .err()
            .expect("missing transitive dependency must fail installation");

        assert!(error.to_string().contains("missing"), "{error}");
        let restored = PackageWorkspace::from_manifest_dir(root)
            .load_manifest_context()
            .unwrap();
        assert!(restored.manifest.dependencies.is_empty());
        assert!(root.join(LOCK_FILE).is_file());
        assert!(PackageSnapshot::acquire(root)
            .unwrap()
            .unwrap()
            .package_names()
            .is_empty());
    }

    #[test]
    fn post_upsert_preparation_failure_rolls_back_the_owned_edge() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(
            root.join(MANIFEST),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let package = root.join("prepared-package");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join(MANIFEST),
            "[package]\nname = \"prepared-package\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        preparation_test_probe::fail_after_upsert();

        let error = install_local_package(&PackageWorkspace::from_manifest_dir(root), &package)
            .err()
            .expect("injected post-upsert failure must abort installation");

        assert!(error.to_string().contains("injected post-upsert"));
        let ctx = PackageWorkspace::from_manifest_dir(root)
            .load_manifest_context()
            .unwrap();
        assert!(!ctx.manifest.dependencies.contains_key("prepared-package"));
        assert!(PackageSnapshot::acquire(root)
            .unwrap()
            .unwrap()
            .package_names()
            .is_empty());
    }

    #[test]
    fn post_upsert_failure_preserves_a_preexisting_dependency_edge() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(
            root.join(MANIFEST),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let package = root.join("existing-package");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join(MANIFEST),
            "[package]\nname = \"existing-package\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let workspace = PackageWorkspace::from_manifest_dir(root);
        install_local_package(&workspace, &package)
            .unwrap()
            .commit();
        let manifest_before = fs::read(root.join(MANIFEST)).unwrap();
        let generation_before = current_generation(root).unwrap();
        preparation_test_probe::fail_after_upsert();

        let error = install_local_package(&workspace, &package)
            .err()
            .expect("injected post-upsert failure must abort installation");

        assert!(error.to_string().contains("injected post-upsert"));
        assert_eq!(fs::read(root.join(MANIFEST)).unwrap(), manifest_before);
        assert_eq!(current_generation(root).unwrap(), generation_before);
        assert_eq!(
            PackageSnapshot::acquire(root)
                .unwrap()
                .unwrap()
                .package_names(),
            &["existing-package".to_string()]
        );
    }

    #[test]
    fn rollback_republishes_the_existing_dependency_generation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(
            root.join(MANIFEST),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let workspace = PackageWorkspace::from_manifest_dir(root);

        let first_package = root.join("first-package");
        fs::create_dir_all(&first_package).unwrap();
        fs::write(
            first_package.join(MANIFEST),
            "[package]\nname = \"first-package\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        install_local_package(&workspace, &first_package)
            .unwrap()
            .commit();

        let manifest_before = fs::read(root.join(MANIFEST)).unwrap();
        let lock_before = fs::read(root.join(LOCK_FILE)).unwrap();

        let second_package = root.join("second-package");
        fs::create_dir_all(&second_package).unwrap();
        fs::write(
            second_package.join(MANIFEST),
            "[package]\nname = \"second-package\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        install_local_package(&workspace, &second_package)
            .unwrap()
            .rollback()
            .unwrap();

        assert_eq!(fs::read(root.join(MANIFEST)).unwrap(), manifest_before);
        assert_eq!(fs::read(root.join(LOCK_FILE)).unwrap(), lock_before);
        assert_eq!(
            PackageSnapshot::acquire(root)
                .unwrap()
                .unwrap()
                .package_names(),
            &["first-package".to_string()]
        );
    }

    #[test]
    fn rollback_preserves_an_interleaved_dependency_install() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(
            root.join(MANIFEST),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let workspace = PackageWorkspace::from_manifest_dir(root);
        let first_package = root.join("first-package");
        let second_package = root.join("second-package");
        for (path, name) in [
            (&first_package, "first-package"),
            (&second_package, "second-package"),
        ] {
            fs::create_dir_all(path).unwrap();
            fs::write(
                path.join(MANIFEST),
                format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
            )
            .unwrap();
        }

        let mutation_lock = acquire_project_mutation_lock(root).unwrap();
        let first =
            install_local_package_locked(&workspace, &first_package, &mutation_lock).unwrap();
        install_local_package_locked(&workspace, &second_package, &mutation_lock)
            .unwrap()
            .commit();
        first.rollback().unwrap();

        let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
        assert!(!manifest.contains("first-package ="));
        assert!(manifest.contains("second-package = { path = \"second-package\" }"));
        let snapshot = PackageSnapshot::acquire(root).unwrap().unwrap();
        assert!(!snapshot
            .package_names()
            .iter()
            .any(|name| name == "first-package"));
        assert!(snapshot
            .package_names()
            .iter()
            .any(|name| name == "second-package"));
    }

    #[test]
    fn idempotent_rollback_keeps_the_preexisting_dependency_edge() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::write(
            root.join(MANIFEST),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let package = root.join("existing-package");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join(MANIFEST),
            "[package]\nname = \"existing-package\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let workspace = PackageWorkspace::from_manifest_dir(root);
        install_local_package(&workspace, &package)
            .unwrap()
            .commit();
        let manifest_before = fs::read(root.join(MANIFEST)).unwrap();

        install_local_package(&workspace, &package)
            .unwrap()
            .rollback()
            .unwrap();

        assert_eq!(fs::read(root.join(MANIFEST)).unwrap(), manifest_before);
    }

    #[test]
    fn ordinary_add_waits_for_local_transaction_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        fs::write(
            root.join(MANIFEST),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let package = root.join("shared-package");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join(MANIFEST),
            "[package]\nname = \"shared-package\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let first =
            install_local_package(&PackageWorkspace::from_manifest_dir(&root), &package).unwrap();
        let (attempted_tx, attempted_rx) = mpsc::channel();
        let add_root = root.clone();
        let add = thread::spawn(move || {
            project_mutation_lock_test_probe::install(move || {
                attempted_tx.send(()).unwrap();
            });
            add_package_to(
                &PackageWorkspace::from_manifest_dir(&add_root),
                "shared-package",
                Some("shared-package"),
                None,
                None,
                None,
                None,
                Some("shared-package"),
                None,
            )
            .unwrap()
        });
        attempted_rx.recv().unwrap();
        first.rollback().unwrap();
        add.join().unwrap();

        let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
        assert!(manifest.contains("shared-package = { path = \"shared-package\" }"));
        assert_eq!(
            PackageSnapshot::acquire(&root)
                .unwrap()
                .unwrap()
                .package_names(),
            &["shared-package".to_string()]
        );
    }
}
