use clap::Args;

/// `harn scan` — read-only structural search + lint over a fileset.
///
/// Two shapes:
///   * `harn scan '<pattern>' [paths...] --lang <lang>` — inline pattern.
///   * `harn scan --rule <file>|--rule-pack <dir> [paths...]` — saved rule(s).
///
/// With `--rule`/`--rule-pack` every positional is a path; otherwise the first
/// positional is the pattern and the rest are paths (the shim disambiguates,
/// since clap can't tell positionally).
#[derive(Debug, Args)]
pub(crate) struct ScanArgs {
    /// `<pattern> [paths...]`, or just `[paths...]` with `--rule`/`--rule-pack`.
    #[arg(value_name = "ARGS")]
    pub args: Vec<String>,
    /// Target language for an inline pattern (e.g. `harn`, `typescript`, `rust`).
    #[arg(long, value_name = "LANG")]
    pub lang: Option<String>,
    /// Run a rule from a TOML file instead of an inline pattern.
    #[arg(long = "rule", value_name = "FILE")]
    pub rule: Option<String>,
    /// Run every `*.toml` rule in a directory (a rule pack).
    #[arg(long = "rule-pack", value_name = "DIR", conflicts_with = "rule")]
    pub rule_pack: Option<String>,
    /// Emit a JSON envelope instead of human-readable output.
    #[arg(long)]
    pub json: bool,
    /// Report-only: emit per-file/total counts (a data table), not each match.
    #[arg(long = "report-only")]
    pub report_only: bool,
}
