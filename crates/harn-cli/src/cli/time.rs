use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct TimeArgs {
    #[command(subcommand)]
    pub command: TimeCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TimeCommand {
    /// Time `harn run` with per-phase wall-clock breakdown
    /// (parse, typecheck, bytecode compile, run setup, run main) plus
    /// per-LLM-call and per-tool-call latency.
    Run(TimeRunArgs),
}

#[derive(Debug, Args)]
pub(crate) struct TimeRunArgs {
    /// Path to a `.harn` file. Required unless `-e` is supplied.
    pub file: Option<String>,
    /// Inline Harn source (replaces `<file>`). Wrapped in a `pipeline
    /// main(task) { ... }` like `harn run -e`.
    #[arg(short = 'e')]
    pub eval: Option<String>,
    /// Emit a structured `JsonEnvelope` to stdout instead of a
    /// human-readable breakdown.
    #[arg(long)]
    pub json: bool,
    /// Skip the on-disk bytecode cache so parse/typecheck/compile always
    /// run cold. Equivalent to setting `HARN_BYTECODE_CACHE=0` for this
    /// invocation only.
    #[arg(long = "no-cache")]
    pub no_cache: bool,
    /// Positional arguments passed to the pipeline as the global `argv`
    /// list. Place them after a `--` separator: `harn time run script.harn -- a b c`.
    #[arg(last = true)]
    pub argv: Vec<String>,
}
