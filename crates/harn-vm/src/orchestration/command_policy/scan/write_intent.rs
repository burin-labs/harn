use super::super::catastrophic::ShellAnalysis;

pub(super) fn has_write_intent(lower: &str, analysis: &ShellAnalysis) -> bool {
    analysis.redirects.iter().any(|redirect| {
        redirect.writes_file()
            && !redirect
                .destination
                .as_deref()
                .is_some_and(|target| is_output_sink_target(&target.to_ascii_lowercase()))
    }) || has_output_redirect_write_intent(lower)
        || lower.contains(" tee ")
        || lower.starts_with("tee ")
        || lower.contains("|tee ")
        || lower.contains(";tee ")
        || lower.contains("sed -i")
        || lower.contains("perl -pi")
        || lower.contains("truncate ")
}

/// Detect unquoted shell output redirects that target files, including compact
/// forms such as `cmd>out`, `1>out`, and `2>err`. POSIX shells define output
/// redirection as `[n]>word`; cmd.exe also treats `>` as output redirection.
/// Do not classify descriptor duplication/close (`2>&1`, `>&-`) or process
/// device sinks (`>/dev/null`, `>nul`) as file write intent.
pub(super) fn has_output_redirect_write_intent(lower: &str) -> bool {
    let mut quote = QuoteMode::None;
    let mut escaped = false;
    let chars = lower.chars().collect::<Vec<_>>();
    let mut idx = 0;
    while idx < chars.len() {
        let ch = chars[idx];
        if escaped {
            escaped = false;
            idx += 1;
            continue;
        }
        if ch == '\\' && quote != QuoteMode::Single {
            escaped = true;
            idx += 1;
            continue;
        }
        quote = update_quote_mode(quote, ch);
        if quote != QuoteMode::None {
            idx += 1;
            continue;
        }
        let amp_redirect = ch == '&' && idx + 1 < chars.len() && chars[idx + 1] == '>';
        let output_redirect = ch == '>';
        if amp_redirect || output_redirect {
            let op_start = idx;
            let mut op_end = idx + 1;
            if amp_redirect {
                op_end += 1;
            }
            if op_end < chars.len() && matches!(chars[op_end], '>' | '|') {
                op_end += 1;
            }
            let after_operator = if !amp_redirect && op_end < chars.len() && chars[op_end] == '&' {
                op_end + 1
            } else {
                op_end
            };
            let target = redirect_target(&chars, after_operator);
            if redirect_target_is_write(target.as_deref()) {
                return true;
            }
            idx = op_end.max(op_start + 1);
            continue;
        }
        idx += 1;
    }
    false
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuoteMode {
    None,
    Single,
    Double,
}

fn update_quote_mode(mode: QuoteMode, ch: char) -> QuoteMode {
    match (mode, ch) {
        (QuoteMode::None, '\'') => QuoteMode::Single,
        (QuoteMode::Single, '\'') => QuoteMode::None,
        (QuoteMode::None, '"') => QuoteMode::Double,
        (QuoteMode::Double, '"') => QuoteMode::None,
        _ => mode,
    }
}

fn redirect_target(chars: &[char], start: usize) -> Option<String> {
    let mut idx = start;
    while idx < chars.len() && chars[idx].is_whitespace() {
        idx += 1;
    }
    if idx >= chars.len() {
        return None;
    }
    let mut quote = QuoteMode::None;
    let mut escaped = false;
    let mut target = String::new();
    while idx < chars.len() {
        let ch = chars[idx];
        if escaped {
            target.push(ch);
            escaped = false;
            idx += 1;
            continue;
        }
        if ch == '\\' && quote != QuoteMode::Single {
            escaped = true;
            idx += 1;
            continue;
        }
        let next_quote = update_quote_mode(quote, ch);
        if next_quote != quote {
            quote = next_quote;
            idx += 1;
            continue;
        }
        if quote == QuoteMode::None
            && (ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | '<' | '>' | '(' | ')'))
        {
            break;
        }
        target.push(ch);
        idx += 1;
    }
    let target = target.trim().to_string();
    (!target.is_empty()).then_some(target)
}

fn redirect_target_is_write(target: Option<&str>) -> bool {
    let Some(target) = target else {
        return true;
    };
    if target == "-" || target.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    !is_output_sink_target(target)
}

fn is_output_sink_target(target: &str) -> bool {
    matches!(
        target.trim_end_matches(':'),
        "/dev/null" | "/dev/stdout" | "/dev/stderr" | "nul"
    ) || target
        .strip_prefix("/dev/fd/")
        .is_some_and(|fd| !fd.is_empty() && fd.bytes().all(|byte| byte.is_ascii_digit()))
}
