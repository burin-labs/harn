use std::io::{self, Write};
use std::path::PathBuf;

use crate::execute;

/// Harn REPL keyword completer.
struct HarnCompleter {
    keywords: Vec<String>,
}

impl reedline::Completer for HarnCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<reedline::Suggestion> {
        let text = &line[..pos];
        let word_start = text
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|i| i + 1)
            .unwrap_or(0);
        let prefix = &text[word_start..];
        if prefix.is_empty() {
            return Vec::new();
        }

        self.keywords
            .iter()
            .filter(|kw| kw.starts_with(prefix) && kw.as_str() != prefix)
            .map(|kw| reedline::Suggestion {
                value: kw.clone(),
                description: None,
                style: None,
                extra: None,
                span: reedline::Span::new(word_start, pos),
                append_whitespace: true,
            })
            .collect()
    }
}

/// Harn REPL syntax highlighter.
struct HarnHighlighter {
    keywords: Vec<String>,
}

impl reedline::Highlighter for HarnHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> reedline::StyledText {
        let mut styled = reedline::StyledText::new();
        let mut remaining = line;

        while !remaining.is_empty() {
            if remaining.starts_with(|c: char| c.is_alphabetic() || c == '_') {
                let end = remaining
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(remaining.len());
                let word = &remaining[..end];
                if self.keywords.contains(&word.to_string()) {
                    styled.push((
                        nu_ansi_term::Style::new()
                            .fg(nu_ansi_term::Color::Blue)
                            .bold(),
                        word.to_string(),
                    ));
                } else if word == "true" || word == "false" || word == "nil" {
                    styled.push((
                        nu_ansi_term::Style::new().fg(nu_ansi_term::Color::Yellow),
                        word.to_string(),
                    ));
                } else {
                    styled.push((nu_ansi_term::Style::new(), word.to_string()));
                }
                remaining = &remaining[end..];
            } else if remaining.starts_with('"') {
                let end = remaining[1..]
                    .find('"')
                    .map(|i| i + 2)
                    .unwrap_or(remaining.len());
                let s = &remaining[..end];
                styled.push((
                    nu_ansi_term::Style::new().fg(nu_ansi_term::Color::Green),
                    s.to_string(),
                ));
                remaining = &remaining[end..];
            } else if remaining.starts_with("//") {
                styled.push((
                    nu_ansi_term::Style::new().fg(nu_ansi_term::Color::DarkGray),
                    remaining.to_string(),
                ));
                remaining = "";
            } else if remaining.starts_with(|c: char| c.is_ascii_digit()) {
                let end = remaining
                    .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '_')
                    .unwrap_or(remaining.len());
                let num = &remaining[..end];
                styled.push((
                    nu_ansi_term::Style::new().fg(nu_ansi_term::Color::Cyan),
                    num.to_string(),
                ));
                remaining = &remaining[end..];
            } else {
                let ch = &remaining[..remaining.ceil_char_boundary(1)];
                styled.push((nu_ansi_term::Style::new(), ch.to_string()));
                remaining = &remaining[ch.len()..];
            }
        }
        styled
    }
}

/// Harn REPL validator for multi-line input.
struct HarnValidator;

impl reedline::Validator for HarnValidator {
    fn validate(&self, line: &str) -> reedline::ValidationResult {
        let open_braces = line.chars().filter(|c| *c == '{').count();
        let close_braces = line.chars().filter(|c| *c == '}').count();
        let open_parens = line.chars().filter(|c| *c == '(').count();
        let close_parens = line.chars().filter(|c| *c == ')').count();
        let open_brackets = line.chars().filter(|c| *c == '[').count();
        let close_brackets = line.chars().filter(|c| *c == ']').count();

        if open_braces > close_braces
            || open_parens > close_parens
            || open_brackets > close_brackets
        {
            reedline::ValidationResult::Incomplete
        } else {
            reedline::ValidationResult::Complete
        }
    }
}

pub(crate) async fn run_repl() {
    use reedline::{DefaultPrompt, DefaultPromptSegment, FileBackedHistory, Reedline, Signal};

    println!("Harn REPL v{}", env!("CARGO_PKG_VERSION"));
    println!("Type expressions or statements. Ctrl+D to exit.");

    let harn_keywords: Vec<String> = [
        "pipeline",
        "fn",
        "let",
        "var",
        "if",
        "else",
        "for",
        "in",
        "while",
        "match",
        "return",
        "break",
        "continue",
        "import",
        "from",
        "try",
        "catch",
        "throw",
        "spawn",
        "parallel",
        "parallel_map",
        "retry",
        "guard",
        "deadline",
        "mutex",
        "enum",
        "struct",
        "type",
        "pub",
        "extends",
        "override",
        "true",
        "false",
        "nil",
        "log",
        "print",
        "println",
        "assert",
        "assert_eq",
        "assert_ne",
        "type_of",
        "to_string",
        "to_int",
        "to_float",
        "json_stringify",
        "json_parse",
        "read_file",
        "write_file",
        "file_exists",
        "exec",
        "env",
        "timestamp",
        "abs",
        "min",
        "max",
        "floor",
        "ceil",
        "round",
        "sqrt",
        "pow",
        "random",
        "regex_match",
        "regex_replace",
        "http_get",
        "http_post",
        "llm_call",
        "llm_stream",
        "channel",
        "send",
        "receive",
        "close",
        "sleep",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let completer = Box::new(HarnCompleter {
        keywords: harn_keywords.clone(),
    });
    let highlighter = Box::new(HarnHighlighter {
        keywords: harn_keywords,
    });
    let validator = Box::new(HarnValidator);

    let history_path = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".harn_history"))
        .unwrap_or_else(|| PathBuf::from(".harn_history"));

    let history = Box::new(
        FileBackedHistory::with_file(1000, history_path)
            .unwrap_or_else(|_| FileBackedHistory::new(1000).expect("history")),
    );

    let mut line_editor = Reedline::create()
        .with_completer(completer)
        .with_highlighter(highlighter)
        .with_validator(validator)
        .with_history(history);

    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("harn".to_string()),
        DefaultPromptSegment::Empty,
    );

    loop {
        // Run reedline in spawn_blocking since it blocks on terminal input
        let input = tokio::task::spawn_blocking({
            let mut editor = std::mem::replace(&mut line_editor, Reedline::create());
            let prompt = prompt.clone();
            move || {
                let result = editor.read_line(&prompt);
                (editor, result)
            }
        })
        .await;

        match input {
            Ok((editor, Ok(Signal::Success(line)))) => {
                line_editor = editor;
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let source = format!("pipeline repl(task) {{\n{line}\n}}");
                match execute(&source, None).await {
                    Ok(output) => {
                        if !output.is_empty() {
                            io::stdout().write_all(output.as_bytes()).ok();
                        }
                    }
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            Ok((_, Ok(Signal::CtrlC))) => continue,
            Ok((_, Ok(Signal::CtrlD))) => {
                println!("Goodbye!");
                break;
            }
            Ok((_editor, Err(e))) => {
                eprintln!("Read error: {e}");
                break;
            }
            Err(e) => {
                eprintln!("Runtime error: {e}");
                break;
            }
        }
    }
}
