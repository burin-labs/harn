use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use super::util::trigger_provider_completion_parser;

#[derive(Debug, Args)]
pub(crate) struct PersonaArgs {
    #[command(subcommand)]
    pub command: PersonaCommand,
    /// Explicit harn.toml path or directory. Defaults to nearest harn.toml from cwd.
    #[arg(long, global = true, value_name = "PATH")]
    pub manifest: Option<PathBuf>,
    /// Directory used for durable persona runtime state and event-log data.
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        default_value = ".harn/personas"
    )]
    pub state_dir: PathBuf,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PersonaCommand {
    /// Scaffold a new Harn-first persona package from a template.
    New(PersonaNewArgs),
    /// Compile a natural-language request into a closed persona blueprint.
    CompilePrompt(PersonaCompilePromptArgs),
    /// Materialize a closed persona blueprint through the canonical scaffold transaction.
    Materialize(PersonaMaterializeArgs),
    /// Validate a persona package end-to-end.
    Doctor(PersonaDoctorArgs),
    /// Validate a persona manifest with the canonical harn-modules schema.
    Check(PersonaCheckArgs),
    /// List personas declared in the resolved harn.toml.
    List(PersonaListArgs),
    /// Inspect one persona from the resolved harn.toml.
    Inspect(PersonaInspectArgs),
    /// Activate an installed package persona with optional authority attenuation.
    Activate(PersonaActivateArgs),
    /// Deactivate an installed package persona for this project.
    Deactivate(PersonaDeactivateArgs),
    /// List project-scoped installed persona activations.
    Activations(PersonaActivationsArgs),
    /// Query durable persona lifecycle, lease, budget, and queue status.
    Status(PersonaStatusArgs),
    /// Pause a persona; matching events queue until resume drains them.
    Pause(PersonaControlArgs),
    /// Resume a persona and drain queued events once under leases.
    Resume(PersonaControlArgs),
    /// Disable a persona; matching events are recorded as dead-lettered.
    Disable(PersonaControlArgs),
    /// Fire a synthetic schedule tick for a persona.
    Tick(PersonaTickArgs),
    /// Fire a synthetic external trigger envelope for a persona.
    Trigger(PersonaTriggerArgs),
    /// Record an expensive-work budget receipt for a persona.
    Spend(PersonaSpendArgs),
    /// Stream the local universal persona supervision feed.
    Supervision(PersonaSupervisionArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PersonaNewArgs {
    /// Persona package name, for example `my_release_captain`.
    #[arg(
        required_unless_present = "from_prompt",
        conflicts_with = "from_prompt"
    )]
    pub name: Option<String>,
    /// Persona control-flow template.
    #[arg(
        long,
        value_enum,
        required_unless_present = "from_prompt",
        conflicts_with = "from_prompt"
    )]
    pub template: Option<PersonaTemplateKind>,
    /// Compile a closed persona package from one natural-language request.
    #[arg(long, value_name = "PROMPT")]
    pub from_prompt: Option<String>,
    /// Override the model-proposed persona name in prompt mode.
    #[arg(long = "name", requires = "from_prompt")]
    pub prompt_name: Option<String>,
    /// LLM provider for prompt compilation; normal Harn routing applies when omitted.
    #[arg(long, requires = "from_prompt")]
    pub provider: Option<String>,
    /// LLM model or alias for prompt compilation.
    #[arg(long, requires = "from_prompt")]
    pub model: Option<String>,
    /// Maximum generated tokens for prompt compilation.
    #[arg(
        long,
        requires = "from_prompt",
        value_parser = clap::value_parser!(u32).range(1..=1200)
    )]
    pub max_tokens: Option<u32>,
    /// Directory under which the persona package is created.
    #[arg(long, value_name = "DIR", default_value = "personas")]
    pub output_root: PathBuf,
    /// Replace an existing generated package.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaCompilePromptArgs {
    /// Natural-language proactive-agent request.
    #[arg(long, value_name = "PROMPT")]
    pub prompt: String,
    /// Override the model-proposed persona name.
    #[arg(long)]
    pub name: Option<String>,
    /// LLM provider; normal Harn routing applies when omitted.
    #[arg(long)]
    pub provider: Option<String>,
    /// LLM model or alias.
    #[arg(long)]
    pub model: Option<String>,
    /// Maximum generated tokens. The compiler never permits more than 1200.
    #[arg(
        long,
        default_value_t = 512,
        value_parser = clap::value_parser!(u32).range(1..=1200)
    )]
    pub max_tokens: u32,
    /// Emit the complete typed compiler receipt as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaMaterializeArgs {
    /// Path to a JSON persona blueprint compiled by Harn at the materialization boundary.
    #[arg(
        long,
        value_name = "JSON",
        required_unless_present = "compile_receipt",
        conflicts_with = "compile_receipt"
    )]
    pub blueprint: Option<PathBuf>,
    /// Path to an accepted compile-prompt receipt reviewed before materialization.
    #[arg(
        long,
        value_name = "JSON",
        required_unless_present = "blueprint",
        conflicts_with = "blueprint"
    )]
    pub compile_receipt: Option<PathBuf>,
    /// Directory under which the persona package is created.
    #[arg(long, value_name = "DIR", default_value = "personas")]
    pub output_root: PathBuf,
    /// Replace an existing generated package after strict validation succeeds.
    #[arg(long)]
    pub force: bool,
    /// Install and activate an accepted compile receipt in the selected project.
    #[arg(
        long,
        requires_all = ["compile_receipt", "manifest", "json"],
        conflicts_with = "blueprint"
    )]
    pub activate: bool,
    /// Emit the typed apply receipt as JSON.
    #[arg(long, requires = "activate")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaDoctorArgs {
    /// Persona name or package directory to validate.
    pub name: String,
    /// Emit a stable JSON report instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
    /// Test timeout for the smoke suite.
    #[arg(long, default_value_t = 10_000)]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum PersonaTemplateKind {
    #[value(name = "deterministic-sweeper")]
    DeterministicSweeper,
    #[value(name = "hybrid-classify-then-act")]
    HybridClassifyThenAct,
    #[value(name = "frontier-judgment-loop")]
    FrontierJudgmentLoop,
}

impl PersonaTemplateKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DeterministicSweeper => "deterministic-sweeper",
            Self::HybridClassifyThenAct => "hybrid-classify-then-act",
            Self::FrontierJudgmentLoop => "frontier-judgment-loop",
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct PersonaCheckArgs {
    /// Persona manifest, harn.toml path, or directory containing harn.toml.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Emit typed validation errors as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaListArgs {
    /// Emit a stable JSON array instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaInspectArgs {
    /// Persona name to inspect.
    pub name: String,
    /// Emit stable JSON for cloud platforms or other hosts.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaActivateArgs {
    /// Qualified installed persona ID, for example `agents/reviewer`.
    pub name: String,
    /// Lower autonomy ceiling. Omit to inherit the exported tier.
    #[arg(
        long,
        value_name = "TIER",
        value_parser = ["shadow", "suggest", "act_with_approval", "act_auto"]
    )]
    pub autonomy_tier: Option<String>,
    /// Retain one exported tool. Repeat to retain multiple tools.
    #[arg(long = "tool", value_name = "NAME", conflicts_with = "no_tools")]
    pub tools: Vec<String>,
    /// Attenuate the persona to no tools.
    #[arg(long, conflicts_with = "tools")]
    pub no_tools: bool,
    /// Retain one exported capability. Repeat to retain multiple capabilities.
    #[arg(
        long = "capability",
        value_name = "NAME",
        conflicts_with = "no_capabilities"
    )]
    pub capabilities: Vec<String>,
    /// Attenuate the persona to no capabilities.
    #[arg(long, conflicts_with = "capabilities")]
    pub no_capabilities: bool,
    /// RFC3339 timestamp to use for deterministic receipts.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit a stable JSON receipt.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaDeactivateArgs {
    /// Qualified installed persona ID, for example `agents/reviewer`.
    pub name: String,
    /// RFC3339 timestamp to use for deterministic receipts.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit a stable JSON receipt.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaActivationsArgs {
    /// Emit a stable JSON array.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaStatusArgs {
    /// Persona name to query.
    pub name: String,
    /// RFC3339 timestamp to use for deterministic budget windows. When
    /// omitted, falls back to the current UTC wall clock.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit stable JSON for cloud platforms or other hosts.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaControlArgs {
    /// Persona name to control.
    pub name: String,
    /// RFC3339 timestamp to use as "now" for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit stable JSON after applying the control.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaTickArgs {
    /// Persona name to wake from its schedule binding.
    pub name: String,
    /// RFC3339 timestamp to use for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Estimated expensive-work cost for budget enforcement.
    #[arg(long, default_value_t = 0.0)]
    pub cost_usd: f64,
    /// Estimated token count for budget enforcement.
    #[arg(long, default_value_t = 0)]
    pub tokens: u64,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaTriggerArgs {
    /// Persona name to wake from an external trigger.
    pub name: String,
    /// Provider name, for example github, linear, slack, or webhook.
    #[arg(
        long,
        value_parser = trigger_provider_completion_parser(),
        hide_possible_values = true
    )]
    pub provider: String,
    /// Provider event kind, for example pull_request, check_run, issue, or message.
    #[arg(long)]
    pub kind: String,
    /// Normalized metadata as key=value. Repeat for multiple fields.
    #[arg(long = "metadata", value_name = "KEY=VALUE")]
    pub metadata: Vec<String>,
    /// RFC3339 timestamp to use for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Estimated expensive-work cost for budget enforcement.
    #[arg(long, default_value_t = 0.0)]
    pub cost_usd: f64,
    /// Estimated token count for budget enforcement.
    #[arg(long, default_value_t = 0)]
    pub tokens: u64,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaSpendArgs {
    /// Persona name to charge.
    pub name: String,
    /// Cost in USD to record.
    #[arg(long)]
    pub cost_usd: f64,
    /// Tokens to record.
    #[arg(long, default_value_t = 0)]
    pub tokens: u64,
    /// RFC3339 timestamp to use for deterministic tests.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Emit stable JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PersonaSupervisionArgs {
    #[command(subcommand)]
    pub command: PersonaSupervisionCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PersonaSupervisionCommand {
    /// Emit local persona/update frames as newline-delimited JSON.
    Tail(PersonaSupervisionTailArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PersonaSupervisionTailArgs {
    /// Persona name to stream. When omitted, streams every persona in the local state directory.
    #[arg(long)]
    pub persona: Option<String>,
    /// Replay events strictly after this event-log cursor.
    #[arg(long, value_name = "N")]
    pub since_event_id: Option<u64>,
    /// RFC3339 timestamp accepted for deterministic harness parity.
    #[arg(long, value_name = "RFC3339")]
    pub at: Option<String>,
    /// Keep waiting for new events after the current backlog drains.
    #[arg(long)]
    pub follow: bool,
    /// Accepted for host symmetry; output is always newline-delimited JSON.
    #[arg(long)]
    pub json: bool,
    /// Maximum number of emitted supervision frames.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}
