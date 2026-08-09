//! One source of truth for repository-wide formatter corpus discovery.
//!
//! The corpus is a source audit, so Git's tracked-plus-unignored projection is
//! the owning boundary. Walking the live worktree admitted `.harn-tmp` files
//! that parallel tests create and delete, making formatter tests race unrelated
//! runtime tests and audit files that are not repository source at all.

use std::path::{Path, PathBuf};
use std::process::Command;

const CORPUS_ROOTS: &[&str] = &[
    "crates/harn-stdlib/src/stdlib",
    "conformance/tests",
    "crates/harn-cli/assets/demo",
    "experiments",
    "scripts",
    "personas",
    "tests",
    "examples",
    "evals",
];

const FORMATTER_SYNTAX_FIXTURES: &[&str] = &[
    "semicolon_statements.harn",
    "semicolon_if_else_invalid.harn",
    "semicolon_try_catch_invalid.harn",
    "semicolon_empty_statement_invalid.harn",
    "import_broken_module_lib.harn",
];

pub(super) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("harn-fmt must live under <workspace>/crates")
        .to_path_buf()
}

/// Tracked and unignored, not-yet-added Harn sources in the canonical corpus.
pub(super) fn repository_harn_files() -> Result<Vec<PathBuf>, String> {
    let root = workspace_root();
    let output = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .args(CORPUS_ROOTS)
        .current_dir(&root)
        .output()
        .map_err(|error| format!("cannot enumerate formatter corpus with Git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Git could not enumerate formatter corpus: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let paths = String::from_utf8(output.stdout)
        .map_err(|error| format!("formatter corpus contains a non-UTF-8 Git path: {error}"))?;
    let mut files = paths
        .split('\0')
        .filter(|relative| !relative.is_empty())
        .map(|relative| root.join(relative))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "harn")
        })
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !FORMATTER_SYNTAX_FIXTURES.contains(&name))
        })
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    Ok(files)
}
