//! End-to-end coverage for the persistent `harn check` result cache (#4391):
//! warm runs replay byte-identical output from `$HARN_CACHE_DIR/check/`, and
//! every keyed input — source content, imported-module content, referenced
//! template files, config, kill-switch env — invalidates or bypasses it.

use std::path::Path;
use std::process::Output;

mod test_util;

fn run_check(cache_dir: &Path, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = test_util::process::harn_e2e_command();
    cmd.arg("check")
        .args(args)
        .current_dir(cwd)
        .env("HARN_CACHE_DIR", cache_dir);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().expect("run harn check")
}

fn assert_same_output(cold: &Output, warm: &Output, context: &str) {
    assert_eq!(
        cold.status.code(),
        warm.status.code(),
        "{context}: exit code drifted"
    );
    assert_eq!(
        String::from_utf8_lossy(&cold.stdout),
        String::from_utf8_lossy(&warm.stdout),
        "{context}: stdout drifted"
    );
    assert_eq!(
        String::from_utf8_lossy(&cold.stderr),
        String::from_utf8_lossy(&warm.stderr),
        "{context}: stderr drifted"
    );
}

fn cache_artifacts(cache_dir: &Path) -> usize {
    std::fs::read_dir(cache_dir.join("check"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext == "harncheck")
                })
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn warm_check_replays_identical_output_and_writes_artifacts() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cache = tempfile::TempDir::new().expect("cache dir");
    let root = temp.path();
    std::fs::write(root.join("lib.harn"), "pub fn shared() {\n  return 1\n}\n").expect("write lib");
    std::fs::write(
        root.join("main.harn"),
        "import { shared } from \"./lib\"\npipeline main(task) {\n  return shared()\n}\n",
    )
    .expect("write main");

    let cold = run_check(cache.path(), root, &["main.harn", "lib.harn"], &[]);
    assert!(
        cold.status.success(),
        "cold check failed:\n{}",
        String::from_utf8_lossy(&cold.stderr)
    );
    assert!(
        cache_artifacts(cache.path()) >= 2,
        "expected cached artifacts after cold run"
    );

    let warm = run_check(cache.path(), root, &["main.harn", "lib.harn"], &[]);
    assert_same_output(&cold, &warm, "warm replay");

    // JSON mode replays from the same artifacts.
    let cold_json = run_check(cache.path(), root, &["--json", "main.harn"], &[]);
    let warm_json = run_check(cache.path(), root, &["--json", "main.harn"], &[]);
    assert_same_output(&cold_json, &warm_json, "json warm replay");
}

#[test]
fn editing_source_or_import_invalidates() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cache = tempfile::TempDir::new().expect("cache dir");
    let root = temp.path();
    std::fs::write(root.join("lib.harn"), "pub fn shared() {\n  return 1\n}\n").expect("lib");
    std::fs::write(
        root.join("main.harn"),
        "import { shared } from \"./lib\"\npipeline main(task) {\n  return shared()\n}\n",
    )
    .expect("main");

    let cold = run_check(cache.path(), root, &["main.harn"], &[]);
    assert!(cold.status.success());

    // Break the *imported* module: the dependent's cached result must not
    // replay (its import-graph hash changed), so the type error surfaces.
    std::fs::write(root.join("lib.harn"), "fn private_now() {\n  return 1\n}\n").expect("lib2");
    let after_import_edit = run_check(cache.path(), root, &["main.harn"], &[]);
    assert!(
        !after_import_edit.status.success(),
        "editing the imported module must invalidate the dependent's cache;\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&after_import_edit.stdout),
        String::from_utf8_lossy(&after_import_edit.stderr)
    );

    // Restore and confirm it heals (also from cache keys, not stale state).
    std::fs::write(root.join("lib.harn"), "pub fn shared() {\n  return 1\n}\n").expect("lib3");
    let healed = run_check(cache.path(), root, &["main.harn"], &[]);
    assert!(healed.status.success());
}

#[test]
fn template_probe_invalidates_when_file_appears() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cache = tempfile::TempDir::new().expect("cache dir");
    let root = temp.path();
    // render() of a missing template -> preflight diagnostic recorded with an
    // existence probe on the candidate paths.
    std::fs::write(
        root.join("main.harn"),
        "pipeline main(task) {\n  return render(\"prompt.md\", {})\n}\n",
    )
    .expect("main");

    let missing = run_check(cache.path(), root, &["main.harn"], &[]);
    assert!(
        !missing.status.success(),
        "missing template should fail preflight;\nstderr:\n{}",
        String::from_utf8_lossy(&missing.stderr)
    );

    // Creating the template must flip the probe and re-check, not replay the
    // stale diagnostic.
    std::fs::write(root.join("prompt.md"), "hello {{name}}\n").expect("template");
    let present = run_check(cache.path(), root, &["main.harn"], &[]);
    assert!(
        present.status.success(),
        "template now exists; cached missing-template diagnostic must not replay;\nstderr:\n{}",
        String::from_utf8_lossy(&present.stderr)
    );

    // And editing the template's content re-validates too (content probe).
    let before_edit = run_check(cache.path(), root, &["main.harn"], &[]);
    std::fs::write(root.join("prompt.md"), "hello {{name\n").expect("break template");
    let after_edit = run_check(cache.path(), root, &["main.harn"], &[]);
    assert!(
        before_edit.status.success(),
        "sanity: pre-edit run should pass"
    );
    assert!(
        !after_edit.status.success(),
        "template syntax break must invalidate the cached ok result;\nstderr:\n{}",
        String::from_utf8_lossy(&after_edit.stderr)
    );
}

#[test]
fn kill_switches_disable_the_cache() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cache = tempfile::TempDir::new().expect("cache dir");
    let root = temp.path();
    std::fs::write(
        root.join("main.harn"),
        "pipeline main(task) {\n  return 1\n}\n",
    )
    .expect("main");

    let off = run_check(
        cache.path(),
        root,
        &["main.harn"],
        &[("HARN_CHECK_RESULT_CACHE", "0")],
    );
    assert!(off.status.success());
    assert_eq!(
        cache_artifacts(cache.path()),
        0,
        "HARN_CHECK_RESULT_CACHE=0 must not write artifacts"
    );

    let off_shared = run_check(
        cache.path(),
        root,
        &["main.harn"],
        &[("HARN_BYTECODE_CACHE", "0")],
    );
    assert!(off_shared.status.success());
    assert_eq!(
        cache_artifacts(cache.path()),
        0,
        "HARN_BYTECODE_CACHE=0 must disable the check cache too"
    );

    let on = run_check(cache.path(), root, &["main.harn"], &[]);
    assert!(on.status.success());
    assert!(
        cache_artifacts(cache.path()) >= 1,
        "cache writes when enabled"
    );
}

#[test]
fn config_change_invalidates() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let cache = tempfile::TempDir::new().expect("cache dir");
    let root = temp.path();
    // An unused local fn produces a lint warning by default.
    std::fs::write(
        root.join("main.harn"),
        "fn helper() {\n  return 1\n}\npipeline main(task) {\n  return 1\n}\n",
    )
    .expect("main");

    let default_run = run_check(cache.path(), root, &["main.harn"], &[]);
    let default_stderr = String::from_utf8_lossy(&default_run.stderr).into_owned();

    // Disabling the rule via harn.toml changes the effective config -> new
    // key -> the warning disappears instead of replaying.
    std::fs::write(
        root.join("harn.toml"),
        "[check]\ndisable_rules = [\"unused-function\"]\n",
    )
    .expect("harn.toml");
    let configured = run_check(cache.path(), root, &["main.harn"], &[]);
    let configured_stderr = String::from_utf8_lossy(&configured.stderr).into_owned();
    assert_ne!(
        default_stderr, configured_stderr,
        "config change must not replay the old diagnostics"
    );
}
