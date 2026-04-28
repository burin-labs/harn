//! `ast.bracket_balance` — count unbalanced `()`, `[]`, `{}` in source.
//!
//! Direct port of `Sources/ASTEngine/BracketBalance.swift`. The lexer
//! treats `//` and `/* */` as comments by default and switches to `#`
//! line comments for Python; string literals (single, double, backtick)
//! suppress bracket counting inside them. The output is signed counts
//! per bracket family — positive means unclosed openers, negative means
//! unmatched closers.

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_string, require_string};

use super::language::Language;

const BUILTIN: &str = "hostlib_ast_bracket_balance";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();
    let source = require_string(BUILTIN, dict, "source")?;
    let language_name = optional_string(BUILTIN, dict, "language")?;
    let is_python = language_name
        .as_deref()
        .and_then(Language::from_name)
        .map(|l| matches!(l, Language::Python))
        .unwrap_or(false);

    let balance = count(&source, is_python);
    Ok(build_dict([
        ("parens", VmValue::Int(balance.parens as i64)),
        ("brackets", VmValue::Int(balance.brackets as i64)),
        ("braces", VmValue::Int(balance.braces as i64)),
    ]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Balance {
    parens: i32,
    brackets: i32,
    braces: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexState {
    Code,
    LineComment,
    BlockComment,
    SingleQuoteString,
    DoubleQuoteString,
    BacktickString,
}

fn count(source: &str, is_python: bool) -> Balance {
    let chars: Vec<char> = source.chars().collect();
    let len = chars.len();
    let mut state = LexState::Code;
    let mut counts = Balance {
        parens: 0,
        brackets: 0,
        braces: 0,
    };
    let mut i: usize = 0;
    while i < len {
        let c = chars[i];
        let next = if i + 1 < len {
            Some(chars[i + 1])
        } else {
            None
        };
        match state {
            LexState::Code => {
                i += process_code_char(c, next, is_python, &mut state, &mut counts);
            }
            other => {
                i += process_non_code_char(c, next, other, &mut state);
            }
        }
    }
    counts
}

fn process_code_char(
    c: char,
    next: Option<char>,
    is_python: bool,
    state: &mut LexState,
    counts: &mut Balance,
) -> usize {
    if let Some(advance) = check_comment_start(c, next, is_python, state) {
        return advance;
    }
    if let Some(string_state) = check_string_start(c) {
        *state = string_state;
        return 1;
    }
    match c {
        '(' => counts.parens += 1,
        ')' => counts.parens -= 1,
        '[' => counts.brackets += 1,
        ']' => counts.brackets -= 1,
        '{' => counts.braces += 1,
        '}' => counts.braces -= 1,
        _ => {}
    }
    1
}

fn process_non_code_char(
    c: char,
    next: Option<char>,
    state_in: LexState,
    state: &mut LexState,
) -> usize {
    match state_in {
        LexState::LineComment => {
            if c == '\n' {
                *state = LexState::Code;
            }
            1
        }
        LexState::BlockComment => {
            if c == '*' && next == Some('/') {
                *state = LexState::Code;
                2
            } else {
                1
            }
        }
        LexState::DoubleQuoteString => process_string_char(c, '"', state),
        LexState::SingleQuoteString => process_string_char(c, '\'', state),
        LexState::BacktickString => process_string_char(c, '`', state),
        LexState::Code => 1,
    }
}

fn process_string_char(c: char, terminator: char, state: &mut LexState) -> usize {
    if c == '\\' {
        return 2;
    }
    if c == terminator {
        *state = LexState::Code;
    }
    1
}

fn check_comment_start(
    c: char,
    next: Option<char>,
    is_python: bool,
    state: &mut LexState,
) -> Option<usize> {
    if c == '/' && next == Some('/') && !is_python {
        *state = LexState::LineComment;
        return Some(2);
    }
    if c == '/' && next == Some('*') && !is_python {
        *state = LexState::BlockComment;
        return Some(2);
    }
    if c == '#' && is_python {
        *state = LexState::LineComment;
        return Some(1);
    }
    None
}

fn check_string_start(c: char) -> Option<LexState> {
    match c {
        '"' => Some(LexState::DoubleQuoteString),
        '\'' => Some(LexState::SingleQuoteString),
        '`' => Some(LexState::BacktickString),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balance(source: &str, lang: &str) -> Balance {
        count(source, lang == "py" || lang == "python")
    }

    #[test]
    fn balanced_source_reports_zero() {
        let b = balance("fn foo() { let x = [1, 2, 3]; }", "rust");
        assert_eq!(b.parens, 0);
        assert_eq!(b.brackets, 0);
        assert_eq!(b.braces, 0);
    }

    #[test]
    fn missing_closing_brace_reports_positive_one() {
        let b = balance("fn foo() {", "rust");
        assert_eq!(b.braces, 1);
    }

    #[test]
    fn extra_closing_paren_reports_negative_one() {
        let b = balance("foo())", "rust");
        assert_eq!(b.parens, -1);
    }

    #[test]
    fn brackets_inside_strings_are_ignored() {
        let b = balance(r#"let s = "[}{)";"#, "rust");
        assert_eq!(b.parens, 0);
        assert_eq!(b.brackets, 0);
        assert_eq!(b.braces, 0);
    }

    #[test]
    fn brackets_inside_line_comments_are_ignored() {
        let b = balance("// {[(\nlet x = 1;", "rust");
        assert_eq!(b.parens, 0);
        assert_eq!(b.brackets, 0);
        assert_eq!(b.braces, 0);
    }

    #[test]
    fn brackets_inside_block_comments_are_ignored() {
        let b = balance("/* {[( */ ()", "rust");
        assert_eq!(b.parens, 0);
        assert_eq!(b.braces, 0);
    }

    #[test]
    fn python_uses_hash_comments() {
        let b = balance("# {[(\nx = 1", "python");
        assert_eq!(b.parens, 0);
    }

    #[test]
    fn python_does_not_treat_double_slash_as_comment() {
        // `//` is integer division in Python — must not flip lexer to LineComment.
        let b = balance("x = 5 // 2  # cmt with [\n", "python");
        assert_eq!(b.brackets, 0);
    }

    #[test]
    fn handler_returns_three_int_fields() {
        let raw = std::collections::BTreeMap::from([
            (
                "source".to_string(),
                VmValue::String(std::rc::Rc::from("fn foo() {")),
            ),
            (
                "language".to_string(),
                VmValue::String(std::rc::Rc::from("rust")),
            ),
        ]);
        let payload = VmValue::Dict(std::rc::Rc::new(raw));
        let result = run(&[payload]).expect("handler runs");
        match &result {
            VmValue::Dict(d) => {
                assert!(matches!(d.get("parens"), Some(VmValue::Int(_))));
                assert!(matches!(d.get("brackets"), Some(VmValue::Int(_))));
                assert!(matches!(d.get("braces"), Some(VmValue::Int(_))));
            }
            _ => panic!("expected dict"),
        }
    }
}
