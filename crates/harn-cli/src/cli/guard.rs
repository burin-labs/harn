use clap::{Args, Subcommand};

/// `harn guard` — manage downloadable on-device injection-detection models.
#[derive(Debug, Args)]
pub(crate) struct GuardArgs {
    #[command(subcommand)]
    pub command: GuardCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum GuardCommand {
    /// List installed models, and (with --catalog) the models available to install.
    List(GuardListArgs),
    /// Download and install a model from the catalog into `~/.harn/guard/`.
    Install(GuardInstallArgs),
    /// Show install status for a model (or all models).
    Status(GuardStatusArgs),
    /// Remove an installed model from `~/.harn/guard/`.
    Remove(GuardRemoveArgs),
}

#[derive(Debug, Args)]
pub(crate) struct GuardListArgs {
    /// Also list the models available to install from the built-in catalog.
    #[arg(long)]
    pub catalog: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GuardInstallArgs {
    /// Catalog model name to install. Defaults to the recommended model.
    pub model: Option<String>,
    /// Accept the model's upstream license. Required — nothing is downloaded
    /// without it (the license text is printed for review first).
    #[arg(long)]
    pub accept_license: bool,
    /// Re-download and overwrite an already-installed model.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct GuardStatusArgs {
    /// Catalog model name. Omit to show every installed model.
    pub model: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct GuardRemoveArgs {
    /// Catalog model name to remove.
    pub model: String,
}
