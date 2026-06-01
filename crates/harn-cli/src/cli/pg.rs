use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct PgArgs {
    #[command(subcommand)]
    pub command: PgCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PgCommand {
    /// Generate Harn record types from a directory of `.sql` migrations.
    ///
    /// Replays every forward migration (`.sql`, excluding `*.down.sql`) in
    /// lexicographic order and emits one `type <Table>Row = {…}` per table,
    /// mirroring the live column set. Annotate a query result with the
    /// generated type to type-check field access without a live database:
    ///
    /// ```harn
    /// let receipt: ReceiptRow = pg_query_one(pool, sql, params)
    /// ```
    ///
    /// Pass `--check` to verify the `--out` file is current (for CI).
    Codegen(PgCodegenArgs),
}

#[derive(Debug, Args)]
pub(crate) struct PgCodegenArgs {
    /// Directory holding `.sql` migration files.
    #[arg(long, default_value = "migrations", value_name = "DIR")]
    pub dir: PathBuf,
    /// Write the generated types to this file. Prints to stdout when omitted.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
    /// Verify `--out` is up to date without writing; exit non-zero on drift.
    #[arg(long, requires = "out")]
    pub check: bool,
    /// Suffix appended to each PascalCase table name (default `Row`).
    #[arg(long, value_name = "SUFFIX")]
    pub suffix: Option<String>,
}
