use clap::Args;

/// `harn dev --watch <root>` — incremental developer loop.
///
/// Watches every `.harn` file under `<root>` and only re-typechecks (and
/// optionally re-runs tests in) the modules invalidated by the latest
/// edit. Invalidation is gated by per-module **interface fingerprints**
/// (BLAKE3 of the public surface), so an internal-only change rebuilds
/// just the edited file; an edit to a public signature transitively
/// invalidates everything that imports it.
#[derive(Debug, Args)]
pub(crate) struct DevArgs {
    /// Watch the filesystem for `.harn` changes and re-run incrementally.
    /// Currently the only supported mode — passing this flag is required
    /// so the surface stays open for future non-watch dev sub-modes
    /// without re-litigating the default.
    #[arg(long)]
    pub watch: bool,
    /// Emit a newline-delimited JSON event stream on stdout. Each line
    /// is a single `JsonEnvelope`-wrapped event (`ready`,
    /// `fingerprint_changed`, `rerun`, `diagnostics`). See
    /// `harn --json-schemas --command dev` for the catalog entry.
    #[arg(long)]
    pub json: bool,
    /// Run tests (`test_*` pipelines / `@test`-attributed pipelines) in
    /// every invalidated module after type-checking succeeds. Off by
    /// default to keep the inner loop fast; flip on once you want a
    /// full red/green watch.
    #[arg(long = "with-tests")]
    pub with_tests: bool,
    /// Test timeout per module when `--with-tests` is set.
    #[arg(long = "test-timeout-ms", default_value_t = 10_000, value_name = "MS")]
    pub test_timeout_ms: u64,
    /// Project root to watch. Defaults to the current working
    /// directory.
    #[arg(value_name = "ROOT")]
    pub root: Option<String>,
}
