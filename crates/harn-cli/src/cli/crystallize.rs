use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct CrystallizeArgs {
    #[command(subcommand)]
    pub command: Option<CrystallizeCommand>,

    /// Directory containing crystallization trace JSON files or persisted run records.
    /// Required when no subcommand is given.
    #[arg(long = "from", value_name = "TRACE_DIR")]
    pub from: Option<String>,
    /// Output path for the generated .harn workflow candidate.
    /// Required when no subcommand is given.
    #[arg(long = "out", value_name = "WORKFLOW")]
    pub out: Option<String>,
    /// Output path for the machine-readable crystallization report.
    /// Required when no subcommand is given.
    #[arg(long = "report", value_name = "REPORT_JSON")]
    pub report: Option<String>,
    /// Optional output path for a minimal eval pack manifest.
    #[arg(long = "eval-pack", value_name = "HARN_EVAL_TOML")]
    pub eval_pack: Option<String>,
    /// Optional bundle directory. When set, the CLI also emits a portable
    /// crystallization bundle (`candidate.json`, `workflow.harn`,
    /// `report.json`, `harn.eval.toml`, redacted `fixtures/`) ready for
    /// Harn Cloud import.
    #[arg(long = "bundle", value_name = "BUNDLE_DIR")]
    pub bundle: Option<String>,
    /// Override the bundle's external_key (defaults to a sanitized workflow name).
    #[arg(long = "bundle-external-key", value_name = "KEY")]
    pub bundle_external_key: Option<String>,
    /// Override the bundle's title.
    #[arg(long = "bundle-title", value_name = "TITLE")]
    pub bundle_title: Option<String>,
    /// Optional team label to place in the bundle manifest.
    #[arg(long = "bundle-team", value_name = "TEAM")]
    pub bundle_team: Option<String>,
    /// Optional repo identifier to place in the bundle manifest.
    #[arg(long = "bundle-repo", value_name = "REPO")]
    pub bundle_repo: Option<String>,
    /// Override the bundle's risk level (low/medium/high).
    #[arg(long = "bundle-risk-level", value_name = "LEVEL")]
    pub bundle_risk_level: Option<String>,
    /// Override the bundle's rollout policy (defaults to `shadow_then_canary`).
    #[arg(long = "bundle-rollout-policy", value_name = "POLICY")]
    pub bundle_rollout_policy: Option<String>,
    /// Minimum number of traces that must contain the repeated sequence.
    #[arg(long = "min-examples", default_value_t = 2)]
    pub min_examples: usize,
    /// Override the generated workflow name.
    #[arg(long = "workflow-name", value_name = "NAME")]
    pub workflow_name: Option<String>,
    /// Package name to place in promotion metadata.
    #[arg(long = "package-name", value_name = "NAME")]
    pub package_name: Option<String>,
    /// Author to include in promotion metadata.
    #[arg(long = "author", value_name = "USER")]
    pub author: Option<String>,
    /// Approver to include in promotion metadata.
    #[arg(long = "approver", value_name = "USER")]
    pub approver: Option<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CrystallizeCommand {
    /// Validate a crystallization candidate bundle on disk.
    Validate(CrystallizeValidateArgs),
    /// Re-run shadow comparison from a bundle's redacted fixtures (no live side effects).
    Shadow(CrystallizeShadowArgs),
    /// Ingest a `release_harn.crystallization_input.v1` fixture bundle (emitted
    /// by `release_harn.harn` in `harn-bump-fleet`) and emit a reviewed
    /// crystallization candidate bundle. The output is bundle-validate /
    /// shadow-replay ready and contains plain-language deterministic-vs-agentic
    /// + recovery summaries.
    Ingest(Box<CrystallizeIngestArgs>),
}

#[derive(Debug, Args)]
pub(crate) struct CrystallizeValidateArgs {
    /// Path to the bundle directory.
    pub bundle_dir: String,
}

#[derive(Debug, Args)]
pub(crate) struct CrystallizeShadowArgs {
    /// Path to the bundle directory.
    pub bundle_dir: String,
}

#[derive(Debug, Args)]
pub(crate) struct CrystallizeIngestArgs {
    /// Directory containing the release-harness fixture bundle (with
    /// `manifest.json` and the `*-events.jsonl` / `tool-observations.jsonl`
    /// streams emitted by `release_harn.harn`).
    #[arg(long = "from", value_name = "FIXTURE_DIR")]
    pub from: String,
    /// Output bundle directory. Will be created if it does not exist.
    #[arg(long = "bundle", value_name = "BUNDLE_DIR")]
    pub bundle: String,
    /// Optional path to also write the standalone `workflow.harn` file
    /// (for diff-friendly review). The bundle always contains the same
    /// file at `<bundle>/workflow.harn`.
    #[arg(long = "out", value_name = "WORKFLOW")]
    pub out: Option<String>,
    /// Optional path to also write the standalone `report.json` file.
    /// Always also written into the bundle as `<bundle>/report.json`.
    #[arg(long = "report", value_name = "REPORT_JSON")]
    pub report: Option<String>,
    /// Optional path to also write the standalone `harn.eval.toml`
    /// eval pack. Always also written into the bundle.
    #[arg(long = "eval-pack", value_name = "HARN_EVAL_TOML")]
    pub eval_pack: Option<String>,
    /// Override the generated workflow name (defaults to `release_harn`).
    #[arg(long = "workflow-name", value_name = "NAME")]
    pub workflow_name: Option<String>,
    /// Override the package name placed in promotion metadata.
    #[arg(long = "package-name", value_name = "NAME")]
    pub package_name: Option<String>,
    /// Author to include in promotion metadata.
    #[arg(long = "author", value_name = "USER")]
    pub author: Option<String>,
    /// Approver to include in promotion metadata.
    #[arg(long = "approver", value_name = "USER")]
    pub approver: Option<String>,
    /// Override the bundle's external_key (defaults to a sanitized workflow name).
    #[arg(long = "bundle-external-key", value_name = "KEY")]
    pub bundle_external_key: Option<String>,
    /// Override the bundle's title.
    #[arg(long = "bundle-title", value_name = "TITLE")]
    pub bundle_title: Option<String>,
    /// Optional team label to place in the bundle manifest.
    #[arg(long = "bundle-team", value_name = "TEAM")]
    pub bundle_team: Option<String>,
    /// Optional repo identifier to place in the bundle manifest.
    #[arg(long = "bundle-repo", value_name = "REPO")]
    pub bundle_repo: Option<String>,
    /// Override the bundle's risk level (low/medium/high).
    #[arg(long = "bundle-risk-level", value_name = "LEVEL")]
    pub bundle_risk_level: Option<String>,
    /// Override the bundle's rollout policy (defaults to `shadow_then_canary`).
    #[arg(long = "bundle-rollout-policy", value_name = "POLICY")]
    pub bundle_rollout_policy: Option<String>,
    /// After writing the bundle, also re-run shadow comparison against
    /// the bundle's own redacted fixtures and fail if it does not pass.
    #[arg(long = "shadow")]
    pub shadow: bool,
}
