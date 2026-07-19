/// The never-approvable command floor: a Rust port of burin's twin
/// catastrophic-command classifier (Swift `CommandSafetyChecker+Floor` and Rust
/// `destructive_command::catastrophic_command_reason`, kept in cross-language
/// parity). A command that this module flags is HARD-DENIED before the child
/// spawns and is never routed to the consent gate — it is a floor, not a
/// consent-eligible risk label.
///
/// The classifier is intentionally precision-over-recall and is NOT a complete
/// sandbox; it catches the obvious irreversible-destruction shapes (fork bomb,
/// `git reset --hard` / `git clean -fd` / force-push, `rm -rf` escaping the
/// workspace root or wiping it in place, `dd of=`, `mkfs`, `chmod -R 000`,
/// `truncate -s 0` of a source file, and `>`/`>>` redirection onto a source
/// file) through adversarial quoting, chained-command splitting, `bash -c`
/// recursion, and the `sudo`/`env`/`nice`/`nohup`/`time`/`timeout`/`command`/
/// `builtin` wrapper family.
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
pub(super) fn reason(command: &str, workspace_roots: &[String]) -> Option<String> {
    reason_inner(command, workspace_roots, 0)
}

pub(super) fn command_segments(command: &str) -> Vec<String> {
    command_segments_inner(command, 0)
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
                let command_index = unwrapped_command_index(&tokens, start, end);
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

fn reason_inner(command: &str, roots: &[String], depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    // Fork bomb is checked on the WHOLE command first: splitting on `;`/`&`
    // would tear `:(){ :|:& };:` apart before it can be recognized.
    if is_fork_bomb(command) {
        return Some(FORK_BOMB_REASON.to_string());
    }
    for segment in split_chained_command(command) {
        if let Some(hit) = segment_catastrophe(&segment, roots, depth) {
            return Some(hit);
        }
        if segment_deletes_project(&segment) {
            return Some(PROJECT_DELETE_REASON.to_string());
        }
    }
    None
}

fn segment_catastrophe(segment: &str, roots: &[String], depth: usize) -> Option<String> {
    // Redirect-onto-source is a whole-segment property → check raw text.
    if let Some(reason) = redirect_over_source_reason(segment) {
        return Some(reason);
    }
    if is_fork_bomb(segment) {
        return Some(FORK_BOMB_REASON.to_string());
    }
    let tokens = shell_words(segment);
    let mut start = 0;
    while start < tokens.len() {
        let end = next_pipeline_boundary(&tokens, start);
        if let Some(hit) = invocation_catastrophe(&tokens, start, end, roots, depth) {
            return Some(hit);
        }
        start = end + 1;
    }
    None
}

fn invocation_catastrophe(
    tokens: &[String],
    start: usize,
    end: usize,
    roots: &[String],
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
            if let Some(hit) = reason_inner(script, roots, depth + 1) {
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
        "truncate" => truncate_catastrophe(args),
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

fn truncate_catastrophe(args: &[String]) -> Option<String> {
    // `-s 0`, `-s0`, `--size 0`, `--size=0`; a leading +/- on the size is
    // stripped so `-s +0` / `-s -0` also count as zero.
    let mut size_zero = false;
    let mut targets: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "-s" || arg == "--size" {
            if args
                .get(index + 1)
                .map(|v| v.trim_start_matches(['+', '-']))
                == Some("0")
            {
                size_zero = true;
            }
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("-s") {
            if value.trim_start_matches(['+', '-']) == "0" {
                size_zero = true;
            }
            index += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--size=") {
            if value.trim_start_matches(['+', '-']) == "0" {
                size_zero = true;
            }
            index += 1;
            continue;
        }
        if !arg.starts_with('-') {
            targets.push(arg);
        }
        index += 1;
    }
    (size_zero && targets.iter().any(|t| is_source_path(t))).then(|| {
        "`truncate -s 0` of a source file is blocked: it would erase the file's contents. Use the edit tool to rewrite it.".to_string()
    })
}

fn redirect_over_source_reason(segment: &str) -> Option<String> {
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
                if cursor < chars.len() && chars[cursor] == '>' {
                    cursor += 1; // `>>` append
                }
                while cursor < chars.len() && chars[cursor].is_whitespace() {
                    cursor += 1;
                }
                if cursor < chars.len() && chars[cursor] == '&' {
                    // `>&2` fd duplication, not a file write — skip.
                    index = cursor + 1;
                    continue;
                }
                let mut target = String::new();
                while cursor < chars.len() && !chars[cursor].is_whitespace() {
                    let tc = chars[cursor];
                    if tc == '\'' || tc == '"' {
                        cursor += 1;
                        continue;
                    }
                    target.push(tc);
                    cursor += 1;
                }
                if is_source_path(&target) {
                    return Some(format!(
                        "shell redirection (`>`/`>>`) onto the source file `{target}` is blocked: it bypasses the reviewable edit pipeline and risks corrupting the file. Use the edit tool to change source files."
                    ));
                }
                index = cursor;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn is_fork_bomb(segment: &str) -> bool {
    let compact: String = segment.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains(":(){:|:&};:") || compact.contains(":(){:|:&;}:")
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

fn strip_trailing_slashes(value: &str) -> &str {
    let mut end = value.len();
    while end > 1 && value.as_bytes()[end - 1] == b'/' {
        end -= 1;
    }
    &value[..end]
}

fn is_source_path(target: &str) -> bool {
    let cleaned = strip_trailing_slashes(target);
    let name = cleaned.rsplit(['/', '\\']).next().unwrap_or(cleaned);
    let Some(dot) = name.rfind('.') else {
        return false;
    };
    let ext = &name[dot + 1..];
    matches!(
        ext,
        "rs" | "swift"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "cxx"
            | "hpp"
            | "hh"
            | "m"
            | "mm"
            | "rb"
            | "php"
            | "cs"
            | "scala"
            | "zig"
            | "ml"
            | "hs"
            | "ex"
            | "exs"
            | "erl"
            | "lua"
            | "dart"
            | "harn"
            | "sh"
            | "bash"
            | "sql"
    )
}

// ---- In-root project-wipe family (`rm -rf .` / `*` / `${PWD}/*` / … ) ----

fn command_deletes_project(command: &str, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    split_chained_command(command)
        .iter()
        .any(|segment| segment_deletes_project_inner(segment, depth))
}

fn segment_deletes_project(segment: &str) -> bool {
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
    let close = rest.find('}')?;
    Some(&rest[close + 1..])
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

fn unwrapped_command_index(tokens: &[String], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        if is_shell_assignment(&tokens[index]) {
            index += 1;
            continue;
        }
        let next = match command_basename(&tokens[index]) {
            "command" => skip_command_wrapper(tokens, index + 1, end),
            "builtin" | "nohup" => index + 1,
            "sudo" => skip_sudo_wrapper(tokens, index + 1, end),
            "env" => skip_env_wrapper(tokens, index + 1, end),
            "nice" => skip_nice_wrapper(tokens, index + 1, end),
            "time" => skip_time_wrapper(tokens, index + 1, end),
            "timeout" => skip_timeout_wrapper(tokens, index + 1, end),
            _ => return index,
        };
        if next <= index {
            return index;
        }
        index = next;
    }
    index
}

fn skip_command_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        let token = &tokens[index];
        if token == "--" {
            return index + 1;
        }
        if !token.starts_with('-') || token == "-" {
            break;
        }
        let flags: Vec<char> = token.chars().skip(1).collect();
        if flags.iter().any(|f| *f == 'v' || *f == 'V') {
            // `command -v rm` prints a path, it does not execute.
            return end;
        }
        if !flags.iter().all(|f| *f == 'p') {
            break;
        }
        index += 1;
    }
    index
}

fn skip_sudo_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        let token = &tokens[index];
        if token == "--" {
            return index + 1;
        }
        if !token.starts_with('-') {
            break;
        }
        index += 1;
        if sudo_option_consumes_argument(token) && index < end {
            index += 1;
        }
    }
    index
}

fn skip_env_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        let token = &tokens[index];
        if token == "--" {
            return index + 1;
        }
        if is_shell_assignment(token) {
            index += 1;
            continue;
        }
        if !token.starts_with('-') {
            break;
        }
        index += 1;
        if env_option_consumes_argument(token) && index < end {
            index += 1;
        }
    }
    index
}

fn skip_nice_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        let token = &tokens[index];
        if token == "--" {
            return index + 1;
        }
        if token == "-n" && index + 1 < end {
            index += 2;
            continue;
        }
        if token.starts_with("-n") || token.starts_with("--adjustment=") {
            index += 1;
            continue;
        }
        if is_nice_numeric_priority(token) {
            index += 1;
            continue;
        }
        if token.starts_with('-') && token != "-" {
            index += 1;
            continue;
        }
        break;
    }
    index
}

fn skip_time_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        let token = &tokens[index];
        if token == "--" {
            return index + 1;
        }
        if !token.starts_with('-') {
            break;
        }
        index += 1;
        if time_option_consumes_argument(token) && index < end {
            index += 1;
        }
    }
    index
}

fn skip_timeout_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        let token = &tokens[index];
        if token == "--" {
            index += 1;
            break;
        }
        if !token.starts_with('-') {
            break;
        }
        index += 1;
        if timeout_option_consumes_argument(token) && index < end {
            index += 1;
        }
    }
    // The first non-option token is the DURATION and is also consumed.
    if index < end {
        index += 1;
    }
    index
}

fn sudo_option_consumes_argument(option: &str) -> bool {
    matches!(
        option,
        "-C" | "-D"
            | "-g"
            | "-h"
            | "-p"
            | "-t"
            | "-T"
            | "-u"
            | "--close-from"
            | "--group"
            | "--host"
            | "--prompt"
            | "--role"
            | "--type"
            | "--user"
    )
}

fn env_option_consumes_argument(option: &str) -> bool {
    matches!(
        option,
        "-C" | "-S" | "-u" | "--chdir" | "--split-string" | "--unset"
    )
}

fn time_option_consumes_argument(option: &str) -> bool {
    matches!(option, "-f" | "-o" | "--format" | "--output")
}

fn timeout_option_consumes_argument(option: &str) -> bool {
    matches!(option, "-s" | "--signal" | "-k" | "--kill-after")
}

fn is_nice_numeric_priority(token: &str) -> bool {
    let Some(rest) = token.strip_prefix(['-', '+']) else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

fn is_shell_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn command_basename(token: &str) -> &str {
    token.rsplit(['/', '\\']).next().unwrap_or(token)
}

// ---- Tokenizer and chained-command splitter (quote-normalizing) ----

fn shell_words(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaping = false;
    for ch in command.chars() {
        if escaping {
            current.push(ch);
            escaping = false;
            continue;
        }
        if ch == '\\' && !in_single_quote {
            escaping = true;
            continue;
        }
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }
        if !in_single_quote && !in_double_quote {
            if ch.is_whitespace() {
                flush_word(&mut tokens, &mut current);
                continue;
            }
            if ch == '|' {
                flush_word(&mut tokens, &mut current);
                tokens.push("|".to_string());
                continue;
            }
        }
        current.push(ch);
    }
    if escaping {
        current.push('\\');
    }
    flush_word(&mut tokens, &mut current);
    tokens
}

fn flush_word(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

fn next_pipeline_boundary(tokens: &[String], start: usize) -> usize {
    let mut index = start;
    while index < tokens.len() {
        if tokens[index] == "|" {
            return index;
        }
        index += 1;
    }
    tokens.len()
}

fn split_chained_command(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaping = false;
    let chars: Vec<char> = command.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if escaping {
            current.push(ch);
            escaping = false;
            index += 1;
            continue;
        }
        if ch == '\\' && !in_single_quote {
            escaping = true;
            current.push(ch);
            index += 1;
            continue;
        }
        if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            current.push(ch);
            index += 1;
            continue;
        }
        if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            current.push(ch);
            index += 1;
            continue;
        }
        if !in_single_quote && !in_double_quote {
            if ch == ';' || ch == '\n' {
                append_segment(&mut segments, &mut current);
                index += 1;
                continue;
            }
            if ch == '&' && chars.get(index + 1) == Some(&'&') {
                append_segment(&mut segments, &mut current);
                index += 2;
                continue;
            }
            if ch == '|' && chars.get(index + 1) == Some(&'|') {
                append_segment(&mut segments, &mut current);
                index += 2;
                continue;
            }
            if ch == '&' {
                let previous = previous_non_whitespace(&chars, index);
                let next = chars.get(index + 1).copied();
                // Do NOT split on redirection `&` forms (`>&`, `<&`, `|&`, `&>`).
                if previous != Some('>')
                    && previous != Some('<')
                    && previous != Some('|')
                    && next != Some('>')
                {
                    append_segment(&mut segments, &mut current);
                    index += 1;
                    continue;
                }
            }
        }
        current.push(ch);
        index += 1;
    }
    append_segment(&mut segments, &mut current);
    segments
}

fn append_segment(segments: &mut Vec<String>, current: &mut String) {
    let segment = std::mem::take(current);
    if !segment.trim().is_empty() {
        segments.push(segment);
    }
}

fn previous_non_whitespace(chars: &[char], index: usize) -> Option<char> {
    chars[..index]
        .iter()
        .rev()
        .find(|c| !c.is_whitespace())
        .copied()
}
