//! Terminal outcome assembly for `harn run`.
//!
//! One owner for the last step of every run path: turn a finished, failed, or
//! dry-run-verified execution into the `RunOutcome` the CLI exits on, and emit
//! the matching NDJSON when `--json` is active. Keeping it beside the driver in
//! `mod.rs` rather than inside it means a change to how a failure is reported
//! cannot quietly change how a run is driven.

use super::*;

/// State for a single `harn run --json` invocation. `execute_run_inner`
/// attaches its sink to the run's ambient execution scope, including setup and
/// terminal handling, so every observable event stays in this stream.
///
/// `finalize_result` / `finalize_error` emit the terminal event and
/// build a [`RunOutcome`] whose stdout/stderr captured-buffer fields
/// stay **empty** — the canonical stream is on `out`.
/// `outcome.exit_code` still carries the process exit code so the
/// binary entry can `process::exit(...)`.
pub(super) struct JsonRunSession {
    emitter: self::json_events::NdjsonEmitter,
}

impl JsonRunSession {
    pub(super) fn new(options: RunJsonOptions, out: Box<dyn io::Write + Send>) -> Self {
        Self {
            emitter: NdjsonEmitter::new(out, options.quiet),
        }
    }

    pub(super) fn sink(&self) -> Arc<dyn harn_vm::run_events::RunEventSink> {
        self.emitter.sink()
    }

    pub(super) fn finalize_result(self, value: serde_json::Value, exit_code: i32) -> RunOutcome {
        self.emitter.emit_result(value, exit_code);
        RunOutcome {
            stdout: String::new(),
            stderr: String::new(),
            exit_code,
        }
    }

    pub(super) fn finalize_error(
        self,
        code: impl Into<String>,
        message: impl Into<String>,
        exit_code: i32,
    ) -> RunOutcome {
        self.finalize_error_with_details(code, message, serde_json::Value::Null, exit_code)
    }

    pub(super) fn finalize_error_with_details(
        self,
        code: impl Into<String>,
        message: impl Into<String>,
        details: serde_json::Value,
        exit_code: i32,
    ) -> RunOutcome {
        self.emitter.emit_error_with_details(code, message, details);
        RunOutcome {
            stdout: String::new(),
            stderr: String::new(),
            exit_code,
        }
    }
}

/// End a run that failed.
///
/// `failure` decides the process exit status. A caller that shells out to Harn
/// has to be able to tell "your dependencies could not be prepared" from "your
/// code returned a failure", and the only place that distinction can be made
/// honestly is here, where the phase that failed is still known.
#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_run_error(
    stdout: String,
    mut stderr: String,
    json_session: Option<JsonRunSession>,
    summary: Option<&RunSummaryOptions>,
    phase: Option<&RunPhaseOptions>,
    rusage: Option<&RunRusageOptions>,
    started: Instant,
    profile: Option<&harn_vm::profile::RunProfile>,
    timing: Option<&RunTiming>,
    main_events: u64,
    cpu_ms_total: Option<u64>,
    failure: crate::exit::RunFailure,
    code: impl Into<String>,
    message: impl Into<String>,
) -> RunOutcome {
    finalize_run_error_with_details(
        stdout,
        stderr,
        json_session,
        summary,
        phase,
        rusage,
        started,
        profile,
        timing,
        main_events,
        cpu_ms_total,
        failure,
        code,
        message,
        serde_json::Value::Null,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_run_error_with_details(
    stdout: String,
    mut stderr: String,
    json_session: Option<JsonRunSession>,
    summary: Option<&RunSummaryOptions>,
    phase: Option<&RunPhaseOptions>,
    rusage: Option<&RunRusageOptions>,
    started: Instant,
    profile: Option<&harn_vm::profile::RunProfile>,
    timing: Option<&RunTiming>,
    main_events: u64,
    cpu_ms_total: Option<u64>,
    failure: crate::exit::RunFailure,
    code: impl Into<String>,
    message: impl Into<String>,
    details: serde_json::Value,
) -> RunOutcome {
    let aux_emission = emit_run_aux_for_exit(
        summary,
        phase,
        rusage,
        started,
        failure.exit_code(),
        profile,
        None,
        timing,
        main_events,
        cpu_ms_total,
        json_session.is_some(),
        &mut stderr,
    );
    if let Some(session) = json_session {
        let mut outcome =
            session.finalize_error_with_details(code, message, details, aux_emission.exit_code);
        outcome.stderr = aux_emission.stderr;
        return outcome;
    }
    RunOutcome {
        stdout,
        stderr,
        exit_code: aux_emission.exit_code,
    }
}

/// Translate a `.harnpack` preflight failure into either the `--json` error
/// event stream or a plain stderr message plus the program-failure status.
///
/// Deliberately not a setup failure, even though it happens before the program
/// runs. Most of what this rejects is a refusal about the bundle itself — an
/// absent signature, a signature that does not cover the bytes on disk — and
/// those are verdicts on the artifact, not faults in the host that was asked to
/// run it. A caller that reads the setup status as "infrastructure, retry it"
/// would retry a tampered bundle forever.
pub(super) fn finalize_harnpack_error(
    mut stderr: String,
    json_session: Option<JsonRunSession>,
    summary: Option<&RunSummaryOptions>,
    phase: Option<&RunPhaseOptions>,
    rusage: Option<&RunRusageOptions>,
    started: Instant,
    err: HarnpackError,
) -> RunOutcome {
    let code = err.code;
    let message = err.message;
    stderr.push_str(&format!("error: {message}\n"));
    finalize_run_error(
        String::new(),
        stderr,
        json_session,
        summary,
        phase,
        rusage,
        started,
        None,
        None,
        0,
        None,
        crate::exit::RunFailure::Program,
        code,
        message,
    )
}

/// Successful `--dry-run-verify` path. Reports the bundle hash and
/// signature outcome on stderr (since stdout belongs to the script) and
/// emits a terminal `result` event when `--json` is active so consumers
/// see the run complete.
pub(super) fn finalize_harnpack_dry_run(
    mut stderr: String,
    json_session: Option<JsonRunSession>,
    summary_options: Option<&RunSummaryOptions>,
    phase_options: Option<&RunPhaseOptions>,
    rusage_options: Option<&RunRusageOptions>,
    started: Instant,
    cpu_ms_total: Option<u64>,
    prepared: &PreparedHarnpack,
) -> RunOutcome {
    let summary = format!(
        "[harn] harnpack verify ok: bundle_hash={}, signature_verified={}, cache_hit={}, execution_artifact={}\n",
        prepared.bundle_hash,
        prepared.signature_verified,
        prepared.cache_hit,
        prepared.execution_artifact_state
    );
    stderr.push_str(&summary);
    let aux_emission = emit_run_aux_for_exit(
        summary_options,
        phase_options,
        rusage_options,
        started,
        0,
        None,
        None,
        None,
        0,
        cpu_ms_total,
        json_session.is_some(),
        &mut stderr,
    );
    if let Some(session) = json_session {
        if let Some(error) = aux_emission.error {
            let mut outcome = session.finalize_error(
                "run_aux",
                format!("failed to emit auxiliary run JSON: {error}"),
                1,
            );
            outcome.stderr = aux_emission.stderr;
            return outcome;
        }
        let value = serde_json::json!({
            "bundle_hash": prepared.bundle_hash,
            "signature_verified": prepared.signature_verified,
            "key_id": prepared.key_id,
            "cache_hit": prepared.cache_hit,
            "dry_run_verify": true,
            "execution_artifact_state": prepared.execution_artifact_state,
            "fallback_reason": prepared.fallback_reason,
            "artifact_decode_ms": prepared.artifact_decode_elapsed.as_millis() as u64,
            "link_report": prepared.manifest.execution_artifact.as_ref().map(|artifact| &artifact.link_report),
        });
        let mut outcome = session.finalize_result(value, aux_emission.exit_code);
        outcome.stderr = aux_emission.stderr;
        return outcome;
    }
    RunOutcome {
        stdout: String::new(),
        stderr,
        exit_code: aux_emission.exit_code,
    }
}

pub(super) fn render_return_value_error(value: &harn_vm::VmValue) -> String {
    let harn_vm::VmValue::EnumVariant(enum_variant) = value else {
        return String::new();
    };
    if !enum_variant.is_variant("Result", "Err") {
        return String::new();
    }
    let rendered = enum_variant
        .fields
        .first()
        .map(|p| p.display())
        .unwrap_or_default();
    if rendered.is_empty() {
        "error\n".to_string()
    } else if rendered.ends_with('\n') {
        rendered
    } else {
        format!("{rendered}\n")
    }
}
