use clap::Args;

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
    /// Path to the .harn file to execute.
    pub file: Option<String>,
    /// Positional arguments passed to the pipeline as the global `argv`
    /// list. Place them after a `--` separator: `harn run script.harn -- a b c`.
    // `last = true` alone routes post-`--` tokens into `argv`; combining it
    // with `trailing_var_arg = true` panics at clap runtime.
    #[arg(last = true)]
    pub argv: Vec<String>,
}
