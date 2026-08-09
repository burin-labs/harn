//! Shell tokenization and wrapper unwrapping for the catastrophic-command classifier.

pub(super) fn unwrapped_command_index(tokens: &[String], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        if is_shell_assignment(&tokens[index]) {
            index += 1;
            continue;
        }
        let next = match command_basename(&tokens[index]) {
            "command" => skip_command_wrapper(tokens, index + 1, end),
            "builtin" | "nohup" => index + 1,
            "exec" => skip_exec_wrapper(tokens, index + 1, end),
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

pub(super) fn skip_exec_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
    let mut index = start;
    while index < end {
        let token = &tokens[index];
        if token == "--" {
            return index + 1;
        }
        if token == "-a" && index + 1 < end {
            index += 2;
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

pub(super) fn skip_command_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
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

pub(super) fn skip_sudo_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
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

pub(super) fn skip_env_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
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

pub(super) fn skip_nice_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
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

pub(super) fn skip_time_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
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

pub(super) fn skip_timeout_wrapper(tokens: &[String], start: usize, end: usize) -> usize {
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

pub(super) fn sudo_option_consumes_argument(option: &str) -> bool {
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

pub(super) fn env_option_consumes_argument(option: &str) -> bool {
    matches!(
        option,
        "-C" | "-S" | "-u" | "--chdir" | "--split-string" | "--unset"
    )
}

pub(super) fn time_option_consumes_argument(option: &str) -> bool {
    matches!(option, "-f" | "-o" | "--format" | "--output")
}

pub(super) fn timeout_option_consumes_argument(option: &str) -> bool {
    matches!(option, "-s" | "--signal" | "-k" | "--kill-after")
}

pub(super) fn is_nice_numeric_priority(token: &str) -> bool {
    let Some(rest) = token.strip_prefix(['-', '+']) else {
        return false;
    };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
}

pub(super) fn is_shell_assignment(token: &str) -> bool {
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

pub(super) fn command_basename(token: &str) -> &str {
    token.rsplit(['/', '\\']).next().unwrap_or(token)
}

// ---- Tokenizer and chained-command splitter (quote-normalizing) ----

pub(super) fn shell_words(command: &str) -> Vec<String> {
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

pub(super) fn flush_word(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

pub(super) fn next_pipeline_boundary(tokens: &[String], start: usize) -> usize {
    let mut index = start;
    while index < tokens.len() {
        if tokens[index] == "|" {
            return index;
        }
        index += 1;
    }
    tokens.len()
}

pub(super) fn split_chained_command(command: &str) -> Vec<String> {
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

/// Split one chain member into pipeline stages without treating quoted or
/// escaped pipes as separators. The returned text keeps its original quoting
/// so downstream policy can still distinguish shell syntax from argument text.
pub(super) fn split_pipeline_command(command: &str) -> Vec<String> {
    let mut stages = Vec::new();
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
        if ch == '|' && !in_single_quote && !in_double_quote {
            append_segment(&mut stages, &mut current);
            index += 1;
            if chars.get(index) == Some(&'&') {
                index += 1;
            }
            continue;
        }
        current.push(ch);
        index += 1;
    }
    append_segment(&mut stages, &mut current);
    stages
}

pub(super) fn append_segment(segments: &mut Vec<String>, current: &mut String) {
    let segment = std::mem::take(current);
    if !segment.trim().is_empty() {
        segments.push(segment);
    }
}

pub(super) fn previous_non_whitespace(chars: &[char], index: usize) -> Option<char> {
    chars[..index]
        .iter()
        .rev()
        .find(|c| !c.is_whitespace())
        .copied()
}
