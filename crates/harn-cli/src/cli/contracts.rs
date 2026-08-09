use clap::{ArgAction, Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct ContractsArgs {
    #[command(subcommand)]
    pub command: ContractsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ContractsCommand {
    /// Export builtin registry metadata.
    Builtins(ContractsOutputArgs),
    /// Export the effective host capability manifest used for preflight.
    HostCapabilities(ContractsHostCapabilitiesArgs),
    /// Export a bundle manifest for one or more pipelines and optionally verify it.
    Bundle(ContractsBundleArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ContractsOutputArgs {
    /// Pretty-print JSON output.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub pretty: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ContractsHostCapabilitiesArgs {
    /// Extra host capability schema to merge into the default manifest.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Pretty-print JSON output.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub pretty: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ContractsBundleArgs {
    /// Extra host capability schema for bundle contract validation.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Alternate root for render/template path checks.
    #[arg(long = "bundle-root")]
    pub bundle_root: Option<String>,
    /// Fail if the selected targets do not pass Harn preflight validation.
    #[arg(long)]
    pub verify: bool,
    /// Pretty-print JSON output.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub pretty: bool,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}
