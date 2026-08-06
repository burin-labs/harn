//! What a directory target expands to.
//!
//! `lint`, `fmt`, and `check` all accept a directory and walk it for sources,
//! so the walk's skip decisions decide what those commands can possibly
//! report. A file named explicitly is never subject to them.

use super::*;

#[test]
fn collect_harn_targets_recurses_directories_and_deduplicates() {
    let dir = unique_temp_dir("harn-check-targets");
    // Project ignore files are only honored inside a project, so the tree
    // needs the repository marker that `harn_vm::ignore_policy` looks for
    // before the `.gitignore` written below means anything.
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::create_dir_all(dir.join(".build").join("generated")).unwrap();
    std::fs::create_dir_all(dir.join(".claude").join("worktrees").join("copy")).unwrap();
    std::fs::create_dir_all(dir.join(".harn-eval-abc123")).unwrap();
    std::fs::create_dir_all(dir.join("node_modules").join("pkg")).unwrap();
    std::fs::write(dir.join("a.harn"), "pipeline a() {}\n").unwrap();
    std::fs::write(dir.join("site.harn.txt"), "pipeline site() {}\n").unwrap();
    std::fs::write(dir.join("nested").join("b.harn"), "pipeline b() {}\n").unwrap();
    std::fs::write(
        dir.join("nested").join("skipped.harn"),
        "pipeline skipped() {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("nested").join("skipped.conformance-skip"), "").unwrap();
    std::fs::write(
        dir.join("ignored_by_gitignore.harn"),
        "pipeline ignored() {}\n",
    )
    .unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored_by_gitignore.harn\n").unwrap();
    std::fs::write(
        dir.join(".build").join("generated").join("ignored.harn"),
        "pipeline generated() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".claude")
            .join("worktrees")
            .join("copy")
            .join("ignored.harn"),
        "pipeline worktree_copy() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".harn-eval-abc123").join("ignored.harn"),
        "pipeline eval_scratch() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".harn-eval-abc123.harn"),
        "pipeline eval_scratch_file() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("node_modules").join("pkg").join("ignored.harn"),
        "pipeline dependency() {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("nested").join("ignore.txt"), "x\n").unwrap();

    let target_dir = dir.display().to_string();
    let target_file = dir.join("a.harn").display().to_string();
    let files = collect_harn_targets(&[target_dir.as_str(), target_file.as_str()]);

    assert_eq!(files.len(), 3);
    assert!(files.contains(&dir.join("a.harn")));
    assert!(files.contains(&dir.join("site.harn.txt")));
    assert!(files.contains(&dir.join("nested").join("b.harn")));
    assert!(!files.contains(&dir.join("ignored_by_gitignore.harn")));
    assert!(!files.contains(&dir.join("nested").join("skipped.harn")));

    let ignored_file = dir.join("ignored_by_gitignore.harn").display().to_string();
    let skipped_file = dir
        .join("nested")
        .join("skipped.harn")
        .display()
        .to_string();
    let site_snippet = dir.join("site.harn.txt").display().to_string();
    let explicit_files = collect_harn_targets(&[
        ignored_file.as_str(),
        skipped_file.as_str(),
        site_snippet.as_str(),
    ]);
    assert_eq!(
        explicit_files,
        vec![
            dir.join("ignored_by_gitignore.harn"),
            dir.join("nested").join("skipped.harn"),
            dir.join("site.harn.txt"),
        ]
    );
    let explicit_generated_dir = dir.join(".harn-eval-abc123").display().to_string();
    let generated_files = collect_harn_targets(&[explicit_generated_dir.as_str()]);
    assert_eq!(
        generated_files,
        vec![dir.join(".harn-eval-abc123").join("ignored.harn")]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A checkout whose *parent* ignores everything still enumerates its own
/// sources.
///
/// This is the shape of every agent worktree under `~/.cursor`, which carries a
/// `.gitignore` of `*` to keep the tool's own state out of git. Git never
/// consults it for the repository below, because git stops looking at the
/// repository root; the `ignore` crate stops there only when a repository
/// anchors its upward search, and this walk used to give it nothing to anchor
/// on. Every directory target in such a checkout then expanded to no files at
/// all — which `lint` and `fmt` report as an error, and which the rule-driven
/// surfaces report as a clean tree.
#[test]
fn collect_harn_targets_ignores_ignore_files_above_the_repository_root() {
    let parent = unique_temp_dir("harn-check-ignoring-parent");
    let repo = parent.join("checkout");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(parent.join(".gitignore"), "*\n").unwrap();
    std::fs::write(repo.join("a.harn"), "pipeline a() {}\n").unwrap();

    let files = collect_harn_targets(&[repo.display().to_string().as_str()]);

    assert_eq!(files, vec![repo.join("a.harn")]);
    let _ = std::fs::remove_dir_all(&parent);
}
