use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct ExplainArgs {
    /// The invariant name to explain, e.g. `fs.writes`.
    #[arg(long = "invariant", value_name = "NAME")]
    pub invariant: String,
    /// The handler / function / tool / pipeline name to inspect.
    pub function: String,
    /// Path to the `.harn` source file.
    pub file: String,
}
