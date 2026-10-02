/// The never-approvable command floor. A command that this module flags is
/// denied before the child spawns and is never routed to the consent gate.
///
/// The classifier is intentionally precision-over-recall and is NOT a complete
/// sandbox; it catches the obvious irreversible-destruction shapes (fork bomb,
/// `git reset --hard` / `git clean -fd` / force-push, `rm -rf` escaping the
/// workspace root or wiping it in place, `dd of=`, `mkfs`, `chmod -R 000`,
/// `truncate -s 0` of a tracked project file, and `>`/`>>` redirection onto a
/// tracked project file) through adversarial quoting, chained-command splitting, `bash -c`
/// recursion, and the `sudo`/`env`/`nice`/`nohup`/`time`/`timeout`/`command`/
/// `builtin`/`exec` wrapper family. Executable identity and nesting come from
/// the shared shell AST; the remaining text checks only inspect syntax that is
/// not an executable command (fork-bomb grammar and tracked-file redirects).
use std::path::Path;

mod project_delete;
mod semantic_shell;
mod shell;
mod tracked_writes;

use project_delete::argv_deletes_project;
pub(super) use semantic_shell::{
    analyze_argv, analyze_shell, analyze_shell_dialect, ShellAnalysis,
};
use shell::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ShellCommandStage {
    pub(super) text: String,
    pub(super) argv: Vec<String>,
    /// Expansion marker for each argv element.
    pub(super) dynamic: Vec<bool>,
}

/// Maximum `bash -c` recursion depth. Each level strips a shell wrapper, so
/// the string strictly shrinks; the cap is a defensive bound only.
const MAX_DEPTH: usize = 8;

const PROJECT_DELETE_REASON: &str = "destructive recursive deletion of the project root is blocked";

const FORK_BOMB_REASON: &str =
    "fork bomb (`:(){ :|:& };:`) is blocked: it would exhaust the machine";

/// Classify `command` against the floor. `workspace_roots` are the absolute
/// workspace roots (an `rm -rf` target is an escape unless it resolves
/// inside one of them); an empty slice means "no root context", under which
/// every absolute path is conservatively treated as an escape. Returns the
/// blocking reason.
pub(super) fn reason_at_with_analysis(
    analysis: &ShellAnalysis,
    workspace_roots: &[String],
    active_cwd: Option<&Path>,
) -> Option<String> {
    reason_from_analysis(analysis, workspace_roots, active_cwd, 0)
}

/// Classify already-resolved argv without interpreting ordinary arguments as
/// shell syntax. Only an actual POSIX shell `-c` payload re-enters `reason_at`.
pub(super) fn reason_argv(
    argv: &[String],
    workspace_roots: &[String],
    active_cwd: Option<&Path>,
) -> Option<String> {
    let analysis = analyze_argv(argv);
    reason_at_with_analysis(&analysis, workspace_roots, active_cwd)
}

pub(super) fn command_segments(command: &str) -> Vec<String> {
    command_segments_inner(command, 0)
}

/// Parse a shell command into chain groups and pipeline stages. This is the
/// shared shell boundary for policy written outside Rust; consumers receive
/// normalized argv while the original stage text remains available for
/// syntax-sensitive checks such as output redirection.
pub(super) fn shell_command_groups(command: &str) -> Vec<Vec<ShellCommandStage>> {
    split_chained_command(command)
        .into_iter()
        .filter_map(|group| {
            let stages = split_pipeline_command(&group)
                .into_iter()
                .filter_map(|text| {
                    let tokens = shell_words(&text);
                    let index = unwrapped_shell_command_index(&tokens, 0, tokens.len());
                    (index < tokens.len()).then(|| ShellCommandStage {
                        text: text.trim().to_string(),
                        argv: tokens[index..].to_vec(),
                        dynamic: vec![false; tokens.len() - index],
                    })
                })
                .collect::<Vec<_>>();
            (!stages.is_empty()).then_some(stages)
        })
        .collect()
}

fn command_segments_inner(command: &str, depth: usize) -> Vec<String> {
    if depth > MAX_DEPTH {
        return Vec::new();
    }
    let mut segments = Vec::new();
    for segment in split_chained_command(command) {
        push_unique(&mut segments, segment.trim());
        let tokens = shell_words(&segment);
        let mut start = 0;
        while start < tokens.len() {
            let end = next_pipeline_boundary(&tokens, start);
            if start < end {
                push_unique(&mut segments, &tokens[start..end].join(" "));
                let command_index = unwrapped_shell_command_index(&tokens, start, end);
                if command_index < end {
                    let command = command_basename(&tokens[command_index]);
                    if matches!(command, "bash" | "sh" | "zsh") {
                        if let Some(script) = shell_c_script(&tokens[(command_index + 1)..end]) {
                            for inner in command_segments_inner(script, depth + 1) {
                                push_unique(&mut segments, &inner);
                            }
                        }
                    }
                }
            }
            start = end + 1;
        }
    }
    segments
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() && !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn reason_inner(
    command: &str,
    roots: &[String],
    active_cwd: Option<&Path>,
    depth: usize,
) -> Option<String> {
    let analysis = analyze_shell(command);
    reason_from_analysis(&analysis, roots, active_cwd, depth)
}

fn reason_from_analysis(
    analysis: &ShellAnalysis,
    roots: &[String],
    active_cwd: Option<&Path>,
    depth: usize,
) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    // Fork bomb is checked on the WHOLE command first: splitting on `;`/`&`
    // would tear `:(){ :|:& };:` apart before it can be recognized.
    if analysis.fork_bomb {
        return Some(FORK_BOMB_REASON.to_string());
    }
    for redirect in &analysis.redirects {
        if redirect.writes_file() && !redirect.dynamic {
            if let Some(destination) = redirect.destination.as_deref() {
                if let Some(reason) = tracked_writes::redirect_target_over_tracked_reason(
                    destination,
                    active_cwd,
                    roots,
                ) {
                    return Some(reason);
                }
            }
        }
    }
    for stage in &analysis.stages {
        if argv_deletes_project(&stage.argv) {
            return Some(PROJECT_DELETE_REASON.to_string());
        }
        if let Some(hit) =
            invocation_catastrophe(&stage.argv, 0, stage.argv.len(), roots, active_cwd, depth)
        {
            return Some(hit);
        }
    }
    None
}

fn invocation_catastrophe(
    tokens: &[String],
    start: usize,
    end: usize,
    roots: &[String],
    active_cwd: Option<&Path>,
    depth: usize,
) -> Option<String> {
    let command_index = unwrapped_command_index(tokens, start, end);
    if command_index >= end {
        return None;
    }
    let argv = &tokens[command_index..end];
    let command = command_basename(&argv[0]);
    let args = &argv[1..];

    // bash/sh/zsh -c '<script>' → recurse into the inner script (category
    // inherited from the inner classification).
    if matches!(command, "bash" | "sh" | "zsh") {
        if let Some(script) = shell_c_script(args) {
            if let Some(hit) = reason_inner(script, roots, active_cwd, depth + 1) {
                return Some(hit);
            }
        }
    }

    match command {
        "git" => git_catastrophe(args),
        "rm" => rm_escape_catastrophe(args, roots),
        "dd" => dd_catastrophe(args),
        "mkfs" | "mke2fs" => Some(format!(
            "`{command}` (filesystem format) is blocked: it would destroy a device"
        )),
        _ if command.starts_with("mkfs.") => Some(format!(
            "`{command}` (filesystem format) is blocked: it would destroy a device"
        )),
        "chmod" => chmod_catastrophe(args),
        "truncate" => tracked_writes::truncate_catastrophe(args, active_cwd, roots),
        _ => None,
    }
}

// -c detection: skip `--`, scan short-flag clusters for 'c', return the NEXT
// token as the script.
fn shell_c_script(args: &[String]) -> Option<&str> {
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if token == "--" {
            index += 1;
            continue;
        }
        if token.starts_with('-') && token != "-" {
            if token.chars().skip(1).any(|flag| flag == 'c') {
                return args.get(index + 1).map(String::as_str);
            }
            index += 1;
            continue;
        }
        return None;
    }
    None
}

/// Split git's argv (after the `git` word) into its subcommand and that
/// subcommand's arguments, skipping global options. Value-taking global options
/// consume the next token.
fn git_subcommand(args: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        match token.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                index += 2;
                continue;
            }
            _ if token.starts_with('-') => {
                index += 1;
                continue;
            }
            _ => break,
        }
    }
    let subcommand = args.get(index)?.as_str();
    Some((subcommand, &args[(index + 1).min(args.len())..]))
}

/// How a `git push` can change remote refs beyond a fast-forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteRefRewrite {
    /// `--delete`/`-d`, `--prune`, or a `:dst` refspec removes a remote ref.
    Delete,
    /// `--force`/`-f`, `--force-with-lease`, `--mirror`, or a `+src:dst`
    /// refspec can overwrite a remote ref with unrelated history.
    Force,
}

/// Classify `git push <args>` from its argv. This is the one owner of the
/// judgment: the never-approvable floor blocks [`RemoteRefRewrite::Force`] and
/// the `git_force_push` label covers both kinds, so the two cannot disagree.
///
/// Long options may be abbreviated to any prefix, as git's option parser
/// accepts unambiguous ones; an ambiguous prefix fails in git, so classifying
/// it costs nothing. Refspecs come from argv only: a configured
/// `remote.<name>.push` or `push.default` is not visible here.
fn git_push_rewrite(args: &[String]) -> Option<RemoteRefRewrite> {
    fn abbreviates(name: &str, option: &str) -> bool {
        name.len() > 2 && option.starts_with(name)
    }

    let mut rewrite = None;
    let mut options = true;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        if options && arg == "--" {
            options = false;
            continue;
        }
        if options && arg.starts_with("--") {
            let (name, value) = match arg.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (arg, None),
            };
            if ["--force", "--force-with-lease", "--mirror"]
                .iter()
                .any(|option| abbreviates(name, option))
            {
                return Some(RemoteRefRewrite::Force);
            }
            if ["--delete", "--prune"]
                .iter()
                .any(|option| abbreviates(name, option))
            {
                rewrite = Some(RemoteRefRewrite::Delete);
            } else if value.is_none()
                && [
                    "--push-option",
                    "--repo",
                    "--receive-pack",
                    "--exec",
                    "--recurse-submodules",
                ]
                .iter()
                .filter(|option| abbreviates(name, option))
                .count()
                    == 1
            {
                index += 1;
            }
            continue;
        }
        if options && arg.starts_with('-') && arg.len() > 1 {
            for (offset, flag) in arg.char_indices().skip(1) {
                match flag {
                    'f' => return Some(RemoteRefRewrite::Force),
                    'd' => rewrite = Some(RemoteRefRewrite::Delete),
                    // `-o` takes the rest of the cluster, or the next token.
                    'o' => {
                        if offset + 1 == arg.len() {
                            index += 1;
                        }
                        break;
                    }
                    _ => {}
                }
            }
            continue;
        }
        if arg.starts_with('+') {
            return Some(RemoteRefRewrite::Force);
        }
        // A bare `:` pushes matching branches without forcing.
        if arg.starts_with(':') && arg != ":" {
            rewrite = Some(RemoteRefRewrite::Delete);
        }
    }
    rewrite
}

/// Whether any command in `analysis`, including one nested in a `bash -c`
/// payload or behind a wrapper such as `env` or `sudo`, is a `git push` that
/// can overwrite or delete a remote ref.
pub(super) fn analysis_rewrites_remote_refs(analysis: &ShellAnalysis) -> bool {
    analysis_has_git_invocation(analysis, 0, &|sub, rest| {
        sub == "push" && git_push_rewrite(rest).is_some()
    })
}

/// Whether any command in `analysis`, unwrapped the same way, discards
/// uncommitted work: worktree edits, untracked files, or stash entries.
pub(super) fn analysis_discards_local_changes(analysis: &ShellAnalysis) -> bool {
    analysis_has_git_invocation(analysis, 0, &git_discards_local_changes)
}

fn analysis_has_git_invocation(
    analysis: &ShellAnalysis,
    depth: usize,
    matches: &dyn Fn(&str, &[String]) -> bool,
) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    analysis.stages.iter().any(|stage| {
        let tokens = &stage.argv;
        let command_index = unwrapped_command_index(tokens, 0, tokens.len());
        let Some(command) = tokens.get(command_index) else {
            return false;
        };
        let args = &tokens[command_index + 1..];
        match command_basename(command) {
            "git" => git_subcommand(args).is_some_and(|(sub, rest)| matches(sub, rest)),
            "bash" | "sh" | "zsh" => shell_c_script(args).is_some_and(|script| {
                analysis_has_git_invocation(&analyze_shell(script), depth + 1, matches)
            }),
            _ => false,
        }
    })
}

/// `git checkout` operands before any `--`, skipping the branch name that
/// `-b`/`-B`/`--orphan` consume. A second operand is a pathspec: `git checkout
/// <tree-ish> <path>` overwrites that path.
fn checkout_operands(rest: &[String]) -> impl Iterator<Item = &String> {
    let mut skip_next = false;
    rest.iter()
        .take_while(|arg| arg.as_str() != "--")
        .filter(move |arg| {
            if std::mem::take(&mut skip_next) {
                return false;
            }
            if matches!(arg.as_str(), "-b" | "-B" | "--orphan") {
                skip_next = true;
            }
            !arg.starts_with('-')
        })
}

/// Classify a git subcommand that throws away work git cannot give back:
/// uncommitted worktree edits, untracked files, or stash entries.
///
/// This is an approval label, not part of the never-approvable floor:
/// reverting one file the agent just edited is routine, so a person decides.
/// `git reset --hard` and `git clean -fd` also carry the label, and the floor
/// still denies them. Stashing itself is not a discard; `git stash list`
/// recovers it. Only the operands written in argv are visible here, so `git
/// checkout name` is read as a branch switch unless the name is a pathspec
/// that cannot be a branch (`.`, `./x`, `:/x`, a glob).
fn git_discards_local_changes(subcommand: &str, rest: &[String]) -> bool {
    let has = |long: &str, short: Option<char>| {
        rest.iter().any(|arg| {
            arg == long
                || short.is_some_and(|flag| {
                    arg.starts_with('-')
                        && !arg.starts_with("--")
                        && arg.chars().skip(1).any(|c| c == flag)
                })
        })
    };
    let pathspec_like = |arg: &String| {
        arg == "."
            || arg.starts_with("./")
            || arg.starts_with(":/")
            || arg.starts_with(":(")
            || arg.contains(['*', '?', '['])
    };
    let after_double_dash = || {
        rest.iter()
            .position(|arg| arg == "--")
            .is_some_and(|index| index + 1 < rest.len())
    };
    match subcommand {
        "checkout" => {
            has("--force", Some('f'))
                || has("--ours", None)
                || has("--theirs", None)
                || rest
                    .iter()
                    .any(|arg| arg.starts_with("--pathspec-from-file"))
                || after_double_dash()
                || checkout_operands(rest).any(pathspec_like)
                || checkout_operands(rest).count() > 1
        }
        // `restore` writes the worktree unless only `--staged` is given, and
        // `--staged` alone only unstages: the edits stay on disk.
        "restore" => {
            let staged = has("--staged", Some('S'));
            let worktree = has("--worktree", Some('W'));
            !staged || worktree
        }
        "switch" => has("--discard-changes", None) || has("--force", Some('f')),
        "reset" => has("--hard", None),
        "clean" => has("--force", Some('f')),
        "stash" => rest
            .iter()
            .find(|arg| !arg.starts_with('-'))
            .is_some_and(|action| matches!(action.as_str(), "drop" | "clear")),
        _ => false,
    }
}

fn git_catastrophe(args: &[String]) -> Option<String> {
    let (subcommand, rest) = git_subcommand(args)?;
    match subcommand {
        "reset" => rest.iter().any(|a| a == "--hard").then(|| {
            "`git reset --hard` is blocked: it discards all uncommitted work. Commit or stash first, then reset on a feature branch.".to_string()
        }),
        "clean" => {
            let mut force = false;
            let mut dirs = false;
            for arg in rest {
                if arg == "--force" {
                    force = true;
                } else if arg == "-d" || arg == "--directory" {
                    dirs = true;
                } else if arg.starts_with('-') && !arg.starts_with("--") {
                    for flag in arg.chars().skip(1) {
                        match flag {
                            'f' => force = true,
                            'd' => dirs = true,
                            _ => {}
                        }
                    }
                }
            }
            (force && dirs).then(|| {
                "`git clean -fd`/`-fdx` is blocked: it permanently deletes untracked files and directories. Inspect with `git clean -nd` first, or remove specific paths.".to_string()
            })
        }
        "push" => (git_push_rewrite(rest) == Some(RemoteRefRewrite::Force)).then(|| {
            "force-push (`--force` / `-f` / `--force-with-lease` / `--mirror` / a `+ref` refspec) is blocked: it can rewrite shared history. Push without forcing, or perform the force-push yourself after review.".to_string()
        }),
        _ => None,
    }
}

fn rm_escape_catastrophe(args: &[String], roots: &[String]) -> Option<String> {
    let mut force = false;
    let mut recursive = false;
    let mut targets: Vec<&str> = Vec::new();
    let mut parsing_options = true;
    for arg in args {
        if parsing_options && arg == "--" {
            parsing_options = false;
            continue;
        }
        if parsing_options && arg.starts_with("--") {
            force = force || arg == "--force";
            recursive = recursive || arg == "--recursive";
            continue;
        }
        if parsing_options && arg.starts_with('-') && arg != "-" {
            for flag in arg.chars().skip(1) {
                force = force || flag == 'f';
                recursive = recursive || flag == 'r' || flag == 'R';
            }
            continue;
        }
        parsing_options = false;
        targets.push(arg);
    }
    if !(force && recursive) {
        return None;
    }
    for target in targets {
        if path_escapes_root(target, roots) {
            return Some(format!(
                "`rm -rf` of `{target}` is blocked: it targets an absolute path or escapes the project root. Delete only paths inside the workspace, and prefer a scoped removal."
            ));
        }
    }
    None
}

fn dd_catastrophe(args: &[String]) -> Option<String> {
    args.iter().any(|a| a.starts_with("of=")).then(|| {
        "`dd of=…` is blocked: a raw block-device/file overwrite is irreversible.".to_string()
    })
}

fn chmod_catastrophe(args: &[String]) -> Option<String> {
    let recursive = args.iter().any(|a| {
        a == "-R"
            || a == "--recursive"
            || (a.starts_with('-') && !a.starts_with("--") && a.contains('R'))
    });
    let strips_all = args.iter().any(|a| a == "000" || a == "0000");
    (recursive && strips_all).then(|| {
        "`chmod -R 000` is blocked: recursively stripping all permissions can lock you out of the tree.".to_string()
    })
}

fn path_escapes_root(target: &str, roots: &[String]) -> bool {
    let cleaned = strip_trailing_slashes(target);
    if cleaned.is_empty() {
        return false;
    }
    // Home / root / device targets always escape regardless of root.
    if cleaned == "~"
        || cleaned.starts_with("~/")
        || cleaned == "/"
        || cleaned == "/*"
        || cleaned.starts_with("$HOME")
        || cleaned.starts_with("${HOME}")
    {
        return true;
    }
    if cleaned.starts_with('/') {
        // Absolute: escapes UNLESS it resolves inside one of the roots. No
        // root context → conservatively treat as an escape.
        if roots.is_empty() {
            return true;
        }
        return !roots.iter().any(|root| {
            let root = strip_trailing_slashes(root);
            cleaned == root || cleaned.starts_with(&format!("{root}/"))
        });
    }
    relative_path_escapes(cleaned)
}

fn relative_path_escapes(path: &str) -> bool {
    let mut depth: i32 = 0;
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

#[expect(
    clippy::string_slice,
    reason = "end sits before an ASCII '/' byte or at value.len()"
)]
fn strip_trailing_slashes(value: &str) -> &str {
    let mut end = value.len();
    while end > 1 && value.as_bytes()[end - 1] == b'/' {
        end -= 1;
    }
    &value[..end]
}
