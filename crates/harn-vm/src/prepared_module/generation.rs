use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{PreparedModuleCache, PreparedModuleCacheStats, MAX_REMEMBERED_INTERFACES};
use crate::module_artifact::ModuleProvenance;
use crate::{ModulePhaseStats, VmError};

/// Complete measurement for one immutable prepared module generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreparedModuleGenerationStats {
    pub phases: ModulePhaseStats,
    pub cache: PreparedModuleCacheStats,
    pub source_modules: u64,
    pub source_bytes: u64,
    pub source_digest_blake3: [u8; 32],
}

impl PreparedModuleCache {
    /// Create a cache large enough to retain one complete immutable module
    /// generation. This shares the same hard ceiling as remembered graph
    /// interfaces, so preparation cannot silently publish a generation whose
    /// earliest artifacts were evicted before its first call.
    pub fn for_immutable_generation() -> Self {
        Self::with_capacity(
            NonZeroUsize::new(MAX_REMEMBERED_INTERFACES)
                .expect("immutable generation capacity is non-zero"),
        )
    }

    pub(crate) fn source_snapshot(&self) -> Arc<BTreeMap<PathBuf, Arc<str>>> {
        Arc::new(
            self.sources
                .lock()
                .expect("prepared-module source lock poisoned")
                .clone(),
        )
    }

    /// Prepare the complete immutable generation rooted at `roots`, including
    /// the root modules themselves, and retain its exact source snapshot.
    pub fn prepare_module_generation(
        &self,
        roots: &[PathBuf],
    ) -> Result<PreparedModuleGenerationStats, VmError> {
        let phases = self.prepare_graph_with_provenance(roots, ModuleProvenance::User, true)?;
        Ok(self.generation_stats(phases, roots))
    }

    /// Trusted-host counterpart to [`Self::prepare_module_generation`].
    pub fn prepare_trusted_host_dispatch_generation(
        &self,
        roots: &[PathBuf],
    ) -> Result<PreparedModuleGenerationStats, VmError> {
        let phases =
            self.prepare_graph_with_provenance(roots, ModuleProvenance::TrustedHostDispatch, true)?;
        Ok(self.generation_stats(phases, roots))
    }

    fn generation_stats(
        &self,
        phases: ModulePhaseStats,
        roots: &[PathBuf],
    ) -> PreparedModuleGenerationStats {
        let sources = self
            .sources
            .lock()
            .expect("prepared-module source lock poisoned");
        let root_dir = roots
            .first()
            .and_then(|root| root.parent())
            .map(harn_modules::canonical_path)
            .unwrap_or_default();
        let logical_sources = sources
            .iter()
            .map(|(path, source)| {
                let canonical = harn_modules::canonical_path(path);
                (logical_generation_path(&root_dir, &canonical), source)
            })
            .collect::<BTreeMap<_, _>>();
        PreparedModuleGenerationStats {
            phases,
            cache: self.stats(),
            source_modules: logical_sources.len() as u64,
            source_bytes: logical_sources
                .values()
                .map(|source| source.len() as u64)
                .sum(),
            source_digest_blake3: {
                let mut hasher = blake3::Hasher::new();
                hasher.update(b"harn-prepared-module-generation-v1\0");
                for (path, source) in logical_sources {
                    hasher.update(&(path.len() as u64).to_le_bytes());
                    hasher.update(path.as_bytes());
                    hasher.update(&(source.len() as u64).to_le_bytes());
                    hasher.update(source.as_bytes());
                }
                *hasher.finalize().as_bytes()
            },
        }
    }
}

fn logical_generation_path(root: &Path, path: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(root) {
        return relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
    }

    let root_parts = root.components().collect::<Vec<_>>();
    let path_parts = path.components().collect::<Vec<_>>();
    let shared = root_parts
        .iter()
        .zip(&path_parts)
        .take_while(|(left, right)| left == right)
        .count();
    if shared == 0 {
        return "<external>".to_string();
    }
    std::iter::repeat_n("..".to_string(), root_parts.len() - shared)
        .chain(
            path_parts[shared..]
                .iter()
                .map(|part| part.as_os_str().to_string_lossy().into_owned()),
        )
        .collect::<Vec<_>>()
        .join("/")
}
