use std::path::PathBuf;

use clap::{Args, ValueEnum};
use harn_parser::RepairSafety;

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
    /// How Harness migrations should satisfy call sites without a local Harness binding.
    #[arg(long, value_enum, default_value_t = HarnessThreadingMode::LocalGlobal)]
    pub harness_threading: HarnessThreadingMode,
    /// Emit the machine-readable RepairPlan JSON.
    #[arg(long)]
    pub json: bool,
    /// .harn file or directory to inspect.
    #[arg(required = true)]
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum HarnessThreadingMode {
    /// Use the VM-level `harness` binding and preserve helper signatures.
    #[default]
    LocalGlobal,
    /// Add `harness: Harness` parameters and update same-file callers.
    ThreadParams,
}

impl HarnessThreadingMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::LocalGlobal => "local-global",
            Self::ThreadParams => "thread-params",
        }
    }
}

impl std::fmt::Display for HarnessThreadingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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
