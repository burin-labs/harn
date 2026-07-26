use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ignore::WalkBuilder;
use tempfile::TempDir;

use super::{configure, effective_policy, is_scratch_path, IgnorePolicy, BUILTIN_IGNORED_DIRS};

/// `GIT_CONFIG_GLOBAL` is process-global; only the determinism receipts touch
/// it, and they take turns.
static GIT_ENV_LOCK: Mutex<()> = Mutex::new(());

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture parent");
    }
    std::fs::write(&path, contents).expect("write fixture");
}

/// A temp directory that looks like a checkout, so `project_root_for` anchors
/// there instead of falling through to the no-project exemption.
fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".git")).expect("create .git");
    dir
}

fn walk_with(base: &Path, policy: IgnorePolicy, include_hidden: bool) -> Vec<String> {
    let mut builder = WalkBuilder::new(base);
    builder.sort_by_file_name(|left, right| left.cmp(right));
    configure(&mut builder, base, policy, include_hidden).expect("configure walk");
    builder
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            entry
                .path()
                .strip_prefix(base)
                .ok()
                .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

/// Files only, hidden included: these fixtures are about the ignore stack, and
/// coupling them to dotfile filtering would test the wrong axis.
fn walk(base: &Path, policy: IgnorePolicy) -> Vec<String> {
    walk_with(base, policy, true)
}

fn has(entries: &[String], name: &str) -> bool {
    entries.iter().any(|entry| entry == name)
}

#[test]
fn builtin_list_omits_vendor() {
    // `go mod vendor` output is committed in many Go repositories; a built-in
    // skip would silently drop tracked source.
    assert!(!BUILTIN_IGNORED_DIRS.contains(&"vendor"));
    for expected in [
        ".git",
        ".harn",
        ".harn-runs",
        ".harn-tmp",
        ".hg",
        ".next",
        ".svn",
        ".venv",
        "__pycache__",
        "build",
        "coverage",
        "dist",
        "node_modules",
        "target",
        "venv",
    ] {
        assert!(
            BUILTIN_IGNORED_DIRS.contains(&expected),
            "missing built-in ignored directory {expected}"
        );
    }
    assert_eq!(BUILTIN_IGNORED_DIRS.len(), 15);
}

#[test]
fn builtin_layer_skips_default_directories_but_not_lookalike_files() {
    let dir = repo();
    write(dir.path(), "src/main.rs", "fn main() {}\n");
    write(dir.path(), "target/debug/app", "binary\n");
    write(dir.path(), "node_modules/pkg/index.js", "\n");
    write(dir.path(), "dist/out.js", "\n");
    // A regular file named `build` is source, not build output. Every built-in
    // pattern carries a trailing slash so it can only match a directory.
    write(dir.path(), "build", "#!/bin/sh\n");

    let entries = walk(dir.path(), IgnorePolicy::Builtin);
    assert!(has(&entries, "src/main.rs"), "{entries:?}");
    assert!(has(&entries, "build"), "{entries:?}");
    assert!(!has(&entries, "target/debug/app"), "{entries:?}");
    assert!(!has(&entries, "node_modules/pkg/index.js"), "{entries:?}");
    assert!(!has(&entries, "dist/out.js"), "{entries:?}");
}

/// The critical receipt: the built-in defaults are a real lowest-precedence
/// layer, not a hard filter. A project ignore file can take them back.
#[test]
fn gitignore_negation_reincludes_a_builtin_directory() {
    let dir = repo();
    write(dir.path(), "dist/out.js", "console.log(1)\n");
    write(dir.path(), ".gitignore", "!dist/\n");

    let builtin_only = walk(dir.path(), IgnorePolicy::Builtin);
    assert!(
        !has(&builtin_only, "dist/out.js"),
        "built-in layer should skip dist without a project file: {builtin_only:?}"
    );

    let project = walk(dir.path(), IgnorePolicy::Project);
    assert!(
        has(&project, "dist/out.js"),
        "`!dist/` must outrank the built-in layer: {project:?}"
    );
}

#[test]
fn agentignore_outranks_ignore_which_outranks_gitignore() {
    let dir = repo();
    write(dir.path(), "only_git.txt", "a\n");
    write(dir.path(), "git_then_ignore.txt", "b\n");
    write(dir.path(), "git_then_ignore_then_agent.txt", "c\n");
    write(
        dir.path(),
        ".gitignore",
        "only_git.txt\ngit_then_ignore.txt\ngit_then_ignore_then_agent.txt\n",
    );
    write(
        dir.path(),
        ".ignore",
        "!git_then_ignore.txt\n!git_then_ignore_then_agent.txt\n",
    );
    write(
        dir.path(),
        ".agentignore",
        "git_then_ignore_then_agent.txt\n",
    );

    let entries = walk(dir.path(), IgnorePolicy::Project);
    assert!(!has(&entries, "only_git.txt"), "{entries:?}");
    assert!(has(&entries, "git_then_ignore.txt"), "{entries:?}");
    assert!(
        !has(&entries, "git_then_ignore_then_agent.txt"),
        "{entries:?}"
    );
}

/// Harn writes a `*` `.gitignore` into its own scratch directories. Honoring
/// it would make `glob("*.json", workspace_temp_dir())` return nothing.
#[test]
fn scratch_root_is_exempt_from_the_ignore_stack() {
    let dir = repo();
    write(dir.path(), ".harn-tmp/.gitignore", "*\n");
    write(dir.path(), ".harn-tmp/data.json", "{}\n");
    // Same layout under an ordinary name, to prove the exemption is what
    // rescues the scratch walk rather than some accident of the fixture.
    write(dir.path(), "scratch/.gitignore", "*\n");
    write(dir.path(), "scratch/data.json", "{}\n");

    let scratch = walk(&dir.path().join(".harn-tmp"), IgnorePolicy::Project);
    assert!(has(&scratch, "data.json"), "{scratch:?}");

    let ordinary = walk(&dir.path().join("scratch"), IgnorePolicy::Project);
    assert!(!has(&ordinary, "data.json"), "{ordinary:?}");
}

#[test]
fn scratch_detection_matches_harn_managed_names() {
    assert!(is_scratch_path(Path::new("/ws/.harn-tmp/run/out.json")));
    assert!(is_scratch_path(Path::new("/ws/.harn-toolchain-cache")));
    assert!(!is_scratch_path(Path::new("/ws/.harn-runs/run")));
    assert!(!is_scratch_path(Path::new("/ws/src")));
}

#[test]
fn base_outside_any_project_keeps_builtin_hygiene() {
    // No `.git` anywhere above and no sandbox workspace root, so there are no
    // project rules to honor and the `*` here must not blank the walk. The
    // built-in layer still applies: an unmanaged directory is exactly where an
    // unwanted `node_modules` descent is most likely.
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), ".gitignore", "*\n");
    write(dir.path(), "node_modules/pkg.js", "\n");
    write(dir.path(), "keep.txt", "\n");

    assert_eq!(
        effective_policy(dir.path(), IgnorePolicy::Project),
        IgnorePolicy::Builtin
    );
    let entries = walk(dir.path(), IgnorePolicy::Project);
    assert!(has(&entries, "keep.txt"), "{entries:?}");
    assert!(!has(&entries, "node_modules/pkg.js"), "{entries:?}");
}

#[test]
fn an_explicit_none_is_never_escalated_into_filtering() {
    // Degrading `Project` to `Builtin` outside a project must not also promote
    // a caller who explicitly asked for a raw walk.
    let dir = tempfile::tempdir().expect("tempdir");
    write(dir.path(), "node_modules/pkg.js", "\n");

    assert_eq!(
        effective_policy(dir.path(), IgnorePolicy::None),
        IgnorePolicy::None
    );
    let entries = walk(dir.path(), IgnorePolicy::None);
    assert!(has(&entries, "node_modules/pkg.js"), "{entries:?}");
}

#[test]
fn the_three_levels_behave_distinctly() {
    let dir = repo();
    write(dir.path(), ".gitignore", "logs/\n");
    write(dir.path(), "logs/a.txt", "\n");
    write(dir.path(), "target/b.txt", "\n");
    write(dir.path(), "src/c.txt", "\n");

    let none = walk(dir.path(), IgnorePolicy::None);
    assert!(has(&none, "logs/a.txt"), "{none:?}");
    assert!(has(&none, "target/b.txt"), "{none:?}");
    assert!(has(&none, "src/c.txt"), "{none:?}");

    let builtin = walk(dir.path(), IgnorePolicy::Builtin);
    assert!(has(&builtin, "logs/a.txt"), "{builtin:?}");
    assert!(!has(&builtin, "target/b.txt"), "{builtin:?}");
    assert!(has(&builtin, "src/c.txt"), "{builtin:?}");

    let project = walk(dir.path(), IgnorePolicy::Project);
    assert!(!has(&project, "logs/a.txt"), "{project:?}");
    assert!(!has(&project, "target/b.txt"), "{project:?}");
    assert!(has(&project, "src/c.txt"), "{project:?}");
}

#[test]
fn hidden_files_are_a_separate_axis_from_the_ignore_stack() {
    let dir = repo();
    write(dir.path(), ".github/workflows/ci.yml", "on: push\n");
    write(dir.path(), "src/main.rs", "\n");

    let with_hidden = walk_with(dir.path(), IgnorePolicy::Project, true);
    assert!(
        has(&with_hidden, ".github/workflows/ci.yml"),
        "{with_hidden:?}"
    );

    let without_hidden = walk_with(dir.path(), IgnorePolicy::Project, false);
    assert!(
        !has(&without_hidden, ".github/workflows/ci.yml"),
        "{without_hidden:?}"
    );
    assert!(has(&without_hidden, "src/main.rs"), "{without_hidden:?}");
}

/// The walk root itself is never matched against the ignore stack, so pointing
/// a caller straight at `target/` still enumerates it.
#[test]
fn a_base_inside_an_ignored_directory_is_still_walked() {
    let dir = repo();
    write(dir.path(), "target/debug/app.txt", "\n");

    let entries = walk(&dir.path().join("target"), IgnorePolicy::Project);
    assert!(has(&entries, "debug/app.txt"), "{entries:?}");
}

/// Determinism: the same repository at the same commit must enumerate
/// identically on any machine. `.git/info/exclude` is per-checkout and
/// uncommitted, so it must not participate.
#[test]
fn git_info_exclude_is_not_honored() {
    let dir = repo();
    write(dir.path(), ".git/info/exclude", "secret.txt\n");
    write(dir.path(), "secret.txt", "\n");

    let entries = walk(dir.path(), IgnorePolicy::Project);
    assert!(has(&entries, "secret.txt"), "{entries:?}");
}

/// Determinism: a developer's `core.excludesFile` is machine-local state and
/// must not change what a walk returns.
#[test]
fn global_core_excludes_file_is_not_honored() {
    let _guard = GIT_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    let home = tempfile::tempdir().expect("tempdir");
    write(home.path(), "global_ignore", "planted.txt\n");
    write(
        home.path(),
        "gitconfig",
        &format!(
            "[core]\n\texcludesFile = {}\n",
            home.path().join("global_ignore").display()
        ),
    );

    let dir = repo();
    write(dir.path(), "planted.txt", "\n");

    let previous = std::env::var_os("GIT_CONFIG_GLOBAL");
    std::env::set_var("GIT_CONFIG_GLOBAL", home.path().join("gitconfig"));
    let entries = walk(dir.path(), IgnorePolicy::Project);
    match previous {
        Some(value) => std::env::set_var("GIT_CONFIG_GLOBAL", value),
        None => std::env::remove_var("GIT_CONFIG_GLOBAL"),
    }

    assert!(has(&entries, "planted.txt"), "{entries:?}");
}

#[test]
fn unknown_level_is_rejected_with_the_accepted_values() {
    let error = IgnorePolicy::parse_for("glob", "gitignore").expect_err("must reject");
    assert!(
        error.contains("unknown ignore_policy `gitignore`"),
        "{error}"
    );
    assert!(error.contains("none, builtin, project"), "{error}");

    for level in [
        IgnorePolicy::None,
        IgnorePolicy::Builtin,
        IgnorePolicy::Project,
    ] {
        assert_eq!(IgnorePolicy::parse(level.as_str()), Some(level));
    }
    assert_eq!(IgnorePolicy::default(), IgnorePolicy::Project);
}

#[test]
fn a_repository_root_rule_applies_to_a_walk_rooted_in_a_subdirectory() {
    // The common shape: a guard globs one crate but the rule that hides
    // generated output lives in the repository-root `.gitignore`, anchored to a
    // path the subdirectory cannot see. Losing this makes the walk *add*
    // generated artifacts.
    let dir = repo();
    write(dir.path(), ".gitignore", "/sub/generated/\n");
    write(dir.path(), "sub/generated/out.rs", "\n");
    write(dir.path(), "sub/kept.rs", "\n");

    let entries = walk(&dir.path().join("sub"), IgnorePolicy::Project);
    assert!(has(&entries, "kept.rs"), "{entries:?}");
    assert!(!has(&entries, "generated/out.rs"), "{entries:?}");
}

#[test]
fn ignore_files_above_the_repository_root_do_not_participate() {
    // `parents(true)` is bounded by the crate's `saw_git` gate: once the walk
    // passes a directory holding `.git`, higher `.gitignore` files stop being
    // consulted. Without that bound a stray file above a checkout would change
    // results per machine.
    let outer = tempfile::tempdir().expect("tempdir");
    write(outer.path(), ".gitignore", "kept.rs\n");
    std::fs::create_dir_all(outer.path().join("checkout/.git")).expect("git dir");
    write(outer.path(), "checkout/kept.rs", "\n");

    let entries = walk(&outer.path().join("checkout"), IgnorePolicy::Project);
    assert!(has(&entries, "kept.rs"), "{entries:?}");
}

#[test]
fn the_materialized_builtin_layer_is_stable_within_a_process() {
    let first: PathBuf = super::builtin_ignore_file()
        .expect("materialize")
        .to_path_buf();
    let second: PathBuf = super::builtin_ignore_file()
        .expect("materialize")
        .to_path_buf();
    assert_eq!(first, second);
    let text = std::fs::read_to_string(&first).expect("read built-in layer");
    assert_eq!(text, super::builtin_ignore_text());
}

/// The canonical negation receipt, mirroring the shape this repository
/// actually ships: a blanket ignore of a built-in name, a negation that takes
/// it back, and later rules that re-exclude specific subpaths.
///
/// Harn's own `conformance/tests/**/.harn/` skill fixtures are tracked only
/// because of exactly this pattern. If the built-in `.harn` skip were a hard
/// filter rather than the lowest layer, they would vanish from every walk and
/// fixture discovery would find nothing — a vacuous pass, not a failure. The
/// fixture is self-contained so it cannot drift with the repository's own
/// `.gitignore`.
#[test]
fn a_blanket_ignore_a_negation_and_a_later_re_exclusion_all_compose() {
    let dir = repo();
    write(
        dir.path(),
        ".gitignore",
        concat!(
            ".harn/\n",
            ".harn*/\n",
            "!fixtures/**/.harn/\n",
            "!fixtures/**/.harn/**\n",
            "fixtures/**/.harn/metadata/\n",
        ),
    );
    // Outside `fixtures/`, the built-in layer and the blanket rule both apply.
    write(dir.path(), ".harn/store.json", "{}\n");
    // Under `fixtures/`, the negation must win over the built-in `.harn` skip.
    write(
        dir.path(),
        "fixtures/basic/.harn/skills/deploy/SKILL.md",
        "# deploy\n",
    );
    write(
        dir.path(),
        "fixtures/basic/.harn/skills/review/SKILL.md",
        "# review\n",
    );
    // ...but the later rule must still re-exclude this subpath.
    write(
        dir.path(),
        "fixtures/basic/.harn/metadata/shard.json",
        "{}\n",
    );

    let project = walk(dir.path(), IgnorePolicy::Project);
    assert!(
        !has(&project, ".harn/store.json"),
        "the built-in skip must still apply outside the negation: {project:?}"
    );
    assert!(
        has(&project, "fixtures/basic/.harn/skills/deploy/SKILL.md"),
        "a `.gitignore` negation must outrank the built-in layer: {project:?}"
    );
    assert!(
        has(&project, "fixtures/basic/.harn/skills/review/SKILL.md"),
        "{project:?}"
    );
    assert!(
        !has(&project, "fixtures/basic/.harn/metadata/shard.json"),
        "a later rule must still re-exclude its subpath: {project:?}"
    );

    // Without project files the negation is unreachable, so the built-in layer
    // hides the whole tree. This is what proves layer 1 is doing the skipping.
    let builtin = walk(dir.path(), IgnorePolicy::Builtin);
    assert!(
        !has(&builtin, "fixtures/basic/.harn/skills/deploy/SKILL.md"),
        "{builtin:?}"
    );
}

/// A sandbox-workspace anchor has no `.git` to bound an upward search, so the
/// search does not start — while the workspace's own ignore files still apply.
#[test]
fn a_sandbox_workspace_anchor_reads_its_own_rules_but_no_ancestor_rules() {
    let outer = tempfile::tempdir().expect("tempdir");
    write(outer.path(), ".gitignore", "kept.txt\n");
    let workspace = outer.path().join("workspace");
    write(&workspace, ".gitignore", "local.txt\n");
    write(&workspace, "kept.txt", "\n");
    write(&workspace, "local.txt", "\n");
    write(&workspace, "src/main.rs", "\n");

    crate::orchestration::push_execution_policy(crate::orchestration::CapabilityPolicy {
        workspace_roots: vec![workspace.display().to_string()],
        ..Default::default()
    });
    let entries = walk(&workspace, IgnorePolicy::Project);
    let anchor = super::project_root_for(&super::absolutize(&workspace));
    crate::orchestration::pop_execution_policy();

    assert!(
        matches!(anchor, Some((_, super::ProjectAnchor::SandboxWorkspace))),
        "expected a workspace anchor, got {anchor:?}"
    );
    assert!(
        has(&entries, "kept.txt"),
        "an ignore file above the workspace root must not apply: {entries:?}"
    );
    assert!(
        !has(&entries, "local.txt"),
        "the workspace's own `.gitignore` must still apply: {entries:?}"
    );
    assert!(has(&entries, "src/main.rs"), "{entries:?}");
}

/// Pattern anchoring is asymmetric, and callers hit it: the walker prunes an
/// ignored directory before any pattern is tested, so naming it inside the
/// pattern finds nothing while naming it as the walk base works.
#[test]
fn an_ignored_directory_is_reachable_as_a_base_but_not_as_a_pattern_prefix() {
    let dir = repo();
    write(dir.path(), "target/debug/build.rs", "\n");

    let mut builder = WalkBuilder::new(dir.path());
    configure(&mut builder, dir.path(), IgnorePolicy::Project, true).expect("configure");
    let pruned = builder
        .build()
        .filter_map(Result::ok)
        .any(|entry| entry.path().ends_with("build.rs"));
    assert!(
        !pruned,
        "the walker prunes `target` before any pattern runs"
    );

    let rooted = walk(&dir.path().join("target"), IgnorePolicy::Project);
    assert!(has(&rooted, "debug/build.rs"), "{rooted:?}");
}
