use clap::Args;
use std::path::PathBuf;
use std::time::Duration;

use super::ProfileArgs;

#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    /// Print the LLM trace summary after execution.
    #[arg(
        long,
        env = "HARN_TRACE",
        action = clap::ArgAction::SetTrue,
        value_parser = clap::builder::BoolishValueParser::new()
    )]
    pub trace: bool,
    #[command(flatten)]
    pub profile: ProfileArgs,
    /// Print static LLM token/cost estimates and do not execute the script.
    #[arg(long = "explain-cost")]
    pub explain_cost: bool,
    /// Deny specific builtins as a comma-separated list.
    #[arg(long, conflicts_with = "allow")]
    pub deny: Option<String>,
    /// Allow only the listed builtins as a comma-separated list.
    #[arg(long, conflicts_with = "deny")]
    pub allow: Option<String>,
    /// Explicitly authorize a named risky operation for this run. Repeatable.
    ///
    /// This is operator authority, not pipeline configuration: names are exact
    /// (for example `git.push`) and the resulting receipt records the grant.
    #[arg(long = "approve-risky", value_name = "OPERATION")]
    pub approve_risky: Vec<String>,
    /// Disable the default worktree filesystem/process sandbox and
    /// network egress fail-closed guard for this run.
    #[arg(long = "no-sandbox", action = clap::ArgAction::SetTrue)]
    pub no_sandbox: bool,
    /// Permit commands spawned by this run to open network sockets while
    /// retaining the worktree filesystem and process sandbox.
    #[arg(
        long = "allow-process-network",
        action = clap::ArgAction::SetTrue,
        conflicts_with = "no_sandbox"
    )]
    pub allow_process_network: bool,
    /// Interrupt the run after this duration and hard-exit with code 124 if it
    /// does not unwind within Harn's subprocess cleanup grace. Supports
    /// integer durations with ms, s, m, h, d, or w suffixes, for example
    /// `500ms`, `8s`, `2m`. Must be greater than zero.
    #[arg(
        long = "timeout",
        value_name = "DURATION",
        value_parser = parse_run_timeout,
        conflicts_with = "as_job"
    )]
    pub timeout: Option<Duration>,
    /// Extra read-only filesystem roots. Repeatable; each path is
    /// readable but never writable.
    #[arg(
        long = "read-only-root",
        value_name = "PATH",
        conflicts_with = "no_sandbox"
    )]
    pub read_only_root: Vec<PathBuf>,
    /// Extra writable filesystem roots. Repeatable; each path becomes
    /// part of the run's write jail while sandboxing stays enabled.
    #[arg(
        long = "write-root",
        visible_alias = "writable-root",
        value_name = "PATH",
        conflicts_with = "no_sandbox"
    )]
    pub write_root: Vec<PathBuf>,
    /// Extra subprocess-only read roots. Repeatable; Harn filesystem builtins
    /// do not gain access to these paths.
    #[arg(
        long = "sandbox-read-root",
        value_name = "PATH",
        conflicts_with = "no_sandbox"
    )]
    pub sandbox_read_root: Vec<PathBuf>,
    /// Extra subprocess-only write roots. Repeatable; Harn filesystem builtins
    /// do not gain access to these paths.
    #[arg(
        long = "sandbox-write-root",
        value_name = "PATH",
        conflicts_with = "no_sandbox"
    )]
    pub sandbox_write_root: Vec<PathBuf>,
    /// Evaluate inline Harn code instead of a file.
    #[arg(short = 'e')]
    pub eval: Option<String>,
    /// Resume a suspended top-level agent from a worker handle or snapshot path.
    #[arg(
        long = "resume",
        value_name = "HANDLE_OR_SNAPSHOT",
        conflicts_with_all = ["eval", "file", "explain_cost", "allow_unsigned", "dry_run_verify"]
    )]
    pub resume: Option<String>,
    /// Run the script's `@job` entrypoint once against a JSON request
    /// instead of executing the default pipeline. The named function must
    /// carry a `@job("name")` attribute; the request JSON is delivered to
    /// the handler as `event.provider_payload.raw`, and the value the job
    /// returns is printed as a JSON report on stdout. Retry / dead-letter
    /// / budget / cancellation come from the trigger dispatcher.
    #[arg(
        long = "as-job",
        action = clap::ArgAction::SetTrue,
        requires = "job",
        requires = "request",
        conflicts_with_all = ["eval", "resume", "explain_cost"]
    )]
    pub as_job: bool,
    /// Name of the `@job` to run (the `@job("name")` argument, or the
    /// function name for a bare `@job`). Requires `--as-job`.
    #[arg(long = "job", value_name = "NAME", requires = "as_job")]
    pub job: Option<String>,
    /// Path to the JSON request file passed to the `@job`. Requires
    /// `--as-job`. Matches the factory worker `--request file.json`
    /// contract.
    #[arg(long = "request", value_name = "PATH", requires = "as_job")]
    pub request: Option<PathBuf>,
    /// Write the job report JSON to this path in addition to printing it
    /// on stdout. Only meaningful with `--as-job`.
    #[arg(long = "result-out", value_name = "PATH", requires = "as_job")]
    pub result_out: Option<PathBuf>,
    /// Extra skill-discovery roots. Repeatable; each path is a
    /// directory of `<name>/SKILL.md` bundles, equivalent to a
    /// single-entry `$HARN_SKILLS_PATH`. Highest-priority layer —
    /// wins ties against every other layer. See `docs/src/skills.md`.
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
    /// Replay LLM responses from a JSONL fixture file instead of
    /// calling the configured provider.
    #[arg(
        long = "llm-mock",
        value_name = "PATH",
        conflicts_with = "llm_mock_record"
    )]
    pub llm_mock: Option<String>,
    /// Record executed LLM responses into a JSONL fixture file.
    #[arg(
        long = "llm-mock-record",
        value_name = "PATH",
        conflicts_with = "llm_mock"
    )]
    pub llm_mock_record: Option<String>,
    /// Accept first-run provider setup prompts.
    #[arg(long)]
    pub yes: bool,
    /// Emit a signed provenance receipt after the run.
    #[arg(long)]
    pub attest: bool,
    /// Write the signed provenance receipt to this path instead of `.harn/receipts/`.
    #[arg(long = "receipt-out", value_name = "PATH", requires = "attest")]
    pub receipt_out: Option<String>,
    /// Agent id used to look up or generate the receipt signing key.
    #[arg(long = "attest-agent", value_name = "ID", requires = "attest")]
    pub attest_agent: Option<String>,
    /// Emit a versioned NDJSON event stream on stdout (epic #1753).
    /// One `JsonEnvelope<RunEvent>` per line with monotonic `seq`.
    /// See `docs/src/cli/run-json.md`.
    #[arg(long = "json", action = clap::ArgAction::SetTrue)]
    pub json: bool,
    /// When running a `.harnpack`, accept bundles that carry no Ed25519
    /// signature. The default refuses unsigned packs so a missing
    /// signature can't be silently glossed over. Only meaningful for
    /// `.harnpack` inputs; ignored for `.harn` sources.
    #[arg(long = "allow-unsigned", action = clap::ArgAction::SetTrue)]
    pub allow_unsigned: bool,
    /// Verify the embedded signature and replay a `.harnpack` into the
    /// content-addressed cache without executing the entrypoint. Exits
    /// 0 on verify success, 1 on signature failure. Only meaningful
    /// for `.harnpack` inputs.
    #[arg(long = "dry-run-verify", action = clap::ArgAction::SetTrue)]
    pub dry_run_verify: bool,
    /// Suppress `stdout` / `stderr` events when `--json` is active.
    /// Transcript, tool, hook, persona-stage, and the final result
    /// events still flow.
    #[arg(long = "quiet", action = clap::ArgAction::SetTrue, requires = "json")]
    pub quiet: bool,
    /// Emit one terminal run-summary JSON object as a single NDJSON line.
    /// Defaults to stderr; use --summary-file or --summary-fd to keep the
    /// summary separate from the script's own stderr.
    #[arg(long = "emit-summary-json", action = clap::ArgAction::SetTrue)]
    pub emit_summary_json: bool,
    /// Write --emit-summary-json output to this file instead of stderr.
    #[arg(
        long = "summary-file",
        value_name = "PATH",
        requires = "emit_summary_json",
        conflicts_with = "summary_fd"
    )]
    pub summary_file: Option<PathBuf>,
    /// Write --emit-summary-json output to this already-open file descriptor.
    #[arg(
        long = "summary-fd",
        value_name = "FD",
        requires = "emit_summary_json",
        conflicts_with = "summary_file"
    )]
    pub summary_fd: Option<i32>,
    /// Emit one terminal run-phase JSON object as a single NDJSON line.
    /// Defaults to stderr; use --phase-file or --phase-fd to keep the
    /// phase report separate from the script's own stderr.
    #[arg(long = "emit-phase-json", action = clap::ArgAction::SetTrue)]
    pub emit_phase_json: bool,
    /// Write --emit-phase-json output to this file instead of stderr.
    #[arg(
        long = "phase-file",
        value_name = "PATH",
        requires = "emit_phase_json",
        conflicts_with = "phase_fd"
    )]
    pub phase_file: Option<PathBuf>,
    /// Write --emit-phase-json output to this already-open file descriptor.
    #[arg(
        long = "phase-fd",
        value_name = "FD",
        requires = "emit_phase_json",
        conflicts_with = "phase_file"
    )]
    pub phase_fd: Option<i32>,
    /// Emit one terminal run-rusage JSON object as a single NDJSON line.
    /// Defaults to stderr; use --rusage-file or --rusage-fd to keep the
    /// CPU sample separate from the script's own stderr.
    #[arg(long = "emit-rusage-json", action = clap::ArgAction::SetTrue)]
    pub emit_rusage_json: bool,
    /// Write --emit-rusage-json output to this file instead of stderr.
    #[arg(
        long = "rusage-file",
        value_name = "PATH",
        requires = "emit_rusage_json",
        conflicts_with = "rusage_fd"
    )]
    pub rusage_file: Option<PathBuf>,
    /// Write --emit-rusage-json output to this already-open file descriptor.
    #[arg(
        long = "rusage-fd",
        value_name = "FD",
        requires = "emit_rusage_json",
        conflicts_with = "rusage_file"
    )]
    pub rusage_fd: Option<i32>,
    /// Path to the .harn file to execute.
    pub file: Option<String>,
    /// Positional arguments passed to the pipeline as the global `argv`
    /// list. Place them after a `--` separator: `harn run script.harn -- a b c`.
    // `last = true` alone routes post-`--` tokens into `argv`; combining it
    // with `trailing_var_arg = true` panics at clap runtime.
    #[arg(last = true)]
    pub argv: Vec<String>,
}

impl RunArgs {
    pub(crate) fn install_operator_approval_grant(
        &self,
    ) -> Option<harn_vm::orchestration::OperatorApprovalGrantGuard> {
        harn_vm::orchestration::OperatorApprovalGrant::from_cli_operations(
            self.approve_risky.clone(),
        )
        .unwrap_or_else(|error| crate::command_error(&error))
        .map(harn_vm::orchestration::install_operator_approval_grant)
    }
}

/// Parse the `--timeout` argument for `harn run`.
///
/// Uses the shared duration grammar (`harn_vm::duration_parse`), then applies
/// one additional rule that is specific to this flag: **zero is rejected**. A
/// `--timeout 0` would arm a deadline that has already expired, killing the
/// run before it starts; that is always a mistake rather than an instruction,
/// so it is caught here rather than obeyed. The check lives at this call site
/// on purpose — it is a property of this flag, not of the duration grammar,
/// and other durations (a zero-length debounce, say) are legitimately zero.
pub(crate) fn parse_run_timeout(raw: &str) -> Result<Duration, String> {
    use harn_vm::duration_parse::DurationParseError;

    let millis = harn_vm::duration_parse::parse_millis(raw).map_err(|error| match error {
        DurationParseError::Empty => "duration must not be empty".to_string(),
        DurationParseError::MissingUnit => {
            "duration must use an ms, s, m, h, d, or w suffix".to_string()
        }
        DurationParseError::NoDigits => "duration amount must be a positive integer".to_string(),
        DurationParseError::UnknownUnit(_) => {
            "duration must use an ms, s, m, h, d, or w suffix".to_string()
        }
        DurationParseError::AmountOverflow | DurationParseError::TooLarge => {
            "duration is too large".to_string()
        }
    })?;
    if millis == 0 {
        return Err("duration must be greater than zero".to_string());
    }
    Ok(Duration::from_millis(millis))
}

#[cfg(test)]
mod tests {
    use super::parse_run_timeout;
    use std::time::Duration;

    #[test]
    fn parse_run_timeout_accepts_explicit_units() {
        assert_eq!(
            parse_run_timeout("500ms").expect("milliseconds"),
            Duration::from_millis(500)
        );
        assert_eq!(
            parse_run_timeout("8s").expect("seconds"),
            Duration::from_secs(8)
        );
        assert_eq!(
            parse_run_timeout("2m").expect("minutes"),
            Duration::from_mins(2)
        );
        assert_eq!(
            parse_run_timeout("1h").expect("hours"),
            Duration::from_hours(1)
        );
    }

    #[test]
    fn parse_run_timeout_rejects_ambiguous_or_zero_values() {
        assert!(parse_run_timeout("8").is_err());
        assert!(parse_run_timeout("0s").is_err());
        assert!(parse_run_timeout("1.5s").is_err());
    }

    #[test]
    fn parse_run_timeout_accepts_the_shared_vocabulary() {
        // `d` and `w` were rejected by this flag's private parser; the shared
        // grammar accepts them everywhere. Compare in seconds: `from_days` is
        // unstable, and `from_secs(2 * 86_400)` would trip a clippy unit lint.
        assert_eq!(parse_run_timeout("2d").expect("days").as_secs(), 2 * 86_400);
        assert_eq!(
            parse_run_timeout("1w").expect("weeks").as_secs(),
            7 * 86_400
        );
    }

    #[test]
    fn parse_run_timeout_still_rejects_zero_in_every_unit() {
        // The one rule this flag keeps beyond the shared grammar.
        for raw in ["0ms", "0s", "0m", "0h", "0d", "0w"] {
            assert!(parse_run_timeout(raw).is_err(), "{raw} must be rejected");
        }
    }
}
