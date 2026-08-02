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

#[cfg(test)]
mod tests {
    use super::{is_harn_internal_entry, PathEntryKind};

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
}
