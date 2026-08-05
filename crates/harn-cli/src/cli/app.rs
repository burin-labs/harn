use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct AppArgs {
    #[command(subcommand)]
    pub command: AppCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AppCommand {
    /// Run a script-declared MCP App in Harn's local browser host.
    Run(AppRunArgs),
}

#[derive(Debug, Args)]
pub(crate) struct AppRunArgs {
    /// Harn script that registers tools and at least one MCP App resource.
    pub file: String,

    /// UI resource URI to open when the script declares more than one.
    #[arg(long, value_name = "UI_URI")]
    pub resource: Option<String>,

    /// Loopback address for the local host. Port zero chooses a free port.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:0")]
    pub bind: SocketAddr,

    /// Print the URL without opening the default browser.
    #[arg(long = "no-open", action = clap::ArgAction::SetFalse, default_value_t = true)]
    pub open: bool,
}

impl Default for AppRunArgs {
    fn default() -> Self {
        Self {
            file: String::new(),
            resource: None,
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            open: true,
        }
    }
}
