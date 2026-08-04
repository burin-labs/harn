use clap::Args;

#[derive(Debug, Args)]
pub(crate) struct ParseArgs {
    /// Harn source file to parse.
    pub path: String,

    /// Emit the parsed AST as a versioned JSON envelope.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TokensArgs {
    /// Harn source file to tokenize.
    pub path: String,

    /// Emit tokens as a versioned JSON envelope.
    #[arg(long)]
    pub json: bool,
}
