use std::path::PathBuf;

use clap::Args;
use harn_parser::{DiagnosticCode, RepairSafety};

#[derive(Debug, Args)]
pub(crate) struct FixArgs {
    /// Emit a repair plan without writing files.
    #[arg(long, conflicts_with = "apply")]
    pub plan: bool,
    /// Apply clean repairs at or below the declared safety ceiling.
    #[arg(long, conflicts_with = "plan")]
    pub apply: bool,
    /// With --apply, report what would change without writing files.
    #[arg(long, requires = "apply")]
    pub dry_run: bool,
    /// Maximum repair safety class to include.
    #[arg(long, value_parser = parse_repair_safety, value_name = "SAFETY")]
    pub safety: Option<RepairSafety>,
    /// Restrict the plan/apply pass to explicit Harness capability migrations.
    #[arg(long)]
    pub capability_migrations_only: bool,
    /// Only plan or apply repairs for this diagnostic code. Repeatable;
    /// omitting it keeps every code.
    #[arg(long = "code", value_parser = parse_diagnostic_code, value_name = "CODE")]
    pub codes: Vec<DiagnosticCode>,
    /// Emit the machine-readable RepairPlan JSON.
    #[arg(long)]
    pub json: bool,
    /// .harn file or directory to inspect.
    #[arg(required = true)]
    pub path: PathBuf,
}

fn parse_diagnostic_code(value: &str) -> Result<DiagnosticCode, String> {
    // The catalog is large, so listing every code on a typo is noise. Name the
    // shape instead and point at the command that does enumerate them.
    value.parse::<DiagnosticCode>().map_err(|_| {
        format!("unknown diagnostic code `{value}`; expected a code like `HARN-LNT-073` (see `harn explain --catalog`)")
    })
}

fn parse_repair_safety(value: &str) -> Result<RepairSafety, String> {
    value.parse::<RepairSafety>().map_err(|_| {
        format!(
            "unknown repair safety `{value}`; expected one of: {}",
            RepairSafety::ALL
                .iter()
                .map(|safety| safety.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}
