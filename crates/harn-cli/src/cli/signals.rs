use std::path::PathBuf;
use std::time::Duration as StdDuration;

use clap::{Args, Subcommand, ValueEnum};

use super::util::parse_duration_arg;

#[derive(Debug, Args)]
pub(crate) struct SignalsArgs {
    /// Inbox directory. Defaults to $HARN_EXT_SIGNALS_DIR, then ~/.harn/signals.
    #[arg(long, global = true, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    #[command(subcommand)]
    pub command: SignalsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SignalsCommand {
    /// List every signal with its state and delivery.
    List(SignalsOutputArgs),
    /// List signals that still owe a reminder, optionally for one session.
    Pending(SignalsPendingArgs),
    /// List registered agent sessions.
    Sessions(SignalsOutputArgs),
    /// Fire a signal by hand, or from a `harn.triage_event.v1` JSON document.
    Fire(Box<SignalsFireArgs>),
    /// Pace captured monitor output at safe host boundaries.
    Dispatch(SignalsDispatchArgs),
    /// Resolve a fired signal.
    Resolve(SignalsResolveArgs),
    /// Ack a leased reminder, so it is not redelivered.
    Ack(SignalsAckArgs),
    /// Run as an agent-harness hook: check the session in, print newly leased
    /// reminders in the harness's hook format, and ack what it printed.
    Hook(SignalsHookArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SignalsOutputArgs {
    /// Emit a JSON envelope.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SignalsPendingArgs {
    /// Only reminders routed to this session.
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    /// Emit a JSON envelope.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SignalsFireArgs {
    /// Active monitor execution epoch established by dispatch start.
    #[arg(long, requires_all = ["event_id", "session"])]
    pub execution: Option<String>,
    /// Capture a distinct occurrence instead of updating a condition. Use a
    /// stable file cursor or producer sequence; identical text may recur.
    #[arg(long, requires = "session")]
    pub event_id: Option<String>,
    /// Exact recipient for a captured occurrence; no repository fallback.
    #[arg(long, requires = "event_id")]
    pub session: Option<String>,
    /// Read a `harn.triage_event.v1` document from this file, or `-` for stdin.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["summary", "source_url"])]
    pub event: Option<PathBuf>,
    /// The condition's identity; firing it again while fired changes nothing.
    /// Defaults to the event's dedupe_key.
    #[arg(long)]
    pub key: Option<String>,
    /// What happened.
    #[arg(long, required_unless_present = "event")]
    pub summary: Option<String>,
    /// Where the condition can be checked.
    #[arg(long, required_unless_present = "event")]
    pub source_url: Option<String>,
    /// Why it deserves attention.
    #[arg(long, default_value = "")]
    pub why: String,
    /// The suggested next step.
    #[arg(
        long,
        default_value = "Check the source and decide whether it affects this work."
    )]
    pub action: String,
    #[arg(long, default_value = "manual")]
    pub provider: String,
    #[arg(long, default_value = "manual")]
    pub kind: String,
    #[arg(long, value_enum, default_value_t = SignalSeverityArg::Warning)]
    pub severity: SignalSeverityArg,
    /// Who is firing the signal; shown beside the reminder.
    #[arg(long, default_value = "harn signals fire")]
    pub producer: String,
    /// Whether the alert's origin was verified: `local` for a condition
    /// observed on this machine, `verified` for a webhook whose signature was
    /// checked, `unverified` otherwise.
    #[arg(long, value_enum, default_value_t = SignalSignatureArg::Local)]
    pub signature: SignalSignatureArg,
    /// Route to sessions in this repository (`owner/name`).
    #[arg(long, value_name = "OWNER/NAME")]
    pub repo: Option<String>,
    /// Route to sessions working inside this directory.
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<PathBuf>,
    #[arg(long)]
    pub topic: Option<String>,
    /// Expire the signal if it is still fired after this long, e.g. `4h`.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration_arg)]
    pub expires_in: Option<StdDuration>,
    /// Emit a JSON envelope.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SignalsResolveArgs {
    pub key: String,
    /// Replace the summary with what recovered.
    #[arg(long)]
    pub summary: Option<String>,
    /// How the recovery report was checked. It may not be weaker than the
    /// signal's own signature.
    #[arg(long, value_enum, default_value_t = SignalSignatureArg::Local)]
    pub signature: SignalSignatureArg,
    /// Emit a JSON envelope.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SignalsAckArgs {
    pub key: String,
    /// The generation to ack. Defaults to the signal's current generation.
    #[arg(long)]
    pub generation: Option<u64>,
    /// Only ack if this session was leased it.
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SignalsHookArgs {
    #[arg(long, value_enum)]
    pub format: SignalsHookFormat,
    /// The hook event this invocation is registered for.
    #[arg(long, value_enum)]
    pub event: SignalsHookEvent,
    /// Register the session under this routing role, e.g. `control`.
    /// Defaults to $HARN_EXT_SIGNALS_ROLE.
    #[arg(long)]
    pub role: Option<String>,
    /// With `--event Stop`: wait in the background for an urgent reminder and
    /// exit 2 with a fixed notice, which wakes an idle Claude Code session
    /// registered with `asyncRewake`; the reminder follows through the woken
    /// turn's `Stop` hook. One watcher runs per session; extra ones exit at
    /// once.
    #[arg(long)]
    pub rewake: bool,
    /// How long `--rewake` waits before exiting quietly. Keep it under the
    /// hook's `timeout`, which defaults to 600 seconds.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration_arg, default_value = "9m")]
    pub max_wait: StdDuration,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum SignalSeverityArg {
    Info,
    Warning,
    Urgent,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum SignalSignatureArg {
    Local,
    Verified,
    Unverified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum SignalsHookFormat {
    ClaudeCode,
    Codex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum SignalsHookEvent {
    #[value(name = "SessionStart", alias = "session-start")]
    SessionStart,
    #[value(name = "UserPromptSubmit", alias = "user-prompt-submit")]
    UserPromptSubmit,
    #[value(name = "PostToolUse", alias = "post-tool-use")]
    PostToolUse,
    #[value(name = "Stop", alias = "stop")]
    Stop,
    #[value(name = "SessionEnd", alias = "session-end")]
    SessionEnd,
}

#[derive(Debug, Args)]
pub(crate) struct SignalsDispatchArgs {
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub execution: String,
    #[command(subcommand)]
    pub command: SignalsDispatchCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SignalsDispatchCommand {
    /// Start an epoch; repeating it preserves its durable pacing state.
    Start {
        #[arg(long)]
        interval_seconds: Option<u32>,
    },
    /// Claim one due batch only when the host can accept it safely.
    Claim,
    /// Commit idempotent durable host acceptance, before model dispatch.
    Accept {
        #[arg(long)]
        batch: String,
        #[arg(long)]
        token: String,
    },
    /// Cancel a monitor, or close the epoch when no monitor is given.
    Cancel {
        #[arg(long)]
        monitor: Option<String>,
    },
}
