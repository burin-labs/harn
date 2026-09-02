use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::Vm;

type ModuleProgressObserver = Arc<dyn Fn(ModulePhaseStats) + Send + Sync>;

/// VM-scoped cumulative work-time and cardinality for module preparation and loading.
///
/// Concurrent child-VM spans are additive, so durations can exceed enclosing
/// wall time. The record is attribution, not another top-level phase clock.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModulePhaseStats {
    /// Wall time spent compiling module sources after cache misses.
    pub module_compile_ms: u64,
    /// Wall time spent reading, hydrating, instantiating, and exporting modules.
    pub module_load_ms: u64,
    /// Module source compilations that completed successfully.
    pub modules_compiled: u64,
    /// Successful first loads across the VMs in this execution tree.
    pub modules_loaded: u64,
}

impl ModulePhaseStats {
    /// Add two snapshots without overflowing counters or durations.
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            module_compile_ms: self
                .module_compile_ms
                .saturating_add(other.module_compile_ms),
            module_load_ms: self.module_load_ms.saturating_add(other.module_load_ms),
            modules_compiled: self.modules_compiled.saturating_add(other.modules_compiled),
            modules_loaded: self.modules_loaded.saturating_add(other.modules_loaded),
        }
    }
}

#[derive(Debug, Default)]
struct ModulePhaseAccumulator {
    compile: Duration,
    load: Duration,
    modules_compiled: u64,
    modules_loaded: u64,
}

/// Opt-in recorder shared by a root VM and the child VMs it creates.
#[derive(Clone, Default)]
pub struct ModulePhaseRecorder {
    inner: Arc<Mutex<ModulePhaseAccumulator>>,
    progress_observer: Arc<Mutex<Option<ModuleProgressObserver>>>,
}

impl std::fmt::Debug for ModulePhaseRecorder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModulePhaseRecorder")
            .field("stats", &self.snapshot())
            .field("observed", &self.progress_observer.lock().is_some())
            .finish()
    }
}

impl ModulePhaseRecorder {
    /// Create an empty recorder independent of any VM.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a stable value snapshot of the accumulated module work.
    pub fn snapshot(&self) -> ModulePhaseStats {
        let stats = self.inner.lock();
        ModulePhaseStats {
            module_compile_ms: duration_ms(stats.compile),
            module_load_ms: duration_ms(stats.load),
            modules_compiled: stats.modules_compiled,
            modules_loaded: stats.modules_loaded,
        }
    }

    /// Observe successful module preparation and load transitions.
    ///
    /// The observer is called outside recorder locks. Merely starting a phase,
    /// or dropping an unsuccessful compilation span, never produces progress.
    pub fn set_progress_observer(
        &self,
        observer: impl Fn(ModulePhaseStats) + Send + Sync + 'static,
    ) {
        *self.progress_observer.lock() = Some(Arc::new(observer));
    }

    fn notify_progress(&self) {
        let observer = self.progress_observer.lock().clone();
        if let Some(observer) = observer {
            observer(self.snapshot());
        }
    }

    pub(crate) fn compile_span(&self) -> ModulePhaseSpan {
        ModulePhaseSpan::new(self.clone(), ModulePhase::Compile)
    }

    pub(crate) fn load_span(&self) -> ModulePhaseSpan {
        ModulePhaseSpan::new(self.clone(), ModulePhase::Load)
    }

    pub(crate) fn record_module_loaded(&self) {
        {
            let mut stats = self.inner.lock();
            stats.modules_loaded = stats.modules_loaded.saturating_add(1);
        }
        self.notify_progress();
    }
}

#[derive(Clone, Copy)]
enum ModulePhase {
    Compile,
    Load,
}

pub(crate) struct ModulePhaseSpan {
    recorder: ModulePhaseRecorder,
    phase: ModulePhase,
    started: Instant,
    successful_compile: bool,
}

impl ModulePhaseSpan {
    fn new(recorder: ModulePhaseRecorder, phase: ModulePhase) -> Self {
        Self {
            recorder,
            phase,
            started: Instant::now(),
            successful_compile: false,
        }
    }

    pub(crate) fn mark_compile_succeeded(&mut self) {
        debug_assert!(matches!(self.phase, ModulePhase::Compile));
        self.successful_compile = true;
    }
}

impl Drop for ModulePhaseSpan {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        {
            let mut stats = self.recorder.inner.lock();
            match self.phase {
                ModulePhase::Compile => {
                    stats.compile = stats.compile.saturating_add(elapsed);
                    if self.successful_compile {
                        stats.modules_compiled = stats.modules_compiled.saturating_add(1);
                    }
                }
                ModulePhase::Load => stats.load = stats.load.saturating_add(elapsed),
            }
        }
        if matches!(self.phase, ModulePhase::Compile) && self.successful_compile {
            self.recorder.notify_progress();
        }
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

impl Vm {
    /// Enable module phase timing and return the recorder owned by this VM tree.
    ///
    /// Calling this more than once preserves the current recording session.
    pub fn enable_module_phase_timing(&mut self) -> ModulePhaseRecorder {
        self.module_phase_recorder
            .get_or_insert_with(ModulePhaseRecorder::new)
            .clone()
    }

    pub(crate) fn module_compile_span(&self) -> Option<ModulePhaseSpan> {
        self.module_phase_recorder
            .as_ref()
            .map(ModulePhaseRecorder::compile_span)
    }

    pub(crate) fn module_load_span(&self) -> Option<ModulePhaseSpan> {
        self.module_phase_recorder
            .as_ref()
            .map(ModulePhaseRecorder::load_span)
    }

    pub(crate) fn record_module_loaded(&mut self) {
        if let Some(count) = &mut self.staged_module_load_count {
            *count = count.saturating_add(1);
            return;
        }
        if let Some(recorder) = &self.module_phase_recorder {
            recorder.record_module_loaded();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_addition_saturates_each_field() {
        let left = ModulePhaseStats {
            module_compile_ms: u64::MAX,
            module_load_ms: 2,
            modules_compiled: 3,
            modules_loaded: u64::MAX,
        };
        let right = ModulePhaseStats {
            module_compile_ms: 1,
            module_load_ms: 4,
            modules_compiled: 5,
            modules_loaded: 1,
        };

        assert_eq!(
            left.saturating_add(right),
            ModulePhaseStats {
                module_compile_ms: u64::MAX,
                module_load_ms: 6,
                modules_compiled: 8,
                modules_loaded: u64::MAX,
            }
        );
    }

    #[test]
    fn child_vms_share_recorder_but_baselines_start_disabled() {
        let mut vm = Vm::new();
        assert!(vm.module_phase_recorder.is_none());

        let recorder = vm.enable_module_phase_timing();
        let mut child = vm.child_vm();
        std::thread::spawn(move || child.record_module_loaded())
            .join()
            .expect("child records from another thread");

        assert_eq!(recorder.snapshot().modules_loaded, 1);
        assert!(vm.baseline().instantiate().module_phase_recorder.is_none());
    }

    #[test]
    fn cancelled_span_releases_its_recorder_handle() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");
        let recorder = ModulePhaseRecorder::new();
        let task_recorder = recorder.clone();

        runtime.block_on(async {
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                let _span = task_recorder.load_span();
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            });
            started_rx.await.expect("span starts");
            task.abort();
            assert!(task.await.expect_err("task is cancelled").is_cancelled());
        });

        assert_eq!(Arc::strong_count(&recorder.inner), 1);
    }

    #[test]
    fn observer_only_sees_successful_module_transitions_and_can_reenter_snapshot() {
        let recorder = ModulePhaseRecorder::new();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let callback_observed = observed.clone();
        let callback_recorder = recorder.clone();
        recorder.set_progress_observer(move |stats| {
            assert_eq!(stats, callback_recorder.snapshot());
            callback_observed.lock().push(stats);
        });

        drop(recorder.compile_span());
        assert!(
            observed.lock().is_empty(),
            "failed compilation is not progress"
        );

        let mut compile = recorder.compile_span();
        compile.mark_compile_succeeded();
        drop(compile);
        recorder.record_module_loaded();

        let observed = observed.lock();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].modules_compiled, 1);
        assert_eq!(observed[0].modules_loaded, 0);
        assert_eq!(observed[1].modules_loaded, 1);
    }

    #[test]
    fn nested_module_loads_are_additive_across_real_child_vms() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");
        let temp = tempfile::tempdir().expect("tempdir");
        let dependency = temp.path().join("dependency.harn");
        let importer = temp.path().join("importer.harn");
        std::fs::write(&dependency, "pub fn value() { return 42 }\n").expect("write dependency");
        std::fs::write(
            &importer,
            "import { value } from \"./dependency\"\npub fn answer() { return value() }\n",
        )
        .expect("write importer");

        let mut vm = Vm::new();
        let recorder = vm.enable_module_phase_timing();
        let mut first = vm.child_vm();
        let mut second = vm.child_vm();

        runtime.block_on(async {
            first
                .load_module_exports(&importer)
                .await
                .expect("first nested load succeeds");
            assert_eq!(recorder.snapshot().modules_loaded, 2);

            second
                .load_module_exports(&importer)
                .await
                .expect("second nested load succeeds");
        });

        assert_eq!(recorder.snapshot().modules_loaded, 4);
    }
}
