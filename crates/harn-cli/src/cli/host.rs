use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub(crate) struct HostArgs {
    #[command(subcommand)]
    pub command: HostCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HostCommand {
    /// Acquire, renew, release, and inspect machine-global host leases.
    Lease(HostLeaseArgs),
}

#[derive(Debug, Args)]
pub(crate) struct HostLeaseArgs {
    #[command(subcommand)]
    pub command: HostLeaseCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HostLeaseCommand {
    /// Acquire an exclusive lease, optionally waiting for its current owner.
    Acquire(HostLeaseAcquireArgs),
    /// Extend the finite expiry on a lease owned by this token.
    Renew(HostLeaseRenewArgs),
    /// Release a lease owned by this token.
    Release(HostLeaseReleaseArgs),
    /// Inspect the current lease for a host.
    Status(HostLeaseStatusArgs),
    /// Run a typed workload while its resource lease is actively supervised.
    Run(HostLeaseRunArgs),
    #[command(hide = true)]
    RunCargoWorker(HostLeaseRunCargoWorkerArgs),
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum HostLeasePriorityArg {
    Interactive,
    Measurement,
    CiVerify,
    #[default]
    Deferrable,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum HostLeaseResourceClassArg {
    #[default]
    WholeMachine,
    RustHeavy,
}

#[derive(Debug, Args)]
pub(crate) struct HostLeaseAcquireArgs {
    /// Host resource name. Defaults to the current machine's hostname.
    #[arg(long)]
    pub host: Option<String>,
    /// Capacity-one resource class on the host.
    #[arg(long, value_enum, default_value_t)]
    pub resource_class: HostLeaseResourceClassArg,
    /// Stable owner identity written into receipts.
    #[arg(long)]
    pub owner: String,
    /// Scheduling class recorded with the lease.
    #[arg(long, value_enum, default_value_t)]
    pub priority_class: HostLeasePriorityArg,
    /// Lease lifetime in milliseconds. Ignored with --no-expiry.
    #[arg(long, default_value_t = 900_000, conflicts_with = "no_expiry")]
    pub ttl_ms: u64,
    /// Keep the lease until explicit release or owner-process death.
    #[arg(long)]
    pub no_expiry: bool,
    /// PID whose process identity proves liveness. Required with --no-expiry.
    #[arg(long)]
    pub owner_pid: Option<u32>,
    /// Maximum event-driven contention wait in milliseconds.
    #[arg(long, default_value_t = 0)]
    pub wait_ms: u64,
    /// Human-readable reason carried as receipt metadata.
    #[arg(long)]
    pub reason: Option<String>,
    /// Emit a structured JSON envelope.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct HostLeaseRenewArgs {
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long, value_enum, default_value_t)]
    pub resource_class: HostLeaseResourceClassArg,
    #[arg(long)]
    pub lease_id: String,
    #[arg(long, default_value_t = 900_000)]
    pub ttl_ms: u64,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct HostLeaseReleaseArgs {
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long, value_enum, default_value_t)]
    pub resource_class: HostLeaseResourceClassArg,
    #[arg(long)]
    pub lease_id: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct HostLeaseStatusArgs {
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long, value_enum, default_value_t)]
    pub resource_class: HostLeaseResourceClassArg,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct HostLeaseRunArgs {
    #[command(subcommand)]
    pub command: HostLeaseRunCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HostLeaseRunCommand {
    /// Run Cargo behind a capacity-one rust-heavy lease.
    Cargo(HostLeaseRunCargoArgs),
}

#[derive(Debug, Args)]
pub(crate) struct HostLeaseRunCargoArgs {
    /// Stable owner identity stored in the lease receipt.
    #[arg(long)]
    pub owner: String,
    /// Host resource name. Defaults to the current machine's hostname.
    #[arg(long)]
    pub host: Option<String>,
    /// Maximum event-driven wait for the rust-heavy resource.
    #[arg(long, default_value_t = 0)]
    pub wait_ms: u64,
    /// Cargo workspace root.
    #[arg(long)]
    pub workspace: PathBuf,
    /// Per-worktree Cargo target directory.
    #[arg(long)]
    pub target_dir: PathBuf,
    /// Cargo intermediate build directory.
    #[arg(long)]
    pub build_dir: Option<PathBuf>,
    /// Cargo arguments, passed after `--`.
    #[arg(last = true, required = true)]
    pub cargo_args: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct HostLeaseRunCargoWorkerArgs {
    #[arg(long)]
    pub workspace: PathBuf,
    #[arg(long)]
    pub target_dir: PathBuf,
    #[arg(long)]
    pub build_dir: Option<PathBuf>,
    #[arg(long)]
    pub run_id: String,
    #[arg(last = true, required = true)]
    pub cargo_args: Vec<String>,
}
