use clap::{ArgGroup, Args, ValueEnum};

/// Arguments for `harn explain`.
///
/// `harn explain` dispatches on three forms through a single subcommand:
///
/// 1. **Code form (default).** A registered diagnostic identifier (e.g.
///    `harn explain HARN-TYP-014`) prints the embedded explanation. Pair
///    with `--json` to get the structured envelope an agent or editor can
///    ingest without parsing prose.
///
/// 2. **Catalog form.** `harn explain --catalog [--format markdown|json|text]`
///    dumps the entire in-binary registry — the same dispatch surface used
///    to regenerate `docs/src/diagnostics.md` and the
///    `docs/diagnostics-catalog.json` sidecar.
///
/// 3. **Legacy invariant form.** When `--invariant <NAME>` is passed (e.g.
///    `harn explain --invariant fs.writes handler path/to/script.harn`)
///    the positional argument is treated as the handler / function /
///    tool / pipeline name and the second positional is the source file,
///    matching the original `harn explain` behaviour for control-flow
///    invariants.
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("explain_mode")
        .args(["target", "catalog"])
        .required(true)
        .multiple(true)
))]
pub(crate) struct ExplainArgs {
    /// A Harn diagnostic code (e.g. `HARN-TYP-014`) — or, with
    /// `--invariant`, the handler / function / tool / pipeline name to
    /// inspect. Optional when `--catalog` is set.
    pub target: Option<String>,
    /// Path to the `.harn` source file (legacy `--invariant` form only).
    pub file: Option<String>,
    /// Explain the control-flow path that violates a Harn invariant
    /// (e.g. `fs.writes`, `approval.reachability`).
    #[arg(long = "invariant", value_name = "NAME")]
    pub invariant: Option<String>,
    /// Dump the entire diagnostic-code catalog instead of a single code.
    /// Pair with `--format` to choose markdown (default), JSON, or plain
    /// text rendering.
    #[arg(long = "catalog", conflicts_with_all = ["invariant", "json"])]
    pub catalog: bool,
    /// Catalog output format. Only meaningful with `--catalog`.
    #[arg(
        long = "format",
        value_enum,
        default_value_t = CatalogFormat::Markdown,
        requires = "catalog",
    )]
    pub format: CatalogFormat,
    /// Emit a structured JSON envelope instead of human-readable text.
    /// Only meaningful in single-code form; for the catalog use
    /// `--format json`.
    #[arg(long = "json", default_value_t = false)]
    pub json: bool,
}

/// Rendering format selected by `harn explain --catalog --format <…>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CatalogFormat {
    /// Generated mdBook-friendly markdown.
    Markdown,
    /// JSON sidecar with `schemaVersion: 1` and per-code metadata.
    Json,
    /// Plain text suitable for grepping. One line per code.
    Text,
}
