//! Detection of recursive commands that wipe the active project root.

use super::{command_basename, strip_trailing_slashes};

// ---- In-root project-wipe family (`rm -rf .` / `*` / `${PWD}/*` / … ) ----

/// Whether one structurally resolved program invocation recursively wipes the
/// current project. Wrapper and shell nesting are owned by `ShellAnalysis`, so
/// this classifier consumes argv directly and never reparses command text.
pub(super) fn argv_deletes_project(argv: &[String]) -> bool {
    let Some((program, args)) = argv.split_first() else {
        return false;
    };
    if command_basename(program) != "rm" {
        return false;
    }
    let mut saw_force = false;
    let mut saw_recursive = false;
    let mut saw_project_target = false;
    let mut parsing_options = true;
    for token in args {
        if parsing_options && token == "--" {
            parsing_options = false;
            continue;
        }
        if parsing_options && token.starts_with("--") {
            saw_force = saw_force || token == "--force";
            saw_recursive = saw_recursive || token == "--recursive";
            continue;
        }
        if parsing_options && token.starts_with('-') && token != "-" {
            for flag in token.chars().skip(1) {
                saw_force = saw_force || flag == 'f';
                saw_recursive = saw_recursive || flag == 'r' || flag == 'R';
            }
            continue;
        }
        parsing_options = false;
        saw_project_target = saw_project_target || is_project_target(token);
    }
    saw_force && saw_recursive && saw_project_target
}

fn is_project_target(token: &str) -> bool {
    let value = strip_trailing_slashes(token);
    is_dot_path(value)
        || is_current_directory_reference(value)
        || is_current_directory_contents_glob(value)
}

fn is_dot_path(value: &str) -> bool {
    let mut remainder = value;
    while let Some(next) = remainder.strip_prefix("./") {
        remainder = next;
    }
    remainder == "."
}

fn is_current_directory_reference(value: &str) -> bool {
    matches!(
        value,
        "$PWD" | "$PWD/." | "$(pwd)" | "$(pwd)/." | "`pwd`" | "`pwd`/."
    ) || pwd_parameter_expansion_suffix(value)
        .map(|suffix| suffix.is_empty() || suffix == "/.")
        .unwrap_or(false)
}

fn is_current_directory_contents_glob(value: &str) -> bool {
    if is_bare_current_directory_contents_glob(value) {
        return true;
    }
    for prefix in ["$PWD/", "${PWD}/", "$(pwd)/", "`pwd`/"] {
        if let Some(suffix) = value.strip_prefix(prefix) {
            return is_bare_current_directory_contents_glob(suffix);
        }
    }
    if let Some(suffix) = pwd_parameter_expansion_suffix(value).and_then(|s| s.strip_prefix('/')) {
        return is_bare_current_directory_contents_glob(suffix);
    }
    false
}

fn pwd_parameter_expansion_suffix(value: &str) -> Option<&str> {
    let rest = value.strip_prefix("${PWD")?;
    if let Some(suffix) = rest.strip_prefix('}') {
        return Some(suffix);
    }
    let yields_pwd_when_set = [":-", "-", ":?", "?", ":=", "="];
    if !yields_pwd_when_set.iter().any(|op| rest.starts_with(op)) {
        return None;
    }
    let (_, suffix) = rest.split_once('}')?;
    Some(suffix)
}

fn is_bare_current_directory_contents_glob(value: &str) -> bool {
    let mut remainder = value;
    while let Some(next) = remainder.strip_prefix("./") {
        remainder = next;
    }
    matches!(
        remainder,
        "*" | ".*" | ".?*" | ".??*" | ".[!.]*" | "..?*" | "**" | "**/*"
    ) || is_brace_expanded_current_directory_contents_glob(remainder)
}

fn is_brace_expanded_current_directory_contents_glob(value: &str) -> bool {
    let Some(body) = value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) else {
        return false;
    };
    let broad = ["*", ".*", ".?*", ".??*", ".[!.]*", "..?*", "**", "**/*"];
    body.split(',').any(|pattern| broad.contains(&pattern))
}

// ---- Wrapper unwrapping (superset of both twins) ----
