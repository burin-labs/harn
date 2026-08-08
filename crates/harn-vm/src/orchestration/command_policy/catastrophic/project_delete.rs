//! Detection of recursive commands that wipe the active project root.

use super::{shell::*, strip_trailing_slashes, MAX_DEPTH};

// ---- In-root project-wipe family (`rm -rf .` / `*` / `${PWD}/*` / … ) ----

fn command_deletes_project(command: &str, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    split_chained_command(command)
        .iter()
        .any(|segment| segment_deletes_project_inner(segment, depth))
}

pub(super) fn segment_deletes_project(segment: &str) -> bool {
    segment_deletes_project_inner(segment, 0)
}

fn segment_deletes_project_inner(segment: &str, depth: usize) -> bool {
    let tokens = shell_words(segment);
    let mut start = 0;
    while start < tokens.len() {
        let end = next_pipeline_boundary(&tokens, start);
        if invocation_deletes_project(&tokens, start, end, depth) {
            return true;
        }
        start = end + 1;
    }
    false
}

fn invocation_deletes_project(tokens: &[String], start: usize, end: usize, depth: usize) -> bool {
    let mut command_index = unwrapped_command_index(tokens, start, end);
    if command_index >= end {
        return false;
    }
    let command = command_basename(&tokens[command_index]);
    if shell_c_invocation_deletes_project(command, tokens, command_index, end, depth) {
        return true;
    }
    if command != "rm" {
        return false;
    }
    command_index += 1;
    let mut saw_force = false;
    let mut saw_recursive = false;
    let mut saw_project_target = false;
    let mut parsing_options = true;
    while command_index < end {
        let token = &tokens[command_index];
        if parsing_options && token == "--" {
            parsing_options = false;
            command_index += 1;
            continue;
        }
        if parsing_options && token.starts_with("--") {
            saw_force = saw_force || token == "--force";
            saw_recursive = saw_recursive || token == "--recursive";
            command_index += 1;
            continue;
        }
        if parsing_options && token.starts_with('-') && token != "-" {
            for flag in token.chars().skip(1) {
                saw_force = saw_force || flag == 'f';
                saw_recursive = saw_recursive || flag == 'r' || flag == 'R';
            }
            command_index += 1;
            continue;
        }
        parsing_options = false;
        saw_project_target = saw_project_target || is_project_target(token);
        command_index += 1;
    }
    saw_force && saw_recursive && saw_project_target
}

fn shell_c_invocation_deletes_project(
    command: &str,
    tokens: &[String],
    command_index: usize,
    end: usize,
    depth: usize,
) -> bool {
    if !matches!(command, "bash" | "sh" | "zsh") {
        return false;
    }
    let mut index = command_index + 1;
    while index < end {
        let token = &tokens[index];
        if token == "--" {
            index += 1;
            continue;
        }
        if token.starts_with('-') && token != "-" {
            if token.chars().skip(1).any(|flag| flag == 'c') {
                return tokens
                    .get(index + 1)
                    .map(|script| command_deletes_project(script, depth + 1))
                    .unwrap_or(false);
            }
            index += 1;
            continue;
        }
        return false;
    }
    false
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
