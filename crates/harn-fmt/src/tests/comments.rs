//! Comment handling: retention, placement, doc-comment canonicalization, and
//! section headers.
//!
//! A comment's meaning lives entirely in where it sits, so moving one silently
//! makes it describe code it was never written about. These tests pin each
//! comment to the construct it was written against.

use super::assert_roundtrip;
use crate::{format_source, format_source_opts, FmtOptions, AUTO_SEPARATOR_WIDTH};

#[test]
fn test_doc_comment_triple_slash_multiline() {
    let source =
        "/// First line.\n/// Second line.\npub fn exposed() -> string {\n  return \"x\"\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("/**\n * First line.\n * Second line.\n */"),
        "expected canonical multi-line /** */ block, got:\n{result}"
    );
    assert!(
        !result.contains("///"),
        "formatter should not emit `///` after normalization, got:\n{result}"
    );
}

#[test]
fn test_doc_comment_triple_slash_compact_one_liner() {
    let source = "/// Short.\npub fn exposed() -> string {\n  return \"x\"\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("/** Short. */"),
        "expected compact one-liner doc comment, got:\n{result}"
    );
}

#[test]
fn test_doc_comment_existing_block_is_canonicalized() {
    let source = "/** messy\n   alignment */\npub fn exposed() -> string {\n  return \"x\"\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("/**\n * messy\n * alignment\n */"),
        "expected canonical multi-line shape, got:\n{result}"
    );
}

#[test]
fn test_inline_match_arm_trailing_comment_stays_on_arm() {
    let source =
        "fn pick(x: int) -> int {\n  match x {\n    1 -> { 10 } // ten\n    _ -> { 0 }\n  }\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("1 -> { 10 }  // ten"),
        "arm comment must stay on the arm line (was flushed to EOF), got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_dict_entry_comments_stay_in_literal() {
    let source =
        "fn f() -> dict {\n  let d = {\n    // section\n    x: 1,\n    y: 2, // why\n  }\n  d\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("    // section\n    x: 1,"),
        "full-line comment must stay above its entry, got:\n{result}"
    );
    assert!(
        result.contains("y: 2,  // why"),
        "trailing entry comment must stay on the entry, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_list_item_trailing_comment_stays_in_literal() {
    let source = "fn f() -> list {\n  let l = [\n    1, // one\n    2,\n  ]\n  l\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("1,  // one"),
        "list item comment must stay on the item, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_statement_comment_after_inline_dict_stays_on_statement() {
    // The dict ends on the statement line, so the comment belongs to the
    // statement and the literal must not absorb it (or force-wrap).
    let source = "fn f() -> dict {\n  let d = {x: 1} // note\n  d\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("let d = {x: 1}  // note"),
        "statement-level comment must stay on the statement, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_method_chain_segment_comments_stay_in_chain() {
    // A `//` comment sitting between the segments of a multi-line method
    // chain used to be orphaned to the end of the program at column 0.
    // It must stay anchored above the segment it precedes.
    let source = "fn f() -> int {\n  let r = [1, 2, 3]\n    // keep the big ones\n    .filter({ x -> x > 1 })\n    // double them\n    .map({ x -> x * 2 })\n  return r.count()\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("    // keep the big ones\n    .filter"),
        "first chain comment must stay above its segment, got:\n{result}"
    );
    assert!(
        result.contains("    // double them\n    .map"),
        "second chain comment must stay above its segment, got:\n{result}"
    );
    assert!(
        !result.trim_end().ends_with("// double them"),
        "chain comment must not be relocated to EOF, got:\n{result}"
    );
    assert_roundtrip(source);
    // Idempotence: formatting the formatted output changes nothing.
    assert_eq!(format_source(&result).unwrap(), result);
}

#[test]
fn test_optional_method_chain_segment_comments_stay_in_chain() {
    let source = "fn f(xs: list?) -> int {\n  let r = xs\n    // may be nil\n    ?.count()\n  return r ?? 0\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("// may be nil"),
        "comment must survive, got:\n{result}"
    );
    assert!(
        !result.trim_end().ends_with("// may be nil"),
        "comment must not be relocated to EOF, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_multiline_binary_operand_comments_preserved() {
    // A comment on the first line of a binary expression that breaks across
    // lines used to be dropped (top level) or relocated out of its block (in a
    // body), because only the statement's last line got a trailing-comment
    // pass. Each broken operand's comment must now survive in place.
    let source = "fn f() -> int {\n  let r = aaa // c1\n    + bbb // c2\n  r\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("aaa  // c1"),
        "left-operand comment must stay on its line, got:\n{result}"
    );
    assert!(
        result.contains("+ bbb  // c2"),
        "right-operand comment must stay on its line, got:\n{result}"
    );
    assert!(
        !result.trim_end().ends_with("// c1"),
        "left-operand comment must not be relocated to EOF, got:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_plain_double_slash_comment_preserved_verbatim() {
    let source = "// plain comment\npub fn exposed() -> string {\n  return \"x\"\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("// plain comment"),
        "plain // comment should be preserved verbatim, got:\n{result}"
    );
    assert!(
        !result.contains("/**"),
        "formatter should not convert // to /** */ (that's the linter's job), got:\n{result}"
    );
}

#[test]
fn test_doc_comment_inside_impl_block() {
    let source =
        "impl Foo {\n  /// Inner method.\n  pub fn bar() -> string {\n    return \"x\"\n  }\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("  /** Inner method. */"),
        "doc comment inside impl body should be normalized, got:\n{result}"
    );
    assert!(
        !result.contains("///"),
        "no `///` should remain after formatting, got:\n{result}"
    );
}

#[test]
fn test_doc_comment_between_attribute_and_fn_is_preserved() {
    // Regression: a `/** */` doc block placed between an attribute and the
    // fn declaration (`@complexity(allow) \n /** */ \n pub fn ...`) used to
    // be dropped and re-emitted above the *next* top-level item. The
    // `missing-harndoc` lint requires the doc block to sit directly above
    // the fn, so the formatter must preserve that position.
    let source = "@complexity(allow)\n/** Documented. */\npub fn foo() -> int {\n  return 1\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("@complexity(allow)\n/** Documented. */\npub fn foo"),
        "doc comment between attribute and fn should be preserved, got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(
        result, result2,
        "formatter is not idempotent with doc between attribute and fn"
    );
}

#[test]
fn test_doc_comment_glued_to_item_blank_line_above() {
    let source =
        "fn first() -> int {\n  return 1\n}\n/// Second docs.\n/// More.\nfn second() -> int {\n  return 2\n}\n";
    let result = format_source(source).unwrap();
    // Blank line above the doc block; doc block glued to fn second.
    assert!(
        result.contains("}\n\n/**\n * Second docs.\n * More.\n */\nfn second"),
        "doc block should have blank line above and be glued to item, got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(
        result, result2,
        "formatter is not idempotent with doc comments between items"
    );
}

fn canonical_bar() -> String {
    separator_bar(100)
}

fn separator_bar(width: usize) -> String {
    let dashes: String = "-".repeat(width.saturating_sub(3));
    format!("// {dashes}")
}

#[test]
fn test_section_header_three_line_canonical_passthrough() {
    let bar = canonical_bar();
    let source = format!(
        "fn a() -> int {{\n  return 1\n}}\n{bar}\n// Helpers\n{bar}\nfn b() -> int {{\n  return 2\n}}\n"
    );
    let result = format_source(&source).unwrap();
    let expected = format!(
        "fn a() -> int {{\n  return 1\n}}\n\n{bar}\n// Helpers\n{bar}\n\nfn b() -> int {{\n  return 2\n}}\n"
    );
    assert_eq!(result, expected, "canonical 3-line header not passthrough");
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "3-line header not idempotent");
}

#[test]
fn test_section_header_three_line_short_bars_normalized() {
    let source =
        "fn a() -> int { return 1 }\n// ----\n// Helpers\n// ----\nfn b() -> int { return 2 }\n";
    let result = format_source(source).unwrap();
    let bar = canonical_bar();
    assert!(
        result.contains(&format!("{bar}\n// Helpers\n{bar}")),
        "short bars should normalize to separator_width, got:\n{result}"
    );
}

#[test]
fn test_section_header_one_line_bar_normalized() {
    let source = "fn a() -> int { return 1 }\n// ----\nfn b() -> int { return 2 }\n";
    let result = format_source(source).unwrap();
    let bar = canonical_bar();
    assert!(
        result.contains(&format!("\n{bar}\n")),
        "one-line bar should normalize, got:\n{result}"
    );
    // Pure bars stay one-liner (no title promotion).
    assert!(
        !result.contains("// Helpers"),
        "pure bar must not gain a title"
    );
}

#[test]
fn test_section_header_one_line_bar_with_title_promoted() {
    let source = "fn a() -> int { return 1 }\n// ---- Helpers ----\nfn b() -> int { return 2 }\n";
    let result = format_source(source).unwrap();
    let bar = canonical_bar();
    assert!(
        result.contains(&format!("{bar}\n// Helpers\n{bar}")),
        "one-liner with title should promote to 3-line form, got:\n{result}"
    );
}

#[test]
fn test_section_header_blank_lines_above_and_below() {
    let source = "fn a() -> int {\n  return 1\n}\n// ----\n// Helpers\n// ----\nfn b() -> int {\n  return 2\n}\n";
    let result = format_source(source).unwrap();
    let bar = canonical_bar();
    // Expect: prev fn close, blank, header, blank, next fn.
    let header = format!("{bar}\n// Helpers\n{bar}");
    let expected_window = format!("}}\n\n{header}\n\nfn b");
    assert!(
        result.contains(&expected_window),
        "expected blank lines above and below section header, got:\n{result}"
    );
}

#[test]
fn test_section_header_respects_custom_separator_width() {
    let opts = FmtOptions {
        line_width: 100,
        separator_width: 40,
    };
    let source = "fn a() -> int { return 1 }\n// ----\nfn b() -> int { return 2 }\n";
    let result = format_source_opts(source, &opts).unwrap();
    let dashes: String = "-".repeat(37);
    let bar = format!("// {dashes}");
    assert!(
        result.contains(&bar),
        "separator should match separator_width=40, got:\n{result}"
    );
}

#[test]
fn test_section_header_auto_width_snapshots_across_line_widths() {
    let source =
        "fn a() -> int { return 1 }\n// ----\n// Helpers  \t\n// ----   \nfn b() -> int { return 2 }\n";
    for line_width in [60, 80, 120] {
        let opts = FmtOptions {
            line_width,
            separator_width: AUTO_SEPARATOR_WIDTH,
        };
        let bar = separator_bar(line_width);
        let expected = format!(
            "fn a() -> int {{\n  return 1\n}}\n\n{bar}\n// Helpers\n{bar}\n\nfn b() -> int {{\n  return 2\n}}\n"
        );
        let result = format_source_opts(source, &opts).unwrap();
        assert_eq!(
            result, expected,
            "line_width={line_width} section-header snapshot drifted"
        );
        assert_eq!(
            result,
            format_source_opts(&result, &opts).unwrap(),
            "line_width={line_width} section-header formatting is not idempotent"
        );
        assert_eq!(
            bar.len(),
            line_width,
            "bar should span the configured line width"
        );
    }
}

#[test]
fn test_trailing_line_comment_preserved_on_top_level_body_statement() {
    let source = "fn main() {\n  let x = 1 // trailing\n  let y = 2\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("let x = 1") && result.contains("// trailing"),
        "trailing comment should be preserved on the same line; got:\n{result}"
    );
    assert!(
        !result.trim_end().ends_with("// trailing"),
        "trailing comment must not be moved to end-of-file; got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "trailing comments must be idempotent");
}

#[test]
fn test_trailing_line_comment_preserved_inside_if_else_blocks() {
    let source = "fn main() {\n  if true {\n    let y = 2 // inner trailing\n  } else {\n    let z = 3 // else trailing\n  } // close brace trailing\n  log(1)\n}\n";
    let result = format_source(source).unwrap();
    for needle in [
        "// inner trailing",
        "// else trailing",
        "// close brace trailing",
    ] {
        assert!(
            result.contains(needle),
            "missing trailing comment '{needle}' in formatted output:\n{result}"
        );
    }
    assert!(
        !result.trim_end().ends_with("// close brace trailing"),
        "trailing comment leaked to end of file:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(
        result, result2,
        "trailing comments inside blocks must be idempotent"
    );
}

#[test]
fn test_trailing_block_comment_preserved_on_statement() {
    let source = "fn main() {\n  let x = 1 /* trailing block */\n}\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("let x = 1") && result.contains("/* trailing block */"),
        "trailing block comment should be preserved on the same line; got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2);
}

#[test]
fn test_trailing_line_comment_preserved_on_top_level_statement() {
    // Regression: trailing same-line comments on *top-level* statements were
    // silently dropped — `format_program` (unlike `format_body`) never
    // attached them. Comments must never be lost by `harn fmt`.
    let source = "let a = 1 // note\nlet b = 2\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("let a = 1") && result.contains("// note"),
        "trailing comment should be preserved on the same line; got:\n{result}"
    );
    assert!(
        !result.trim_end().ends_with("// note"),
        "trailing comment must not be relocated to end-of-file; got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "must be idempotent");
}

#[test]
fn test_trailing_block_comment_preserved_on_top_level_statement() {
    // Same regression as above, for a single-line block comment.
    let source = "let a = 1 /* note */\nlet b = 2\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("let a = 1") && result.contains("/* note */"),
        "trailing block comment should be preserved on the same line; got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "must be idempotent");
}

#[test]
fn test_top_level_trailing_comment_on_import_preserved() {
    // Imports go through `format_sorted_import_block`, which also needs to
    // attach trailing comments rather than drop them.
    let source = "import { a } from \"std/io\" // why\n";
    let result = format_source(source).unwrap();
    assert!(
        result.contains("// why"),
        "trailing comment on an import should be preserved; got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "must be idempotent");
}

// ---------------------------------------------------------------------------
// Trailing comments vs. line width.
//
// Policy (matches rustfmt/Prettier/gofmt — see `docs/src/cli-reference.md`):
// a trailing same-line comment is treated as an unbreakable token that does
// NOT count toward `line_width`. The formatter never relocates a trailing
// comment to its own line and never reflows code to "make room" for one — it
// simply lets the line overflow. Code still wraps on its own merits (the
// comment is appended afterward, to the last physical line).
// ---------------------------------------------------------------------------

#[test]
fn test_trailing_comment_overflow_is_allowed_not_wrapped() {
    // Short statement, long comment: the comment is left over-length on the
    // same line — never moved up and never wrapped.
    let source =
        "let r = f(x) // this explanatory comment is long enough to push the whole line past one hundred columns\n";
    let result = format_source(source).unwrap();
    let line = result.lines().next().unwrap();
    assert!(
        line.starts_with("let r = f(x)") && line.contains("// this explanatory comment"),
        "code and trailing comment must stay on one line; got:\n{result}"
    );
    assert_eq!(
        result.lines().count(),
        1,
        "comment must not be relocated to its own line; got:\n{result}"
    );
    assert!(
        line.len() > 100,
        "the line is expected to overflow rather than be reflowed; got width {}",
        line.len()
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "must be idempotent");
}

#[test]
fn test_trailing_comment_does_not_trigger_statement_wrap() {
    // The code fits within `line_width` on its own; only the trailing comment
    // pushes the line over. The formatter must NOT wrap the code to compensate
    // (don't reflow code to fit a comment).
    let source = "let xs = [alpha, beta, gamma, delta, epsilon, zeta, eta, theta] // greek letters that overflow\n";
    let result = format_source(source).unwrap();
    assert_eq!(
        result.lines().count(),
        1,
        "code under the width must stay inline despite the comment; got:\n{result}"
    );
    assert!(
        result.contains("[alpha, beta, gamma, delta, epsilon, zeta, eta, theta]"),
        "list literal must remain inline; got:\n{result}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "must be idempotent");
}

#[test]
fn test_wrapped_statement_keeps_trailing_comment_on_last_line() {
    // When the *code itself* exceeds the width it wraps one-item-per-line; the
    // trailing comment then rides on the last physical line (the closer), still
    // trailing — never promoted to its own line.
    let source = "let xs = [alpha, beta, gamma, delta, epsilon, zeta, eta, theta, iota, kappa, lambda, mu, nu, xi, omicron, pi, rho] // greek\n";
    let result = format_source(source).unwrap();
    assert!(
        result.lines().count() > 3,
        "long list literal should wrap one item per line; got:\n{result}"
    );
    let last = result.trim_end().lines().last().unwrap();
    assert!(
        last.trim_start().starts_with(']') && last.contains("// greek"),
        "trailing comment should ride the closing `]` line; got last line: {last:?}"
    );
    let result2 = format_source(&result).unwrap();
    assert_eq!(result, result2, "must be idempotent");
}

/// The line a comment lands on, relative to the line matching `anchor`.
/// Panics with the whole rendering when either marker is missing, so a failure
/// shows what actually happened rather than just an index.
fn assert_comment_stays_in_block(source: &str, comment: &str, closing_anchor: &str) {
    let formatted = format_source(source).unwrap();
    let comment_line = formatted
        .lines()
        .position(|l| l.contains(comment))
        .unwrap_or_else(|| panic!("comment vanished entirely:\n{formatted}"));
    let anchor_line = formatted
        .lines()
        .position(|l| l.contains(closing_anchor))
        .unwrap_or_else(|| panic!("anchor `{closing_anchor}` missing:\n{formatted}"));
    assert!(
        comment_line < anchor_line,
        "comment escaped its block (line {comment_line} vs anchor {anchor_line}):\n{formatted}"
    );
}

// A block in EXPRESSION position (bound to a value) used to render through a
// path that only claimed trailing comments, so a standalone comment inside it
// stayed unclaimed — and the next top-level item's leading-comment sweep then
// adopted it, moving it out of the function and onto an unrelated declaration
// where it was simply false.
#[test]
fn comment_in_catch_block_stays_in_the_catch_block() {
    assert_comment_stays_in_block(
        "fn risky() -> int { return 1 }\n\
         fn caller() -> dict {\n\
           const result = try {\n\
             risky()\n\
           } catch (err) {\n\
             // BELONGS TO THE CATCH\n\
             {available: false}\n\
           }\n\
           return result\n\
         }\n\
         fn unrelated() -> int { return 2 }\n",
        "BELONGS TO THE CATCH",
        "fn unrelated",
    );
}

#[test]
fn comment_in_try_body_stays_in_the_try_body() {
    assert_comment_stays_in_block(
        "fn risky() -> int { return 1 }\n\
         fn caller() -> int {\n\
           const result = try {\n\
             // BELONGS TO THE TRY\n\
             risky()\n\
           } catch (err) {\n\
             0\n\
           }\n\
           return result\n\
         }\n\
         fn unrelated() -> int { return 2 }\n",
        "BELONGS TO THE TRY",
        "fn unrelated",
    );
}

#[test]
fn comment_in_value_position_else_stays_in_the_else() {
    assert_comment_stays_in_block(
        "fn caller(flag: bool) -> dict {\n\
           const result = if flag {\n\
             {a: 1}\n\
           } else {\n\
             // BELONGS TO THE ELSE\n\
             {a: 2}\n\
           }\n\
           return result\n\
         }\n\
         fn unrelated() -> int { return 2 }\n",
        "BELONGS TO THE ELSE",
        "fn unrelated",
    );
}

// Not only the comment adjacent to a trailing bare expression: a comment above
// an ordinary statement inside an expression-position block escaped too.
#[test]
fn comment_above_a_statement_in_a_catch_block_stays_put() {
    assert_comment_stays_in_block(
        "fn risky() -> int { return 1 }\n\
         fn caller() -> dict {\n\
           const result = try {\n\
             risky()\n\
           } catch (err) {\n\
             // BELONGS TO THE CATCH\n\
             const fallback = {a: 1}\n\
             fallback\n\
           }\n\
           return result\n\
         }\n\
         fn unrelated() -> int { return 2 }\n",
        "BELONGS TO THE CATCH",
        "fn unrelated",
    );
}

#[test]
fn comment_in_a_closure_body_stays_in_the_closure() {
    assert_comment_stays_in_block(
        "fn caller(items: list) -> list {\n\
           return items.map({ item ->\n\
             // BELONGS TO THE CLOSURE\n\
             item + 1\n\
           })\n\
         }\n\
         fn unrelated() -> int { return 2 }\n",
        "BELONGS TO THE CLOSURE",
        "fn unrelated",
    );
}

// A comment ABOVE a match arm documents the arm, so it must not be sucked into
// the arm's body — the bound for an arm is its own pattern, not the `match`.
#[test]
fn comment_above_a_match_arm_stays_above_the_arm() {
    let formatted = format_source(
        "fn caller(x: int) -> int {\n\
           return match x {\n\
             // ABOUT THE FIRST ARM\n\
             1 -> {\n\
               let y = 1\n\
               y\n\
             }\n\
             _ -> { 0 }\n\
           }\n\
         }\n",
    )
    .unwrap();
    let comment_line = formatted
        .lines()
        .position(|l| l.contains("ABOUT THE FIRST ARM"))
        .unwrap_or_else(|| panic!("comment vanished:\n{formatted}"));
    let arm_line = formatted
        .lines()
        .position(|l| l.contains("1 -> {"))
        .unwrap_or_else(|| panic!("arm missing:\n{formatted}"));
    assert!(
        comment_line < arm_line,
        "the arm's comment did not stay above its arm — it was either pulled inside \
         the body or evicted from the match entirely:\n{formatted}"
    );
}

// A tool decl splices a synthesized zero-span `description(...)` node into its
// body. A zero span names no source line, so it must not anchor a comment range
// — treating its `end_line` of 0 as a bound would claim every unclaimed comment
// in the file above the decl.
#[test]
fn synthesized_zero_span_body_node_does_not_swallow_earlier_comments() {
    let formatted = format_source(
        "// ABOUT THE FIRST FUNCTION\n\
         fn first() -> int { return 1 }\n\
         \n\
         pub tool probe(x: int) -> int {\n\
           \"describe me\"\n\
           return x\n\
         }\n",
    )
    .unwrap();
    let comment_line = formatted
        .lines()
        .position(|l| l.contains("ABOUT THE FIRST FUNCTION"))
        .unwrap_or_else(|| panic!("comment vanished:\n{formatted}"));
    let first_fn_line = formatted
        .lines()
        .position(|l| l.contains("fn first"))
        .unwrap_or_else(|| panic!("fn first missing:\n{formatted}"));
    assert!(
        comment_line < first_fn_line,
        "a comment was dragged into the tool body:\n{formatted}"
    );
}

/// A comment written after the last statement of a body has nothing to its
/// right to attach to, so before the tail flush it stayed unclaimed and the
/// top-level sweep adopted it onto the NEXT declaration — where it silently
/// described unrelated code.
#[test]
fn comment_after_the_last_statement_stays_in_the_function() {
    assert_comment_stays_in_block(
        "fn g() -> int {\n  const y = 2\n  return y\n  // TAIL\n}\n\nfn later() -> int {\n  return 3\n}\n",
        "// TAIL",
        "}",
    );
}

#[test]
fn comment_after_the_last_statement_of_an_else_stays_in_the_else() {
    let source = "fn h() -> int {\n  if a {\n    b()\n  } else {\n    c()\n    // TAIL_ELSE\n  }\n  return 1\n}\n";
    let formatted = format_source(source).unwrap();
    let comment = formatted.find("// TAIL_ELSE").expect("comment vanished");
    let ret = formatted.find("return 1").expect("no return");
    assert!(
        comment < ret,
        "the else's trailing comment escaped its block:\n{formatted}"
    );
    assert_roundtrip(source);
}

#[test]
fn comment_after_the_last_statement_of_a_finally_stays_in_the_finally() {
    let source = "fn t() -> int {\n  try {\n    x()\n  } finally {\n    z()\n    // TAIL_FINALLY\n  }\n  return 2\n}\n";
    let formatted = format_source(source).unwrap();
    let comment = formatted.find("// TAIL_FINALLY").expect("comment vanished");
    let ret = formatted.find("return 2").expect("no return");
    assert!(
        comment < ret,
        "the finally's trailing comment escaped its block:\n{formatted}"
    );
    assert_roundtrip(source);
}

#[test]
fn comment_after_the_last_statement_of_a_loop_stays_in_the_loop() {
    assert_comment_stays_in_block(
        "fn g() -> int {\n  for x in xs {\n    use(x)\n    // TAIL_LOOP\n  }\n  return 1\n}\n",
        "// TAIL_LOOP",
        "  }",
    );
}

/// KNOWN DEFECT, pinned deliberately: a body with a SIBLING after it (`then`
/// before an `else`, `try` before its `catch`) still loses its trailing comment
/// to that sibling. The tail flush cannot reach here — the boundary is the
/// `} else {` line, and the AST records a span for the whole `if` node but none
/// for the individual blocks, so there is no line to bound with. Bounding with
/// the node's end would sweep the else's OWN leading comments backwards into
/// the then, trading one misplacement for another.
///
/// This is the BLOCK-level half of the comment-eviction class and a different
/// mechanism from the member-level half #4806 fixed: the statements here do
/// carry spans, but the block they sit in does not. Tracked as #4890.
///
/// This asserts what the formatter does TODAY rather than what it should do, so
/// the behaviour is visible and a fix has to update a failing test instead of
/// changing comment placement silently. Invert it — the comment belongs before
/// `} else {` — once blocks carry spans (#4890).
#[test]
fn comment_after_a_then_branch_is_currently_dragged_into_the_else() {
    let source = "fn h() -> int {\n  if a {\n    b()\n    // TAIL_THEN\n  } else {\n    c()\n  }\n  return 1\n}\n";
    let formatted = format_source(source).unwrap();
    let comment = formatted
        .find("// TAIL_THEN")
        .expect("the comment must still exist somewhere, even when misplaced");
    let else_kw = formatted.find("} else {").expect("no else");
    assert!(
        comment > else_kw,
        "the then-branch's trailing comment is expected to land in the else until #4806; \
         if it now stays put, invert this assertion:\n{formatted}"
    );
}

// ---------------------------------------------------------------------------
// Member-level comments (#4806)
// ---------------------------------------------------------------------------
//
// A struct field, enum variant, interface member, or match arm is a member of a
// block, not a statement in one. Members carried no span, so nothing could
// anchor a comment written against them and `format_program`'s leading-comment
// sweep adopted it onto the NEXT top-level declaration — which is why a field's
// doc comment ended up describing an unrelated struct. `later()` below is that
// next declaration: each test asserts the comment did not travel to it.

/// The reported case: a documented struct. Every field doc must stay on its
/// field, in order, and none may reach `later`.
#[test]
fn struct_field_doc_comments_stay_on_their_fields() {
    let source = "pub struct Config {\n  /** How many times to retry. */\n  retries: int,\n  /** Seconds to wait between retries. */\n  delay: int,\n}\n\npub struct Other {\n  name: string,\n}\n";
    let formatted = format_source(source).unwrap();
    let retries_doc = formatted
        .find("/** How many times to retry. */")
        .expect("retries doc was lost");
    let retries = formatted.find("retries: int").expect("no retries field");
    let delay_doc = formatted
        .find("/** Seconds to wait between retries. */")
        .expect("delay doc was lost");
    let delay = formatted.find("delay: int").expect("no delay field");
    let other = formatted.find("struct Other").expect("no Other");
    assert!(
        retries_doc < retries && retries < delay_doc && delay_doc < delay,
        "each field doc must sit directly above its own field, got:\n{formatted}"
    );
    assert!(
        delay < other,
        "field docs must not be evicted onto the next declaration, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

#[test]
fn struct_field_line_comment_stays_in_the_struct() {
    let source =
        "struct S {\n  a: int\n  // FIELD_B_NOTE\n  b: int\n}\n\nfn later() -> int {\n  return 2\n}\n";
    let formatted = format_source(source).unwrap();
    let comment = formatted.find("// FIELD_B_NOTE").expect("comment was lost");
    let b = formatted.find("b: int").expect("no b field");
    let later = formatted.find("fn later").expect("no later");
    assert!(
        comment < b && b < later,
        "the comment must stay above `b` inside the struct, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

#[test]
fn struct_field_trailing_comment_stays_on_the_field_line() {
    let source =
        "struct S {\n  a: int  // A_NOTE\n  b: int\n}\n\nfn later() -> int {\n  return 2\n}\n";
    let formatted = format_source(source).unwrap();
    assert!(
        formatted.contains("a: int  // A_NOTE"),
        "a trailing comment must stay on its field's own line, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

#[test]
fn comment_before_the_first_struct_field_stays_in_the_struct() {
    let source = "struct S {\n  // LEADING\n  a: int\n}\n\nfn later() -> int {\n  return 2\n}\n";
    let formatted = format_source(source).unwrap();
    let comment = formatted.find("// LEADING").expect("comment was lost");
    let open = formatted.find("struct S {").expect("no struct");
    let a = formatted.find("a: int").expect("no a field");
    assert!(
        open < comment && comment < a,
        "the comment must stay between `{{` and the first field, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

/// Asserted against the struct's CLOSING BRACE, not against `later`. An evicted
/// comment lands immediately above `later` and so is still textually before it —
/// a `comment < later` assertion holds whether or not the bug is present, and
/// passes against the broken formatter. Only the brace tells the two apart.
#[test]
fn comment_after_the_last_struct_field_stays_in_the_struct() {
    let source = "struct S {\n  a: int\n  // TRAILING\n}\n\nfn later() -> int {\n  return 2\n}\n";
    let formatted = format_source(source).unwrap();
    assert!(
        formatted.contains("a: int\n  // TRAILING\n}"),
        "the comment must stay inside the struct, below the last field and above \
         the closing brace, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

/// An empty body has no member to anchor against, so the comment is held by the
/// tail flush alone. Asserted against the closing brace, for the reason given on
/// `comment_after_the_last_struct_field_stays_in_the_struct`.
#[test]
fn comment_in_an_empty_struct_body_stays_in_the_struct() {
    let source = "struct S {\n  // ONLY\n}\n\nfn later() -> int {\n  return 2\n}\n";
    let formatted = format_source(source).unwrap();
    assert!(
        formatted.contains("struct S {\n  // ONLY\n}"),
        "the comment must stay inside the empty struct body, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

#[test]
fn enum_variant_comments_stay_on_their_variants() {
    let source = "enum Color {\n  // RED_NOTE\n  Red\n  Green  // GREEN_NOTE\n  // TAIL_NOTE\n}\n\nfn later() -> int {\n  return 2\n}\n";
    let formatted = format_source(source).unwrap();
    let red_note = formatted.find("// RED_NOTE").expect("RED_NOTE was lost");
    let red = formatted.find("Red").expect("no Red");
    let tail = formatted.find("// TAIL_NOTE").expect("TAIL_NOTE was lost");
    let later = formatted.find("fn later").expect("no later");
    assert!(
        red_note < red,
        "a variant's leading comment must stay above it, got:\n{formatted}"
    );
    assert!(
        formatted.contains("Green  // GREEN_NOTE"),
        "a variant's trailing comment must stay on its line, got:\n{formatted}"
    );
    assert!(
        tail < later,
        "the tail comment must stay inside the enum, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

#[test]
fn interface_member_comments_stay_on_their_members() {
    let source = "interface Shape {\n  // ITEM_NOTE\n  type Item\n  // AREA_NOTE\n  fn area() -> float\n}\n\nfn later() -> int {\n  return 2\n}\n";
    let formatted = format_source(source).unwrap();
    let item_note = formatted.find("// ITEM_NOTE").expect("ITEM_NOTE was lost");
    let item = formatted.find("type Item").expect("no type Item");
    let area_note = formatted.find("// AREA_NOTE").expect("AREA_NOTE was lost");
    let area = formatted.find("fn area").expect("no fn area");
    let later = formatted.find("fn later").expect("no later");
    assert!(
        item_note < item && item < area_note && area_note < area && area < later,
        "each interface member's comment must stay above that member, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

/// The parser sorts an interface body into an associated-type list and a method
/// list, losing the written order. Rendering by span puts it back — otherwise a
/// comment anchors to whichever member the reordering left next to it.
#[test]
fn interleaved_interface_members_keep_their_written_order() {
    let source = "interface I {\n  fn first() -> int\n  type Item\n  fn second() -> int\n}\n";
    let formatted = format_source(source).unwrap();
    let first = formatted.find("fn first").expect("no first");
    let item = formatted.find("type Item").expect("no Item");
    let second = formatted.find("fn second").expect("no second");
    assert!(
        first < item && item < second,
        "interface members must render in the order they were written, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

/// The compact arm form (`1 -> { x }`) has nowhere to put a comment written on
/// its own line, so an arm carrying one must fall back to the block form rather
/// than choose the layout first and strand the comment.
#[test]
fn comment_inside_a_match_arm_stays_in_the_arm() {
    let source = "fn pick(x: int) -> int {\n  match x {\n    1 -> {\n      // ARM_NOTE\n      10\n    }\n    _ -> { 0 }\n  }\n}\n";
    let formatted = format_source(source).unwrap();
    let note = formatted.find("// ARM_NOTE").expect("ARM_NOTE was lost");
    let ten = formatted.find("10").expect("no arm body");
    let wildcard = formatted.find("_ ->").expect("no wildcard arm");
    assert!(
        note < ten && ten < wildcard,
        "the comment must stay inside its own arm, above the body, got:\n{formatted}"
    );
    assert_roundtrip(source);
}

/// Asserted against the match's closing brace, for the reason given on
/// `comment_after_the_last_struct_field_stays_in_the_struct`.
#[test]
fn comment_after_the_last_match_arm_stays_in_the_match() {
    let source = "fn pick(x: int) -> int {\n  match x {\n    1 -> { 10 }\n    _ -> { 0 }\n    // TAIL_ARM\n  }\n}\n\nfn later() -> int {\n  return 2\n}\n";
    let formatted = format_source(source).unwrap();
    assert!(
        formatted.contains("_ -> { 0 }\n    // TAIL_ARM\n  }"),
        "the tail comment must stay inside the match, below the last arm and \
         above the closing brace, got:\n{formatted}"
    );
    assert_roundtrip(source);
}
