use std::path::PathBuf;

use clap::{Args, ValueEnum};

/// `harn usage` — aggregate LLM spend/usage analytics from the local
/// event log's `provider_call_response` records.
#[derive(Debug, Args)]
pub(crate) struct UsageArgs {
    /// Project root whose `.harn/events.sqlite` to read. Defaults to the
    /// current working directory. Ignored when `--all` is set.
    #[arg(long)]
    pub project: Option<PathBuf>,

    /// Discover and aggregate across every `<project>/.harn/events.sqlite`
    /// found under the search roots (current dir, then `~/projects`),
    /// skipping vendored `.cargo/registry` copies.
    #[arg(long)]
    pub all: bool,

    /// Only include calls at or after this date (inclusive). Accepts
    /// `YYYY-MM-DD` or an RFC3339 timestamp.
    #[arg(long)]
    pub since: Option<String>,

    /// Only include calls strictly before this date (exclusive). Accepts
    /// `YYYY-MM-DD` or an RFC3339 timestamp.
    #[arg(long)]
    pub until: Option<String>,

    /// Roll up by one dimension. Defaults to `provider`.
    #[arg(long = "group-by", value_enum, default_value_t = UsageGroupBy::Provider)]
    pub group_by: UsageGroupBy,

    /// Only include rows for this provider.
    #[arg(long)]
    pub provider: Option<String>,

    /// Only include rows for this model.
    #[arg(long)]
    pub model: Option<String>,

    /// Emit the stable JSON envelope instead of the text table.
    #[arg(long)]
    pub json: bool,

    /// Emit CSV rows instead of the text table.
    #[arg(long, conflicts_with = "json")]
    pub csv: bool,

    /// Reconcile local event-log totals against provider billing APIs or
    /// Langfuse. Not implemented in v1 — reserved for a follow-up.
    #[arg(long, value_name = "SOURCE", hide = true)]
    pub backfill: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum UsageGroupBy {
    /// One row per provider.
    Provider,
    /// One row per `provider/model`.
    Model,
    /// One row per calendar day (UTC), as a cumulative time series.
    Day,
    /// One row per ISO week (UTC), as a cumulative time series.
    Week,
    /// One row per calendar month (UTC), as a cumulative time series.
    Month,
}
