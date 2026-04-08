use clap::{ArgAction, Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "harn",
    about = "The agent harness language",
    version,
    disable_help_subcommand = false,
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Execute a .harn file or an inline expression.
    Run(RunArgs),
    /// Type-check .harn files or directories without executing them.
    Check(CheckArgs),
    /// Lint .harn files or directories for common issues.
    Lint(PathTargetsArgs),
    /// Format .harn files or directories.
    Fmt(FmtArgs),
    /// Run user tests or the conformance suite.
    Test(TestArgs),
    /// Scaffold a new project with harn.toml.
    Init(InitArgs),
    /// Serve a .harn agent over HTTP using A2A.
    Serve(ServeArgs),
    /// Start the ACP server on stdio.
    Acp(AcpArgs),
    /// Expose a .harn tool bundle as an MCP server on stdio.
    McpServe(McpServeArgs),
    /// Manage remote MCP OAuth credentials and status.
    Mcp(McpArgs),
    /// Watch a .harn file and re-run it on changes.
    Watch(WatchArgs),
    /// Launch the local Harn observability portal.
    Portal(PortalArgs),
    /// Inspect persisted workflow run records.
    Runs(RunsArgs),
    /// Replay a persisted workflow run record.
    Replay(ReplayArgs),
    /// Evaluate a run record, run directory, or eval manifest.
    Eval(EvalArgs),
    /// Start the interactive REPL.
    Repl,
    /// Install dependencies declared in harn.toml.
    Install,
    /// Add a dependency to harn.toml.
    Add(AddArgs),
    /// Print the decorated version banner.
    Version,
    /// Regenerate docs/theme/harn-keywords.js from the live lexer + stdlib sets.
    ///
    /// Dev-only. Hidden from `--help` — invoke via
    /// `cargo run -p harn-cli -- dump-highlight-keywords` or the
    /// `make gen-highlight` target.
    #[command(hide = true, name = "dump-highlight-keywords")]
    DumpHighlightKeywords(DumpHighlightKeywordsArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    /// Print the LLM trace summary after execution.
    #[arg(long)]
    pub trace: bool,
    /// Deny specific builtins as a comma-separated list.
    #[arg(long, conflicts_with = "allow")]
    pub deny: Option<String>,
    /// Allow only the listed builtins as a comma-separated list.
    #[arg(long, conflicts_with = "deny")]
    pub allow: Option<String>,
    /// Evaluate inline Harn code instead of a file.
    #[arg(short = 'e')]
    pub eval: Option<String>,
    /// Path to the .harn file to execute.
    pub file: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    /// Extra host capability schema for preflight validation.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Alternate root for render/template path checks.
    #[arg(long = "bundle-root")]
    pub bundle_root: Option<String>,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PathTargetsArgs {
    /// Automatically apply safe fixes.
    #[arg(long)]
    pub fix: bool,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct FmtArgs {
    /// Check formatting without rewriting files.
    #[arg(long)]
    pub check: bool,
    /// Maximum line width before wrapping.
    #[arg(long = "line-width", default_value_t = 100)]
    pub line_width: usize,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct TestArgs {
    /// Only run tests whose names or paths contain this pattern.
    #[arg(long)]
    pub filter: Option<String>,
    /// Write a JUnit XML report to this path.
    #[arg(long)]
    pub junit: Option<String>,
    /// Per-test timeout in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    pub timeout: u64,
    /// Run user tests concurrently where supported.
    #[arg(long)]
    pub parallel: bool,
    /// Re-run user tests when watched files change.
    #[arg(long)]
    pub watch: bool,
    /// Show per-test timing and detailed failures.
    #[arg(short = 'v', long = "verbose", action = ArgAction::SetTrue)]
    pub verbose: bool,
    /// Record LLM fixtures to .harn-fixtures/.
    #[arg(long)]
    pub record: bool,
    /// Replay LLM fixtures from .harn-fixtures/.
    #[arg(long)]
    pub replay: bool,
    /// User test path, or `conformance` to target the conformance suite.
    pub target: Option<String>,
    /// Optional file or directory under conformance/ when target is `conformance`.
    pub selection: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    /// Optional project name to scaffold.
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ServeArgs {
    /// Port to bind the A2A server to.
    #[arg(long, default_value_t = 8080)]
    pub port: u16,
    /// Path to the .harn file to serve.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct AcpArgs {
    /// Optional pipeline to expose through ACP.
    pub pipeline: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct McpServeArgs {
    /// Path to the .harn file that defines the MCP surface.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Log in to a remote MCP server via OAuth.
    Login(McpLoginArgs),
    /// Remove a stored OAuth token.
    Logout(McpServerRefArgs),
    /// Show stored OAuth status for a server.
    Status(McpServerRefArgs),
    /// Print the default OAuth redirect URI.
    RedirectUri,
}

#[derive(Debug, Args)]
pub(crate) struct McpLoginArgs {
    /// MCP server name from harn.toml or a direct URL.
    pub target: Option<String>,
    /// Explicit server URL for ad hoc login or status checks.
    #[arg(long)]
    pub url: Option<String>,
    /// Explicit OAuth client ID.
    #[arg(long = "client-id")]
    pub client_id: Option<String>,
    /// Explicit OAuth client secret.
    #[arg(long = "client-secret")]
    pub client_secret: Option<String>,
    /// Requested OAuth scope string.
    #[arg(long = "scope")]
    pub scope: Option<String>,
    /// OAuth redirect URI for the local callback listener.
    #[arg(
        long = "redirect-uri",
        default_value = "http://127.0.0.1:9783/oauth/callback"
    )]
    pub redirect_uri: String,
}

#[derive(Debug, Args)]
pub(crate) struct McpServerRefArgs {
    /// MCP server name from harn.toml or a direct URL.
    pub target: Option<String>,
    /// Explicit server URL for ad hoc login or status checks.
    #[arg(long)]
    pub url: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct WatchArgs {
    /// Deny specific builtins as a comma-separated list.
    #[arg(long, conflicts_with = "allow")]
    pub deny: Option<String>,
    /// Allow only the listed builtins as a comma-separated list.
    #[arg(long, conflicts_with = "deny")]
    pub allow: Option<String>,
    /// Path to the .harn file to watch.
    pub file: String,
}

#[derive(Debug, Args)]
pub(crate) struct PortalArgs {
    /// Directory containing persisted run records.
    #[arg(long, default_value = ".harn-runs")]
    pub dir: String,
    /// Host interface to bind.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
    /// Port to serve the portal on.
    #[arg(long, default_value_t = 4721)]
    pub port: u16,
    /// Open the portal in a browser after starting.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub open: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RunsArgs {
    #[command(subcommand)]
    pub command: RunsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RunsCommand {
    /// Inspect a persisted run record and optionally diff it against another.
    Inspect(RunsInspectArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RunsInspectArgs {
    /// Path to the run record JSON file.
    pub path: String,
    /// Optional baseline run record to diff against.
    #[arg(long)]
    pub compare: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ReplayArgs {
    /// Path to the run record JSON file.
    pub path: String,
}

#[derive(Debug, Args)]
pub(crate) struct EvalArgs {
    /// Run record path, run directory, or eval manifest path.
    pub path: String,
    /// Optional baseline run record for diffing.
    #[arg(long)]
    pub compare: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct DumpHighlightKeywordsArgs {
    /// Path to the generated keyword file (relative to the repo root).
    #[arg(long, default_value = "docs/theme/harn-keywords.js")]
    pub output: String,
    /// Verify the on-disk file matches what would be generated; exit non-zero
    /// if stale. Used by CI to prevent drift between the highlighter and the
    /// lexer/stdlib.
    #[arg(long)]
    pub check: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AddArgs {
    /// Dependency name to add to harn.toml.
    pub name: String,
    /// Git URL for a remote dependency.
    #[arg(long, conflicts_with = "path")]
    pub git: Option<String>,
    /// Git tag to pin for a remote dependency.
    #[arg(long)]
    pub tag: Option<String>,
    /// Local path for a path dependency.
    #[arg(long, conflicts_with = "git")]
    pub path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, McpCommand, RunsCommand};
    use clap::Parser;

    #[test]
    fn test_parses_conformance_target_selection() {
        let cli = Cli::parse_from([
            "harn",
            "test",
            "conformance",
            "tests/worktree_runtime.harn",
            "--verbose",
        ]);

        let Command::Test(args) = cli.command.unwrap() else {
            panic!("expected test command");
        };
        assert_eq!(args.target.as_deref(), Some("conformance"));
        assert_eq!(
            args.selection.as_deref(),
            Some("tests/worktree_runtime.harn")
        );
        assert!(args.verbose);
    }

    #[test]
    fn test_run_rejects_deny_allow_conflict() {
        let err = Cli::try_parse_from([
            "harn",
            "run",
            "--deny",
            "read_file",
            "--allow",
            "exec",
            "main.harn",
        ])
        .unwrap_err();

        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn test_parses_mcp_login_flags() {
        let cli = Cli::parse_from([
            "harn",
            "mcp",
            "login",
            "notion",
            "--url",
            "https://example.com/mcp",
            "--client-id",
            "abc",
        ]);

        let Command::Mcp(args) = cli.command.unwrap() else {
            panic!("expected mcp command");
        };
        let McpCommand::Login(login) = args.command else {
            panic!("expected mcp login");
        };
        assert_eq!(login.target.as_deref(), Some("notion"));
        assert_eq!(login.url.as_deref(), Some("https://example.com/mcp"));
        assert_eq!(login.client_id.as_deref(), Some("abc"));
    }

    #[test]
    fn test_parses_runs_inspect_compare() {
        let cli = Cli::parse_from([
            "harn",
            "runs",
            "inspect",
            "run.json",
            "--compare",
            "baseline.json",
        ]);

        let Command::Runs(args) = cli.command.unwrap() else {
            panic!("expected runs command");
        };
        let RunsCommand::Inspect(inspect) = args.command;
        assert_eq!(inspect.path, "run.json");
        assert_eq!(inspect.compare.as_deref(), Some("baseline.json"));
    }

    #[test]
    fn test_parses_portal_flags() {
        let cli = Cli::parse_from([
            "harn", "portal", "--dir", "runs", "--host", "0.0.0.0", "--port", "4900", "--open",
            "false",
        ]);

        let Command::Portal(args) = cli.command.unwrap() else {
            panic!("expected portal command");
        };
        assert_eq!(args.dir, "runs");
        assert_eq!(args.host, "0.0.0.0");
        assert_eq!(args.port, 4900);
        assert!(!args.open);
    }
}
