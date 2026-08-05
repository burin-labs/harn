//! Trailing-comma normalization shared by `harn fmt` and the
//! `trailing-comma` lint rule.
//!
//! ## Policy
//! - Multi-line comma-separated lists (call args, list/dict/struct
//!   literals, selective imports) MUST end with a trailing comma.
//! - Single-line comma-separated lists MUST NOT end with a trailing
//!   comma.
//!
//! ## Design
//! Earlier versions of this rule walked tokens with a brace-vs-block
//! heuristic and got fooled by constructs like `eval_pack pack "p"
//! { cases: [...] for case in cases { ... } summarize { ... } }` and
//! multi-field `struct Foo { bar: int fn name() ... }`, both of which
//! have a `key: value` first line that looks like a dict entry to a
//! token-only walker.
//!
//! The current implementation parses the source, walks the AST via
//! [`harn_parser::visit`] to collect every confirmed comma-separated
//! bracket pair, and only enforces the policy at those positions. For
//! sources that fail to parse, no fixes are produced — the lint and
//! formatter pipelines surface the parse error separately.

use std::collections::{HashMap, HashSet};

use harn_lexer::{FixEdit, Lexer, Span, Token, TokenKind};
use harn_parser::{visit, Node, Parser, SNode};

#[derive(Debug, Clone)]
pub struct TrailingCommaIssue {
    pub edit: FixEdit,
    pub kind: TrailingCommaKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailingCommaKind {
    /// Multi-line list missing a trailing comma; the edit inserts ",".
    Missing,
    /// Single-line list with an extraneous trailing comma; the edit
    /// deletes the comma plus any whitespace through the matching
    /// closer.
    Extraneous,
}

/// Apply trailing-comma normalization to `source` until stable. One
/// pass usually suffices; the loop guards against pathological cases
/// where one fix exposes another.
pub fn apply_trailing_comma_fixes(source: &str) -> String {
    let mut current = source.to_string();
    for _ in 0..8 {
        let issues = trailing_comma_issues(&current);
        if issues.is_empty() {
            return current;
        }
        let next = apply_edits(&current, issues.into_iter().map(|i| i.edit).collect());
        if next == current {
            return current;
        }
        current = next;
    }
    current
}

/// Compute the set of trailing-comma issues in `source`. Returns an
/// empty vector when the source fails to lex or parse.
pub fn trailing_comma_issues(source: &str) -> Vec<TrailingCommaIssue> {
    let Some((tokens, program)) = lex_and_parse(source) else {
        return Vec::new();
    };
    let pairs = build_bracket_pairs(source, &tokens);
    let pair_by_close: HashMap<usize, BracketPair> =
        pairs.iter().map(|p| (p.close_byte, p.clone())).collect();

    let eligible_opens = collect_eligible_opens(&program, &pair_by_close, &pairs);
    let mut issues = Vec::new();
    for pair in pairs {
        if !eligible_opens.contains(&pair.open_byte) {
            continue;
        }
        if let Some(issue) = pair.evaluate(source) {
            issues.push(issue);
        }
    }
    issues
}

fn lex_and_parse(source: &str) -> Option<(Vec<Token>, Vec<SNode>)> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize_with_comments().ok()?;
    let parser_tokens: Vec<Token> = tokens
        .iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } | TokenKind::Newline
            )
        })
        .cloned()
        .collect();
    let program = Parser::new(parser_tokens).parse().ok()?;
    Some((tokens, program))
}

#[derive(Debug, Clone)]
struct BracketPair {
    open_byte: usize,
    open_line: usize,
    close_byte: usize,
    close_line: usize,
    /// `,` span if a comma at depth 1 is followed only by whitespace
    /// before the closer — i.e. it is the trailing comma.
    trailing_comma: Option<Span>,
    last_item_token: Option<Span>,
    kind: BracketKind,
}

impl BracketPair {
    fn evaluate(&self, source: &str) -> Option<TrailingCommaIssue> {
        let inner = &source[self.open_byte + 1..self.close_byte];
        if inner.trim().is_empty() {
            return None;
        }
        let has_trailing_comma = self.trailing_comma.is_some();
        if self.close_line > self.open_line {
            if has_trailing_comma {
                return None;
            }
            let insert_pos = self.last_item_token?.end;
            let span = span_at_offset(source, insert_pos, insert_pos);
            Some(TrailingCommaIssue {
                edit: FixEdit {
                    span,
                    replacement: ",".to_string(),
                },
                kind: TrailingCommaKind::Missing,
            })
        } else if has_trailing_comma {
            let comma_span = self.trailing_comma?;
            let mut delete_end = comma_span.end;
            while delete_end < self.close_byte {
                match source.as_bytes()[delete_end] {
                    b' ' | b'\t' => delete_end += 1,
                    _ => break,
                }
            }
            let span = Span::with_offsets(
                comma_span.start,
                delete_end,
                comma_span.line,
                comma_span.column,
            );
            Some(TrailingCommaIssue {
                edit: FixEdit {
                    span,
                    replacement: String::new(),
                },
                kind: TrailingCommaKind::Extraneous,
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BracketKind {
    Paren,
    Bracket,
    Brace,
}

fn build_bracket_pairs(_source: &str, tokens: &[Token]) -> Vec<BracketPair> {
    struct OpenFrame {
        kind: BracketKind,
        open_byte: usize,
        open_line: usize,
        last_comma_at_depth_1: Option<Span>,
        last_token_at_depth_1: Option<Span>,
    }
    let mut stack: Vec<OpenFrame> = Vec::new();
    let mut pairs = Vec::new();
    for tok in tokens {
        match &tok.kind {
            TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } | TokenKind::Newline => {
                continue
            }
            TokenKind::LParen => stack.push(OpenFrame {
                kind: BracketKind::Paren,
                open_byte: tok.span.start,
                open_line: tok.span.line,
                last_comma_at_depth_1: None,
                last_token_at_depth_1: None,
            }),
            TokenKind::LBracket => stack.push(OpenFrame {
                kind: BracketKind::Bracket,
                open_byte: tok.span.start,
                open_line: tok.span.line,
                last_comma_at_depth_1: None,
                last_token_at_depth_1: None,
            }),
            TokenKind::LBrace => stack.push(OpenFrame {
                kind: BracketKind::Brace,
                open_byte: tok.span.start,
                open_line: tok.span.line,
                last_comma_at_depth_1: None,
                last_token_at_depth_1: None,
            }),
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                let Some(frame) = stack.pop() else { continue };
                if !brackets_match(frame.kind, &tok.kind) {
                    continue;
                }
                let trailing_comma = frame.last_comma_at_depth_1.filter(|comma| {
                    frame
                        .last_token_at_depth_1
                        .is_none_or(|token| token.start < comma.start)
                });
                pairs.push(BracketPair {
                    open_byte: frame.open_byte,
                    open_line: frame.open_line,
                    close_byte: tok.span.start,
                    close_line: tok.span.line,
                    trailing_comma,
                    last_item_token: frame.last_token_at_depth_1,
                    kind: frame.kind,
                });
                if let Some(parent) = stack.last_mut() {
                    parent.last_token_at_depth_1 = Some(tok.span);
                }
            }
            TokenKind::Comma => {
                if let Some(top) = stack.last_mut() {
                    top.last_comma_at_depth_1 = Some(tok.span);
                }
            }
            _ => {
                if let Some(top) = stack.last_mut() {
                    top.last_token_at_depth_1 = Some(tok.span);
                }
            }
        }
    }
    pairs
}

fn brackets_match(open: BracketKind, close: &TokenKind) -> bool {
    matches!(
        (open, close),
        (BracketKind::Paren, TokenKind::RParen)
            | (BracketKind::Bracket, TokenKind::RBracket)
            | (BracketKind::Brace, TokenKind::RBrace)
    )
}

fn collect_eligible_opens(
    program: &[SNode],
    pair_by_close: &HashMap<usize, BracketPair>,
    pairs_in_order: &[BracketPair],
) -> HashSet<usize> {
    let mut eligible: HashSet<usize> = HashSet::new();
    let mark_close_at = |eligible: &mut HashSet<usize>, expected_close: usize| {
        if let Some(pair) = pair_by_close.get(&expected_close) {
            eligible.insert(pair.open_byte);
        }
    };
    let mark_unique_in_span = |eligible: &mut HashSet<usize>, span: Span, kind: BracketKind| {
        let mut found: Option<usize> = None;
        for pair in pairs_in_order {
            if pair.kind != kind {
                continue;
            }
            if pair.open_byte >= span.start && pair.close_byte < span.end {
                if found.is_some() {
                    return; // ambiguous
                }
                found = Some(pair.open_byte);
            }
        }
        if let Some(open) = found {
            eligible.insert(open);
        }
    };

    visit::walk_program(program, &mut |node| match &node.node {
        Node::ListLiteral(items) if !items.is_empty() => {
            mark_close_at(&mut eligible, end_minus_one(node.span));
        }
        Node::DictLiteral(entries) if !entries.is_empty() => {
            mark_close_at(&mut eligible, end_minus_one(node.span));
        }
        Node::StructConstruct { fields, .. } if !fields.is_empty() => {
            mark_close_at(&mut eligible, end_minus_one(node.span));
        }
        Node::FunctionCall { args, .. } if !args.is_empty() => {
            mark_close_at(&mut eligible, end_minus_one(node.span));
        }
        Node::ValueCall { args, .. } if !args.is_empty() => {
            mark_close_at(&mut eligible, end_minus_one(node.span));
        }
        Node::MethodCall { args, .. } | Node::OptionalMethodCall { args, .. }
            if !args.is_empty() =>
        {
            mark_close_at(&mut eligible, end_minus_one(node.span));
        }
        Node::EnumConstruct { args, .. } if !args.is_empty() => {
            mark_close_at(&mut eligible, end_minus_one(node.span));
        }
        Node::SelectiveImport { .. } => {
            mark_unique_in_span(&mut eligible, node.span, BracketKind::Brace);
        }
        _ => {}
    });
    eligible
}

fn end_minus_one(span: Span) -> usize {
    span.end.saturating_sub(1)
}

fn apply_edits(source: &str, edits: Vec<FixEdit>) -> String {
    FixEdit::apply_all(source, &edits)
}

fn span_at_offset(source: &str, start: usize, end: usize) -> Span {
    let line = source[..start].bytes().filter(|b| *b == b'\n').count() + 1;
    let line_start = source[..start].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    Span::with_offsets(start, end, line, start - line_start + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(src: &str) -> String {
        apply_trailing_comma_fixes(src)
    }

    #[test]
    fn inserts_trailing_comma_on_multiline_list() {
        let src = "const xs = [\n  1,\n  2\n]\n";
        assert_eq!(fix(src), "const xs = [\n  1,\n  2,\n]\n");
    }

    #[test]
    fn inserts_missing_comma_before_trailing_line_comment() {
        let src = "const xs = [\n  1 // one\n]\n";
        assert_eq!(fix(src), "const xs = [\n  1, // one\n]\n");
    }

    #[test]
    fn preserves_existing_trailing_comma_before_line_comment() {
        let src = "const xs = [\n  1, // one\n]\n";
        assert_eq!(fix(src), src);
    }

    #[test]
    fn removes_trailing_comma_on_single_line_list() {
        assert_eq!(fix("const xs = [1, 2,]\n"), "const xs = [1, 2]\n");
    }

    #[test]
    fn handles_multiline_dict_with_missing_comma() {
        let src = "const d = {\n  a: 1,\n  b: 2\n}\n";
        assert_eq!(fix(src), "const d = {\n  a: 1,\n  b: 2,\n}\n");
    }

    #[test]
    fn nested_lists_settle_in_one_call() {
        let src = "const xs = [\n  [1, 2,],\n  [3, 4]\n]\n";
        assert_eq!(fix(src), "const xs = [\n  [1, 2],\n  [3, 4],\n]\n");
    }

    #[test]
    fn ignores_function_body_block() {
        let src = "fn f() {\n  const x = 1\n  x\n}\n";
        assert_eq!(fix(src), src);
    }

    #[test]
    fn does_not_treat_eval_pack_block_as_dict() {
        let src = "eval_pack pack \"p\" {\n  cases: [{id: \"one\"}]\n  for case in cases {\n    __io_println(case.id)\n  }\n  summarize {\n    __io_println(pack.id)\n  }\n}\n";
        assert_eq!(fix(src), src);
    }

    #[test]
    fn struct_decl_block_is_not_treated_as_dict() {
        let src = "struct Foo {\n  bar: int\n}\n";
        assert_eq!(fix(src), src);
    }

    #[test]
    fn idempotent_on_clean_input() {
        let src = "const xs = [1, 2, 3]\nconst d = {\n  a: 1,\n  b: 2,\n}\n";
        assert_eq!(fix(src), src);
    }

    #[test]
    fn fires_on_multiline_function_call() {
        let src = "fn f() {\n  __io_print(\n    \"hello\",\n    \"world\"\n  )\n}\n";
        assert_eq!(
            fix(src),
            "fn f() {\n  __io_print(\n    \"hello\",\n    \"world\",\n  )\n}\n"
        );
    }

    #[test]
    fn fires_on_method_call_args() {
        let src = "fn f() {\n  const xs = [1, 2]\n  xs.map(fn(x) {\n    x + 1\n  })\n}\n";
        let _ = fix(src); // smoke test — the method call has args, span should be marked.
    }

    #[test]
    fn handles_struct_construction() {
        let src = "fn f() {\n  const p = Point {\n    x: 1,\n    y: 2\n  }\n  p\n}\n";
        assert_eq!(
            fix(src),
            "fn f() {\n  const p = Point {\n    x: 1,\n    y: 2,\n  }\n  p\n}\n"
        );
    }

    #[test]
    fn handles_selective_import() {
        let src = "import {\n  alpha,\n  beta\n} from \"std/io\"\n";
        assert_eq!(fix(src), "import {\n  alpha,\n  beta,\n} from \"std/io\"\n");
    }

    #[test]
    fn fires_on_computed_dict_key() {
        let src =
            "fn f() {\n  const k = \"a\"\n  const d = {\n    [k]: 1,\n    b: 2\n  }\n  d\n}\n";
        assert_eq!(
            fix(src),
            "fn f() {\n  const k = \"a\"\n  const d = {\n    [k]: 1,\n    b: 2,\n  }\n  d\n}\n"
        );
    }
}
