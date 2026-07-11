#![recursion_limit = "256"]

//! Contract tests for the scaffolding cluster.
//!
//! Each test runs the same `harn` subcommand in isolated tempdirs and
//! asserts that the generated file tree is deterministic. Subprocesses
//! are used because the dispatched `.harn` script spawns a full VM and
//! would overflow the default `#[tokio::test]` thread stack if run
//! in-process.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Snapshot of every file in a directory tree, keyed by path relative
/// to the snapshot root. Used to compare two scaffolded layouts.
type Snapshot = BTreeMap<String, Vec<u8>>;

fn harn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_harn")
}

fn run_scaffold(args: &[&str], cwd: &Path) -> Output {
    let mut cmd = Command::new(harn_bin());
    cmd.args(args).current_dir(cwd);
    cmd.output().expect("spawn harn subcommand")
}

fn snapshot_tree(root: &Path) -> Snapshot {
    let mut out = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, out: &mut Snapshot) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                walk(root, &path, out);
            } else if file_type.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .expect("file under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = fs::read(&path).expect("read file");
                out.insert(rel, bytes);
            }
        }
    }
    walk(root, root, &mut out);
    out
}

fn assert_snapshots_match(label: &str, harn_snapshot: &Snapshot, repeat_snapshot: &Snapshot) {
    let harn_keys: Vec<&String> = harn_snapshot.keys().collect();
    let repeat_keys: Vec<&String> = repeat_snapshot.keys().collect();
    assert_eq!(
        harn_keys, repeat_keys,
        "{label}: file list diverges between repeated scaffold runs"
    );
    for (path, harn_bytes) in harn_snapshot {
        let repeat_bytes = repeat_snapshot
            .get(path)
            .unwrap_or_else(|| panic!("{label}: missing {path} in repeat snapshot"));
        if harn_bytes != repeat_bytes {
            let harn_text = String::from_utf8_lossy(harn_bytes);
            let repeat_text = String::from_utf8_lossy(repeat_bytes);
            panic!(
                "{label}: byte mismatch in {path}\n--- first run ---\n{harn_text}\n--- repeat run ---\n{repeat_text}\n",
            );
        }
    }
}

fn pair_run(
    label: &str,
    args_with: impl Fn(&Path) -> Vec<String>,
    project_subdir: Option<&str>,
) -> (Snapshot, Snapshot) {
    let harn_tmp = tempfile::tempdir().expect("tempdir harn");
    let repeat_tmp = tempfile::tempdir().expect("tempdir repeat");

    let harn_args = args_with(harn_tmp.path());
    let repeat_args = args_with(repeat_tmp.path());
    let harn_argv: Vec<&str> = harn_args.iter().map(String::as_str).collect();
    let repeat_argv: Vec<&str> = repeat_args.iter().map(String::as_str).collect();

    let harn_out = run_scaffold(&harn_argv, harn_tmp.path());
    assert!(
        harn_out.status.success(),
        "{label}: .harn dispatch failed: stdout={} stderr={}",
        String::from_utf8_lossy(&harn_out.stdout),
        String::from_utf8_lossy(&harn_out.stderr),
    );
    let repeat_out = run_scaffold(&repeat_argv, repeat_tmp.path());
    assert!(
        repeat_out.status.success(),
        "{label}: repeat run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&repeat_out.stdout),
        String::from_utf8_lossy(&repeat_out.stderr),
    );

    let harn_root: PathBuf = project_subdir
        .map(|s| harn_tmp.path().join(s))
        .unwrap_or_else(|| harn_tmp.path().to_path_buf());
    let repeat_root: PathBuf = project_subdir
        .map(|s| repeat_tmp.path().join(s))
        .unwrap_or_else(|| repeat_tmp.path().to_path_buf());
    let harn_snap = snapshot_tree(&harn_root);
    let repeat_snap = snapshot_tree(&repeat_root);
    assert!(
        !harn_snap.is_empty(),
        "{label}: .harn impl produced no files under {}",
        harn_root.display()
    );
    (harn_snap, repeat_snap)
}

#[test]
fn tool_new_dispatch_is_deterministic() {
    let (harn_snap, repeat_snap) = pair_run(
        "tool new",
        |tmp| {
            vec![
                "tool".into(),
                "new".into(),
                "acme-tool".into(),
                "--dir".into(),
                tmp.join("acme-tool").display().to_string(),
                "--description".into(),
                "Acme tool for testing determinism.".into(),
            ]
        },
        Some("acme-tool"),
    );
    assert_snapshots_match("tool new", &harn_snap, &repeat_snap);
    // Spot-check key files so a structural drift fails loudly even if
    // the byte comparison happens to coincide.
    assert!(harn_snap.contains_key("harn.toml"), "harn.toml present");
    assert!(
        harn_snap.contains_key("lib/tools.harn"),
        "lib/tools.harn present"
    );
    assert!(
        harn_snap.contains_key(".github/workflows/harn-package.yml"),
        "workflow yaml present"
    );
}

#[test]
fn tool_new_force_preserves_pre_existing_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("acme-tool");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("keep.txt"), "keep").unwrap();
    let out = run_scaffold(
        &[
            "tool",
            "new",
            "acme-tool",
            "--dir",
            &dest.display().to_string(),
            "--force",
        ],
        tmp.path(),
    );
    assert!(
        out.status.success(),
        "tool new --force failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(dest.join("keep.txt")).unwrap(), "keep");
    assert!(dest.join("harn.toml").exists());
}

#[test]
fn init_basic_dispatch_is_deterministic() {
    // `harn init` writes into the current working directory, so each
    // impl runs inside its own tempdir.
    let (harn_snap, repeat_snap) = pair_run("init basic", |_tmp| vec!["init".into()], None);
    assert_snapshots_match("init basic", &harn_snap, &repeat_snap);
    assert!(harn_snap.contains_key("harn.toml"));
    assert!(harn_snap.contains_key("main.harn"));
    assert!(harn_snap.contains_key("lib/helpers.harn"));
    assert!(harn_snap.contains_key("tests/test_main.harn"));
}

#[test]
fn init_basic_project_runs_and_tests_green() {
    // A fresh `harn init` project must run its own pipeline and pass its
    // bundled tests. Guards against the module-visibility regression where a
    // whole-module `import "lib/helpers"` no longer binds non-`pub` names.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let init = run_scaffold(&["init"], root);
    assert!(
        init.status.success(),
        "harn init failed: stderr={}",
        String::from_utf8_lossy(&init.stderr)
    );

    let run = run_scaffold(&["run", "main.harn"], root);
    assert!(
        run.status.success(),
        "harn run main.harn failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("Hello"),
        "scaffold run should print the greeting: stdout={}",
        String::from_utf8_lossy(&run.stdout)
    );

    let test = run_scaffold(&["test", "tests/"], root);
    assert!(
        test.status.success(),
        "harn test tests/ failed: stdout={} stderr={}",
        String::from_utf8_lossy(&test.stdout),
        String::from_utf8_lossy(&test.stderr),
    );
}

#[test]
fn new_package_dispatch_is_deterministic() {
    let (harn_snap, repeat_snap) = pair_run(
        "new package",
        |_tmp| vec!["new".into(), "package".into(), "sample-pkg".into()],
        Some("sample-pkg"),
    );
    assert_snapshots_match("new package", &harn_snap, &repeat_snap);
    assert!(harn_snap.contains_key("harn.toml"));
    assert!(harn_snap.contains_key("lib/main.harn"));
    assert!(harn_snap.contains_key("docs/api.md"));
}

#[test]
fn new_connector_dispatch_is_deterministic() {
    let (harn_snap, repeat_snap) = pair_run(
        "new connector",
        |_tmp| vec!["new".into(), "connector".into(), "sample-conn".into()],
        Some("sample-conn"),
    );
    assert_snapshots_match("new connector", &harn_snap, &repeat_snap);
    assert!(harn_snap.contains_key("connectors/echo.harn"));
}

#[test]
fn init_agent_dispatch_is_deterministic() {
    let (harn_snap, repeat_snap) = pair_run(
        "init agent",
        |_tmp| vec!["init".into(), "--template".into(), "agent".into()],
        None,
    );
    assert_snapshots_match("init agent", &harn_snap, &repeat_snap);
    assert!(harn_snap.contains_key("tests/test_agent.harn"));
}

#[test]
fn init_chat_dispatch_is_deterministic() {
    let (harn_snap, repeat_snap) = pair_run(
        "init chat",
        |_tmp| vec!["init".into(), "--template".into(), "chat".into()],
        None,
    );
    assert_snapshots_match("init chat", &harn_snap, &repeat_snap);
    let main_harn = String::from_utf8(
        harn_snap
            .get("main.harn")
            .expect("chat scaffold writes main.harn")
            .clone(),
    )
    .expect("main.harn is utf-8");
    assert!(
        main_harn.contains("import { read_line } from \"std/io\""),
        "chat scaffold should use std/io.read_line structured result"
    );
    assert!(
        main_harn.contains("const raw = read_line()"),
        "chat scaffold should call std/io.read_line"
    );
    assert!(
        !main_harn.contains("harness.stdio.read_line()"),
        "chat scaffold must not mix legacy harness read_line with structured ok/value handling"
    );
}

#[test]
fn init_mcp_server_dispatch_is_deterministic() {
    let (harn_snap, repeat_snap) = pair_run(
        "init mcp-server",
        |_tmp| vec!["init".into(), "--template".into(), "mcp-server".into()],
        None,
    );
    assert_snapshots_match("init mcp-server", &harn_snap, &repeat_snap);
}

#[test]
fn init_eval_dispatch_is_deterministic() {
    let (harn_snap, repeat_snap) = pair_run(
        "init eval",
        |_tmp| vec!["init".into(), "--template".into(), "eval".into()],
        None,
    );
    assert_snapshots_match("init eval", &harn_snap, &repeat_snap);
}

#[test]
fn init_pipeline_lab_dispatch_is_deterministic() {
    let (harn_snap, repeat_snap) = pair_run(
        "init pipeline-lab",
        |_tmp| vec!["init".into(), "--template".into(), "pipeline-lab".into()],
        None,
    );
    assert_snapshots_match("init pipeline-lab", &harn_snap, &repeat_snap);
}
