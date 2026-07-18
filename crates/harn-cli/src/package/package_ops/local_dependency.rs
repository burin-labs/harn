use super::*;
use harn_modules::package_snapshot::{package_current_path, PackageSnapshot};

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
    manifest: FileSnapshot,
    lock: FileSnapshot,
    generation_pointer: FileSnapshot,
    _prior_generation: Option<PackageSnapshot>,
}

impl LocalDependencyRollback {
    fn capture(
        workspace: &PackageWorkspace,
        ctx: &ManifestContext,
        package_root: &Path,
        alias: &str,
    ) -> Result<Self, PackageError> {
        let prior_generation = PackageSnapshot::acquire(&ctx.dir)
            .map_err(|error| PackageError::Lockfile(error.to_string()))?;
        Ok(Self {
            workspace: workspace.clone(),
            package_root: package_root.to_path_buf(),
            alias: alias.to_string(),
            owned_edge_added: !ctx.manifest.dependencies.contains_key(alias),
            manifest: FileSnapshot::capture(ctx.manifest_path())?,
            lock: FileSnapshot::capture(ctx.lock_path())?,
            generation_pointer: FileSnapshot::capture(package_current_path(&ctx.dir))?,
            _prior_generation: prior_generation,
        })
    }

    fn restore_after_failed_install(self) -> Result<(), PackageError> {
        self.restore_owned_dependency_from_manifest()
    }

    fn restore_owned_dependency(self) -> Result<(), PackageError> {
        self.restore_owned_dependency_from_manifest()
    }

    fn restore_owned_dependency_from_manifest(self) -> Result<(), PackageError> {
        let ctx = self.workspace.load_manifest_context()?;
        match (
            self.owned_edge_added,
            ctx.manifest.dependencies.get(&self.alias),
        ) {
            (true, Some(dependency))
                if dependency_targets(&ctx.dir, dependency, &self.package_root) =>
            {
                remove_dependency_from_manifest(&ctx.manifest_path(), &self.alias)?;
            }
            (true, Some(_)) => {
                return Err(PackageError::Ops(format!(
                    "refusing to roll back local dependency '{}' because its target changed",
                    self.alias
                )))
            }
            _ => {}
        }

        let manifest_now = fs::read(ctx.manifest_path()).map_err(|error| {
            PackageError::Ops(format!(
                "failed to read {} during rollback: {error}",
                ctx.manifest_path().display()
            ))
        })?;
        if same_toml_document(self.manifest.before.as_deref(), Some(&manifest_now))? {
            return collect_restore_errors([
                self.manifest.restore_unconditionally(),
                self.lock.restore_unconditionally(),
                self.generation_pointer.restore_unconditionally(),
            ]);
        }

        // Another operation added unrelated manifest state. Preserve it and
        // republish a generation from the current manifest after removing only
        // the dependency edge owned by this transaction.
        install_packages_in(&self.workspace, false, None, false).map(|_| ())
    }
}

fn same_toml_document(left: Option<&[u8]>, right: Option<&[u8]>) -> Result<bool, PackageError> {
    fn parse(bytes: Option<&[u8]>) -> Result<Option<toml::Value>, PackageError> {
        bytes
            .map(|bytes| {
                std::str::from_utf8(bytes)
                    .map_err(|error| PackageError::Manifest(error.to_string()))
                    .and_then(|source| {
                        let mut document: toml::Value =
                            toml::from_str(source).map_err(|error| {
                                PackageError::Manifest(format!(
                                    "failed to compare manifest during rollback: {error}"
                                ))
                            })?;
                        if let Some(table) = document.as_table_mut() {
                            if table
                                .get("dependencies")
                                .and_then(toml::Value::as_table)
                                .is_some_and(toml::map::Map::is_empty)
                            {
                                table.remove("dependencies");
                            }
                        }
                        Ok(document)
                    })
            })
            .transpose()
    }
    Ok(parse(left)? == parse(right)?)
}

fn collect_restore_errors<const N: usize>(
    results: [Result<(), PackageError>; N],
) -> Result<(), PackageError> {
    let errors = results
        .into_iter()
        .filter_map(Result::err)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(PackageError::Ops(errors.join("; ")))
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

struct FileSnapshot {
    path: PathBuf,
    before: Option<Vec<u8>>,
}

impl FileSnapshot {
    fn capture(path: PathBuf) -> Result<Self, PackageError> {
        let before = read_optional_file(&path)?;
        Ok(Self { path, before })
    }

    fn restore_unconditionally(self) -> Result<(), PackageError> {
        restore_file(&self.path, self.before.as_deref())
    }
}

fn read_optional_file(path: &Path) -> Result<Option<Vec<u8>>, PackageError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(PackageError::Ops(format!(
            "failed to read {}: {error}",
            path.display()
        ))),
    }
}

fn restore_file(path: &Path, bytes: Option<&[u8]>) -> Result<(), PackageError> {
    match bytes {
        Some(bytes) => harn_vm::atomic_io::atomic_write(path, bytes).map_err(|error| {
            PackageError::Ops(format!("failed to restore {}: {error}", path.display()))
        }),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PackageError::Ops(format!(
                "failed to remove {} during rollback: {error}",
                path.display()
            ))),
        },
    }
}

pub(crate) fn install_local_package(
    workspace: &PackageWorkspace,
    package_root: &Path,
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

    let ctx = workspace.load_manifest_context()?;
    let dependency_path = local_dependency_path(&ctx.dir, &package_root)?;
    let preferred = derive_package_alias_from_path(&package_root)?;
    let alias = local_dependency_alias(&ctx, &package_root, &preferred, &dependency_path)?;
    let manifest_path = ctx.manifest_path();
    let rollback = LocalDependencyRollback::capture(workspace, &ctx, &package_root, &alias)?;
    let manifest_before =
        rollback.manifest.before.clone().ok_or_else(|| {
            PackageError::Manifest(format!("{} is missing", manifest_path.display()))
        })?;
    let generation_before = rollback
        ._prior_generation
        .as_ref()
        .map(|snapshot| snapshot.generation().to_string());

    let install_result = add_package_to(
        workspace,
        &alias,
        Some(&alias),
        None,
        None,
        None,
        None,
        Some(&dependency_path),
        None,
    );
    let (_, installed_packages) = match install_result {
        Ok(result) => result,
        Err(error) => return failed_install_with_rollback(rollback, error),
    };
    let manifest_after = match fs::read(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            return failed_install_with_rollback(
                rollback,
                PackageError::Manifest(format!(
                    "failed to read {}: {error}",
                    manifest_path.display()
                )),
            )
        }
    };
    let generation = match current_generation(&ctx.dir) {
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
            manifest_changed: manifest_before != manifest_after,
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
    selected
        .to_str()
        .map(str::to_string)
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

    #[test]
    fn failed_generation_restores_manifest_lock_and_pointer() {
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
        assert_eq!(fs::read(&manifest).unwrap(), manifest_before);
        assert!(!root.join(LOCK_FILE).exists());
        assert!(!package_current_path(root).exists());
    }

    #[test]
    fn rollback_restores_an_existing_generation_exactly() {
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
        let pointer_before = fs::read(package_current_path(root)).unwrap();
        let generation_before = current_generation(root).unwrap().unwrap();

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
            fs::read(package_current_path(root)).unwrap(),
            pointer_before
        );
        assert_eq!(
            current_generation(root).unwrap().unwrap(),
            generation_before
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

        let first = install_local_package(&workspace, &first_package).unwrap();
        install_local_package(&workspace, &second_package)
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
}
