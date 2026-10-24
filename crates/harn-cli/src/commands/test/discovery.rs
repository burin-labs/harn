//! What a requested `harn test` target actually contains, versus what ran.
//!
//! `harn test <path>` discovers test pipelines. Conformance cases are not test
//! pipelines: they are `.harn` files driven by a sibling `.expected` file and
//! executed by a different runner. So `harn test conformance/tests` walks 2,165
//! files, finds the two dozen that happen to declare a pipeline, and prints a
//! confident `24 passed, 24 total` — a green verdict over one percent of the
//! suite, with no line anywhere saying the other 2,141 were never considered.
//!
//! Two things close that. Every path run reports how many cases it produced
//! against how many source files it selected, so a collapsed selection is
//! visible instead of implied. And a target that contains conformance fixtures
//! is refused outright, naming the suite-root form that does run them.

use std::path::Path;

use crate::test_runner::TestSourceInventory;

/// The suite root whose cases this runner does not execute.
const CONFORMANCE_ROOT_NAME: &str = "conformance";

/// One requested target's discoverable content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TargetCensus {
    /// Recursively discovered `.harn` files and explicitly selected files.
    pub source_files: usize,
    /// Of those, the `.harn` files a sibling `.expected` file marks as conformance
    /// fixtures, which the user-test runner never executes.
    pub conformance_fixtures: usize,
}

fn is_conformance_fixture(path: &Path) -> bool {
    path.with_extension("expected").is_file()
}

/// Walk one requested target and count what it holds.
///
/// Use the runner's source inventory so the denominator and execution agree.
#[cfg(test)]
pub(crate) fn census_target(path: &Path) -> TargetCensus {
    census_targets(&[path]).expect("fixture inventory")
}

/// Census every requested target as one.
pub(crate) fn census_targets<P: AsRef<Path>>(paths: &[P]) -> Result<TargetCensus, String> {
    let paths = paths
        .iter()
        .map(|path| path.as_ref().to_path_buf())
        .collect::<Vec<_>>();
    Ok(census_inventory(&TestSourceInventory::discover(&paths)?))
}

pub(super) fn census_inventory(inventory: &TestSourceInventory) -> TargetCensus {
    let mut census = TargetCensus {
        source_files: inventory.source_files().len(),
        conformance_fixtures: 0,
    };
    for path in inventory.source_files() {
        if path
            .extension()
            .is_some_and(|extension| extension == "harn")
        {
            census.conformance_fixtures += usize::from(is_conformance_fixture(path));
        }
    }
    census
}

/// True when the target sits inside the conformance suite.
///
/// Membership is decided by the path, not by the presence of `.expected` files
/// alone: scaffolded persona templates ship a `.harn` plus a `.expected` beside
/// a real test pipeline, and those are ordinary user tests that must keep
/// running. Only the suite that owns a different runner is refused.
pub(crate) fn is_inside_conformance_suite(path: &Path) -> bool {
    let absolute = path
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(path));
    absolute
        .components()
        .any(|component| component.as_os_str() == CONFORMANCE_ROOT_NAME)
}

/// The refusal for a target holding cases this runner will never execute.
///
/// Returns `None` when no requested target is inside the conformance suite,
/// which is every ordinary test directory.
pub(crate) fn conformance_fixture_refusal(
    path_strs: &[String],
    census: &TargetCensus,
) -> Option<String> {
    if census.conformance_fixtures == 0 {
        return None;
    }
    if !path_strs
        .iter()
        .any(|path| is_inside_conformance_suite(Path::new(path)))
    {
        return None;
    }
    Some(format!(
        "{} of the {} source file(s) under {} are conformance fixtures, which are driven by their `.expected` file and are never executed by `harn test <path>`. This invocation would report a confident verdict over the remainder and say nothing about the rest. Run `harn test conformance` for the whole suite, or `harn test conformance --filter <name>` for one case.",
        census.conformance_fixtures,
        census.source_files,
        path_strs.join(", "),
    ))
}

/// The line printed after every path run, naming what ran against what was
/// walked.
///
/// A count is only evidence when the denominator is visible: `24 passed` and
/// `24 passed out of 2165 files walked` are the same number and completely
/// different verdicts.
pub(crate) fn coverage_line(path_strs: &[String], census: &TargetCensus, cases: usize) -> String {
    format!(
        "test targets: ran {cases} case(s) from {} discoverable source file(s) under {}",
        census.source_files,
        path_strs.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture dir");
        }
        std::fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn a_directory_inside_the_conformance_suite_is_refused_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("conformance/tests");
        write(&target.join("one.harn"), "// fixture\n");
        write(&target.join("one.expected"), "ok\n");
        write(&target.join("two.harn"), "// fixture\n");
        write(&target.join("two.expected"), "ok\n");
        write(&target.join("real_test.harn"), "// pipeline\n");

        let census = census_target(&target);
        assert_eq!(census.source_files, 3);
        assert_eq!(census.conformance_fixtures, 2);

        let refusal =
            conformance_fixture_refusal(&[target.to_string_lossy().into_owned()], &census)
                .expect("a target inside the conformance suite must be refused");
        assert!(refusal.starts_with("2 of the 3"));
        assert!(refusal.contains("harn test conformance"));
    }

    #[test]
    fn an_ordinary_test_directory_is_not_refused() {
        // The negative control. Without it the refusal could reject every
        // target and still look correct in the case above.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("tests");
        write(&target.join("a_test.harn"), "// pipeline\n");
        write(&target.join("b_test.harn"), "// pipeline\n");

        let census = census_target(&target);
        assert_eq!(census.source_files, 2);
        assert_eq!(census.conformance_fixtures, 0);
        assert_eq!(
            conformance_fixture_refusal(&[target.to_string_lossy().into_owned()], &census),
            None
        );
    }

    #[test]
    fn a_scaffolded_template_with_an_expected_sibling_still_runs() {
        // Persona templates ship a `.harn` plus a `.expected` beside a real
        // test pipeline. They are ordinary user tests, and refusing them would
        // trade one silent-zero bug for a loud false one.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("assets/persona-templates/sweeper/tests");
        write(&target.join("smoke.harn"), "// pipeline\n");
        write(&target.join("smoke.expected"), "ok\n");

        let census = census_target(&target);
        assert_eq!(census.conformance_fixtures, 1);
        assert_eq!(
            conformance_fixture_refusal(&[target.to_string_lossy().into_owned()], &census),
            None
        );
    }

    #[test]
    fn a_single_conformance_file_is_refused_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("conformance/tests/one.harn");
        write(&target, "// fixture\n");
        write(&target.with_extension("expected"), "ok\n");

        let census = census_target(&target);
        assert_eq!(census.source_files, 1);
        assert_eq!(census.conformance_fixtures, 1);
        assert!(
            conformance_fixture_refusal(&[target.to_string_lossy().into_owned()], &census)
                .is_some()
        );
    }

    #[test]
    fn explicit_sources_without_harn_extensions_count_without_becoming_conformance_fixtures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plain = dir.path().join("source_without_extension");
        let text = dir.path().join("explicit.harn.txt");
        for path in [&plain, &text] {
            write(path, "pipeline test_explicit() {}\n");
            write(&path.with_extension("expected"), "ok\n");
        }
        let census = census_targets(&[plain, text]).expect("explicit source inventory");
        assert_eq!(census.source_files, 2);
        assert_eq!(census.conformance_fixtures, 0);
    }

    #[test]
    fn the_coverage_line_names_both_numbers() {
        let census = TargetCensus {
            source_files: 2165,
            conformance_fixtures: 0,
        };
        let line = coverage_line(&["conformance/tests".to_string()], &census, 24);
        assert!(line.contains("ran 24 case(s)"));
        assert!(line.contains("2165 discoverable"));
    }
}
