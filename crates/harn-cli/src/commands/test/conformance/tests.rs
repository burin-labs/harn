use super::{
    collect_harn_files_sorted, evaluate_conformance_case, lint_expectation_error, logical_path,
    parse_xfail_marker, resolve_conformance_selection, select_conformance_shard,
    ConformanceRunOptions,
};
use std::fs;
use std::path::Path;

struct TempTestDir {
    dir: tempfile::TempDir,
}

impl TempTestDir {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("harn-cli-test-")
            .tempdir()
            .unwrap();
        Self { dir }
    }

    fn write(&self, relative: &str) {
        self.write_content(relative, "// test");
    }

    fn write_content(&self, relative: &str, content: &str) {
        let path = self.dir.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

#[test]
fn collect_harn_files_sorted_descends_and_sorts() {
    let temp = TempTestDir::new();
    temp.write("suite/zeta.harn");
    temp.write("suite/alpha.harn");
    temp.write("suite/nested/beta.harn");
    fs::write(temp.path().join("suite/ignore.txt"), "").unwrap();

    let files = collect_harn_files_sorted(&temp.path().join("suite"));
    let relative: Vec<String> = files
        .iter()
        .map(|path| logical_path(path.strip_prefix(temp.path()).unwrap()))
        .collect();

    assert_eq!(
        relative,
        vec![
            "suite/alpha.harn",
            "suite/nested/beta.harn",
            "suite/zeta.harn"
        ]
    );
}

#[test]
fn logical_path_uses_slashes_for_native_test_paths() {
    let path = Path::new("suite").join("nested").join("beta.harn");

    assert_eq!(logical_path(&path), "suite/nested/beta.harn");
}

#[test]
fn conformance_shards_are_disjoint_and_cover_the_sorted_suite() {
    let suite = (0..10).collect::<Vec<_>>();
    let shards = (1..=3)
        .map(|index| {
            select_conformance_shard(
                suite.clone(),
                Some(crate::test_runner::TestShard::new(index, 3).unwrap()),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(shards, [vec![0, 3, 6, 9], vec![1, 4, 7], vec![2, 5, 8]]);
    let mut covered = shards.into_iter().flatten().collect::<Vec<_>>();
    covered.sort_unstable();
    assert_eq!(covered, suite);
}

#[test]
fn resolve_conformance_selection_accepts_suite_relative_file() {
    let temp = TempTestDir::new();
    temp.write("conformance/tests/sample.harn");

    let files =
        resolve_conformance_selection(&temp.path().join("conformance"), Some("tests/sample.harn"))
            .unwrap();

    assert_eq!(files.len(), 1);
    assert!(files[0].ends_with("conformance/tests/sample.harn"));
}

#[test]
fn resolve_conformance_selection_rejects_paths_outside_suite_root() {
    let temp = TempTestDir::new();
    temp.write("conformance/tests/sample.harn");
    temp.write("outside.harn");

    let error =
        resolve_conformance_selection(&temp.path().join("conformance"), Some("../outside.harn"))
            .unwrap_err();

    assert!(error.contains("must be inside"));
}

#[test]
fn parse_xfail_marker_recognizes_top_of_file_marker() {
    let src = "// @xfail: tracked in #1240\npipeline main(task) {}\n";
    assert_eq!(parse_xfail_marker(src).as_deref(), Some("tracked in #1240"));
}

#[test]
fn parse_xfail_marker_recognizes_indented_marker() {
    let src = "    // @xfail: skill matching #1240\n";
    assert_eq!(
        parse_xfail_marker(src).as_deref(),
        Some("skill matching #1240")
    );
}

#[test]
fn parse_xfail_marker_returns_none_when_absent() {
    let src = "// regular comment\npipeline main(task) {}\n";
    assert!(parse_xfail_marker(src).is_none());
}

#[test]
fn parse_xfail_marker_ignores_marker_past_first_50_lines() {
    let mut src = String::new();
    for _ in 0..60 {
        src.push_str("// filler\n");
    }
    src.push_str("// @xfail: too late\n");
    assert!(parse_xfail_marker(&src).is_none());
}

#[test]
fn parse_xfail_marker_ignores_empty_reason() {
    let src = "// @xfail:   \n";
    assert!(parse_xfail_marker(src).is_none());
}

#[test]
fn parse_xfail_marker_recognizes_one_line_doc_comment() {
    let src = "/** @xfail: tracked in #1240 */\npipeline test() {}\n";
    assert_eq!(parse_xfail_marker(src).as_deref(), Some("tracked in #1240"));
}

#[test]
fn parse_xfail_marker_recognizes_multi_line_doc_comment() {
    let src = "/**\n * @xfail: tracked in #1238\n */\nfn foo() {}\n";
    assert_eq!(parse_xfail_marker(src).as_deref(), Some("tracked in #1238"));
}

#[test]
fn parse_xfail_marker_recognizes_block_comment() {
    let src = "/* @xfail: tracked in #1239 */\nfn foo() {}\n";
    assert_eq!(parse_xfail_marker(src).as_deref(), Some("tracked in #1239"));
}

#[test]
fn lint_expectations_require_at_least_one_assertion() {
    assert_eq!(
        lint_expectation_error("", "\n  \n").as_deref(),
        Some("lint expectation file is empty")
    );
}

#[test]
fn lint_expectations_support_required_and_forbidden_patterns() {
    let actual = "HARN-LNT-066 error lint[discarded-pure-result]";
    assert_eq!(
        lint_expectation_error(actual, "HARN-LNT-066\n!HARN-RMD-003"),
        None
    );
    assert_eq!(
        lint_expectation_error(actual, "!HARN-LNT-066").as_deref(),
        Some("forbidden lint matched: HARN-LNT-066")
    );
}

fn discarded_pure_result_source() -> &'static str {
    r#"fn main(harness: Harness) {
  const items = []
  items.push(1)
  harness.stdio.println("")
}
"#
}

fn conformance_options() -> ConformanceRunOptions<'static> {
    ConformanceRunOptions {
        verbose: false,
        timing: false,
        differential_optimizations: false,
        json: false,
        shard: None,
        cli_skill_dirs: &[],
    }
}

#[tokio::test]
async fn expected_output_and_lint_expectations_are_additive() {
    let temp = TempTestDir::new();
    temp.write_content(
        "conformance/tests/additive.harn",
        discarded_pure_result_source(),
    );
    temp.write_content("conformance/tests/additive.expected", "");
    temp.write_content(
        "conformance/tests/additive.lint",
        "HARN-LNT-066\n!HARN-RMD-003\n",
    );
    let harn_file = temp.path().join("conformance/tests/additive.harn");

    let evaluation = evaluate_conformance_case(
        &harn_file,
        &harn_file.with_extension("expected"),
        &harn_file.with_extension("error"),
        &harn_file.with_extension("lint"),
        "tests/additive.harn",
        2_000,
        &conformance_options(),
    )
    .await;

    assert!(evaluation.passed, "{:?}", evaluation.message);
    assert_eq!(evaluation.diagnostic_codes, ["HARN-LNT-066"]);
}

#[tokio::test]
async fn expected_error_and_lint_expectations_are_additive() {
    let temp = TempTestDir::new();
    temp.write_content(
        "conformance/tests/additive_error.harn",
        r#"fn main(harness: Harness) {
  const items = []
  items.push(1)
  harness.stdio.println("")
  throw "boom"
}
"#,
    );
    temp.write_content("conformance/tests/additive_error.error", "boom");
    temp.write_content(
        "conformance/tests/additive_error.lint",
        "HARN-LNT-066\n!HARN-RMD-003\n",
    );
    let harn_file = temp.path().join("conformance/tests/additive_error.harn");

    let evaluation = evaluate_conformance_case(
        &harn_file,
        &harn_file.with_extension("expected"),
        &harn_file.with_extension("error"),
        &harn_file.with_extension("lint"),
        "tests/additive_error.harn",
        2_000,
        &conformance_options(),
    )
    .await;

    assert!(evaluation.passed, "{:?}", evaluation.message);
    assert_eq!(evaluation.diagnostic_codes, ["HARN-LNT-066"]);
}

#[tokio::test]
async fn expected_output_fails_on_unasserted_error_lint() {
    let temp = TempTestDir::new();
    temp.write_content(
        "conformance/tests/unasserted.harn",
        discarded_pure_result_source(),
    );
    temp.write_content("conformance/tests/unasserted.expected", "");
    let harn_file = temp.path().join("conformance/tests/unasserted.harn");

    let evaluation = evaluate_conformance_case(
        &harn_file,
        &harn_file.with_extension("expected"),
        &harn_file.with_extension("error"),
        &harn_file.with_extension("lint"),
        "tests/unasserted.harn",
        2_000,
        &conformance_options(),
    )
    .await;

    assert!(!evaluation.passed);
    assert!(evaluation
        .message
        .as_deref()
        .is_some_and(|message| message.contains("unasserted error lint")));
}

#[tokio::test]
async fn empty_lint_fixture_fails_instead_of_passing_vacuously() {
    let temp = TempTestDir::new();
    temp.write_content(
        "conformance/tests/empty_lint.harn",
        discarded_pure_result_source(),
    );
    temp.write_content("conformance/tests/empty_lint.lint", "\n");
    let harn_file = temp.path().join("conformance/tests/empty_lint.harn");

    let evaluation = evaluate_conformance_case(
        &harn_file,
        &harn_file.with_extension("expected"),
        &harn_file.with_extension("error"),
        &harn_file.with_extension("lint"),
        "tests/empty_lint.harn",
        2_000,
        &conformance_options(),
    )
    .await;

    assert!(!evaluation.passed);
    assert!(evaluation
        .message
        .as_deref()
        .is_some_and(|message| message.contains("lint expectation file is empty")));
}

#[tokio::test]
async fn conformance_harness_sidecar_error_fails_expected_error_fixture() {
    let temp = TempTestDir::new();
    temp.write_content(
        "conformance/tests/harness_sidecar_error.harn",
        r#"fn main(harness: Harness) {
  harness.env.get("TOKEN")
}
"#,
    );
    temp.write_content(
        "conformance/tests/harness_sidecar_error.error",
        "NullHarness denied",
    );
    temp.write_content(
        "conformance/tests/harness_sidecar_error.harness.json",
        r#"{
  "mode": "null",
  "expect_deny_events": [
    {
      "sub_handle": "env",
      "method": "wrong",
      "args": ["TOKEN"]
    }
  ]
}
"#,
    );

    let harn_file = temp
        .path()
        .join("conformance/tests/harness_sidecar_error.harn");
    let expected_file = harn_file.with_extension("expected");
    let error_file = harn_file.with_extension("error");
    let lint_file = harn_file.with_extension("lint");
    let options = ConformanceRunOptions {
        verbose: false,
        timing: false,
        differential_optimizations: false,
        json: false,
        shard: None,
        cli_skill_dirs: &[],
    };

    let evaluation = evaluate_conformance_case(
        &harn_file,
        &expected_file,
        &error_file,
        &lint_file,
        "tests/harness_sidecar_error.harn",
        2_000,
        &options,
    )
    .await;

    assert!(!evaluation.passed);
    let message = evaluation.message.unwrap_or_default();
    assert!(
        message.contains("harness deny events differed"),
        "unexpected message: {message}"
    );
}
