//! Workspace-effect classification for tool-batch phase planning.
//!
//! The agent loop groups a tool batch into phases so that observations run
//! before the mutations they justify. That ordering is only sound if
//! "observation" means *this command does not change the workspace*.
//!
//! The obvious shortcut is to ask the command risk scanner and treat "nothing
//! alarming" as "does not write". That is wrong, and wrong in the unsafe
//! direction: the scanner answers "is this dangerous", not "does this write".
//! `rm src/foo.rs`, `mv a b`, `cp a b`, `git checkout -- .`, `cargo build`,
//! and `make` all clear the danger scanner — none of them is catastrophic, a
//! whole-tree wipe, or an output redirect — yet every one of them mutates the
//! workspace. Classifying them as observations would let a batch run a
//! deletion before the read that was supposed to justify it.
//!
//! So the decision is an allowlist, not a denylist. A command is a read effect
//! only when every stage it runs is a program known to be read-only, nothing
//! redirects to a file, and the scanner is independently happy. Anything the
//! allowlist does not recognize is `unknown`, which keeps it out of the
//! observation phase. New read-only tooling is a one-line data edit here; the
//! cost of forgetting is a missed batching opportunity, never a reordered
//! mutation.

use serde_json::Value as JsonValue;

use crate::value::{VmError, VmValue};

use super::command_risk_scan_json;
use super::scan::{command_text, risk_labels_from_scan, security_command_analysis};

/// Risk labels that positively establish a workspace write.
///
/// Presence proves a write; absence proves nothing, which is the whole reason
/// this module does not stop here.
const WORKSPACE_WRITE_LABELS: [&str; 3] = ["catastrophic", "destructive", "write_intent"];

/// Programs whose every invocation only reads.
///
/// Deliberately excludes build and package tooling (`cargo`, `make`, `npm`,
/// `pnpm`, `go`, `pytest`): those write target directories, lockfiles, and
/// caches even when the user thinks of them as "just checking".
const READ_ONLY_PROGRAMS: &[&str] = &[
    "basename",
    "cat",
    "cksum",
    "cmp",
    "column",
    "comm",
    "cut",
    "df",
    "diff",
    "dirname",
    "du",
    "echo",
    "expr",
    "grep",
    "head",
    "id",
    "jq",
    "ls",
    "md5sum",
    "nl",
    "od",
    "pgrep",
    "printenv",
    "printf",
    "ps",
    "pwd",
    "readlink",
    "realpath",
    "seq",
    "sha1sum",
    "sha256sum",
    "stat",
    "tail",
    "tr",
    "type",
    "uname",
    "wc",
    "which",
    "whoami",
];

/// Subcommand-dispatching programs and the subcommands that only read.
///
/// `git status` reads; `git checkout` writes. The program name alone cannot
/// answer the question, so these are resolved on argv[1].
const READ_ONLY_SUBCOMMANDS: &[(&str, &[&str])] = &[(
    "git",
    &[
        "blame",
        "branch",
        "cat-file",
        "config",
        "describe",
        "diff",
        "grep",
        "log",
        "ls-files",
        "ls-tree",
        "rev-list",
        "rev-parse",
        "shortlog",
        "show",
        "show-ref",
        "status",
        "tag",
        "whatchanged",
    ],
)];

/// `git config --get x` reads; `git config x y` writes. `git branch` lists;
/// `git branch -d` deletes. `git tag` lists; `git tag v1` creates. For these,
/// a bare or explicitly-reading invocation is a read and everything else is
/// not.
fn subcommand_is_read_only(program: &str, subcommand: &str, rest: &[String]) -> bool {
    if rest.iter().any(|arg| {
        arg == "--output"
            || arg.starts_with("--output=")
            || arg == "--ext-diff"
            || arg == "--textconv"
            || arg == "--open-files-in-pager"
            || arg.starts_with("--open-files-in-pager=")
    }) {
        return false;
    }
    match (program, subcommand) {
        ("git", "config") => rest.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "--get" | "--get-all" | "--get-regexp" | "--get-urlmatch" | "--list" | "-l"
            )
        }),
        ("git", "branch") | ("git", "tag") => rest.iter().all(|arg| {
            matches!(
                arg.as_str(),
                "-a" | "--all"
                    | "-l"
                    | "--list"
                    | "-v"
                    | "-vv"
                    | "--verbose"
                    | "--merged"
                    | "--no-merged"
                    | "-r"
                    | "--remotes"
                    | "--show-current"
            )
        }),
        _ => true,
    }
}

/// Some generally read-shaped programs have an explicit execution or output
/// mode. Keep those option grammars beside the allowlist instead of pretending
/// every invocation of the binary has the same effect.
fn standalone_program_is_read_only(program: &str, args: &[String]) -> bool {
    match program {
        "rg" => !args
            .iter()
            .any(|arg| arg == "--pre" || arg.starts_with("--pre=")),
        "sort" => !args.iter().any(|arg| {
            arg == "-o"
                || arg == "--output"
                || arg.starts_with("--output=")
                || arg == "--compress-program"
                || arg.starts_with("--compress-program=")
        }),
        "tree" => !args
            .iter()
            .any(|arg| arg == "-o" || arg.starts_with("--output=")),
        "yq" => !args.iter().any(|arg| {
            arg == "-i"
                || arg == "--inplace"
                || arg == "--split-exp"
                || arg.starts_with("--split-exp=")
                || arg == "--split-exp-file"
                || arg.starts_with("--split-exp-file=")
        }),
        _ => READ_ONLY_PROGRAMS.contains(&program),
    }
}

fn program_basename(argv0: &str) -> &str {
    argv0
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argv0)
        .trim_end_matches(".exe")
}

/// Whether one parsed stage of a pipeline only reads.
fn stage_is_read_only(words: &[String], dynamic: &[bool]) -> bool {
    if words.is_empty() || dynamic.iter().any(|value| *value) {
        return false;
    }
    let program = program_basename(&words[0]);
    if standalone_program_is_read_only(program, &words[1..]) {
        return true;
    }
    if let Some((_, subcommands)) = READ_ONLY_SUBCOMMANDS
        .iter()
        .find(|(name, _)| *name == program)
    {
        // Skip global flags to find the subcommand (`git -C dir status`).
        let mut index = 1;
        while index < words.len() {
            let word = &words[index];
            if word == "-C" || word == "--git-dir" || word == "--work-tree" {
                index += 2;
                continue;
            }
            if word.starts_with('-') {
                index += 1;
                continue;
            }
            break;
        }
        let Some(subcommand) = words.get(index) else {
            return false;
        };
        return subcommands.contains(&subcommand.as_str())
            && subcommand_is_read_only(program, subcommand, &words[index + 1..]);
    }
    false
}

/// Workspace-effect class used by tool-batch phase planning.
///
/// `read_effect` means every stage is a recognized read-only program, so the
/// call can share an observation batch. `write_effect` means a write was
/// positively established. `unknown` covers everything else, including
/// commands that are merely unrecognized — those stay out of the observation
/// phase rather than being assumed harmless.
pub fn command_workspace_effect_json(ctx: &JsonValue) -> JsonValue {
    let command = command_text(ctx);
    if command.trim().is_empty() {
        return serde_json::json!({
            "effect": "unknown",
            "risk_labels": [],
        });
    }
    let scan = command_risk_scan_json(ctx, None);
    let labels = risk_labels_from_scan(&scan);
    if labels
        .iter()
        .any(|label| WORKSPACE_WRITE_LABELS.contains(&label.as_str()))
    {
        return serde_json::json!({
            "effect": "write_effect",
            "risk_labels": labels,
        });
    }
    let recommended = scan
        .get("recommended_action")
        .and_then(JsonValue::as_str)
        .unwrap_or("deny");
    // Consume the same typed argv/shell-AST boundary as the command safety
    // policy. The display-oriented `shell_command_groups` projection flattens
    // argv and cannot prove whether metacharacters were syntax or literals.
    let analysis = security_command_analysis(ctx);
    let parsed_any = !analysis.stages.is_empty();
    let all_resolved = !analysis.unresolved
        && analysis
            .stages
            .iter()
            .all(|stage| stage_is_read_only(&stage.argv, &stage.dynamic));
    let no_file_writes = analysis
        .redirects
        .iter()
        .all(|redirect| !redirect.writes_file());
    let effect = if recommended == "allow" && parsed_any && all_resolved && no_file_writes {
        "read_effect"
    } else {
        "unknown"
    };
    serde_json::json!({
        "effect": effect,
        "risk_labels": labels,
    })
}

pub fn command_workspace_effect_value(ctx: &VmValue) -> Result<VmValue, VmError> {
    let json = crate::llm::vm_value_to_json(ctx);
    Ok(crate::stdlib::json_to_vm_value(
        &command_workspace_effect_json(&json),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect(command: &str) -> String {
        let ctx = serde_json::json!({ "request": { "command": command } });
        command_workspace_effect_json(&ctx)["effect"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn read_only_probes_join_the_observation_phase() {
        for command in [
            "git status --short --branch",
            "git -C sub diff --stat",
            "git log --oneline -n 5",
            "ls -la src",
            "rg needle src",
            "cat Cargo.toml",
            "wc -l src/lib.rs",
        ] {
            assert_eq!(effect(command), "read_effect", "{command}");
        }
    }

    /// The regression this module exists for. Every one of these clears the
    /// danger scanner, so a risk-label proxy called them observations.
    #[test]
    fn quiet_workspace_writers_are_not_observations() {
        for command in [
            "rm src/foo.rs",
            "mv a b",
            "cp a b",
            "git checkout -- .",
            "git commit -am wip",
            "cargo build",
            "cargo test --workspace",
            "make",
            "pnpm run lint",
            "sed -i s/a/b/ file",
        ] {
            assert_ne!(effect(command), "read_effect", "{command}");
        }
    }

    #[test]
    fn established_writes_are_write_effect() {
        for command in ["echo hi > file", "tee out.txt"] {
            assert_eq!(effect(command), "write_effect", "{command}");
        }
    }

    #[test]
    fn unrecognized_commands_are_unknown_not_read() {
        assert_eq!(effect("some-unknown-tool --check"), "unknown");
    }

    #[test]
    fn a_pipeline_is_read_only_only_if_every_stage_is() {
        assert_eq!(effect("git status | wc -l"), "read_effect");
        assert_ne!(effect("git status | tee log.txt"), "read_effect");
    }

    #[test]
    fn workspace_effect_is_unknown_without_a_command() {
        let ctx = serde_json::json!({ "request": {} });
        assert_eq!(
            command_workspace_effect_json(&ctx)["effect"]
                .as_str()
                .unwrap(),
            "unknown"
        );
    }

    #[test]
    fn git_config_reads_only_when_it_reads() {
        assert_eq!(effect("git config --get user.name"), "read_effect");
        assert_ne!(effect("git config user.name someone"), "read_effect");
    }

    #[test]
    fn read_shaped_programs_cannot_hide_execution_or_output_modes() {
        for command in [
            "awk 'BEGIN { system(\"touch changed\") }'",
            "find . -exec touch changed ;",
            "env sh -c 'touch changed'",
            "rg --pre 'touch changed' needle .",
            "sort -o changed input",
            "sort --compress-program='touch changed' input",
            "tree -o changed",
            "yq -i '.x = 1' config.yaml",
            "yq --split-exp '.name' config.yaml",
            "uniq input changed",
            "xxd -r input.hex changed",
            "file -C magic",
            "date -s tomorrow",
            "hostname changed",
        ] {
            assert_ne!(effect(command), "read_effect", "{command}");
        }
    }

    #[test]
    fn git_read_subcommands_reject_file_output_and_external_execution() {
        for command in [
            "git diff --output=changed",
            "git log --output changed",
            "git show --ext-diff HEAD",
            "git grep --open-files-in-pager=touch needle",
            "git ls-remote --upload-pack='touch changed' .",
        ] {
            assert_ne!(effect(command), "read_effect", "{command}");
        }
    }

    #[test]
    fn nested_shell_writers_prevent_read_effect_classification() {
        for command in ["echo $(touch changed)", "printf '%s' `rm changed`"] {
            assert_ne!(effect(command), "read_effect", "{command}");
        }
    }
}
