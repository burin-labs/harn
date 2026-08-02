use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathEntryKind {
    File,
    Directory,
}

impl PathEntryKind {
    pub(crate) fn from_is_directory(is_directory: bool) -> Self {
        if is_directory {
            Self::Directory
        } else {
            Self::File
        }
    }
}

pub(crate) fn is_harn_internal_entry(name: &str, kind: PathEntryKind) -> bool {
    kind == PathEntryKind::Directory && (name == ".harn" || name.starts_with(".harn-"))
}

/// Return whether an entry is the repository-root generated documentation tree.
///
/// This is deliberately root-anchored. Other generated-source walkers have
/// broader component rules because they own a different input boundary.
pub(crate) fn is_generated_docs_output(relative: &Path, kind: PathEntryKind) -> bool {
    kind == PathEntryKind::Directory && relative == Path::new("docs").join("dist")
}

#[cfg(test)]
mod tests {
    use super::{is_generated_docs_output, is_harn_internal_entry, PathEntryKind};
    use std::path::Path;

    #[test]
    fn harn_internal_state_is_a_typed_directory_policy() {
        for name in [".harn", ".harn-runs", ".harn-policy"] {
            assert!(
                is_harn_internal_entry(name, PathEntryKind::Directory),
                "{name}"
            );
            assert!(!is_harn_internal_entry(name, PathEntryKind::File), "{name}");
        }
        for name in ["harn", ".git", "target", "node_modules"] {
            assert!(
                !is_harn_internal_entry(name, PathEntryKind::Directory),
                "{name}"
            );
        }
    }

    #[test]
    fn generated_docs_output_is_root_anchored_and_directory_only() {
        assert!(is_generated_docs_output(
            Path::new("docs/dist"),
            PathEntryKind::Directory
        ));
        assert!(!is_generated_docs_output(
            Path::new("nested/docs/dist"),
            PathEntryKind::Directory
        ));
        assert!(!is_generated_docs_output(
            Path::new("docs/dist"),
            PathEntryKind::File
        ));
    }
}
