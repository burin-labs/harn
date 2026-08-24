//! The verdict for a conformance run that executed no tests.
//!
//! Kept apart from the runner because it is the one place that decides what
//! "ran nothing" means, what it is worth reporting about, and how it exits.
//! Both the sequential runner and the parallel parent call in here, so the two
//! paths cannot drift in exit code, error code, or wording.

use std::path::{Path, PathBuf};
use std::process;

use crate::json_envelope::{self, JsonEnvelope};

use super::{ConformanceJsonReport, CONFORMANCE_TEST_SCHEMA_VERSION};

/// The sibling that tells the runner what to check a conformance test against.
///
/// A `.harn` file with none is not a test. That is a normal shape here, not a
/// defect: the suite also holds library modules that real tests import — the
/// shared `_common.harn` and the `modules/` helper family — so a missing
/// sibling cannot be an error on its own without failing those too.
pub(super) fn expectation_sibling(harn_file: &Path) -> Option<PathBuf> {
    ["expected", "error", "lint"]
        .into_iter()
        .map(|extension| harn_file.with_extension(extension))
        .find(|candidate| candidate.exists())
}

/// How many of `selected` the runner can see but cannot run.
///
/// Recomputed from the selection rather than plumbed out of the run, so both
/// the sequential path and the parallel parent can name the count without a
/// change to the worker JSON contract.
pub(super) fn unrunnable_count(selected: &[(PathBuf, String)]) -> usize {
    selected
        .iter()
        .filter(|(harn_file, _)| expectation_sibling(harn_file).is_none())
        .count()
}

/// The diagnosis for a run that executed no tests.
///
/// Names what was asked for and the cause worth checking, so the message is
/// actionable without reading the runner's source.
pub(super) fn empty_run_message(
    suite_root: &Path,
    selection: Option<&str>,
    filter: Option<&str>,
    selected: &[(PathBuf, String)],
) -> String {
    let mut message = format!(
        "no conformance tests ran under {} (selection: {}, filter: {})",
        suite_root.display(),
        selection.unwrap_or("<whole suite>"),
        filter.unwrap_or("<none>"),
    );
    let unrunnable = unrunnable_count(selected);
    if unrunnable > 0 {
        message.push_str(&format!(
            "; {unrunnable} of {} selected file(s) have no .expected, .error, or .lint sibling, so the runner could see them but not run them",
            selected.len()
        ));
    } else if selected.is_empty() {
        message.push_str("; the selection matched no files at all - check the path and the filter");
    }
    message.push_str(
        ". A run that executed nothing is reported as a failure because an empty selection is otherwise indistinguishable from every test passing. Pass --allow-empty if an empty selection is expected here.",
    );
    message
}

/// Report a run that executed nothing and exit non-zero.
///
/// Exits 1, the same as a run with a failing test — this is a verdict about
/// the run, not a usage error, so it deliberately does not take the
/// setup-error path that prints a usage block.
pub(super) fn report_empty_run(json: bool, message: String) -> ! {
    if json {
        let envelope: JsonEnvelope<ConformanceJsonReport> = JsonEnvelope::err(
            CONFORMANCE_TEST_SCHEMA_VERSION,
            "conformance_empty_selection",
            message,
        );
        println!("{}", json_envelope::to_string_pretty(&envelope));
    } else {
        eprintln!("{message}");
    }
    process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempTestDir {
        dir: tempfile::TempDir,
    }

    impl TempTestDir {
        fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("harn-cli-empty-run-")
                .tempdir()
                .unwrap();
            Self { dir }
        }

        fn write(&self, relative: &str) {
            let path = self.dir.path().join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, "// test").unwrap();
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    #[test]
    fn expectation_sibling_finds_each_accepted_extension() {
        for extension in ["expected", "error", "lint"] {
            let temp = TempTestDir::new();
            temp.write("tests/case.harn");
            temp.write(&format!("tests/case.{extension}"));
            let harn_file = temp.path().join("tests/case.harn");
            assert_eq!(
                expectation_sibling(&harn_file),
                Some(harn_file.with_extension(extension)),
                "{extension} sibling should make the file runnable"
            );
        }
    }

    /// A library module a real test imports has no sibling and is not a defect.
    /// It is only notable when it is the whole selection, which is what the
    /// unrunnable count in the diagnosis is for.
    #[test]
    fn expectation_sibling_absent_for_a_library_module() {
        let temp = TempTestDir::new();
        temp.write("tests/_common.harn");
        assert_eq!(
            expectation_sibling(&temp.path().join("tests/_common.harn")),
            None
        );
    }

    #[test]
    fn unrunnable_count_counts_only_the_files_without_a_sibling() {
        let temp = TempTestDir::new();
        temp.write("tests/runnable.harn");
        temp.write("tests/runnable.expected");
        temp.write("tests/_common.harn");
        temp.write("tests/modules/helper.harn");
        let selected: Vec<(PathBuf, String)> = [
            "tests/runnable.harn",
            "tests/_common.harn",
            "tests/modules/helper.harn",
        ]
        .into_iter()
        .map(|relative| (temp.path().join(relative), relative.to_string()))
        .collect();

        assert_eq!(unrunnable_count(&selected), 2);
    }

    /// The diagnosis has to name the cause, not just the symptom: a selection that
    /// matched files the runner could see but not run reads very differently from
    /// one that matched nothing at all.
    #[test]
    fn empty_run_message_names_the_selection_the_filter_and_the_cause() {
        let temp = TempTestDir::new();
        temp.write("tests/_common.harn");
        let selected = vec![(
            temp.path().join("tests/_common.harn"),
            "tests/_common.harn".to_string(),
        )];

        let message = empty_run_message(
            temp.path(),
            Some("tests/_common.harn"),
            Some("common"),
            &selected,
        );
        assert!(message.contains("tests/_common.harn"), "{message}");
        assert!(message.contains("common"), "{message}");
        assert!(
            message.contains("1 of 1 selected file(s) have no .expected"),
            "{message}"
        );
        assert!(message.contains("--allow-empty"), "{message}");
    }

    #[test]
    fn empty_run_message_distinguishes_a_selection_that_matched_nothing() {
        let temp = TempTestDir::new();
        let message = empty_run_message(temp.path(), None, None, &[]);
        assert!(message.contains("<whole suite>"), "{message}");
        assert!(message.contains("<none>"), "{message}");
        assert!(message.contains("matched no files at all"), "{message}");
        assert!(
            !message.contains("selected file(s) have no"),
            "a selection that matched nothing must not blame missing siblings: {message}"
        );
    }
}
