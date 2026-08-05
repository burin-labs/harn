//! Per-VM runtime effect recording state: shared receipt set + local call memo.

use std::sync::{Arc, Mutex};

use super::effect_call_cache::RuntimeEffectCallCache;
use super::effects::{EffectRecord, ExecutedEffectRecorder};
use crate::VmValue;

/// Shared execution-tree recorder plus a VM-local receipt memo.
///
/// Child VMs clone the recorder `Arc` and start with an empty memo so repeat
/// materialization stays local while evidence still converges on one receipt.
pub(crate) struct RuntimeEffectState {
    pub recorder: Arc<Mutex<ExecutedEffectRecorder>>,
    pub cache: RuntimeEffectCallCache,
}

impl Default for RuntimeEffectState {
    fn default() -> Self {
        Self::fresh()
    }
}

impl RuntimeEffectState {
    pub(crate) fn fresh() -> Self {
        Self {
            recorder: Arc::new(Mutex::new(ExecutedEffectRecorder::default())),
            cache: RuntimeEffectCallCache::default(),
        }
    }

    pub(crate) fn with_shared_recorder(recorder: Arc<Mutex<ExecutedEffectRecorder>>) -> Self {
        Self {
            recorder,
            cache: RuntimeEffectCallCache::default(),
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<EffectRecord> {
        self.recorder
            .lock()
            .expect("effect recorder poisoned")
            .snapshot()
    }

    pub(crate) fn clear(&mut self) {
        self.recorder
            .lock()
            .expect("effect recorder poisoned")
            .clear();
        self.cache.clear();
    }

    pub(crate) fn record_specs(
        &mut self,
        specs: &'static [harn_builtin_meta::EffectSpec],
        args: &[VmValue],
    ) {
        if self.cache.contains(specs, args) {
            return;
        }
        self.recorder
            .lock()
            .expect("effect recorder poisoned")
            .record(specs, args);
        self.cache.remember(specs, args);
    }

    pub(crate) fn record_capability(
        &mut self,
        capability: harn_builtin_meta::CapabilityId,
        method: &str,
        args: &[VmValue],
    ) {
        let Some(entry) = crate::stdlib::capability_method_manifest_entry(capability, method)
        else {
            return;
        };
        self.record_specs(entry.contract.effects, args);
    }
}
