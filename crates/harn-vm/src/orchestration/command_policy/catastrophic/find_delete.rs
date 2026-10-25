//! `find` as a deleter: `-delete`, or an `-exec`-family action running `rm`.

use super::project_delete::is_project_target;
use super::shell::{command_basename, unwrapped_command_index};
use super::{path_escapes_root, PROJECT_DELETE_REASON};

/// Tests that select a subset of entries, with their operand counts. Everything
/// else (output actions, traversal options, constants) leaves a following delete
/// unfiltered.
const SELECTING: &[(&str, usize)] = &[
    ("-amin", 1),
    ("-anewer", 1),
    ("-atime", 1),
    ("-cmin", 1),
    ("-cnewer", 1),
    ("-context", 1),
    ("-ctime", 1),
    ("-empty", 0),
    ("-executable", 0),
    ("-fstype", 1),
    ("-gid", 1),
    ("-group", 1),
    ("-ilname", 1),
    ("-iname", 1),
    ("-inum", 1),
    ("-ipath", 1),
    ("-iregex", 1),
    ("-iwholename", 1),
    ("-links", 1),
    ("-lname", 1),
    ("-mmin", 1),
    ("-mtime", 1),
    ("-name", 1),
    ("-newer", 1),
    ("-nogroup", 0),
    ("-nouser", 0),
    ("-path", 1),
    ("-perm", 1),
    ("-readable", 0),
    ("-regex", 1),
    ("-samefile", 1),
    ("-size", 1),
    ("-type", 1),
    ("-uid", 1),
    ("-used", 1),
    ("-user", 1),
    ("-wholename", 1),
    ("-writable", 0),
    ("-xtype", 1),
];

/// Non-selecting primaries that consume operands, which must not be read as primaries.
const OPERAND_ONLY: &[(&str, usize)] = &[
    ("-fls", 1),
    ("-fprint", 1),
    ("-fprint0", 1),
    ("-fprintf", 2),
    ("-maxdepth", 1),
    ("-mindepth", 1),
    ("-printf", 1),
    ("-regextype", 1),
];

/// `Some((selects, operands))` for a primary that takes operands or selects.
fn primary_shape(token: &str) -> Option<(bool, usize)> {
    if let Some((_, count)) = SELECTING.iter().find(|(name, _)| *name == token) {
        return Some((true, *count));
    }
    if token.starts_with("-newer") && token.len() == 8 {
        return Some((true, 1));
    }
    OPERAND_ONLY
        .iter()
        .find(|(name, _)| *name == token)
        .map(|(_, count)| (false, *count))
}

const EXEC_ACTIONS: &[&str] = &["-exec", "-execdir", "-ok", "-okdir"];

/// A `find` that deletes outside the workspace, whatever its predicates, or
/// that deletes the whole project root without a filtering predicate.
pub(super) fn find_delete_catastrophe(args: &[String], roots: &[String]) -> Option<String> {
    let mut index = 0;
    // Global options come before the start paths: `-H`, `-L`, `-P`, `-Olevel`, `-D debugopts`.
    while index < args.len() {
        match args[index].as_str() {
            "-D" => index += 2,
            option if matches!(option, "-H" | "-L" | "-P") || option.starts_with("-O") => {
                index += 1;
            }
            _ => break,
        }
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
    let mut unknown_starts = false;
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
        } else if token == "-files0-from" {
            // The start paths come from a file this scan cannot read.
            unknown_starts = true;
            position += 2;
            continue;
        } else if let Some((selects, operands)) = primary_shape(token) {
            // find evaluates left to right: a test after the action filters nothing.
            filtered |= selects && !deletes;
            position += 1 + operands;
            continue;
        }
        position += 1;
    }
    if !deletes {
        return None;
    }
    if unknown_starts {
        return Some(
            "`find -files0-from … -delete` is blocked: its start paths are read from a file, so the delete cannot be confined to the workspace."
                .to_string(),
        );
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
