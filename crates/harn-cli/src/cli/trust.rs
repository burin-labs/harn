use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub(crate) struct TrustArgs {
    #[command(subcommand)]
    pub command: TrustCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TrustCommand {
    /// Query trust records from the event log.
    Query(TrustQueryArgs),
    /// Verify the trust graph hash chain.
    VerifyChain(TrustVerifyChainArgs),
    /// Promote an agent to a higher autonomy tier.
    Promote(TrustPromoteArgs),
    /// Demote an agent to a lower autonomy tier.
    Demote(TrustDemoteArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum TrustTierArg {
    Shadow,
    Suggest,
    ActWithApproval,
    ActAuto,
}

impl From<TrustTierArg> for harn_vm::AutonomyTier {
    fn from(value: TrustTierArg) -> Self {
        match value {
            TrustTierArg::Shadow => harn_vm::AutonomyTier::Shadow,
            TrustTierArg::Suggest => harn_vm::AutonomyTier::Suggest,
            TrustTierArg::ActWithApproval => harn_vm::AutonomyTier::ActWithApproval,
            TrustTierArg::ActAuto => harn_vm::AutonomyTier::ActAuto,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum TrustOutcomeArg {
    Success,
    Failure,
    Denied,
    Timeout,
}

impl From<TrustOutcomeArg> for harn_vm::TrustOutcome {
    fn from(value: TrustOutcomeArg) -> Self {
        match value {
            TrustOutcomeArg::Success => harn_vm::TrustOutcome::Success,
            TrustOutcomeArg::Failure => harn_vm::TrustOutcome::Failure,
            TrustOutcomeArg::Denied => harn_vm::TrustOutcome::Denied,
            TrustOutcomeArg::Timeout => harn_vm::TrustOutcome::Timeout,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct TrustQueryArgs {
    /// Filter by agent id.
    #[arg(long)]
    pub agent: Option<String>,
    /// Filter by action class.
    #[arg(long)]
    pub action: Option<String>,
    /// Include records at or after this RFC3339/unix timestamp.
    #[arg(long)]
    pub since: Option<String>,
    /// Include records at or before this RFC3339/unix timestamp.
    #[arg(long)]
    pub until: Option<String>,
    /// Filter by autonomy tier.
    #[arg(long, value_enum)]
    pub tier: Option<TrustTierArg>,
    /// Filter by trust outcome.
    #[arg(long, value_enum)]
    pub outcome: Option<TrustOutcomeArg>,
    /// Limit results to the newest N matching records.
    #[arg(long)]
    pub limit: Option<usize>,
    /// Group results into trace buckets instead of returning a flat list.
    #[arg(long)]
    pub grouped_by_trace: bool,
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
    /// Summarize records per agent.
    #[arg(long)]
    pub summary: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TrustVerifyChainArgs {
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TrustPromoteArgs {
    /// Agent id to promote.
    pub agent: String,
    /// Target autonomy tier.
    #[arg(long, value_enum)]
    pub to: TrustTierArg,
}

#[derive(Debug, Args)]
pub(crate) struct TrustDemoteArgs {
    /// Agent id to demote.
    pub agent: String,
    /// Target autonomy tier.
    #[arg(long, value_enum)]
    pub to: TrustTierArg,
    /// Reason for the demotion.
    #[arg(long)]
    pub reason: String,
}
