//! Test result contracts, timing rollups, and diagnostic rendering.

use serde::Serialize;

use crate::test_timing::DurationSummary;

#[derive(Clone, Debug, Serialize)]
pub struct TestResult {
    pub name: String,
    pub file: String,
    pub passed: bool,
    pub error: Option<String>,
    /// Everything the case wrote via `log`/`print`/`println`/etc, in
    /// execution order. `None` when nothing was written — keeps quiet,
    /// passing cases from padding reports. Discovery and worker-start
    /// errors never reach a VM and always leave this absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_output: Option<String>,
    /// Typed timeout metadata. Consumers must use this instead of parsing
    /// human-readable error text.
    pub timeout: Option<TestTimeout>,
    pub duration_ms: u64,
    /// Per-phase timings for an executed case. Discovery and worker-start
    /// errors have no execution timeline and leave this absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phases: Option<PhaseTimings>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct TestTimeout {
    pub phase: TestPhase,
    pub limit_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestPhase {
    Execute,
}

#[derive(Clone, Debug, Serialize)]
pub struct TestSummary {
    pub results: Vec<TestResult>,
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    pub duration_ms: u64,
    /// Distribution of per-test wall-clock durations.
    pub timing: DurationSummary,
    /// Aggregated phase costs across the entire run.
    pub aggregate: AggregateTimings,
}

/// Wall-clock cost of each phase of a single test execution.
///
/// Sums to the test's `duration_ms` modulo measurement overhead. Surfaced
/// so consumers can attribute cold-start vs assertion cost without
/// having to instrument the runner externally.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct PhaseTimings {
    /// VM construction + stdlib/hostlib registration + skill install +
    /// runtime extension install + manifest hooks/triggers install.
    pub setup_ms: u64,
    /// `Compiler::compile_named` time for this test's chunk.
    pub compile_ms: u64,
    /// `vm.execute(chunk)` wall time, i.e. the actual user-test body.
    pub execute_ms: u64,
    /// VM/LocalSet task cancellation and `reset_thread_local_state` between tests.
    pub teardown_ms: u64,
    /// Module attribution overlapping setup and execute. These values are
    /// diagnostic subtotals and must not be added to the top-level phases.
    pub modules: harn_vm::ModulePhaseStats,
}

/// Cumulative worker-time across the run. Mirrors [`PhaseTimings`] plus the
/// suite-level collection cost (discover + parse). Parallel case phases overlap,
/// so these totals may exceed suite wall time.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct AggregateTimings {
    pub collection_ms: u64,
    pub setup_ms: u64,
    pub compile_ms: u64,
    pub execute_ms: u64,
    pub teardown_ms: u64,
    /// Sum of per-case module attribution. Overlaps setup and execute.
    pub modules: harn_vm::ModulePhaseStats,
}

impl AggregateTimings {
    pub(super) fn from_results(collection_ms: u64, results: &[TestResult]) -> Self {
        results.iter().filter_map(|result| result.phases).fold(
            Self {
                collection_ms,
                ..Self::default()
            },
            |acc, phases| Self {
                collection_ms: acc.collection_ms,
                setup_ms: acc.setup_ms.saturating_add(phases.setup_ms),
                compile_ms: acc.compile_ms.saturating_add(phases.compile_ms),
                execute_ms: acc.execute_ms.saturating_add(phases.execute_ms),
                teardown_ms: acc.teardown_ms.saturating_add(phases.teardown_ms),
                modules: acc.modules.saturating_add(phases.modules),
            },
        )
    }
}

impl TestResult {
    /// Emit a one-line phase breakdown to stderr. Driven by `--diagnose`
    /// / `HARN_TEST_DIAGNOSE=1`. The format is intentionally
    /// machine-readable so downstream eval pipelines can grep it.
    pub(super) fn emit_diagnose(&self) {
        let outcome = if self.passed { "ok" } else { "FAIL" };
        let phases = self
            .phases
            .expect("diagnostics are emitted only for executed cases");
        eprintln!(
            "[harn test diag] {} {} setup={}ms compile={}ms execute={}ms teardown={}ms module_compile={}ms module_load={}ms modules_compiled={} modules_loaded={} total={}ms",
            outcome,
            self.name,
            phases.setup_ms,
            phases.compile_ms,
            phases.execute_ms,
            phases.teardown_ms,
            phases.modules.module_compile_ms,
            phases.modules.module_load_ms,
            phases.modules.modules_compiled,
            phases.modules.modules_loaded,
            self.duration_ms,
        );
    }
}
