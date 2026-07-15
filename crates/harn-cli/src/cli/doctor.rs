use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Emit a single JSON document instead of human-readable output. The
    /// document conforms to the `harn --json-schemas --command doctor`
    /// envelope (`schemaVersion: 2`).
    #[arg(long)]
    pub json: bool,
    /// Actively probe every configured provider over the network
    /// (`/v1/models` GET or the provider's healthcheck endpoint) and
    /// record per-provider reachability and latency. Off by default
    /// because probes add network round-trips.
    #[arg(long)]
    pub check_providers: bool,
    /// Deprecated no-op. `harn doctor` stays offline unless
    /// `--check-providers` is supplied.
    #[arg(long, conflicts_with = "check_providers")]
    pub no_network: bool,
    /// Actively `cargo check --target <triple>` every Rustup-installed
    /// target plus the canonical Linux/macOS/Windows/WASM triples. Off
    /// by default because each probe spawns Cargo.
    #[arg(long)]
    pub check_targets: bool,
}
