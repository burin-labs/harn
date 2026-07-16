use super::*;

#[derive(Debug, Clone)]
pub(crate) struct DiscoverablePersona {
    pub id: String,
    pub persona: PersonaManifestEntry,
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub provenance: PersonaCatalogProvenance,
}

#[derive(Debug, Clone)]
pub(crate) enum PersonaCatalogProvenance {
    Root,
    Installed(InstalledPersonaProvenance),
}

#[derive(Debug, Clone)]
pub(crate) struct InstalledPersonaProvenance {
    pub package_alias: String,
    pub package_version: Option<String>,
    pub content_hash: Option<String>,
    pub integrity: String,
    pub source: String,
    pub permissions: Vec<String>,
    pub host_requirements: Vec<String>,
}

impl DiscoverablePersona {
    fn root(persona: PersonaManifestEntry, catalog: &ResolvedPersonaManifest) -> Self {
        Self {
            id: persona.name.clone().unwrap_or_default(),
            persona,
            manifest_path: catalog.manifest_path.clone(),
            manifest_dir: catalog.manifest_dir.clone(),
            provenance: PersonaCatalogProvenance::Root,
        }
    }

    fn installed(
        persona: PersonaManifestEntry,
        catalog: &ResolvedPersonaManifest,
        entry: &LockEntry,
        integrity: &str,
    ) -> Self {
        let name = persona.name.clone().unwrap_or_default();
        Self {
            id: format!("{}/{name}", entry.name),
            persona,
            manifest_path: catalog.manifest_path.clone(),
            manifest_dir: catalog.manifest_dir.clone(),
            provenance: PersonaCatalogProvenance::Installed(InstalledPersonaProvenance {
                package_alias: entry.name.clone(),
                package_version: entry.package_version.clone(),
                content_hash: entry.content_hash.clone(),
                integrity: integrity.to_string(),
                source: entry.source.clone(),
                permissions: entry.permissions.clone(),
                host_requirements: entry.host_requirements.clone(),
            }),
        }
    }

    pub(crate) fn installed_provenance(&self) -> Option<&InstalledPersonaProvenance> {
        match &self.provenance {
            PersonaCatalogProvenance::Root => None,
            PersonaCatalogProvenance::Installed(provenance) => Some(provenance),
        }
    }

    pub(crate) fn report(&self) -> Option<InstalledPersonaReport> {
        let provenance = self.installed_provenance()?;
        Some(InstalledPersonaReport {
            id: self.id.clone(),
            name: self.persona.name.clone()?,
            package_alias: provenance.package_alias.clone(),
            package_version: provenance.package_version.clone(),
            content_hash: provenance.content_hash.clone(),
            integrity: provenance.integrity.clone(),
            manifest_path: self.manifest_path.display().to_string(),
            source: provenance.source.clone(),
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PersonaCatalogIssue {
    pub package_alias: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Default)]
pub(crate) struct InstalledPersonaCatalog {
    pub personas: Vec<DiscoverablePersona>,
    pub issues: Vec<PersonaCatalogIssue>,
}

pub(crate) fn installed_persona_catalog(
    snapshot: Option<&harn_modules::package_snapshot::PackageSnapshot>,
    lock: &LockFile,
) -> InstalledPersonaCatalog {
    let mut out = InstalledPersonaCatalog::default();
    for entry in &lock.packages {
        if entry.exports.personas.is_empty() {
            continue;
        }
        load_installed_package_personas(snapshot, entry, &mut out);
    }
    out.personas.sort_by(|left, right| left.id.cmp(&right.id));
    out.issues.sort_by(|left, right| {
        (&left.package_alias, left.code).cmp(&(&right.package_alias, right.code))
    });
    out
}

fn load_installed_package_personas(
    snapshot: Option<&harn_modules::package_snapshot::PackageSnapshot>,
    entry: &LockEntry,
    out: &mut InstalledPersonaCatalog,
) {
    if let Err(error) = validate_package_alias(&entry.name) {
        out.issues.push(PersonaCatalogIssue {
            package_alias: entry.name.clone(),
            code: "persona-package-alias-invalid",
            message: error.to_string(),
        });
        return;
    }
    let Some(package_path) =
        materialized_package_path(snapshot, entry).filter(|path| path.exists())
    else {
        out.issues.push(PersonaCatalogIssue {
            package_alias: entry.name.clone(),
            code: "persona-package-not-materialized",
            message: format!(
                "package {} exports personas but is not materialized",
                entry.name
            ),
        });
        return;
    };

    let catalog = match load_personas_from_manifest_path(&package_path) {
        Ok(catalog) => catalog,
        Err(errors) => {
            out.issues.push(PersonaCatalogIssue {
                package_alias: entry.name.clone(),
                code: "persona-manifest-invalid",
                message: errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            });
            return;
        }
    };

    let mut locked_names = entry.exports.personas.clone();
    locked_names.sort();
    let mut actual_names = catalog
        .personas
        .iter()
        .filter_map(|persona| persona.name.clone())
        .collect::<Vec<_>>();
    actual_names.sort();
    if actual_names != locked_names {
        out.issues.push(PersonaCatalogIssue {
            package_alias: entry.name.clone(),
            code: "persona-exports-stale",
            message: format!(
                "package {} locks persona exports {:?}, but its manifest validates {:?}",
                entry.name, locked_names, actual_names
            ),
        });
        return;
    }

    if let Err(message) = validate_entry_workflows(&catalog) {
        out.issues.push(PersonaCatalogIssue {
            package_alias: entry.name.clone(),
            code: "persona-entry-workflow-invalid",
            message,
        });
        return;
    }

    let integrity = package_integrity_status(snapshot, entry);
    out.personas.extend(
        catalog
            .personas
            .iter()
            .cloned()
            .map(|persona| DiscoverablePersona::installed(persona, &catalog, entry, &integrity)),
    );
}

fn validate_entry_workflows(catalog: &ResolvedPersonaManifest) -> Result<(), String> {
    if catalog
        .manifest_path
        .extension()
        .and_then(|ext| ext.to_str())
        == Some("harn")
    {
        return Ok(());
    }
    for persona in &catalog.personas {
        let name = persona.name.as_deref().unwrap_or("<unnamed>");
        persona_runtime_callable(name, persona, &catalog.manifest_dir)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn validate_package_personas(
    ctx: &ManifestContext,
    errors: &mut Vec<PackageCheckDiagnostic>,
) -> Vec<PackagePersonaExportReport> {
    if ctx.manifest.personas.is_empty() {
        return Vec::new();
    }
    let catalog = match load_personas_from_manifest_path(&ctx.manifest_path()) {
        Ok(catalog) => catalog,
        Err(validation_errors) => {
            errors.extend(
                validation_errors
                    .into_iter()
                    .map(|error| PackageCheckDiagnostic {
                        field: error.field_path,
                        message: error.message,
                    }),
            );
            return Vec::new();
        }
    };
    if let Err(message) = validate_entry_workflows(&catalog) {
        errors.push(PackageCheckDiagnostic {
            field: "[[personas]].entry_workflow".to_string(),
            message,
        });
        return Vec::new();
    }
    let mut reports = catalog
        .personas
        .iter()
        .filter_map(|persona| {
            Some(PackagePersonaExportReport {
                name: persona.name.clone()?,
                version: persona.version.clone(),
                entry_workflow: persona.entry_workflow.clone()?,
            })
        })
        .collect::<Vec<_>>();
    reports.sort_by(|left, right| left.name.cmp(&right.name));
    reports
}

pub(crate) fn load_discoverable_personas(
    manifest: Option<&Path>,
) -> Result<Vec<DiscoverablePersona>, String> {
    let root = load_root_persona_catalog(manifest)?;
    let mut personas = root
        .personas
        .iter()
        .cloned()
        .map(|persona| DiscoverablePersona::root(persona, &root))
        .collect::<Vec<_>>();
    let installed = load_installed_catalog_for_root(&root)?;
    fail_on_catalog_issues(&installed.issues)?;
    personas.extend(installed.personas);
    personas.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(personas)
}

pub(crate) fn resolve_discoverable_persona(
    manifest: Option<&Path>,
    name: &str,
) -> Result<DiscoverablePersona, String> {
    let root = load_root_persona_catalog(manifest)?;
    resolve_discoverable_persona_in_root(&root, name)
}

pub(crate) fn resolve_discoverable_persona_in_root(
    root: &ResolvedPersonaManifest,
    name: &str,
) -> Result<DiscoverablePersona, String> {
    if let Some(persona) = root
        .personas
        .iter()
        .find(|persona| persona.name.as_deref() == Some(name))
        .cloned()
    {
        return Ok(DiscoverablePersona::root(persona, root));
    }
    let Some((package_alias, persona_name)) = name.split_once('/') else {
        return Err(format!(
            "persona '{name}' not found in {}",
            root.manifest_path.display()
        ));
    };
    let installed = load_one_installed_catalog_for_root(root, package_alias)?;
    fail_on_catalog_issues(&installed.issues)?;
    installed
        .personas
        .into_iter()
        .find(|persona| persona.persona.name.as_deref() == Some(persona_name))
        .ok_or_else(|| format!("persona '{name}' is not exported by package {package_alias}"))
}

pub(crate) fn load_root_persona_catalog(
    manifest: Option<&Path>,
) -> Result<ResolvedPersonaManifest, String> {
    let result = if let Some(path) = manifest {
        load_personas_from_manifest_path(path).map(Some)
    } else {
        load_personas_config(None)
    };
    match result {
        Ok(Some(catalog)) => Ok(catalog),
        Ok(None) => Err(
            "no harn.toml found; pass --manifest <path> or run inside a Harn project".to_string(),
        ),
        Err(errors) => Err(errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

fn load_installed_catalog_for_root(
    root: &ResolvedPersonaManifest,
) -> Result<InstalledPersonaCatalog, String> {
    let Some((snapshot, lock)) = root_package_snapshot_and_lock(root)? else {
        return Ok(InstalledPersonaCatalog::default());
    };
    Ok(installed_persona_catalog(Some(&snapshot), &lock))
}

fn load_one_installed_catalog_for_root(
    root: &ResolvedPersonaManifest,
    package_alias: &str,
) -> Result<InstalledPersonaCatalog, String> {
    let Some((snapshot, lock)) = root_package_snapshot_and_lock(root)? else {
        return Err(format!("package '{package_alias}' is not installed"));
    };
    let Some(entry) = lock
        .packages
        .iter()
        .find(|entry| entry.name == package_alias)
    else {
        return Err(format!("package '{package_alias}' is not installed"));
    };
    let mut catalog = InstalledPersonaCatalog::default();
    if entry.exports.personas.is_empty() {
        return Err(format!("package '{package_alias}' exports no personas"));
    }
    load_installed_package_personas(Some(&snapshot), entry, &mut catalog);
    Ok(catalog)
}

fn root_package_snapshot_and_lock(
    root: &ResolvedPersonaManifest,
) -> Result<Option<(harn_modules::package_snapshot::PackageSnapshot, LockFile)>, String> {
    if root.manifest_path.file_name() != Some(OsStr::new(MANIFEST)) {
        return Ok(None);
    }
    let Some(snapshot) =
        harn_modules::package_snapshot::PackageSnapshot::acquire(&root.manifest_dir)
            .map_err(|error| error.to_string())?
    else {
        let workspace_lock = LockFile::load(&root.manifest_dir.join(LOCK_FILE))
            .map_err(|error| error.to_string())?;
        if workspace_lock.as_ref().is_some_and(|lock| {
            lock.packages
                .iter()
                .any(|entry| !entry.exports.personas.is_empty())
        }) {
            return Err(format!(
                "{} is missing; run `harn install`",
                harn_modules::package_snapshot::package_current_path(&root.manifest_dir).display()
            ));
        }
        return Ok(None);
    };
    let lock = LockFile::load(snapshot.lock_path())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("{} is missing", snapshot.lock_path().display()))?;
    Ok(Some((snapshot, lock)))
}

fn fail_on_catalog_issues(issues: &[PersonaCatalogIssue]) -> Result<(), String> {
    if issues.is_empty() {
        return Ok(());
    }
    Err(issues
        .iter()
        .map(|issue| format!("{} [{}] {}", issue.package_alias, issue.code, issue.message))
        .collect::<Vec<_>>()
        .join("\n"))
}
