//! `find` as a deleter: `-delete`, or an `-exec`-family action running `rm`.

use super::project_delete::is_project_target;
use super::shell::{command_basename, unwrapped_command_index};
use super::{path_escapes_root, PROJECT_DELETE_REASON};

/// Primaries that change traversal or output but select no subset of files.
const NON_FILTERING: &[&str] = &[
    "-daystart",
    "-depth",
    "-follow",
    "-ignore_readdir_race",
    "-ls",
    "-maxdepth",
    "-mindepth",
    "-mount",
    "-noignore_readdir_race",
    "-noleaf",
    "-nowarn",
    "-print",
    "-print0",
    "-true",
    "-a",
    "-and",
    "-warn",
    "-xdev",
];

const EXEC_ACTIONS: &[&str] = &["-exec", "-execdir", "-ok", "-okdir"];

/// A `find` that deletes outside the workspace, whatever its predicates, or
/// that deletes the whole project root without a filtering predicate.
pub(super) fn find_delete_catastrophe(args: &[String], roots: &[String]) -> Option<String> {
    let mut index = 0;
    while index < args.len() && matches!(args[index].as_str(), "-H" | "-L" | "-P") {
        index += 1;
    }
    let mut starts = Vec::new();
    while index < args.len() && !is_expression_start(&args[index]) {
        starts.push(args[index].as_str());
        index += 1;
    }
    if starts.is_empty() {
        starts.push(".");
    }
    let expression = &args[index..];
    let mut deletes = false;
    let mut filtered = false;
    // Alternation, negation and constant tests leave a delete reaching most or all entries.
    let mut unselective = false;
    let mut position = 0;
    while position < expression.len() {
        let token = expression[position].as_str();
        if token == "-delete" {
            deletes = true;
        } else if EXEC_ACTIONS.contains(&token) {
            let end = expression[position + 1..]
                .iter()
                .position(|arg| arg == ";" || arg == "+")
                .map_or(expression.len(), |offset| position + 1 + offset);
            let program = &expression[position + 1..end];
            let command = unwrapped_command_index(program, 0, program.len());
            deletes = deletes
                || program
                    .get(command)
                    .is_some_and(|word| command_basename(word) == "rm");
            position = end + 1;
            continue;
        } else if !deletes && matches!(token, "-o" | "-or" | "," | "!" | "-not" | "-false") {
            unselective = true;
        } else if matches!(token, "-maxdepth" | "-mindepth") {
            position += 2;
            continue;
        } else if !deletes && token.starts_with('-') && !NON_FILTERING.contains(&token) {
            // find evaluates left to right: a test after the action filters nothing.
            filtered = true;
        }
        position += 1;
    }
    if !deletes {
        return None;
    }
    if let Some(start) = starts.iter().find(|start| path_escapes_root(start, roots)) {
        return Some(format!(
            "`find {start} … -delete` is blocked: it deletes under an absolute path or outside the project root. Search and delete only inside the workspace."
        ));
    }
    ((unselective || !filtered) && starts.iter().any(|start| is_project_target(start)))
        .then(|| PROJECT_DELETE_REASON.to_string())
}

fn is_expression_start(token: &str) -> bool {
    token.starts_with('-') || matches!(token, "(" | ")" | "!" | ",")
}
