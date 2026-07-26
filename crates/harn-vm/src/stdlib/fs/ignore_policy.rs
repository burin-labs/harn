//! The single owner of ignore policy for every Harn filesystem walk.
//!
//! Every surface that enumerates files — `glob`, `walk_dir`, `find_text`,
//! `find_evidence`, `project_scan`, `project_enrich`, and the hostlib
//! `tools/search` builtin — routes its skip decisions through this module so
//! there is exactly one answer to "is this path interesting?".
//!
//! # Layer stack
//!
//! Lowest to highest precedence:
//!
//! 1. [`BUILTIN_IGNORED_DIRS`] — Harn's own defaults, a *negatable* layer.
//! 2. `.gitignore` — committed project rules.
//! 3. `.ignore` — the tool-agnostic ripgrep/fd convention.
//! 4. `.agentignore` — the cross-tool agent convention.
//! 5. The per-call [`IgnorePolicy`] level, which selects how much of the stack
//!    runs at all.
//!
//! All matching mechanics come from the `ignore` crate; nothing here
//! hand-rolls a matcher. The crate's own precedence chain
//! (`m_custom_ignore.or(m_ignore).or(m_gi).or(m_gi_exclude).or(m_global)
//! .or(m_explicit)`) already *is* this stack: a custom ignore filename ranks
//! highest, explicit ignore files rank lowest, and `.ignore` outranks
//! `.gitignore` natively. So `.agentignore` is registered through
//! [`ignore::WalkBuilder::add_custom_ignore_filename`] and the built-in
//! defaults through [`ignore::WalkBuilder::add_ignore`], and no extra
//! precedence plumbing is needed.
//!
//! # Hidden files are a separate axis
//!
//! Dotfile filtering is *not* coupled to the ignore stack — this mirrors
//! ripgrep's orthogonal `--hidden` and `--no-ignore`. Callers pass
//! `include_hidden` explicitly; `glob` and `walk_dir` pass `true` because
//! globbing `.github/workflows/*.yml` must keep working.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::stdlib::sandbox::workspace_env;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

/// Directory names Harn skips unless a project ignore file re-includes them.
///
/// These are the lowest-precedence layer, not a hard filter: a `.gitignore`
/// (or `.ignore`, or `.agentignore`) carrying `!dist/` re-includes `dist`
/// because every one of those sources outranks this list.
///
/// `vendor` is deliberately absent. `go mod vendor` output is committed in
/// many Go repositories, so a built-in skip would silently drop tracked
/// source files. Do not add it.
pub const BUILTIN_IGNORED_DIRS: &[&str] = &[
    // Version-control metadata.
    ".git",
    ".hg",
    ".svn",
    // Language toolchain caches and build output.
    ".next",
    ".venv",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
    "venv",
    // Harn's own runtime state.
    ".harn",
    ".harn-runs",
    ".harn-tmp",
];

/// The custom ignore filename that gives agent tooling the last word.
const AGENT_IGNORE_FILENAME: &str = ".agentignore";

/// Project ignore files, lowest precedence first.
///
/// `.ignore` is the tool-agnostic ripgrep/fd convention; `.agentignore` is
/// the cross-tool agent convention and outranks both.
pub const PROJECT_IGNORE_FILENAMES: &[&str] = &[".gitignore", ".ignore", AGENT_IGNORE_FILENAME];

/// How much of the ignore stack a single call runs.
///
/// Monotone: each level is a superset of the one below it. A bool could not
/// express "built-in defaults but no project files", which is what a caller
/// wants when it must enumerate a tree whose ignore files are not its own.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum IgnorePolicy {
    /// Raw walk. Nothing is skipped.
    None,
    /// [`BUILTIN_IGNORED_DIRS`] only. Project ignore files are not read.
    Builtin,
    /// The full stack: built-in defaults, `.gitignore`, `.ignore`,
    /// `.agentignore`.
    #[default]
    Project,
}

impl IgnorePolicy {
    /// The option name every surface uses. One spelling, one meaning.
    pub const OPTION_KEY: &'static str = "ignore_policy";

    /// Accepted spellings, in stack order, for error messages and docs.
    pub const VALUES: &'static [&'static str] = &["none", "builtin", "project"];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Builtin => "builtin",
            Self::Project => "project",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "none" => Some(Self::None),
            "builtin" => Some(Self::Builtin),
            "project" => Some(Self::Project),
            _ => None,
        }
    }

    /// Parse with the canonical error message, so every builtin rejects an
    /// unknown level identically.
    pub fn parse_for(builtin: &str, raw: &str) -> Result<Self, String> {
        Self::parse(raw).ok_or_else(|| {
            format!(
                "{builtin}: unknown {key} `{raw}` (expected one of: {values})",
                key = Self::OPTION_KEY,
                values = Self::VALUES.join(", "),
            )
        })
    }

    /// Whether project ignore files (`.gitignore`, `.ignore`, `.agentignore`)
    /// participate at this level.
    #[must_use]
    pub fn reads_project_files(self) -> bool {
        matches!(self, Self::Project)
    }
}

/// Configure `builder` for a walk rooted at `base`.
///
/// This is the only place any Harn walk decides what to skip. It also pins
/// the determinism guarantees: the same repository at the same commit must
/// enumerate byte-identically on any machine — a developer laptop, CI, or
/// harn-cloud — or replay and eval comparisons stop meaning anything.
///
/// Returns an error only when the built-in layer cannot be materialized;
/// silently continuing there would drop a layer the caller asked for.
pub fn configure(
    builder: &mut WalkBuilder,
    base: &Path,
    policy: IgnorePolicy,
    include_hidden: bool,
) -> Result<(), String> {
    // The upward search runs only when something can bound it.
    //
    // A VCS anchor bounds it: with `require_git` on, the crate marks the
    // repository root as "has git" (`Ignore::add_parents`) and its `saw_git`
    // gate stops consulting `.gitignore` past that directory. That is worth
    // having, because a walk rooted in a subdirectory must still honor
    // root-anchored repository rules — `/crates/harn-cli/generated/` is
    // invisible from inside that crate otherwise, and losing it would make
    // this module *add* generated artifacts to results.
    //
    // A sandbox-workspace anchor cannot bound it: `require_git` must stay off
    // there or `any_git` is false and the workspace's own `.gitignore` is
    // never read at all — but with it off, `has_git` is never computed, the
    // gate never trips, and the search runs to the filesystem root where a
    // stray `~/.gitignore` would change results per machine. So the search
    // does not start. The base's own ignore files are still read: the in-walk
    // chain is iterated separately from the absolute parents.
    //
    // Residual, and the one honest exception: `.ignore` and `.agentignore`
    // carry no `saw_git` gate, so one placed *above* a repository root still
    // applies. That is a deliberate, visible act by whoever put it there,
    // unlike the git config Harn now refuses to read.
    let anchored_by_vcs = matches!(
        project_root_for(&absolutize(base)),
        Some((_, ProjectAnchor::Vcs))
    );
    builder
        .hidden(!include_hidden)
        // Uncommitted, machine-local ignore sources are never read: the
        // developer's `core.excludesFile` and `.git/info/exclude` differ
        // between machines and are not part of the commit.
        .git_global(false)
        .git_exclude(false)
        .parents(anchored_by_vcs)
        .require_git(anchored_by_vcs);

    match effective_policy(base, policy) {
        IgnorePolicy::None => {
            builder.ignore(false).git_ignore(false);
        }
        IgnorePolicy::Builtin => {
            builder.ignore(false).git_ignore(false);
            add_builtin_layer(builder)?;
        }
        IgnorePolicy::Project => {
            builder
                .ignore(true)
                .git_ignore(true)
                .add_custom_ignore_filename(AGENT_IGNORE_FILENAME);
            add_builtin_layer(builder)?;
        }
    }
    Ok(())
}

/// One merged matcher for callers that classify paths individually instead of
/// walking a tree — a filesystem watcher, say, which is handed one event at a
/// time and never descends anything.
///
/// Within a single [`Gitignore`] the last matching pattern wins, so appending
/// the sources in stack order reproduces the same precedence
/// [`configure`] gets from the crate's multi-source chain.
///
/// Unlike [`configure`], this reads ignore files only at `base` and does not
/// search upward. That is deliberate rather than an oversight: a walk has a
/// tree to interpret rules against, so honoring a repository-root rule for a
/// subdirectory walk is meaningful. A per-event classifier has one path and a
/// long-lived subscription, and re-reading an ancestor chain that can change
/// underneath it would make identical events classify differently over time.
/// Callers that need repository-root rules should walk instead.
///
/// Match paths with [`Gitignore::matched_path_or_any_parents`], not
/// [`Gitignore::matched`]: a directory rule such as `node_modules/` matches
/// only the directory entry, so matching a leaf path alone lets everything
/// beneath an ignored directory through.
#[must_use]
pub fn matcher(base: &Path, policy: IgnorePolicy) -> Gitignore {
    let effective = effective_policy(base, policy);
    if effective == IgnorePolicy::None {
        return Gitignore::empty();
    }
    let mut builder = GitignoreBuilder::new(base);
    for name in BUILTIN_IGNORED_DIRS {
        let _ = builder.add_line(None, &format!("{name}/"));
    }
    if effective.reads_project_files() {
        for name in PROJECT_IGNORE_FILENAMES {
            let _ = builder.add(base.join(name));
        }
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// The level that actually applies to a walk rooted at `base`.
///
/// Project ignore files are only meaningful inside a project. Harn writes a
/// `.gitignore` containing `*` into its own scratch directories, so honoring
/// project files under a scratch base would make
/// `harness.fs.glob("*.json", harness.fs.workspace_temp_dir())` return `[]`;
/// and a base outside any project has no project rules to honor at all.
///
/// Both cases degrade to [`IgnorePolicy::Builtin`] rather than to a raw walk.
/// The trap is caused by a project *file*, so dropping project files is enough
/// to defeat it, and universal hygiene is still worth having: a scratch tree or
/// an unmanaged directory is exactly where an unwanted `node_modules` walk is
/// most likely. Degrading all the way to `None` would reintroduce the
/// unbounded walk this module exists to prevent. A scratch base still
/// enumerates its own contents because a walk root is never matched against
/// the stack.
///
/// An explicitly requested `None` is honored as `None`: a caller that asked
/// for a raw walk is never escalated into filtering.
#[must_use]
pub fn effective_policy(base: &Path, requested: IgnorePolicy) -> IgnorePolicy {
    if requested == IgnorePolicy::None {
        return IgnorePolicy::None;
    }
    let resolved = absolutize(base);
    if is_scratch_path(&resolved) || project_root_for(&resolved).is_none() {
        return IgnorePolicy::Builtin;
    }
    requested
}

/// What marks the project a walk belongs to.
///
/// The distinction decides how far the upward ignore search may reach, so it
/// has to travel with the root rather than being re-derived.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectAnchor {
    /// A version-control entry (`.git` or `.jj`). The `ignore` crate can bound
    /// its own upward `.gitignore` search against this.
    Vcs,
    /// A sandbox workspace root carrying no version-control entry. Nothing
    /// marks where a parent search should stop, so it must not start.
    SandboxWorkspace,
}

/// Nearest ancestor (inclusive) that anchors a project, and what marks it.
#[must_use]
pub fn project_root_for(path: &Path) -> Option<(PathBuf, ProjectAnchor)> {
    for ancestor in path.ancestors() {
        // `.jj` as well as `.git`: the `ignore` crate treats both as a
        // repository marker, and the bound below only holds if this agrees
        // with what the crate will detect.
        if ancestor.join(".git").exists() || ancestor.join(".jj").exists() {
            return Some((ancestor.to_path_buf(), ProjectAnchor::Vcs));
        }
    }
    let roots = sandbox_workspace_roots();
    path.ancestors()
        .find(|ancestor| roots.iter().any(|root| root == *ancestor))
        .map(|ancestor| (ancestor.to_path_buf(), ProjectAnchor::SandboxWorkspace))
}

/// Whether `path` lives inside a directory Harn manages as scratch.
///
/// These directories self-ignore with a `*` `.gitignore` (see
/// `fs::workspace_temp_root` and `sandbox::workspace_env`), which is correct
/// for git and fatal for a walk that honors it.
#[must_use]
pub fn is_scratch_path(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(workspace_env::WORKSPACE_TMPDIR_NAME)
                | Some(workspace_env::WORKSPACE_TOOLCHAIN_CACHE_NAME)
        )
    })
}

fn sandbox_workspace_roots() -> Vec<PathBuf> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Vec::new();
    };
    policy
        .workspace_roots
        .iter()
        .map(|root| absolutize(Path::new(root)))
        .collect()
}

fn absolutize(path: &Path) -> PathBuf {
    path.canonicalize()
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn add_builtin_layer(builder: &mut WalkBuilder) -> Result<(), String> {
    let path = builtin_ignore_file()?;
    // A partial error means one pattern failed to compile while the rest were
    // applied; the pattern list is a compile-time constant, so this is
    // unreachable in practice and would be a bug in the list, not user input.
    if let Some(error) = builder.add_ignore(path) {
        return Err(format!(
            "ignore policy: built-in ignore layer at {} is invalid: {error}",
            path.display()
        ));
    }
    Ok(())
}

/// Gitignore text for the built-in layer.
///
/// Each entry is a bare directory name with a trailing slash: no leading
/// slash, so gitignore semantics match it at *any* depth regardless of where
/// the matcher is rooted, and a trailing slash so a regular file that happens
/// to be named `build` or `dist` is still enumerated.
fn builtin_ignore_text() -> String {
    let mut text =
        String::from("# Generated by Harn. Lowest-precedence ignore layer; safe to delete.\n");
    for name in BUILTIN_IGNORED_DIRS {
        text.push_str(name);
        text.push_str("/\n");
    }
    text
}

/// Path to the materialized built-in layer, created once per process.
///
/// The `ignore` crate keeps `IgnoreBuilder::add_ignore(Gitignore)` — the
/// in-memory injection point — `pub(crate)`; the only public way into the
/// lowest-precedence `explicit_ignores` slot is
/// [`WalkBuilder::add_ignore`], which takes a *file path*. Consulting a
/// hand-built matcher in `filter_entry` instead would not work: the walker
/// hands `filter_entry` no way to learn that a higher layer *whitelisted* a
/// path, so `!dist/` could never re-include anything. Materializing the
/// patterns keeps the `ignore` crate as the single matching engine and keeps
/// the built-in layer genuinely negatable.
///
/// `WalkBuilder::add_ignore` roots the resulting matcher at the process
/// working directory rather than at the file's own directory. That is
/// harmless here precisely because every pattern is a bare name: bare
/// patterns are depth- and root-independent. It would *not* be safe for
/// anchored patterns, which is why arbitrary ignore files are never routed
/// through this path.
fn builtin_ignore_file() -> Result<&'static Path, String> {
    static PATH: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    PATH.get_or_init(materialize_builtin_ignore_file)
        .as_deref()
        .map_err(Clone::clone)
}

fn materialize_builtin_ignore_file() -> Result<PathBuf, String> {
    let text = builtin_ignore_text();
    let digest = hex16(Sha256::digest(text.as_bytes()).as_slice());
    let dir = std::env::temp_dir();
    // Content-addressed, so concurrent Harn processes converge on one file
    // and a stale file from an older pattern list is never reused.
    let target = dir.join(format!("harn-builtin-ignore-{digest}"));
    if std::fs::read_to_string(&target).is_ok_and(|existing| existing == text) {
        return Ok(target);
    }
    let scratch = dir.join(format!(
        "harn-builtin-ignore-{digest}.{}.{}.partial",
        std::process::id(),
        uuid::Uuid::now_v7().simple(),
    ));
    std::fs::write(&scratch, &text).map_err(|error| {
        format!(
            "ignore policy: cannot materialize the built-in ignore layer at {}: {error}",
            scratch.display()
        )
    })?;
    // Rename publishes atomically, so a concurrent reader never sees a torn
    // file. If another user already owns `target` in a sticky temp dir the
    // rename fails; the process keeps its private copy rather than losing the
    // layer.
    match std::fs::rename(&scratch, &target) {
        Ok(()) => Ok(target),
        Err(_) => Ok(scratch),
    }
}

fn hex16(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
