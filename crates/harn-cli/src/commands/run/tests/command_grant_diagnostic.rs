// Both grant controls share this bounded failure projection. It excludes child
// output, environment, denial command/resource text, and cleanup command names.
pub(super) const COMMAND_GRANT_FAILURE_DIAGNOSTIC: &str = r"
fn command_grant_failure(result: dict) -> dict {
  return {
    status: result?.status,
    exit_code: result?.exit_code,
    timed_out: result?.timed_out,
    signal: result?.signal,
    duration_ms: result?.duration_ms,
    denial: result?.denial == nil ? nil : {
      schema: result.denial?.schema,
      gate: result.denial?.gate,
      backend: result.denial?.backend,
      operation: result.denial?.operation,
      mechanism: result.denial?.mechanism,
      observability: result.denial?.observability,
      count: result.denial?.count,
      retryable: result.denial?.retryable,
    },
    sandbox: result?.sandbox == nil ? nil : {
      kind: result.sandbox?.kind,
      enforced: result.sandbox?.enforced,
      denial_reporting: result.sandbox?.denial_reporting,
    },
    process_cleanup: result?.process_cleanup == nil ? nil : {
      root_pid: result.process_cleanup?.root_pid,
      attempted_signals: result.process_cleanup?.attempted_signals,
      observed_child_count: result.process_cleanup?.observed_child_count,
      reaped_child_count: result.process_cleanup?.reaped_child_count,
      survivor_count: result.process_cleanup?.survivor_count,
    },
  }
}
";
