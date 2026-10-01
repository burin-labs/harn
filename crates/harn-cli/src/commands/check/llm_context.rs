use std::path::Path;

use futures::FutureExt;
use sha2::{Digest, Sha256};

use crate::package::{self, RuntimeExtensions};

/// One project's model aliases and capability policy, captured before workers
/// start. The runtime loader owns parsing and normalization; its exact manifest
/// identity also prevents cached checks from outliving their policy.
pub(super) struct LlmCheckContext {
    extensions: Result<RuntimeExtensions, String>,
}

impl LlmCheckContext {
    pub(super) fn load(file: &Path) -> Self {
        Self {
            extensions: package::try_load_root_runtime_extensions(file)
                .map_err(|error| error.to_string()),
        }
    }

    pub(super) fn with<T>(&self, check: impl FnOnce() -> T) -> Result<T, &str> {
        let extensions = self.extensions.as_ref().map_err(String::as_str)?;
        // Checking is synchronous. Polling the existing runtime scope once
        // installs this file's overlays and restores the worker's prior context,
        // including when the check unwinds. Neighboring projects cannot leak.
        Ok(harn_vm::orchestration::scope_llm_runtime_overrides(
            extensions.llm.clone(),
            extensions.capabilities.clone(),
            async { check() },
        )
        .now_or_never()
        .expect("a synchronous check completes in one poll"))
    }

    pub(super) fn cache_key(&self, base: [u8; 32]) -> [u8; 32] {
        let extensions = self
            .extensions
            .as_ref()
            .expect("invalid project configuration never reaches the cache");
        let mut hash = Sha256::new();
        hash.update(b"harn-check-project-llm-context-v1");
        hash.update(base);
        match extensions.root_manifest_identity {
            Some(identity) => {
                hash.update([1]);
                hash.update(identity);
            }
            None => hash.update([0]),
        }
        hash.finalize().into()
    }
}

#[cfg(test)]
#[path = "llm_context_tests.rs"]
mod tests;
