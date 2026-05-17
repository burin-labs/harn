use clap::Args;

/// Arguments for `harn explain`.
///
/// `harn explain` dispatches on two forms through a single subcommand:
///
/// 1. **Code form (default).** A registered diagnostic identifier (e.g.
///    `harn explain HARN-TYP-014`) prints the embedded explanation. Pair
///    with `--json` to get the structured envelope an agent or editor can
///    ingest without parsing prose.
///
/// 2. **Legacy invariant form.** When `--invariant <NAME>` is passed (e.g.
///    `harn explain --invariant fs.writes handler path/to/script.harn`)
///    the positional argument is treated as the handler / function /
///    tool / pipeline name and the second positional is the source file,
///    matching the original `harn explain` behaviour for control-flow
///    invariants.
#[derive(Debug, Args)]
pub(crate) struct ExplainArgs {
    /// A Harn diagnostic code (e.g. `HARN-TYP-014`) — or, with
    /// `--invariant`, the handler / function / tool / pipeline name to
    /// inspect.
    pub target: String,
    /// Path to the `.harn` source file (legacy `--invariant` form only).
    pub file: Option<String>,
    /// Explain the control-flow path that violates a Harn invariant
    /// (e.g. `fs.writes`, `approval.reachability`).
    #[arg(long = "invariant", value_name = "NAME")]
    pub invariant: Option<String>,
    /// Emit a structured JSON envelope instead of human-readable text.
    /// Only meaningful in code form.
    #[arg(long = "json", default_value_t = false)]
    pub json: bool,
}
