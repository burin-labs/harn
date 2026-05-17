use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct SkillsArgs {
    #[command(subcommand)]
    pub command: SkillsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillsCommand {
    /// List the embedded canonical Harn skill corpus.
    List(SkillsListArgs),
    /// Print one embedded skill's frontmatter (or full body with `--full`).
    Get(SkillsGetArgs),
    /// Write the embedded skill corpus to disk for offline review.
    Dump(SkillsDumpArgs),
    /// Show layered-resolution skills discovered from the filesystem.
    Resolved(SkillsResolvedArgs),
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
    /// Emit a [`JsonEnvelope`]-wrapped catalog of embedded skills.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsGetArgs {
    /// Embedded skill name (e.g. `harn-language`).
    pub name: String,
    /// Include the full SKILL.md body alongside the frontmatter.
    #[arg(long)]
    pub full: bool,
    /// Emit a [`JsonEnvelope`]-wrapped record instead of human-readable text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsDumpArgs {
    /// Required selector — reserved for future filters.
    /// Today only `--all` is accepted; without it `dump` refuses to run.
    #[arg(long)]
    pub all: bool,
    /// Destination directory. Defaults to `./skills` in the current
    /// working directory. Each skill is written to `<dir>/<name>/SKILL.md`.
    #[arg(long = "out", value_name = "DIR")]
    pub out: Option<String>,
    /// Overwrite existing files in the destination instead of refusing.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SkillsResolvedArgs {
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
    /// One-line `short:` card for the SKILL.md frontmatter.
    #[arg(long)]
    pub description: Option<String>,
    /// Override the destination directory. Defaults to `.harn/skills/<name>/`.
    #[arg(long = "dir", value_name = "PATH")]
    pub dir: Option<String>,
    /// Overwrite any existing files at the destination.
    #[arg(long)]
    pub force: bool,
}
