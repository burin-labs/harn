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
        let named = targets
            .iter()
            .find_map(|target| {
                protected_project_file_reason(target, active_cwd, workspace_roots)
                    .map(|reason| reason.describe(target))
            })
            .unwrap_or_else(|| "the target".to_string());
        format!("`truncate -s 0` is blocked: it would erase {named}")
    })
}

/// Why a write target is protected by the never-approvable floor.
///
/// The floor blocks all three, but they are not the same finding and a reader
/// acts on them differently. Naming only the first sent readers hunting a Git
/// tracking problem in workspaces that have no Git at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProtectedTargetReason {
    /// Git tracks the file: overwriting it replaces reviewed project state.
    GitTracked,
    /// No Git tracking is available and the file already exists. The floor
    /// cannot tell reviewed state from scratch output, so it refuses.
    ExistsWithoutGit,
    /// No execution root is known, so the target cannot be resolved at all.
    Unresolvable,
}

impl ProtectedTargetReason {
    /// Why this specific target is refused, in the reader's terms.
    fn describe(self, target: &str) -> String {
        match self {
            Self::GitTracked => format!(
                "`{target}` is tracked by Git, so writing it would replace reviewed project state. Use the edit tool to change it."
            ),
            Self::ExistsWithoutGit => format!(
                "`{target}` already exists and this workspace has no Git tracking to consult, so the floor cannot tell reviewed project state from scratch output. Use the edit tool to change it, or write to a path that does not exist yet."
            ),
            Self::Unresolvable => format!(
                "`{target}` cannot be resolved because no execution root is known, so whether it is project state cannot be established."
            ),
        }
    }
}

pub(super) fn redirect_target_over_tracked_reason(
    target: &str,
    active_cwd: Option<&Path>,
    workspace_roots: &[String],
) -> Option<String> {
    let reason = protected_project_file_reason(target, active_cwd, workspace_roots)?;
    Some(format!(
        "shell redirection onto {} is blocked.",
        reason.describe(target)
    ))
}

fn protected_project_file_reason(
    target: &str,
    active_cwd: Option<&Path>,
    _workspace_roots: &[String],
) -> Option<ProtectedTargetReason> {
    if target.contains(['$', '*', '?', '[', ']', '{', '}', '~']) {
        // This floor is never approvable, so unresolved shell expansion must
        // not turn a possible tracked path into a hard denial. Literal targets
        // still use Git state below; the sandbox and approval layer own dynamic
        // targets after the shell resolves them.
        return None;
    }
    let Some(cwd) = active_cwd else {
        return Some(ProtectedTargetReason::Unresolvable);
    };
    let target = resolved_target(cwd, target);
    match git_tracks_file(cwd, &target) {
        Some(true) => Some(ProtectedTargetReason::GitTracked),
        Some(false) => None,
        None if target.is_absolute() && !target.starts_with(cwd) => None,
        None => target
            .symlink_metadata()
            .is_ok()
            .then_some(ProtectedTargetReason::ExistsWithoutGit),
    }
}

fn is_protected_project_file(
    target: &str,
    active_cwd: Option<&Path>,
    workspace_roots: &[String],
) -> bool {
    protected_project_file_reason(target, active_cwd, workspace_roots).is_some()
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

        assert!(redirect_target_over_tracked_reason("generated.rs", Some(cwd), &[]).is_none());
        std::fs::write(cwd.join("generated.rs"), "old").unwrap();
        git(cwd, &["add", "generated.rs"]);
        assert!(redirect_target_over_tracked_reason("generated.rs", Some(cwd), &[]).is_some());
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

        assert!(redirect_target_over_tracked_reason("generated.rs", Some(cwd), &[]).is_none());
    }

    #[test]
    fn files_outside_the_project_are_not_project_state() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        init_git(cwd);

        assert!(redirect_target_over_tracked_reason("/dev/null", Some(cwd), &[]).is_none());
    }

    #[test]
    fn unresolved_shell_expansions_are_not_hard_denied() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();

        assert!(redirect_target_over_tracked_reason(
            "$HARN_OUTPUTS_DIR/result.txt",
            Some(cwd),
            &[],
        )
        .is_none());
        assert!(redirect_target_over_tracked_reason("$OUTPUT", None, &[]).is_none());

        init_git(cwd);
        assert!(redirect_target_over_tracked_reason("$PWD/result.txt", Some(cwd), &[],).is_none());
        assert!(redirect_target_over_tracked_reason(
            "$workspace/harn.toml",
            Some(cwd),
            &[cwd.display().to_string()],
        )
        .is_none());
    }

    #[test]
    fn absolute_paths_outside_a_non_git_cwd_are_not_project_state() {
        assert!(redirect_target_over_tracked_reason(
            "/dev/null",
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

        assert!(
            redirect_target_over_tracked_reason("tracked-without-extension", Some(cwd), &[],)
                .is_some()
        );
    }

    #[test]
    fn normalized_tracked_destination_remains_protected() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        init_git(cwd);
        std::fs::write(cwd.join("tracked file"), "old").unwrap();
        git(cwd, &["add", "tracked file"]);

        assert!(redirect_target_over_tracked_reason("tracked file", Some(cwd), &[]).is_some());
    }

    /// THE MISDIAGNOSIS THIS CLOSES.
    ///
    /// Outside a Git repository the floor has no tracking to consult and treats
    /// every existing file as protected. That is the conservative choice, but
    /// the refusal used to say the target was a "tracked project file" in a
    /// workspace with no tracking at all, sending readers to hunt a Git problem
    /// that does not exist. The block is unchanged; only the reason is now true.
    #[test]
    fn an_existing_file_without_git_is_refused_by_existence_not_by_tracking() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        // Deliberately NOT a Git repository.
        assert!(redirect_target_over_tracked_reason("proof.txt", Some(cwd), &[]).is_none());
        std::fs::write(cwd.join("proof.txt"), "first").unwrap();

        let reason = redirect_target_over_tracked_reason("proof.txt", Some(cwd), &[])
            .expect("a second write to an existing file is still refused");
        assert!(
            reason.contains("already exists") && reason.contains("no Git tracking"),
            "the refusal must name existence, not tracking: {reason}"
        );
        assert!(
            !reason.contains("tracked by Git"),
            "a workspace with no Git must not be told its file is tracked by Git: {reason}"
        );

        let truncate_args = ["-s".to_string(), "0".to_string(), "proof.txt".to_string()];
        let truncate_reason = truncate_catastrophe(&truncate_args, Some(cwd), &[])
            .expect("truncation of an existing file is still refused");
        assert!(
            truncate_reason.contains("already exists"),
            "truncation names the same reason as redirection: {truncate_reason}"
        );
    }

    /// DIRECTION CONTROL. A genuinely tracked file keeps the tracking reason,
    /// so the new wording cannot swallow the case it was written for.
    #[test]
    fn a_git_tracked_file_is_still_refused_for_being_tracked() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path();
        init_git(cwd);
        std::fs::write(cwd.join("reviewed.rs"), "old").unwrap();
        git(cwd, &["add", "reviewed.rs"]);

        let reason = redirect_target_over_tracked_reason("reviewed.rs", Some(cwd), &[])
            .expect("a tracked file is refused");
        assert!(
            reason.contains("tracked by Git"),
            "a tracked file keeps its own reason: {reason}"
        );
        assert!(
            !reason.contains("already exists"),
            "the existence wording must not displace the tracking finding: {reason}"
        );
    }

    /// DIRECTION CONTROL. With no execution root nothing can be resolved, and
    /// that is its own finding rather than either of the other two.
    #[test]
    fn an_unresolvable_target_says_so_rather_than_claiming_tracking() {
        let reason = redirect_target_over_tracked_reason("anything.txt", None, &[])
            .expect("an unresolvable target is still refused");
        assert!(
            reason.contains("no execution root"),
            "the refusal names the missing root: {reason}"
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
