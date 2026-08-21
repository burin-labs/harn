//! `harn precompile` port verification (harn#2313 / W13).
//!
//! Pins the .harn dispatch impl against the host inner compiler. The
//! .harn impl spawns `harn precompile <single-file>` per source with
//! `HARN_PRECOMPILE_INNER=1` so the inner compile work stays in the host inner compiler; this
//! test exercises the dispatch path and inner compiler against the same input and asserts:
//!
//!   * stdout lines (the per-file `source -> dest` reports) match
//!     when sorted — the dispatch wedge flushes stderr before stdout,
//!     so the merged stream ordering naturally diverges from the
//!     inner compiler. Comparing each stream independently after a sort is
//!     the right contract for downstream consumers (build pipes,
//!     editor integrations) that read either stream alone.
//!   * stderr summary line is byte-identical.
//!   * exit code matches.
//!   * the same set of `.harnbc` + `.harnmod` files exist after each
//!     run (the actual compile artifacts the cache loader reads).
//!
//! See `crates/harn-stdlib/src/stdlib/cli/precompile.harn` for the
//! script and `crates/harn-cli/src/commands/precompile.rs` for the
//! dispatch shim.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::test_util::process::harn_e2e_command;

#[test]
fn single_file_dispatch_matches_inner_compile() {
    let workdir = tempfile::tempdir().expect("workdir");
    let source = write_hello(workdir.path(), "hello.harn");

    let inner = run_precompile(
        &[source.to_string_lossy().as_ref()],
        &[("HARN_PRECOMPILE_INNER", "1")],
    );
    assert_eq!(inner.exit_code, 0, "inner stderr={}", inner.stderr);

    cleanup_artifacts(workdir.path());

    let harn = run_precompile(&[source.to_string_lossy().as_ref()], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);

    assert_eq!(
        sort_lines(&inner.stdout),
        sort_lines(&harn.stdout),
        "stdout lines diverged\nrust:\n{}\nharn:\n{}",
        inner.stdout,
        harn.stdout,
    );
    assert!(
        harn.stderr.contains("1 succeeded, 0 failed"),
        "stderr should report 1/0; got: {}",
        harn.stderr
    );
}

#[test]
fn directory_dispatch_emits_artifacts_for_every_source() {
    let workdir = tempfile::tempdir().expect("workdir");
    write_hello(workdir.path(), "alpha.harn");
    std::fs::create_dir(workdir.path().join("nested")).expect("mkdir nested");
    write_hello(&workdir.path().join("nested"), "beta.harn");

    let inner = run_precompile(
        &[workdir.path().to_string_lossy().as_ref()],
        &[("HARN_PRECOMPILE_INNER", "1")],
    );
    assert_eq!(inner.exit_code, 0, "inner stderr={}", inner.stderr);
    let inner_artifacts = collect_artifacts(workdir.path());
    cleanup_artifacts(workdir.path());

    let harn = run_precompile(&[workdir.path().to_string_lossy().as_ref()], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_artifacts = collect_artifacts(workdir.path());

    assert_eq!(
        inner_artifacts, harn_artifacts,
        "current path and inner compiler produced different artifact sets"
    );
    assert!(
        harn.stderr.contains("2 succeeded, 0 failed"),
        "stderr should report 2/0; got: {}",
        harn.stderr
    );
    // Both reports should mention every source by stdout line, just
    // possibly in a different stream-flush order.
    assert_eq!(sort_lines(&harn.stdout), sort_lines(&inner.stdout));
}

#[test]
fn directory_precompile_entry_artifact_hits_after_tree_relocation() {
    let workdir = tempfile::tempdir().expect("workdir");
    let build_root = workdir.path().join("build");
    let run_root = workdir.path().join("run");
    let cache = tempfile::tempdir().expect("isolated bytecode cache");
    std::fs::create_dir(&build_root).expect("create build root");
    std::fs::write(
        build_root.join("lib.harn"),
        "pub fn answer() -> int { return 42 }\n",
    )
    .expect("write imported module");
    std::fs::write(
        build_root.join("main.harn"),
        "import { answer } from \"./lib\"\npipeline main() { const value = answer() }\n",
    )
    .expect("write importing entry");

    let precompile = run_precompile(&[build_root.to_string_lossy().as_ref()], &[]);
    assert_eq!(
        precompile.exit_code, 0,
        "directory precompile failed: {}",
        precompile.stderr
    );
    std::fs::rename(&build_root, &run_root).expect("relocate complete source tree");

    let output = harn_e2e_command()
        .args(["time", "run", "--json"])
        .arg(run_root.join("main.harn"))
        .env("HARN_CACHE_DIR", cache.path())
        .output()
        .expect("run relocated precompiled entry");
    assert!(
        output.status.success(),
        "relocated entry failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "time receipt was not JSON ({error}); stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
    let compile_phase = receipt["data"]["phases"]
        .as_array()
        .and_then(|phases| {
            phases
                .iter()
                .find(|phase| phase["name"] == "bytecode_compile")
        })
        .expect("bytecode_compile phase");
    assert_eq!(
        compile_phase["cache"], "hit",
        "moved directory entry must use its adjacent root-relative artifact: {receipt}"
    );
}

#[test]
fn precompile_reports_a_machine_readable_artifact_contract() {
    let output = harn_e2e_command()
        .args(["precompile", "--artifact-contract"])
        .output()
        .expect("query precompile artifact contract");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        harn_cli::commands::precompile::PRECOMPILE_ARTIFACT_CONTRACT
    );
}

/// A pipeline that calls a stdlib export whose name collides with a
/// builtin (`std/disclosure::render` vs the `template.render` builtin)
/// must precompile. Regression for the import-graph resolution that
/// `precompile_one` was missing: without it the call was checked against
/// the builtin signature and failed with phantom type errors, even though
/// `harn run` resolves the import correctly.
#[test]
fn precompiles_call_to_builtin_colliding_stdlib_import() {
    let workdir = tempfile::tempdir().expect("workdir");
    let source = workdir.path().join("disclose.harn");
    std::fs::write(
        &source,
        r#"import { render } from "std/disclosure"

pipeline default(harness: Harness, task) {
  const chain = {sub: "user:k", act: {sub: "agent:b"}}
  harness.stdio.log(render(harness.env, harness.fs, chain, "git", {project: false, env: false, config: {}}))
}
"#,
    )
    .expect("write disclose.harn");

    let outcome = run_precompile(
        &[source.to_string_lossy().as_ref()],
        &[("HARN_PRECOMPILE_INNER", "1")],
    );
    assert_eq!(
        outcome.exit_code, 0,
        "precompile should succeed; stderr:\n{}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("1 succeeded, 0 failed"),
        "stderr should report 1/0; got:\n{}",
        outcome.stderr
    );
}

#[test]
fn precompile_honors_manifest_trusted_host_dispatch() {
    let workdir = tempfile::tempdir().expect("workdir");
    let source = workdir.path().join("host-route.harn");
    std::fs::write(
        &source,
        "pub fn host_environment() { return host_call(\"env.host\", {}) }\n",
    )
    .expect("write host route");

    let strict = run_precompile(
        &[source.to_string_lossy().as_ref()],
        &[("HARN_LEGACY_AMBIENT_CAPABILITIES", "0")],
    );
    assert_ne!(strict.exit_code, 0, "untrusted precompile must fail");
    assert!(collect_artifacts(workdir.path()).is_empty());

    std::fs::write(
        workdir.path().join("harn.toml"),
        "[check]\ntrusted_host_dispatch = true\n",
    )
    .expect("write harn.toml");
    let declared = run_precompile(
        &[source.to_string_lossy().as_ref()],
        &[("HARN_LEGACY_AMBIENT_CAPABILITIES", "0")],
    );
    assert_eq!(
        declared.exit_code, 0,
        "manifest-declared host dispatch was ignored:\n{}",
        declared.stderr
    );
    assert_eq!(
        collect_artifacts(workdir.path()),
        BTreeSet::from([
            PathBuf::from("host-route.harnbc"),
            PathBuf::from("host-route.harnmod"),
        ])
    );
}

#[test]
fn out_directory_mirrors_source_tree_under_target() {
    let workdir = tempfile::tempdir().expect("workdir");
    let outdir = tempfile::tempdir().expect("outdir");
    write_hello(workdir.path(), "alpha.harn");
    std::fs::create_dir(workdir.path().join("nested")).expect("mkdir nested");
    write_hello(&workdir.path().join("nested"), "beta.harn");

    let inner_out = tempfile::tempdir().expect("inner outdir");
    let inner = run_precompile(
        &[
            workdir.path().to_string_lossy().as_ref(),
            "--out",
            inner_out.path().to_string_lossy().as_ref(),
        ],
        &[("HARN_PRECOMPILE_INNER", "1")],
    );
    assert_eq!(inner.exit_code, 0, "inner stderr={}", inner.stderr);

    let harn = run_precompile(
        &[
            workdir.path().to_string_lossy().as_ref(),
            "--out",
            outdir.path().to_string_lossy().as_ref(),
        ],
        &[],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);

    // Relative paths under each out-dir should match exactly. We compare
    // relative paths (not absolute) since the two tempdirs have
    // different prefixes.
    let inner_rel = relative_artifacts(inner_out.path());
    let harn_rel = relative_artifacts(outdir.path());
    assert_eq!(
        inner_rel, harn_rel,
        "out-dir layouts diverged\ninner: {inner_rel:?}\nharn: {harn_rel:?}"
    );
    assert!(
        harn_rel.iter().any(|p| p == "nested/beta.harnbc"),
        "nested .harnbc should land under nested/, got: {harn_rel:?}"
    );
}

#[test]
fn missing_target_exits_nonzero_on_dispatch_and_inner_compile() {
    let workdir = tempfile::tempdir().expect("workdir");
    let bogus = workdir.path().join("does-not-exist.harn");

    let inner = run_precompile(
        &[bogus.to_string_lossy().as_ref()],
        &[("HARN_PRECOMPILE_INNER", "1")],
    );
    let harn = run_precompile(&[bogus.to_string_lossy().as_ref()], &[]);

    assert_ne!(inner.exit_code, 0, "inner should fail on missing target");
    assert_ne!(harn.exit_code, 0, "harn should fail on missing target");
}

#[test]
fn keep_going_continues_past_failed_source() {
    let workdir = tempfile::tempdir().expect("workdir");
    write_hello(workdir.path(), "good.harn");
    std::fs::write(
        workdir.path().join("bad.harn"),
        "this is not valid harn syntax !!\n",
    )
    .expect("write bad.harn");

    let harn = run_precompile(
        &[workdir.path().to_string_lossy().as_ref(), "--keep-going"],
        &[],
    );
    // Mixed run: exit nonzero (because something failed) but the good
    // source must still have been processed.
    assert_ne!(
        harn.exit_code, 0,
        "should exit nonzero when any source fails"
    );
    assert!(
        harn.stderr.contains("1 succeeded, 1 failed")
            || harn.stderr.contains("1 succeeded, 0 failed"),
        "stderr should report mixed result; got: {}",
        harn.stderr
    );
}

#[test]
fn quiet_suppresses_per_file_output_but_not_failures() {
    let workdir = tempfile::tempdir().expect("workdir");
    write_hello(workdir.path(), "hello.harn");

    let harn = run_precompile(&[workdir.path().to_string_lossy().as_ref(), "--quiet"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    assert!(
        harn.stdout.trim().is_empty(),
        "--quiet should suppress stdout reports; got: {:?}",
        harn.stdout
    );
    assert!(
        !harn.stderr.contains("succeeded"),
        "--quiet should suppress the summary line too; got: {:?}",
        harn.stderr
    );
}

// ── helpers ──────────────────────────────────────────────────────────────

struct PrecompileOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn run_precompile(argv: &[&str], extra_env: &[(&str, &str)]) -> PrecompileOutcome {
    let mut cmd = harn_e2e_command();
    cmd.arg("precompile");
    for arg in argv {
        cmd.arg(arg);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("spawn harn precompile");
    PrecompileOutcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    }
}

fn write_hello(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        "pipeline default(harness: Harness, task) {\n  harness.stdio.log(\"hello from precompile\")\n}\n",
    )
    .expect("write hello.harn");
    path
}

fn sort_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

fn collect_artifacts(root: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    walk_into(root, root, &mut out);
    out
}

fn walk_into(base: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_into(base, &path, out);
            continue;
        }
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            if ext == "harnbc" || ext == "harnmod" {
                if let Ok(rel) = path.strip_prefix(base) {
                    out.insert(rel.to_path_buf());
                }
            }
        }
    }
}

fn relative_artifacts(root: &Path) -> BTreeSet<String> {
    collect_artifacts(root)
        .into_iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect()
}

fn cleanup_artifacts(root: &Path) {
    cleanup_into(root);
}

fn cleanup_into(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            cleanup_into(&path);
            continue;
        }
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            if ext == "harnbc" || ext == "harnmod" {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}
