use std::path::Path;

use crate::path_policy::{is_harn_internal_entry, PathEntryKind};

pub(crate) fn should_exclude_package_entry(relative: &Path, kind: PathEntryKind) -> bool {
    if kind == PathEntryKind::Directory && relative == Path::new("docs").join("dist") {
        return true;
    }
    let Some(name) = relative.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(name, ".git" | ".harn")
        || is_harn_internal_entry(name, kind)
        || (kind == PathEntryKind::Directory && matches!(name, "target" | "node_modules"))
}

#[cfg(test)]
mod tests {
    use super::should_exclude_package_entry;
    use crate::path_policy::PathEntryKind;
    use std::path::Path;

    #[test]
    fn package_entry_policy_is_closed_over_kind_and_relative_path() {
        use PathEntryKind::{Directory, File};

        let cases = [
            (".harn", Directory, true),
            (".harn", File, true),
            ("src/.harn-runs", Directory, true),
            (".harn-version", File, false),
            (".git", File, true),
            ("target", Directory, true),
            ("nested/node_modules", Directory, true),
            ("docs/dist", Directory, true),
            ("nested/docs/dist", Directory, false),
            ("lib/main.harn", File, false),
        ];
        for (relative, kind, expected) in cases {
            assert_eq!(
                should_exclude_package_entry(Path::new(relative), kind),
                expected,
                "{relative} ({kind:?})"
            );
        }
    }
}
