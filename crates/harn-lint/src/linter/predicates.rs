//! Predicates that classify a source position: is this a test context, an
//! entry pipeline, an assert, an approval record.
//!
//! These are pure classification with no walk state beyond the file path, so
//! they sit apart from the walk itself.

use super::Linter;

impl<'a> Linter<'a> {
    pub(super) fn in_test_pipeline(&self) -> bool {
        self.test_pipeline_depth > 0
    }

    /// Whether an `assert` here is a test assert.
    ///
    /// [`in_test_pipeline`](Self::in_test_pipeline) is lexical and sees only a
    /// `pipeline test_*` body, so a helper `fn` that a test pipeline calls
    /// reads as production control flow no matter where it lives. Keying the
    /// rule on the enclosing function rather than on the file is what made
    /// every assert in a test helper a finding, which is a false positive by
    /// construction: a file under a test root is a test file in all of it.
    ///
    /// A file linted without a path (stdin, an embedded snippet) keeps the
    /// old behaviour, because there is nothing to read a root from.
    pub(super) fn in_test_source(&self) -> bool {
        self.in_test_pipeline()
            || self
                .file_path
                .as_deref()
                .is_some_and(crate::is_test_source_path)
    }

    pub(super) fn is_test_pipeline_name(name: &str) -> bool {
        name == "test" || name.starts_with("test_")
    }

    pub(super) fn is_entry_pipeline_name(name: &str) -> bool {
        matches!(name, "default" | "main" | "auto")
    }

    pub(super) fn is_assert_builtin(name: &str) -> bool {
        matches!(name, "assert" | "assert_eq" | "assert_ne")
    }

    pub(super) fn is_approval_record_builtin(name: &str) -> bool {
        name == "request_approval"
    }
}
