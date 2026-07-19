use super::*;

#[derive(Debug, Clone)]
pub(crate) struct PackageWorkspace {
    manifest_dir: PathBuf,
    cache_dir: Option<PathBuf>,
    registry_source: Option<String>,
    read_process_env: bool,
}

impl PackageWorkspace {
    pub(crate) fn from_current_dir() -> Result<Self, PackageError> {
        let manifest_dir = std::env::current_dir()
            .map_err(|error| PackageError::Manifest(format!("failed to read cwd: {error}")))?;
        Ok(Self::from_manifest_dir(manifest_dir))
    }

    pub(crate) fn from_manifest_dir(manifest_dir: impl Into<PathBuf>) -> Self {
        Self {
            manifest_dir: manifest_dir.into(),
            cache_dir: None,
            registry_source: None,
            read_process_env: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        manifest_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            manifest_dir: manifest_dir.into(),
            cache_dir: Some(cache_dir.into()),
            registry_source: None,
            read_process_env: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_registry_source(mut self, source: impl Into<String>) -> Self {
        self.registry_source = Some(source.into());
        self
    }

    pub(crate) fn manifest_dir(&self) -> &Path {
        &self.manifest_dir
    }

    pub(crate) fn load_manifest_context(&self) -> Result<ManifestContext, PackageError> {
        let manifest_path = self.manifest_dir.join(MANIFEST);
        let manifest = read_manifest_from_path(&manifest_path)?;
        Ok(ManifestContext {
            manifest,
            dir: self.manifest_dir.clone(),
        })
    }

    pub(crate) fn cache_root(&self) -> Result<PathBuf, PackageError> {
        if let Some(cache_dir) = &self.cache_dir {
            return Ok(cache_dir.clone());
        }
        if self.read_process_env {
            if let Ok(value) = std::env::var(HARN_CACHE_DIR_ENV) {
                if !value.trim().is_empty() {
                    return Ok(PathBuf::from(value));
                }
            }
        }

        let home = harn_vm::user_dirs::home_dir().ok_or_else(|| {
            "neither HOME nor USERPROFILE is set and HARN_CACHE_DIR was not provided".to_string()
        })?;
        if cfg!(target_os = "macos") {
            return Ok(home.join("Library/Caches/harn"));
        }
        if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
            return Ok(PathBuf::from(xdg).join("harn"));
        }
        Ok(home.join(".cache/harn"))
    }

    pub(crate) fn resolve_registry_source(
        &self,
        explicit: Option<&str>,
    ) -> Result<String, PackageError> {
        if let Some(explicit) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
            return Ok(explicit.to_string());
        }
        if let Some(source) = self
            .registry_source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if Url::parse(source).is_ok() || PathBuf::from(source).is_absolute() {
                return Ok(source.to_string());
            }
            return Ok(self.manifest_dir.join(source).display().to_string());
        }
        if self.read_process_env {
            if let Ok(value) = std::env::var(HARN_PACKAGE_REGISTRY_ENV) {
                let value = value.trim();
                if !value.is_empty() {
                    return Ok(value.to_string());
                }
            }
        }

        if let Some((manifest, manifest_dir)) = find_nearest_manifest(&self.manifest_dir) {
            if let Some(raw) = manifest
                .registry
                .url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                if Url::parse(raw).is_ok() || PathBuf::from(raw).is_absolute() {
                    return Ok(raw.to_string());
                }
                return Ok(manifest_dir.join(raw).display().to_string());
            }
        }

        Ok(DEFAULT_PACKAGE_REGISTRY_URL.to_string())
    }
}
