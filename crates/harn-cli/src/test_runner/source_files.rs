use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use harn_package::precompile::{
    should_skip_recursive_source_dir, should_skip_recursive_source_file,
};
use harn_vm::ignore_policy::{self, IgnorePolicy};

/// The source files and test candidates captured for one requested selection.
pub(crate) struct TestSourceInventory {
    sources: Vec<PathBuf>,
    tests: Vec<PathBuf>,
    pub(super) started_at: Instant,
    pub(super) collection_ms: u64,
}

impl TestSourceInventory {
    pub(super) fn collect(targets: &[PathBuf], started_at: Instant) -> Result<Self, String> {
        let mut sources = BTreeSet::new();
        let mut explicit = BTreeSet::new();
        for target in targets {
            let target = super::canonicalize_existing_path(target);
            if target.is_dir() {
                collect_directory(&target, &mut sources)?;
            } else {
                sources.insert(target.clone());
                explicit.insert(target);
            }
        }
        let sources = sources.into_iter().collect::<Vec<_>>();
        let mut tests = Vec::new();
        for path in &sources {
            let selected = if explicit.contains(path) {
                true
            } else {
                let text = std::fs::read_to_string(path)
                    .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
                text.contains("test_") || text.contains("@test")
            };
            if selected {
                tests.push(path.clone());
            }
        }
        Ok(Self {
            sources,
            tests,
            started_at,
            collection_ms: started_at.elapsed().as_millis() as u64,
        })
    }

    pub(crate) fn source_files(&self) -> &[PathBuf] {
        &self.sources
    }

    pub(crate) fn test_files(&self) -> &[PathBuf] {
        &self.tests
    }
}

fn collect_directory(root: &Path, sources: &mut BTreeSet<PathBuf>) -> Result<(), String> {
    let mut walker = ignore::WalkBuilder::new(root);
    ignore_policy::configure(&mut walker, root, IgnorePolicy::Project, true)
        .map_err(|error| format!("failed to discover tests under {}: {error}", root.display()))?;
    walker.follow_links(false).filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            !should_skip_recursive_source_dir(entry.path())
        } else {
            !should_skip_recursive_source_file(entry.path())
        }
    });
    for entry in walker.build() {
        let entry = entry.map_err(|error| {
            format!("failed to discover tests under {}: {error}", root.display())
        })?;
        let path = entry.path();
        if entry.file_type().is_some_and(|kind| kind.is_file())
            && path
                .extension()
                .is_some_and(|extension| extension == "harn")
        {
            sources.insert(super::canonicalize_existing_path(path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, name: &str, source: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create directory");
        std::fs::write(&path, source).expect("write source");
        path.canonicalize().expect("canonical source")
    }

    #[test]
    fn recursive_inventory_preserves_project_sources_and_excludes_internal_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("project");
        std::fs::create_dir_all(root.join(".git")).expect("project anchor");
        write(temp.path(), ".gitignore", "*\n");
        write(&root, ".gitignore", "ignored/\n");
        write(&root, ".agentignore", "agent_ignored/\n");
        let source = "pipeline test_example() {}\n";
        let mut expected = [
            "tests/test_visible.harn",
            ".github/test_hidden_source.harn",
            "vendor/test_vendor.harn",
        ]
        .map(|name| write(&root, name, source))
        .to_vec();
        write(&root, "vendor/test_vendor.conformance-skip", "parked\n");
        for name in [
            ".harn/vendor/pkg/test_dependency.harn",
            ".harn-runs/test_snapshot.harn",
            ".harn-eval-snapshot.harn",
            "target/test_generated.harn",
            "node_modules/pkg/test_dependency.harn",
            "ignored/test_ignored.harn",
            "agent_ignored/test_ignored.harn",
        ] {
            write(&root, name, source);
        }
        let helper = write(
            &root,
            "lib/helper.harn",
            "pub fn value() -> int { return 1 }\n",
        );
        expected.sort();
        let inventory = TestSourceInventory::discover(&[root.clone(), root.join("tests")])
            .expect("discover project");
        assert_eq!(inventory.test_files(), expected);
        expected.push(helper);
        expected.sort();
        assert_eq!(inventory.source_files(), expected);
    }

    #[test]
    fn explicit_files_and_directory_roots_override_recursive_exclusions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        let source = "pipeline test_explicit() {}\n";
        let dependency = write(root, ".harn/vendor/pkg/test_dependency.harn", source);
        let generated = write(root, ".harn-eval-source.harn", source);
        let inventory = TestSourceInventory::discover(&[
            root.join(".harn/vendor/pkg"),
            dependency.clone(),
            generated.clone(),
        ])
        .expect("discover explicit targets");
        let mut expected = vec![dependency, generated];
        expected.sort();
        assert_eq!(inventory.source_files(), expected);
        assert_eq!(inventory.test_files(), expected);
    }

    #[test]
    fn unreadable_source_content_is_not_an_empty_inventory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let invalid = temp.path().join("invalid.harn");
        std::fs::write(&invalid, [0xff]).expect("write invalid UTF-8");
        let error = TestSourceInventory::discover(&[temp.path().to_path_buf()])
            .err()
            .expect("source read must fail");
        assert!(error.contains("failed to read"), "{error}");
        assert!(error.contains("invalid.harn"), "{error}");
    }
}
