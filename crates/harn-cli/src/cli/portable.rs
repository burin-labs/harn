use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum PortableEntryKindArg {
    Function,
    Pipeline,
}

#[derive(Debug, Args)]
pub(crate) struct PortableArgs {
    #[command(subcommand)]
    pub command: PortableCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PortableCommand {
    /// Compile a source file and its imports into one portable artifact.
    Compile(PortableCompileArgs),
    /// Resolve source imports into the data-only browser package projection.
    Package(PortablePackageArgs),
    /// Start a portable artifact with JSON input and host grants.
    Start(PortableStartArgs),
    /// Resume a suspended artifact with one typed capability result.
    Resume(PortableResumeArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PortablePackageArgs {
    /// Root Harn source file.
    pub source: PathBuf,
    /// Destination for the deterministic source package manifest.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,
    /// Check that the destination already equals the canonical projection.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PortableCompileArgs {
    /// Root Harn source file.
    pub source: PathBuf,
    /// Function or pipeline name to compile.
    #[arg(long, default_value = "main")]
    pub entry: String,
    /// Kind of callable named by --entry.
    #[arg(long = "entry-kind", value_enum, default_value_t = PortableEntryKindArg::Function)]
    pub entry_kind: PortableEntryKindArg,
    /// Destination for the deterministic portable artifact.
    #[arg(short, long, value_name = "PATH")]
    pub output: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct PortableStartArgs {
    /// Portable artifact produced by `harn portable compile`.
    pub artifact: PathBuf,
    /// JSON input file passed to the artifact entry.
    #[arg(long, value_name = "PATH")]
    pub input: PathBuf,
    /// Host grants JSON. Omit for pure execution.
    #[arg(long, value_name = "PATH")]
    pub grants: Option<PathBuf>,
    /// Destination for a snapshot if execution suspends.
    #[arg(long = "snapshot-out", value_name = "PATH")]
    pub snapshot_out: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct PortableResumeArgs {
    /// Portable artifact used to create the snapshot.
    pub artifact: PathBuf,
    /// Snapshot emitted by the previous start or resume step.
    #[arg(long, value_name = "PATH")]
    pub snapshot: PathBuf,
    /// Typed capability result JSON.
    #[arg(long, value_name = "PATH")]
    pub result: PathBuf,
    /// Host grants JSON, including the same snapshot key.
    #[arg(long, value_name = "PATH")]
    pub grants: PathBuf,
    /// Destination for a replacement snapshot if execution suspends again.
    #[arg(long = "snapshot-out", value_name = "PATH")]
    pub snapshot_out: PathBuf,
}
