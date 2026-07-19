//! Line-breaking layout.
//!
//! One rule: a break is decided by the width of the FORMATTED result, at the
//! depth the fragment will actually occupy. These tests pin that rule against
//! the three ways it was previously violated — deciding from source newlines,
//! wrapping args at the wrong depth, and measuring a prefix instead of a line.

use super::assert_roundtrip;
use crate::{
    format_source, format_source_opts, line_width_violations, FmtOptions, LINE_WIDTH_DEFAULT,
};

fn formatted(source: &str) -> String {
    format_source(source).unwrap()
}

fn assert_within_line_width(source: &str) {
    let out = formatted(source);
    for line in out.lines() {
        assert!(
            line.chars().count() <= LINE_WIDTH_DEFAULT,
            "line exceeds width {}: {} chars\n{line}",
            LINE_WIDTH_DEFAULT,
            line.chars().count(),
        );
    }
}

#[test]
fn formatted_repository_corpus_has_no_breakable_width_overflow() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("harn-fmt lives two levels below the repository root");
    let roots = [
        "crates/harn-stdlib/src/stdlib",
        "conformance/tests",
        "experiments",
        "scripts",
        "crates/harn-cli/assets/demo",
        "personas",
        "tests",
        "examples",
        "evals",
    ];
    let skipped = [
        "semicolon_statements.harn",
        "semicolon_if_else_invalid.harn",
        "semicolon_try_catch_invalid.harn",
        "semicolon_empty_statement_invalid.harn",
        "import_broken_module_lib.harn",
    ];

    let mut files = Vec::new();
    for root in roots {
        collect_harn_files(&repo_root.join(root), &skipped, &mut files);
    }
    files.sort();

    for path in files {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let formatted = format_source(&source)
            .unwrap_or_else(|error| panic!("failed to format {}: {error}", path.display()));
        let reformatted = format_source(&formatted).unwrap_or_else(|error| {
            panic!("formatted {} is not parseable: {error}", path.display())
        });
        assert_eq!(
            reformatted,
            formatted,
            "formatted {} is not idempotent",
            path.display()
        );
        let violations = line_width_violations(&formatted, LINE_WIDTH_DEFAULT);
        assert!(
            violations.is_empty(),
            "{} has breakable width overflow: {:?}",
            path.display(),
            violations
        );
    }
}

fn collect_harn_files(
    root: &std::path::Path,
    skipped: &[&str],
    files: &mut Vec<std::path::PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_harn_files(&path, skipped, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "harn")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !skipped.contains(&name))
        {
            files.push(path);
        }
    }
}

/// The author's line breaks are not a layout instruction. The same call written
/// inline and written across several lines must format identically.
#[test]
fn source_newlines_do_not_split_a_short_method_call() {
    let inline = formatted(r#"fn t(v: any) { v.push(Violation {key: "a", blocking: true}) }"#);
    let exploded = formatted(
        "fn t(v: any) {\n  v.push(Violation {\n    key: \"a\",\n    blocking: true,\n  })\n}\n",
    );
    assert_eq!(
        inline, exploded,
        "layout must depend on the formatted width, not on how the author broke lines"
    );
    assert!(
        inline.contains(r#"v.push(Violation {key: "a", blocking: true})"#),
        "a short call must stay on one line, not split its receiver from its method:\n{inline}"
    );
}

/// Reformatting a chain must preserve whether each hop is nil-propagating.
/// This also guards the mechanical corpus rewrite: `?.` is behavior, not
/// layout, even when the receiver is known non-nil at one call site.
#[test]
fn chain_layout_preserves_safe_navigation() {
    let source = "fn t(value: any) {\n  return value?.actor_token_types\n}\n";
    let out = formatted(source);
    assert!(
        out.contains("value?.actor_token_types"),
        "safe navigation was changed into a strict property access:\n{out}"
    );
    assert_roundtrip(source);
}

fn indent_of(out: &str, needle: &str) -> Option<usize> {
    out.lines()
        .find(|l| l.trim_start().starts_with(needle))
        .map(|l| l.len() - l.trim_start().len())
}

/// When a chain wraps onto its own line, its arguments must wrap one level
/// INSIDE it. An argument must never sit level with, or outdent from, the call
/// that owns it.
///
/// The test asserts its own precondition: if the chain stops wrapping, this
/// FAILS rather than passing vacuously with an unexercised body.
#[test]
fn wrapped_chain_indents_args_inside_the_method_that_owns_them() {
    // A comment between the segments forces the wrap, which is the path under
    // test. Using a comment rather than sheer length keeps the precondition
    // independent of the width arithmetic this same rule decides.
    let out = formatted(
        "fn t(v: any, alpha: string, beta: string) {\n  v\n    // wrap me\n    \
         .second(Violation {key: alpha, message: beta + alpha + beta + alpha + beta + alpha + beta + alpha, blocking: true})\n}\n",
    );

    let call_col = indent_of(&out, ".second(").unwrap_or_else(|| {
        panic!("precondition not met: the chain did not wrap onto its own line:\n{out}")
    });
    let arg_col = indent_of(&out, "Violation {").unwrap_or_else(|| {
        panic!("precondition not met: the struct-literal argument did not wrap:\n{out}")
    });

    assert!(
        arg_col > call_col,
        "argument at col {arg_col} must be deeper than the call at col {call_col} that owns it:\n{out}"
    );
}

/// An attached chain still starts after the enclosing block's indentation.
/// The argument layout must count those known columns, not measure the
/// receiver as though it started at column zero.
#[test]
fn nested_unwrapped_chain_counts_indent_at_width_twenty() {
    let source = "fn t() {\n  if true {\n    values.add(a, b, c)\n  }\n}\n";
    let out = format_source_opts(
        source,
        &FmtOptions {
            line_width: 20,
            ..FmtOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        out,
        "fn t() {\n  if true {\n    values.add(\n      a,\n      b,\n      c,\n    )\n  }\n}\n"
    );
}

/// The wrap decision has to budget for the whole line: `)`, the return type,
/// and the ` {` the caller appends. Counting only the prefix let signatures
/// land past the declared width.
#[test]
fn signature_width_accounts_for_return_type_and_body_brace() {
    let source = "pub fn check_baseline(counts: dict, baseline: CountBaseline, vocabulary: RatchetVocabulary) -> list<Violation> {\n  return []\n}\n";
    assert_within_line_width(source);
}

/// The boundary, from both sides. Budgeting for the suffix must not tip into
/// double-counting the `)` that `format_comma_sequence` already adds: a
/// signature that fits EXACTLY must stay on one line.
#[test]
fn signature_that_fits_exactly_is_not_wrapped() {
    let source =
        "fn __verification_gate_classification(value, current_hashes: dict, hashes_available: bool) -> dict {\n  return {}\n}\n";
    let first = source.lines().next().unwrap();
    assert_eq!(
        first.chars().count(),
        LINE_WIDTH_DEFAULT,
        "precondition: this fixture must sit exactly on the width"
    );
    let out = formatted(source);
    assert!(
        out.lines().next() == Some(first),
        "a signature that fits exactly must not wrap:\n{out}"
    );
    assert_within_line_width(source);
}

#[test]
fn deep_plus_chains_format_without_recursive_stack_growth() {
    let terms = (0..80)
        .map(|index| format!("part_{index}"))
        .collect::<Vec<_>>();
    let source = format!("fn t() {{\n  return {}\n}}\n", terms.join(" + "));
    let out = formatted(&source);
    assert_within_line_width(&out);
    assert_roundtrip(&source);
}

#[test]
fn signature_one_column_over_does_wrap() {
    let source =
        "fn __verification_gate_classification(value, current_hashes: dict, hashes_available: booly) -> dict {\n  return {}\n}\n";
    assert_eq!(
        source.lines().next().unwrap().chars().count(),
        LINE_WIDTH_DEFAULT + 1,
        "precondition: this fixture must sit one column over"
    );
    assert_within_line_width(source);
}

#[test]
fn signature_without_return_type_still_respects_width() {
    let source = "pub fn configure_the_ratchet(counts: dict, baseline: CountBaseline, vocabulary: RatchetVocabulary) {\n  log(\"x\")\n}\n";
    assert_within_line_width(source);
}

#[test]
fn expression_prefix_counts_toward_line_width() {
    assert_within_line_width(
        "fn t() {\n  let x = some_function_with_a_pretty_long_name_that_will_wrap_its_args(arg_one, arg_two, arg_three)?.map(item)\n}\n",
    );
}

#[test]
fn require_messages_wrap_after_the_condition() {
    let source = "fn t() {\n  require true, \"this diagnostic message is intentionally just long enough to require a continuation line\"\n}\n";
    let out = formatted(source);
    assert!(out.contains("require true,\n"), "{out}");
    assert_within_line_width(source);
}

/// #4878 repro 1, verbatim.
#[test]
fn issue_4878_push_with_struct_literal_argument() {
    let out = formatted(
        "pub struct Violation {\n  key: string\n  blocking: bool\n}\n\n\
         fn build() -> list<Violation> {\n  const violations: list<Violation> = []\n  \
         violations.push(Violation {\n    key: \"a\",\n    blocking: true,\n  })\n  \
         return violations\n}\n",
    );
    // The defect rendered the receiver alone on its own line, with `.push(`
    // orphaned beneath it. Assert on the line, not on a substring that
    // `return violations` also satisfies.
    assert!(
        !out.lines().any(|l| l.trim() == "violations"),
        "receiver must not be split onto its own line for a call that fits:\n{out}"
    );
    assert!(
        !out.lines().any(|l| l.trim_start().starts_with(".push(")),
        "`.push(` must stay attached to its receiver for a call that fits:\n{out}"
    );
}

/// #4878 repro 2, verbatim.
#[test]
fn issue_4878_signature_is_not_joined_past_the_width() {
    assert_within_line_width(
        "pub fn check_baseline(counts: dict, baseline: CountBaseline, vocabulary: RatchetVocabulary) -> list<Violation> {\n  return []\n}\n",
    );
}

/// Layout must be stable: formatting the output again changes nothing.
#[test]
fn wrapped_layout_is_idempotent() {
    let source = "fn t(v: any, a: string, b: string, c: string, d: string) {\n  \
         v.chained(a).push(Violation {key: a, message: b + c + d + a + b + c + d + a + b, blocking: true})\n}\n";
    let once = formatted(source);
    let twice = formatted(&once);
    assert_eq!(
        once, twice,
        "formatter is not idempotent for wrapped layout"
    );
}

// Relocated from `tests.rs`: these are layout tests, and they belong beside the
// rule they exercise rather than in the general formatter suite.
#[test]
fn test_method_call_args_dont_overcount_multiline_receiver() {
    // When the receiver of a method call wraps to multiple lines, the
    // method-call args should be laid out based on the new line's column,
    // not the receiver's total byte length. With short args, they should
    // stay on the same line as the `.method(`.
    //
    // The receiver here is multi-line because its OWN arguments overflow the
    // width. That is the condition this test names. It previously wrote the
    // receiver on one line and split the chain in the source instead, which
    // only exercised the multi-line path because layout was (wrongly) keyed on
    // source newlines; the formatter now keys on the formatted receiver.
    let source = r"pipeline default(task) {
  let x = some_function_with_a_pretty_long_name_that_will_wrap_its_args(argument_number_one, argument_number_two, argument_number_three).map(item)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains('\n'),
        "precondition: the receiver must wrap:\n{result}"
    );
    // Short args list (just `item`) must NOT wrap onto its own line just
    // because the receiver wrapped onto multiple lines above.
    assert!(
        result.contains(".map(item)"),
        "trailing method args wrapped unnecessarily after multi-line receiver:\n{result}"
    );
    assert_roundtrip(source);
}

#[test]
fn test_optional_method_call_args_dont_overcount_multiline_receiver() {
    // See the sibling above: the receiver is multi-line by width, not by where
    // the author happened to press return.
    let source = r"pipeline default(task) {
  let x = some_function_with_a_pretty_long_name_that_will_wrap_its_args(argument_number_one, argument_number_two, argument_number_three)?.map(item)
}";
    let result = format_source(source).unwrap();
    assert!(
        result.contains('\n'),
        "precondition: the receiver must wrap:\n{result}"
    );
    assert!(
        result.contains("?.map(item)"),
        "trailing optional method args wrapped unnecessarily after multi-line receiver:\n{result}"
    );
    assert_roundtrip(source);
}
