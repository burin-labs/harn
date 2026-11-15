use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use super::ToolSchemaSurface;

#[derive(Debug, Args)]
pub(crate) struct CodegenArgs {
    #[command(subcommand)]
    pub command: CodegenCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CodegenCommand {
    /// Compile a prepared tool catalog into the portable code-generation IR.
    Ir(CodegenIrArgs),
    /// Validate an IR document and print its normalized JSON without source access.
    ValidateIr(CodegenValidateArgs),
    /// Run a code-generation backend twice and commit its validated artifacts.
    Generate(CodegenGenerateArgs),
}

#[derive(Debug, Args)]
pub(crate) struct CodegenIrArgs {
    /// Harn source file whose exports or script registry supply the contract.
    #[arg(long, value_name = "FILE")]
    pub file: PathBuf,
    #[arg(long, value_enum, default_value_t = ToolSchemaSurface::Exports)]
    pub surface: ToolSchemaSurface,
    /// Select exact operation names; defaults to every portable operation.
    #[arg(long, value_name = "OPERATION")]
    pub select: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct CodegenValidateArgs {
    #[arg(long, value_name = "FILE")]
    pub input: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum CodegenRole {
    Client,
    Server,
    #[default]
    Both,
}

impl From<CodegenRole> for harn_vm::codegen::CodegenRole {
    fn from(value: CodegenRole) -> Self {
        match value {
            CodegenRole::Client => Self::Client,
            CodegenRole::Server => Self::Server,
            CodegenRole::Both => Self::Both,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct CodegenGenerateArgs {
    #[command(flatten)]
    pub source: CodegenIrArgs,
    /// Optional frozen IR; its three digests must match this source compilation.
    #[arg(long, value_name = "FILE")]
    pub ir: Option<PathBuf>,
    /// Installed one-shot backend executable. Receives only the validated request.
    #[arg(long, value_name = "EXECUTABLE")]
    pub plugin: PathBuf,
    /// Read-only backend installation or dependency roots; input source files stay denied.
    #[arg(long, value_name = "DIR")]
    pub plugin_read_root: Vec<PathBuf>,
    #[arg(long, value_enum, default_value_t = CodegenRole::Both)]
    pub role: CodegenRole,
    #[arg(long, value_name = "DIR")]
    pub out: PathBuf,
    /// Compare generated artifacts without changing the output directory.
    #[arg(long)]
    pub check: bool,
}
