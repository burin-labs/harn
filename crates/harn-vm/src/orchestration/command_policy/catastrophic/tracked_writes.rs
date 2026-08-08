use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn truncate_catastrophe(
    args: &[String],
    active_cwd: Option<&Path>,
    workspace_roots: &[String],
) -> Option<String> {
    let mut size_zero = false;
    let mut targets: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "-s" || arg == "--size" {
            if args
                .get(index + 1)
                .map(|value| value.trim_start_matches(['+', '-']))
                == Some("0")
            {
                size_zero = true;
            }
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-s") {
            size_zero |= value.trim_start_matches(['+', '-']) == "0";
            index += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--size=") {
            size_zero |= value.trim_start_matches(['+', '-']) == "0";
            index += 1;
            continue;
        }
        if !arg.starts_with('-') {
            targets.push(arg);
        }
        index += 1;
    }

    (size_zero
        && targets
            .iter()
            .any(|target| is_protected_project_file(target, active_cwd, workspace_roots)))
    .then(|| {
        "`truncate -s 0` of a tracked project file is blocked: it would erase the file's contents. Use the edit tool to rewrite it.".to_string()
    })
}

pub(super) fn redirect_over_tracked_reason(
    segment: &str,
    active_cwd: Option<&Path>,
    workspace_roots: &[String],
) -> Option<String> {
    let chars: Vec<char> = segment.chars().collect();
    let mut in_single = false;
    let mut in_double = false;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        match ch {
            '\\' if !in_single => {
                index += 2;
                continue;
            }
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '>' if !in_single && !in_double => {
                let mut cursor = index + 1;
                if cursor < chars.len() && matches!(chars[cursor], '>' | '|') {
                    cursor += 1;
                }
                while cursor < chars.len() && chars[cursor].is_whitespace() {
                    cursor += 1;
                }
                if cursor < chars.len() && chars[cursor] == '&' {
                    index = cursor + 1;
                    continue;
                }
                let (target, end) = redirect_target(&chars, cursor);
                if is_protected_project_file(&target, active_cwd, workspace_roots) {
                    return Some(format!(
                        "shell redirection (`>`/`>>`) onto the tracked project file `{target}` is blocked: it would replace or append to reviewed project state. Use the edit tool to change it."
                    ));
                }
                index = end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn redirect_target(chars: &[char], mut cursor: usize) -> (String, usize) {
    let mut target = String::new();
    let mut in_single = false;
    let mut in_double = false;
    while cursor < chars.len() {
        let ch = chars[cursor];
        match ch {
            '\\' if !in_single => {
                cursor += 1;
                if let Some(escaped) = chars.get(cursor) {
                    target.push(*escaped);
                }
            }
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            _ if !in_single
                && !in_double
                && (ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | '<' | '>')) =>
            {
                break;
            }
            _ => target.push(ch),
        }
        cursor += 1;
    }
    (target, cursor)
}

fn is_protected_project_file(
    target: &str,
    active_cwd: Option<&Path>,
    _workspace_roots: &[String],
) -> bool {
    if target.contains(['$', '*', '?', '[', ']', '{', '}', '~']) {
        // This floor is never approvable, so unresolved shell expansion must
        // not turn a possible tracked path into a hard denial. Literal targets
        // still use Git state below; the sandbox and approval layer own dynamic
        // targets after the shell resolves them.
        return false;
    }
    let Some(cwd) = active_cwd else {
        return true;
    };
    let target = resolved_target(cwd, target);
    match git_tracks_file(cwd, &target) {
        Some(tracked) => tracked,
        None if target.is_absolute() && !target.starts_with(cwd) => false,
        None => target.symlink_metadata().is_ok(),
    }
}

fn resolved_target(cwd: &Path, target: &str) -> PathBuf {
    let path = Path::new(target);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn git_tracks_file(cwd: &Path, target: &Path) -> Option<bool> {
    let root = git_root(cwd)?;
    let absolute_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        cwd.join(target)
    };
    let absolute_target = canonical_target(&absolute_target)?;
    let Ok(relative) = absolute_target.strip_prefix(&root) else {
        return Some(false);
    };
    let output = git_command(&root)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(relative)
        .output()
        .ok()?;
    Some(output.status.success())
}

fn git_root(cwd: &Path) -> Option<PathBuf> {
    let root_output = git_command(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !root_output.status.success() {
        return None;
    }
    PathBuf::from(String::from_utf8(root_output.stdout).ok()?.trim())
        .canonicalize()
        .ok()
}

fn canonical_target(target: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = target.canonicalize() {
        return Some(canonical);
    }
    let parent = target.parent()?.canonicalize().ok()?;
    Some(parent.join(target.file_name()?))
}

fn git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX");
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_files_are_writable_but_tracked_overwrites_are_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        init_git(cwd);

        assert!(redirect_over_tracked_reason("printf x > generated.rs", Some(cwd), &[]).is_none());
        std::fs::write(cwd.join("generated.rs"), "old").unwrap();
        git(cwd, &["add", "generated.rs"]);
        assert!(redirect_over_tracked_reason("printf x > generated.rs", Some(cwd), &[]).is_some());
    }

    #[test]
    fn new_files_are_writable_but_tracked_truncation_is_blocked() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        init_git(cwd);
        let args = [
            "-s".to_string(),
            "0".to_string(),
            "generated.harn".to_string(),
        ];

        assert!(truncate_catastrophe(&args, Some(cwd), &[]).is_none());
        std::fs::write(cwd.join("generated.harn"), "old").unwrap();
        git(cwd, &["add", "generated.harn"]);
        assert!(truncate_catastrophe(&args, Some(cwd), &[]).is_some());
    }

    #[test]
    fn untracked_generated_files_remain_writable() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        init_git(cwd);
        std::fs::write(cwd.join("generated.rs"), "old").unwrap();

        assert!(redirect_over_tracked_reason("printf x > generated.rs", Some(cwd), &[]).is_none());
    }

    #[test]
    fn files_outside_the_project_are_not_project_state() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        init_git(cwd);

        assert!(redirect_over_tracked_reason("printf x > /dev/null", Some(cwd), &[]).is_none());
    }

    #[test]
    fn unresolved_shell_expansions_are_not_hard_denied() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();

        assert!(redirect_over_tracked_reason(
            "printf x > \"$HARN_OUTPUTS_DIR/result.txt\"",
            Some(cwd),
            &[],
        )
        .is_none());
        assert!(redirect_over_tracked_reason("printf x > $OUTPUT", None, &[]).is_none());

        init_git(cwd);
        assert!(
            redirect_over_tracked_reason("printf x > $PWD/result.txt", Some(cwd), &[],).is_none()
        );
        assert!(redirect_over_tracked_reason(
            "printf x > \"$workspace/harn.toml\"",
            Some(cwd),
            &[cwd.display().to_string()],
        )
        .is_none());
    }

    #[test]
    fn absolute_paths_outside_a_non_git_cwd_are_not_project_state() {
        assert!(redirect_over_tracked_reason(
            "printf x > /dev/null",
            Some(Path::new("/workspace/project")),
            &[],
        )
        .is_none());
    }

    #[test]
    fn deleted_tracked_files_remain_protected() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        init_git(cwd);
        std::fs::write(cwd.join("tracked-without-extension"), "old").unwrap();
        git(cwd, &["add", "tracked-without-extension"]);
        std::fs::remove_file(cwd.join("tracked-without-extension")).unwrap();

        assert!(redirect_over_tracked_reason(
            "printf x > tracked-without-extension",
            Some(cwd),
            &[],
        )
        .is_some());
    }

    #[test]
    fn quoted_and_escaped_tracked_targets_remain_protected() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        init_git(cwd);
        std::fs::write(cwd.join("tracked file"), "old").unwrap();
        git(cwd, &["add", "tracked file"]);

        assert!(
            redirect_over_tracked_reason("printf x >| \"tracked file\"", Some(cwd), &[]).is_some()
        );
        assert!(
            redirect_over_tracked_reason("printf x > tracked\\ file", Some(cwd), &[]).is_some()
        );
    }

    fn init_git(root: &Path) {
        git(root, &["init", "-q"]);
    }

    fn git(root: &Path, args: &[&str]) {
        let status = git_command(root).args(args).status().unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}
