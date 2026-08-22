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
    analyze_argv, analyze_shell, analyze_shell_dialect, ShellAnalysis, ShellDialect,
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

fn git_catastrophe(args: &[String]) -> Option<String> {
    // Skip leading git global options; value-taking ones consume a token.
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        match token.as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
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
    let rest = &args[(index + 1).min(args.len())..];
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
        "push" => {
            let force = rest.iter().any(|a| {
                a == "--force"
                    || a == "-f"
                    || a == "--force-with-lease"
                    || a.starts_with("--force-with-lease=")
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains('f'))
            });
            force.then(|| {
                "force-push (`git push --force` / `-f` / `--force-with-lease`) is blocked: it can rewrite shared history. Push without `--force`, or perform the force-push yourself after review.".to_string()
            })
        }
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
