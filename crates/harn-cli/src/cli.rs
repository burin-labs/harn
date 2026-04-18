use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

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
    #[command(long_about = "\
Execute a .harn file or an inline expression.

USAGE
    harn run script.harn
    harn run -e 'println(\"hello\")'
    harn run script.harn -- arg1 arg2   (script reads `argv` as list<string>)

CONCURRENCY
    Harn supports first-class concurrency primitives:
      - spawn { ... }         — launch a task, return a handle
      - parallel each LIST    — concurrent map
      - parallel settle LIST  — concurrent map, collect Ok/Err
      - parallel N            — N-way fan-out
      - with { max_concurrent: N }  — cap in-flight workers
      - channels, retry, select
    https://harn.burincode.com/concurrency.html

LLM THROTTLING
    Providers can be rate-limited via `rpm:` in harn.toml / providers.toml
    or via `HARN_RATE_LIMIT_<PROVIDER>=N`. Rate limits control throughput
    (RPM); `max_concurrent` on `parallel` caps simultaneous in-flight jobs.

SCRIPTING
    LLM-readable one-pager: https://harn.burincode.com/docs/llm/harn-quickref.md
    Human cheatsheet:       https://harn.burincode.com/scripting-cheatsheet.html
    Full docs:              https://harn.burincode.com/
")]
    Run(RunArgs),
    /// Type-check .harn files or directories without executing them.
    Check(CheckArgs),
    /// Export machine-readable Harn contracts and bundle manifests.
    Contracts(ContractsArgs),
    /// Lint .harn files or directories for common issues.
    Lint(PathTargetsArgs),
    /// Format .harn files or directories.
    Fmt(FmtArgs),
    /// Run user tests or the conformance suite.
    Test(TestArgs),
    /// Scaffold a new project with harn.toml.
    Init(InitArgs),
    /// Scaffold a new project from a starter template.
    New(InitArgs),
    /// Diagnose the local Harn environment and provider setup.
    Doctor(DoctorArgs),
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
    /// Run a pipeline against a Harn-native host module for fast iteration.
    Playground(PlaygroundArgs),
    /// Inspect persisted workflow run records.
    Runs(RunsArgs),
    /// Replay a persisted workflow run record.
    Replay(ReplayArgs),
    /// Evaluate a run record, run directory, or eval manifest.
    Eval(EvalArgs),
    /// Start the interactive REPL.
    Repl,
    /// Benchmark a .harn pipeline over repeated runs.
    Bench(BenchArgs),
    /// Render a .harn file as a Mermaid workflow graph.
    Viz(VizArgs),
    /// Install dependencies declared in harn.toml.
    Install,
    /// Add a dependency to harn.toml.
    Add(AddArgs),
    /// Print resolved metadata for a model alias or model id as JSON.
    ModelInfo(ModelInfoArgs),
    /// Manage and inspect Harn skills (list, inspect, match, install, new).
    Skills(SkillsArgs),
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
    /// Extra skill-discovery roots. Repeatable; each path is a
    /// directory of `<name>/SKILL.md` bundles, equivalent to a
    /// single-entry `$HARN_SKILLS_PATH`. Highest-priority layer —
    /// wins ties against every other layer. See `docs/src/skills.md`.
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
    /// Path to the .harn file to execute.
    pub file: Option<String>,
    /// Positional arguments passed to the pipeline as the global `argv`
    /// list. Place them after a `--` separator: `harn run script.harn -- a b c`.
    // `last = true` alone routes post-`--` tokens into `argv`; combining it
    // with `trailing_var_arg = true` panics at clap runtime.
    #[arg(last = true)]
    pub argv: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    /// Extra host capability schema for preflight validation.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Alternate root for render/template path checks.
    #[arg(long = "bundle-root")]
    pub bundle_root: Option<String>,
    /// Flag unvalidated boundary-API values used in field access.
    #[arg(long = "strict-types")]
    pub strict_types: bool,
    /// Check every `.harn` file under `[workspace].pipelines` in the
    /// nearest `harn.toml`. Positional targets are additive.
    #[arg(long = "workspace")]
    pub workspace: bool,
    /// Downgrade preflight diagnostics to warnings (or suppress them
    /// entirely with `off`). Overrides `[check].preflight_severity`.
    /// Accepted values: `error` (default), `warning`, `off`.
    #[arg(long = "preflight", value_name = "SEVERITY")]
    pub preflight: Option<String>,
    /// One or more .harn files or directories. Optional when `--workspace`
    /// is set.
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct ContractsArgs {
    #[command(subcommand)]
    pub command: ContractsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ContractsCommand {
    /// Export builtin registry metadata.
    Builtins(ContractsOutputArgs),
    /// Export the effective host capability manifest used for preflight.
    HostCapabilities(ContractsHostCapabilitiesArgs),
    /// Export a bundle manifest for one or more pipelines and optionally verify it.
    Bundle(ContractsBundleArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ContractsOutputArgs {
    /// Pretty-print JSON output.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub pretty: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ContractsHostCapabilitiesArgs {
    /// Extra host capability schema to merge into the default manifest.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Pretty-print JSON output.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub pretty: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ContractsBundleArgs {
    /// Extra host capability schema for bundle contract validation.
    #[arg(long = "host-capabilities")]
    pub host_capabilities: Option<String>,
    /// Alternate root for render/template path checks.
    #[arg(long = "bundle-root")]
    pub bundle_root: Option<String>,
    /// Fail if the selected targets do not pass Harn preflight validation.
    #[arg(long)]
    pub verify: bool,
    /// Pretty-print JSON output.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub pretty: bool,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PathTargetsArgs {
    /// Automatically apply safe fixes.
    #[arg(long)]
    pub fix: bool,
    /// Force-enable the `require-file-header` rule (overrides harn.toml).
    #[arg(long = "require-file-header")]
    pub require_file_header: bool,
    /// One or more .harn files or directories.
    #[arg(required = true)]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct FmtArgs {
    /// Check formatting without rewriting files.
    #[arg(long)]
    pub check: bool,
    /// Maximum line width before wrapping. Overrides `[fmt] line_width` in harn.toml.
    #[arg(long = "line-width")]
    pub line_width: Option<usize>,
    /// Total width of `// ----` separator bars. Overrides `[fmt] separator_width`.
    #[arg(long = "separator-width")]
    pub separator_width: Option<usize>,
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
    /// Show per-test timing and summary statistics.
    #[arg(long, action = ArgAction::SetTrue)]
    pub timing: bool,
    /// Record LLM fixtures to .harn-fixtures/.
    #[arg(long)]
    pub record: bool,
    /// Replay LLM fixtures from .harn-fixtures/.
    #[arg(long)]
    pub replay: bool,
    /// Extra skill-discovery roots (repeatable). See `harn run
    /// --skill-dir` — applied the same way to user tests and
    /// conformance fixtures so bundled `skills/` dirs are picked up.
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
    /// User test path, or `conformance` to target the conformance suite.
    pub target: Option<String>,
    /// Optional file or directory under conformance/ when target is `conformance`.
    pub selection: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    /// Optional project name to scaffold.
    pub name: Option<String>,
    /// Starter template to scaffold.
    #[arg(long, value_enum, default_value_t = ProjectTemplate::Basic)]
    pub template: ProjectTemplate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProjectTemplate {
    Basic,
    Agent,
    #[value(name = "mcp-server")]
    McpServer,
    Eval,
    #[value(name = "pipeline-lab")]
    PipelineLab,
}

#[derive(Debug, Args)]
pub(crate) struct DoctorArgs {
    /// Skip provider connectivity checks.
    #[arg(long)]
    pub no_network: bool,
}

#[derive(Debug, Args)]
pub(crate) struct VizArgs {
    /// Path to the .harn file to visualize.
    pub file: String,
    /// Optional output path. Defaults to stdout.
    #[arg(short, long)]
    pub output: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct BenchArgs {
    /// Path to the .harn file to benchmark.
    pub file: String,
    /// Number of benchmark iterations to run.
    #[arg(short = 'n', long, default_value_t = 10)]
    pub iterations: usize,
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
    /// Optional Server Card JSON to advertise (MCP v2.1). Path to a
    /// `.json` file OR an inline JSON string. The card is embedded in
    /// the `initialize` response's `serverInfo.card` field AND exposed
    /// as a static resource at `well-known://mcp-card`.
    #[arg(long = "card", value_name = "PATH_OR_JSON")]
    pub card: Option<String>,
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
pub(crate) struct PlaygroundArgs {
    /// Host module exporting the capabilities the script expects.
    #[arg(long, default_value = "host.harn")]
    pub host: String,
    /// Pipeline entrypoint to execute.
    #[arg(long, default_value = "pipeline.harn")]
    pub script: String,
    /// Runtime task string exposed via `runtime_task()`.
    #[arg(long)]
    pub task: Option<String>,
    /// Provider/model override as `provider:model`.
    #[arg(long)]
    pub llm: Option<String>,
    /// Re-run when the script or host module changes.
    #[arg(long)]
    pub watch: bool,
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

#[derive(Debug, Args)]
pub(crate) struct ModelInfoArgs {
    /// Model alias or provider-native model id.
    pub model: String,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsArgs {
    #[command(subcommand)]
    pub command: SkillsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillsCommand {
    /// Show resolved skills in priority order, with collision warnings.
    List(SkillsListArgs),
    /// Dump the resolved SKILL.md plus bundled files and metadata for one skill.
    Inspect(SkillsInspectArgs),
    /// Run the metadata matcher against a prompt and show ranked candidates.
    #[command(name = "match")]
    Match(SkillsMatchArgs),
    /// Resolve a git ref or local path into `.harn/skills-cache/` so the layered resolver picks it up.
    Install(SkillsInstallArgs),
    /// Scaffold a new SKILL.md bundle under `.harn/skills/<name>/`.
    New(SkillsNewArgs),
}

#[derive(Debug, Args)]
pub(crate) struct SkillsListArgs {
    /// Emit newline-delimited JSON (one record per skill) instead of a table.
    #[arg(long)]
    pub json: bool,
    /// Optional path used to anchor manifest and project discovery (defaults to cwd).
    #[arg(long = "from", value_name = "PATH")]
    pub from: Option<String>,
    /// Extra skill-discovery roots (repeatable). Same as `harn run --skill-dir`.
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
    /// Include shadowed (lower-priority) entries as well as the winners.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsInspectArgs {
    /// Skill id to inspect (e.g. `deploy` or `acme/ops/deploy`).
    pub name: String,
    /// Emit JSON instead of a human-readable dump.
    #[arg(long)]
    pub json: bool,
    /// Optional path used to anchor manifest and project discovery (defaults to cwd).
    #[arg(long = "from", value_name = "PATH")]
    pub from: Option<String>,
    /// Extra skill-discovery roots (repeatable).
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsMatchArgs {
    /// Prompt text to score every discovered skill against.
    pub query: String,
    /// Top-N matches to display. Default 5.
    #[arg(long, default_value_t = 5)]
    pub top_n: usize,
    /// Emit JSON instead of a ranked table.
    #[arg(long)]
    pub json: bool,
    /// Simulate working-file path globs (repeatable).
    #[arg(long = "working-file", value_name = "PATH")]
    pub working_files: Vec<String>,
    /// Optional path used to anchor manifest and project discovery.
    #[arg(long = "from", value_name = "PATH")]
    pub from: Option<String>,
    /// Extra skill-discovery roots (repeatable).
    #[arg(long = "skill-dir", value_name = "PATH")]
    pub skill_dir: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsInstallArgs {
    /// Spec: a git URL, `owner/repo`, or a local filesystem path.
    pub spec: String,
    /// Optional human name / directory label for the cached install.
    #[arg(long)]
    pub name: Option<String>,
    /// Git tag or branch to pin (only applies to git specs).
    #[arg(long)]
    pub tag: Option<String>,
    /// Optional namespace prefix registered with the installed skills.
    #[arg(long)]
    pub namespace: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsNewArgs {
    /// Skill identifier (used as the directory name and default `name:`).
    pub name: String,
    /// One-line description for the SKILL.md frontmatter.
    #[arg(long)]
    pub description: Option<String>,
    /// Override the destination directory. Defaults to `.harn/skills/<name>/`.
    #[arg(long = "dir", value_name = "PATH")]
    pub dir: Option<String>,
    /// Overwrite any existing files at the destination.
    #[arg(long)]
    pub force: bool,
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, McpCommand, ProjectTemplate, RunsCommand, SkillsCommand};
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

    #[test]
    fn test_parses_new_template() {
        let cli = Cli::parse_from(["harn", "new", "review-bot", "--template", "agent"]);

        let Command::New(args) = cli.command.unwrap() else {
            panic!("expected new command");
        };
        assert_eq!(args.name.as_deref(), Some("review-bot"));
        assert_eq!(args.template, ProjectTemplate::Agent);
    }

    #[test]
    fn test_parses_pipeline_lab_template() {
        let cli = Cli::parse_from([
            "harn",
            "new",
            "pipeline-lab-demo",
            "--template",
            "pipeline-lab",
        ]);

        let Command::New(args) = cli.command.unwrap() else {
            panic!("expected new command");
        };
        assert_eq!(args.template, ProjectTemplate::PipelineLab);
    }

    #[test]
    fn test_parses_playground_args() {
        let cli = Cli::parse_from([
            "harn",
            "playground",
            "--host",
            "examples/playground/host.harn",
            "--script",
            "examples/playground/echo.harn",
            "--task",
            "hi",
            "--llm",
            "ollama:qwen2.5-coder:latest",
            "--watch",
        ]);

        let Command::Playground(args) = cli.command.unwrap() else {
            panic!("expected playground command");
        };
        assert_eq!(args.host, "examples/playground/host.harn");
        assert_eq!(args.script, "examples/playground/echo.harn");
        assert_eq!(args.task.as_deref(), Some("hi"));
        assert_eq!(args.llm.as_deref(), Some("ollama:qwen2.5-coder:latest"));
        assert!(args.watch);
    }

    #[test]
    fn test_parses_doctor_flags() {
        let cli = Cli::parse_from(["harn", "doctor", "--no-network"]);

        let Command::Doctor(args) = cli.command.unwrap() else {
            panic!("expected doctor command");
        };
        assert!(args.no_network);
    }

    #[test]
    fn test_parses_viz_args() {
        let cli = Cli::parse_from(["harn", "viz", "main.harn", "--output", "graph.mmd"]);

        let Command::Viz(args) = cli.command.unwrap() else {
            panic!("expected viz command");
        };
        assert_eq!(args.file, "main.harn");
        assert_eq!(args.output.as_deref(), Some("graph.mmd"));
    }

    #[test]
    fn test_parses_bench_args() {
        let cli = Cli::parse_from(["harn", "bench", "main.harn", "--iterations", "25"]);

        let Command::Bench(args) = cli.command.unwrap() else {
            panic!("expected bench command");
        };
        assert_eq!(args.file, "main.harn");
        assert_eq!(args.iterations, 25);
    }

    #[test]
    fn test_parses_skills_subcommands() {
        let cli = Cli::parse_from(["harn", "skills", "list", "--json", "--all"]);
        let Command::Skills(args) = cli.command.unwrap() else {
            panic!("expected skills command");
        };
        let SkillsCommand::List(list) = args.command else {
            panic!("expected skills list");
        };
        assert!(list.json);
        assert!(list.all);

        let cli = Cli::parse_from(["harn", "skills", "match", "deploy the app", "--top-n", "3"]);
        let Command::Skills(args) = cli.command.unwrap() else {
            panic!("expected skills command");
        };
        let SkillsCommand::Match(matcher) = args.command else {
            panic!("expected skills match");
        };
        assert_eq!(matcher.query, "deploy the app");
        assert_eq!(matcher.top_n, 3);

        let cli = Cli::parse_from([
            "harn",
            "skills",
            "install",
            "https://example.com/acme/harn-skills.git",
            "--tag",
            "v1.0",
            "--namespace",
            "acme",
        ]);
        let Command::Skills(args) = cli.command.unwrap() else {
            panic!("expected skills command");
        };
        let SkillsCommand::Install(install) = args.command else {
            panic!("expected skills install");
        };
        assert_eq!(install.tag.as_deref(), Some("v1.0"));
        assert_eq!(install.namespace.as_deref(), Some("acme"));

        let cli = Cli::parse_from([
            "harn",
            "skills",
            "new",
            "deploy",
            "--description",
            "Ship things",
        ]);
        let Command::Skills(args) = cli.command.unwrap() else {
            panic!("expected skills command");
        };
        let SkillsCommand::New(new_args) = args.command else {
            panic!("expected skills new");
        };
        assert_eq!(new_args.name, "deploy");
        assert_eq!(new_args.description.as_deref(), Some("Ship things"));
    }

    #[test]
    fn test_parses_model_info_args() {
        let cli = Cli::parse_from(["harn", "model-info", "tog-gemma4-31b"]);

        let Command::ModelInfo(args) = cli.command.unwrap() else {
            panic!("expected model-info command");
        };
        assert_eq!(args.model, "tog-gemma4-31b");
    }
}
