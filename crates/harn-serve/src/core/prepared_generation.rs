use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use harn_vm::{PreparedModuleCache, Vm, VmBaseline};

use super::{classify_vm_error, install_dispatch_vm_runtime, DispatchCore, DispatchCoreConfig};
use crate::{DispatchError, ExportCatalog};

/// Measured work used to construct one immutable dispatch generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchGenerationReceipt {
    pub preparation_ms: u64,
    pub module_compile_ms: u64,
    pub module_load_ms: u64,
    pub modules_compiled: u64,
    pub source_modules: u64,
    pub source_bytes: u64,
    pub worker_count: usize,
    pub cache_entries: usize,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub source_digest_blake3: [u8; 32],
}

/// Per-call timing and prepared-generation usage. `queue_ms = None` means the
/// caller bypassed [`crate::DispatchRuntime`], while `Some(0)` is a measured
/// uncontended queue. A replay has no VM execution and therefore reports
/// `generation_cache_hit = None` and `execution_ms = None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchCallReceipt {
    pub generation_cache_hit: Option<bool>,
    pub queue_ms: Option<u64>,
    pub execution_ms: Option<u64>,
}

/// Stable source and bytecode templates shared by every call to one server.
/// Runtime stacks, module state, cancellation, and host configuration remain
/// per invocation through [`VmBaseline::instantiate`].
pub(super) struct PreparedDispatchGeneration {
    baseline: VmBaseline,
    source: Arc<str>,
    receipt: DispatchGenerationReceipt,
}

impl PreparedDispatchGeneration {
    pub(super) fn prepare(
        config: &DispatchCoreConfig,
        catalog: &ExportCatalog,
    ) -> Result<Self, DispatchError> {
        let started = Instant::now();
        let source = std::fs::read_to_string(&config.script_path).map_err(|error| {
            DispatchError::Io(format!(
                "failed to read {}: {error}",
                config.script_path.display()
            ))
        })?;

        let mut vm = Vm::new();
        if config.trusted_host_dispatch {
            vm.enable_trusted_host_dispatch()
                .map_err(classify_vm_error)?;
        }
        install_dispatch_vm_runtime(
            &mut vm,
            &config.script_path,
            &source,
            Arc::new(AtomicBool::new(false)),
        );

        let cache = PreparedModuleCache::for_immutable_generation();
        let stats = if config.trusted_host_dispatch {
            cache
                .prepare_trusted_host_dispatch_generation(std::slice::from_ref(&config.script_path))
        } else {
            cache.prepare_module_generation(std::slice::from_ref(&config.script_path))
        }
        .map_err(classify_vm_error)?;
        if stats.cache.entries < stats.source_modules as usize {
            return Err(DispatchError::Validation(format!(
                "prepared generation has {} source modules but retained only {} artifacts",
                stats.source_modules, stats.cache.entries
            )));
        }
        vm.set_prepared_module_generation(cache);

        let receipt = DispatchGenerationReceipt {
            preparation_ms: started.elapsed().as_millis() as u64,
            module_compile_ms: stats.phases.module_compile_ms,
            module_load_ms: stats.phases.module_load_ms,
            modules_compiled: stats.phases.modules_compiled,
            source_modules: stats.source_modules,
            source_bytes: stats.source_bytes,
            worker_count: dispatch_worker_count(config, catalog),
            cache_entries: stats.cache.entries,
            cache_hits: stats.cache.hits,
            cache_misses: stats.cache.misses,
            source_digest_blake3: stats.source_digest_blake3,
        };
        if std::env::var_os("HARN_DISPATCH_GENERATION_DEBUG").is_some() {
            eprintln!(
                "[harn] prepared dispatch generation: preparation_ms={} module_compile_ms={} module_load_ms={} modules_compiled={} source_modules={} source_bytes={} worker_count={} cache_entries={} cache_hits={} cache_misses={}",
                receipt.preparation_ms,
                receipt.module_compile_ms,
                receipt.module_load_ms,
                receipt.modules_compiled,
                receipt.source_modules,
                receipt.source_bytes,
                receipt.worker_count,
                receipt.cache_entries,
                receipt.cache_hits,
                receipt.cache_misses,
            );
        }

        Ok(Self {
            baseline: vm.baseline(),
            source: Arc::from(source),
            receipt,
        })
    }

    pub(super) fn instantiate(&self, cancel_token: Arc<AtomicBool>) -> Vm {
        let mut vm = self.baseline.instantiate();
        vm.install_cancel_token(cancel_token);
        vm
    }

    pub(super) fn source(&self) -> &str {
        &self.source
    }

    pub(super) fn receipt(&self) -> DispatchGenerationReceipt {
        self.receipt
    }
}

impl DispatchCore {
    pub fn generation_receipt(&self) -> DispatchGenerationReceipt {
        self.generation.receipt()
    }

    pub(crate) fn dispatch_worker_count(&self) -> usize {
        self.generation.receipt().worker_count
    }

    pub(crate) fn is_concurrent_dispatch(&self, function: &str) -> bool {
        self.catalog()
            .function(function)
            .and_then(|function| function.annotations)
            .is_some_and(|annotations| {
                annotations.read_only == Some(true) && annotations.idempotent == Some(true)
            })
    }
}

fn dispatch_worker_count(config: &DispatchCoreConfig, catalog: &ExportCatalog) -> usize {
    let has_concurrent_export = catalog.functions.values().any(|function| {
        function.annotations.is_some_and(|annotations| {
            annotations.read_only == Some(true) && annotations.idempotent == Some(true)
        })
    });
    if has_concurrent_export {
        config.max_dispatch_workers.get()
    } else {
        1
    }
}
