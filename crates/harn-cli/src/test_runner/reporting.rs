//! Rendering for per-case diagnostic timing receipts.

use super::TestResult;

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
