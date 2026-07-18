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
            rollback.restore()?;
        }
        Ok(self.receipt)
    }
}

struct LocalDependencyRollback {
    manifest: FileSnapshot,
    lock: FileSnapshot,
    generation_pointer: FileSnapshot,
    _prior_generation: Option<PackageSnapshot>,
}

impl LocalDependencyRollback {
    fn capture(ctx: &ManifestContext) -> Result<Self, PackageError> {
        let prior_generation = PackageSnapshot::acquire(&ctx.dir)
            .map_err(|error| PackageError::Lockfile(error.to_string()))?;
        Ok(Self {
            manifest: FileSnapshot::capture(ctx.manifest_path())?,
            lock: FileSnapshot::capture(ctx.lock_path())?,
            generation_pointer: FileSnapshot::capture(package_current_path(&ctx.dir))?,
            _prior_generation: prior_generation,
        })
    }

    fn record_installed_state(&mut self) -> Result<(), PackageError> {
        self.manifest.record_current()?;
        self.lock.record_current()?;
        self.generation_pointer.record_current()?;
        Ok(())
    }

    fn restore(self) -> Result<(), PackageError> {
        // Restore the pointer last so readers see either complete generation.
        collect_restore_errors([
            self.manifest.restore(),
            self.lock.restore(),
            self.generation_pointer.restore(),
        ])
    }

    fn restore_after_failed_install(self) -> Result<(), PackageError> {
        collect_restore_errors([
            self.manifest.restore_unconditionally(),
            self.lock.restore_unconditionally(),
            self.generation_pointer.restore_unconditionally(),
        ])
    }
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
    installed: Option<Vec<u8>>,
    installed_recorded: bool,
}

impl FileSnapshot {
    fn capture(path: PathBuf) -> Result<Self, PackageError> {
        let before = read_optional_file(&path)?;
        Ok(Self {
            path,
            before,
            installed: None,
            installed_recorded: false,
        })
    }

    fn record_current(&mut self) -> Result<(), PackageError> {
        self.installed = read_optional_file(&self.path)?;
        self.installed_recorded = true;
        Ok(())
    }

    fn restore(self) -> Result<(), PackageError> {
        if !self.installed_recorded {
            return Err(PackageError::Ops(format!(
                "local dependency rollback for {} has no installed-state snapshot",
                self.path.display()
            )));
        }
        let current = read_optional_file(&self.path)?;
        if current != self.installed {
            return Err(PackageError::Ops(format!(
                "refusing to roll back local dependency install because {} changed afterward",
                self.path.display()
            )));
        }
        restore_file(&self.path, self.before.as_deref())
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
    let mut rollback = LocalDependencyRollback::capture(&ctx)?;
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

    if let Err(error) = rollback.record_installed_state() {
        return failed_install_with_rollback(rollback, error);
    }

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
}
