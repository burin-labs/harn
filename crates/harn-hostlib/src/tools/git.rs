//! `tools/git` — read-only git inspection.
//!
//! Per the issue plan we'd prefer `gix`, but the scaffold's `Cargo.toml`
//! intentionally omits it because gix 0.82 fails to compile on the repo's
//! current rustc. To avoid blocking the rest of the deterministic-tool
//! work on a toolchain bump, this module shells out to the system `git`
//! binary using `Command` with an **arg-list invocation only** (never
//! `sh -c <string>`). That keeps the surface free of shell injection and
//! preserves the contract documented in `schemas/tools/git.{request,response}.json`.
//! When B2's toolchain bump unblocks `gix`, individual operations can be
//! migrated one at a time without touching the public surface.

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_int, optional_string, require_string, str_value,
};

const BUILTIN: &str = "hostlib_tools_git";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Status,
    Diff,
    Log,
    Blame,
    Show,
    BranchList,
    CurrentBranch,
    RemoteList,
}

impl Operation {
    fn parse(raw: &str) -> Result<Self, HostlibError> {
        match raw {
            "status" => Ok(Operation::Status),
            "diff" => Ok(Operation::Diff),
            "log" => Ok(Operation::Log),
            "blame" => Ok(Operation::Blame),
            "show" => Ok(Operation::Show),
            "branch_list" => Ok(Operation::BranchList),
            "current_branch" => Ok(Operation::CurrentBranch),
            "remote_list" => Ok(Operation::RemoteList),
            other => Err(HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "operation",
                message: format!(
                    "unknown operation `{other}`; expected one of \
                    [status, diff, log, blame, show, branch_list, current_branch, remote_list]"
                ),
            }),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Operation::Status => "status",
            Operation::Diff => "diff",
            Operation::Log => "log",
            Operation::Blame => "blame",
            Operation::Show => "show",
            Operation::BranchList => "branch_list",
            Operation::CurrentBranch => "current_branch",
            Operation::RemoteList => "remote_list",
        }
    }
}

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let operation_raw = require_string(BUILTIN, dict, "operation")?;
    let operation = Operation::parse(&operation_raw)?;

    let repo = optional_string(BUILTIN, dict, "repo")?
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let path = optional_string(BUILTIN, dict, "path")?;
    let rev = optional_string(BUILTIN, dict, "rev")?;
    let rev_range = optional_string(BUILTIN, dict, "rev_range")?;
    let max_count = optional_int(BUILTIN, dict, "max_count", 0)?;
    if max_count < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_count",
            message: "must be >= 0".to_string(),
        });
    }

    let data = match operation {
        Operation::Status => run_status(&repo)?,
        Operation::Diff => run_diff(&repo, path.as_deref(), rev.as_deref())?,
        Operation::Log => run_log(
            &repo,
            path.as_deref(),
            rev.as_deref(),
            rev_range.as_deref(),
            max_count,
        )?,
        Operation::Blame => run_blame(&repo, path.as_deref(), rev.as_deref())?,
        Operation::Show => run_show(&repo, rev.as_deref())?,
        Operation::BranchList => run_branch_list(&repo)?,
        Operation::CurrentBranch => run_current_branch(&repo)?,
        Operation::RemoteList => run_remote_list(&repo)?,
    };

    Ok(build_dict([
        ("operation", str_value(operation.as_str())),
        ("repo", str_value(repo.to_string_lossy())),
        ("data", data),
    ]))
}

fn run_status(repo: &PathBuf) -> Result<VmValue, HostlibError> {
    let stdout = run_git(repo, &["status", "--porcelain=v1", "-z"])?;
    let mut entries: Vec<VmValue> = Vec::new();
    for raw_entry in stdout.split('\0') {
        if raw_entry.len() < 3 {
            continue;
        }
        let bytes = raw_entry.as_bytes();
        // Format is `XY <path>` where X and Y are status codes for index/worktree.
        let index_status = bytes[0] as char;
        let worktree_status = bytes[1] as char;
        let path = String::from_utf8_lossy(&bytes[3..]).into_owned();
        entries.push(build_dict([
            ("index", str_value(index_status.to_string())),
            ("worktree", str_value(worktree_status.to_string())),
            ("path", str_value(path)),
        ]));
    }
    Ok(VmValue::List(Rc::new(entries)))
}

fn run_diff(
    repo: &PathBuf,
    path: Option<&str>,
    rev: Option<&str>,
) -> Result<VmValue, HostlibError> {
    let mut argv: Vec<String> = vec!["diff".to_string()];
    if let Some(rev) = rev {
        validate_rev(rev)?;
        argv.push(rev.to_string());
    }
    argv.push("--".to_string());
    if let Some(path) = path {
        argv.push(path.to_string());
    }
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = run_git(repo, &argv_refs)?;
    Ok(str_value(&stdout))
}

fn run_log(
    repo: &PathBuf,
    path: Option<&str>,
    rev: Option<&str>,
    rev_range: Option<&str>,
    max_count: i64,
) -> Result<VmValue, HostlibError> {
    let mut argv: Vec<String> = vec![
        "log".to_string(),
        "--pretty=format:%H%x1f%an%x1f%ae%x1f%aI%x1f%s%x1e".to_string(),
    ];
    if max_count > 0 {
        argv.push(format!("-{}", max_count));
    }
    if let Some(rev) = rev {
        validate_rev(rev)?;
        argv.push(rev.to_string());
    } else if let Some(range) = rev_range {
        validate_rev(range)?;
        argv.push(range.to_string());
    }
    if let Some(path) = path {
        argv.push("--".to_string());
        argv.push(path.to_string());
    }
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = run_git(repo, &argv_refs)?;
    let mut commits: Vec<VmValue> = Vec::new();
    for record in stdout.split('\u{001e}') {
        let trimmed = record.trim_matches(['\n']);
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('\u{001f}').collect();
        if parts.len() != 5 {
            continue;
        }
        commits.push(build_dict([
            ("sha", str_value(parts[0])),
            ("author_name", str_value(parts[1])),
            ("author_email", str_value(parts[2])),
            ("author_date", str_value(parts[3])),
            ("subject", str_value(parts[4])),
        ]));
    }
    Ok(VmValue::List(Rc::new(commits)))
}

fn run_blame(
    repo: &PathBuf,
    path: Option<&str>,
    rev: Option<&str>,
) -> Result<VmValue, HostlibError> {
    let path = path.ok_or(HostlibError::MissingParameter {
        builtin: BUILTIN,
        param: "path",
    })?;
    let mut argv: Vec<String> = vec!["blame".to_string(), "--line-porcelain".to_string()];
    if let Some(rev) = rev {
        validate_rev(rev)?;
        argv.push(rev.to_string());
    }
    argv.push("--".to_string());
    argv.push(path.to_string());
    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let stdout = run_git(repo, &argv_refs)?;

    let mut blame_entries: Vec<VmValue> = Vec::new();
    let mut current_sha = String::new();
    let mut current_author = String::new();
    let mut line_no: i64 = 0;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("author ") {
            current_author = rest.to_string();
        } else if line.len() >= 40 && line.chars().take(40).all(|c| c.is_ascii_hexdigit()) {
            current_sha = line[..40].to_string();
        } else if let Some(stripped) = line.strip_prefix('\t') {
            line_no += 1;
            blame_entries.push(build_dict([
                ("line", VmValue::Int(line_no)),
                ("sha", str_value(&current_sha)),
                ("author", str_value(&current_author)),
                ("text", str_value(stripped)),
            ]));
        }
    }
    Ok(VmValue::List(Rc::new(blame_entries)))
}

fn run_show(repo: &PathBuf, rev: Option<&str>) -> Result<VmValue, HostlibError> {
    let rev = rev.ok_or(HostlibError::MissingParameter {
        builtin: BUILTIN,
        param: "rev",
    })?;
    validate_rev(rev)?;
    let stdout = run_git(repo, &["show", "--stat", "--patch", rev])?;
    Ok(str_value(&stdout))
}

fn run_branch_list(repo: &PathBuf) -> Result<VmValue, HostlibError> {
    // `for-each-ref --format` does not honor `%x1f` literal-byte escapes
    // (those are `git log`-only). Use a tab as the separator since branch
    // names cannot contain tabs.
    let stdout = run_git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)\t%(objectname)",
            "refs/heads/",
        ],
    )?;
    let mut branches: Vec<VmValue> = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(2, '\t').collect();
        if parts.len() != 2 {
            continue;
        }
        branches.push(build_dict([
            ("name", str_value(parts[0])),
            ("sha", str_value(parts[1])),
        ]));
    }
    Ok(VmValue::List(Rc::new(branches)))
}

fn run_current_branch(repo: &PathBuf) -> Result<VmValue, HostlibError> {
    let stdout = run_git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .or_else(|_| run_git(repo, &["rev-parse", "--short", "HEAD"]))?;
    Ok(str_value(stdout.trim()))
}

fn run_remote_list(repo: &PathBuf) -> Result<VmValue, HostlibError> {
    let stdout = run_git(repo, &["remote", "-v"])?;
    let mut remotes: Vec<VmValue> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let name = parts[0];
        let url = parts[1];
        if seen.contains(&name.to_string()) {
            continue;
        }
        seen.push(name.to_string());
        remotes.push(build_dict([
            ("name", str_value(name)),
            ("url", str_value(url)),
        ]));
    }
    Ok(VmValue::List(Rc::new(remotes)))
}

/// Validate a user-supplied revision/refspec/range. We forward the value
/// to `git` as a positional argument (never via shell), so the only
/// classes of injection we need to defend against are arguments that
/// could be misinterpreted as flags. Reject anything starting with `-`
/// (so `--exec` and friends can't sneak through), and anything containing
/// NUL or newlines.
fn validate_rev(rev: &str) -> Result<(), HostlibError> {
    if rev.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "rev",
            message: "must not be empty".to_string(),
        });
    }
    if rev.starts_with('-') {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "rev",
            message: "must not start with `-` (would be parsed as a flag by git)".to_string(),
        });
    }
    if rev.contains('\0') || rev.contains('\n') {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "rev",
            message: "must not contain NUL or newline".to_string(),
        });
    }
    Ok(())
}

fn run_git(repo: &PathBuf, args: &[&str]) -> Result<String, HostlibError> {
    let mut cmd = Command::new("git");
    // Strip ambient `GIT_*` environment variables so that being invoked
    // from inside a parent `git` process (e.g. a pre-push hook running
    // tests, or a git alias) doesn't leak `GIT_DIR` / `GIT_INDEX_FILE` /
    // etc. into our isolated repo paths.
    for (key, _) in std::env::vars() {
        if key.starts_with("GIT_") {
            cmd.env_remove(&key);
        }
    }
    cmd.arg("-C").arg(repo);
    for arg in args {
        cmd.arg(arg);
    }
    let output = cmd.output().map_err(|err| HostlibError::Backend {
        builtin: BUILTIN,
        message: format!(
            "spawn `git {} ...`: {err}",
            args.first().copied().unwrap_or("?")
        ),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(HostlibError::Backend {
            builtin: BUILTIN,
            message: format!(
                "git {} exited with status {}: {}",
                args.first().copied().unwrap_or("?"),
                output.status,
                stderr.trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rev_rejects_flag_lookalikes() {
        assert!(validate_rev("--exec=rm").is_err());
        assert!(validate_rev("-x").is_err());
    }

    #[test]
    fn validate_rev_rejects_control_bytes() {
        assert!(validate_rev("HEAD\nrm").is_err());
        assert!(validate_rev("HEAD\0").is_err());
    }

    #[test]
    fn validate_rev_accepts_normal_inputs() {
        for ok in ["HEAD", "main", "abc1234", "v1.2.3", "main..feature"] {
            assert!(validate_rev(ok).is_ok(), "{ok} should be accepted");
        }
    }

    #[test]
    fn operation_parse_is_total() {
        assert!(Operation::parse("status").is_ok());
        assert!(Operation::parse("nope").is_err());
    }
}
