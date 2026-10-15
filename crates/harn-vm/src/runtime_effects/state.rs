//! Per-VM runtime effect recording state: shared receipt set + local call memo.

use std::sync::{Arc, Mutex};

use super::cache::RuntimeEffectCallCache;
use super::{EffectRecord, ExecutedEffectRecorder};
use crate::VmValue;

/// Shared execution-tree recorder plus a VM-local receipt memo.
///
/// Child VMs clone the recorder `Arc` and start with an empty memo so repeat
/// materialization stays local while evidence still converges on one receipt.
pub(crate) struct RuntimeEffectState {
    pub(crate) recorder: Arc<Mutex<ExecutedEffectRecorder>>,
    pub(crate) cache: RuntimeEffectCallCache,
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
}
