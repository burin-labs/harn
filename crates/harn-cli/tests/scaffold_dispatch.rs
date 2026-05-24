#![recursion_limit = "256"]

//! Parity tests for the scaffolding cluster (harn#2308 / W8).
//!
//! Each test runs the same `harn` subcommand twice — once with the
//! default `.harn` dispatch impl, and once with `HARN_CLI_IMPL=rust`
//! forcing the legacy handler — and asserts that the generated file
//! tree is byte-identical between the two. Subprocesses are used
//! because the dispatched `.harn` script spawns a full VM and would
//! overflow the default `#[tokio::test]` thread stack if run in-process.
//!
//! The C1 ratchet (harn#2314) removes the legacy Rust paths once these
//! parity tests have ridden in production for a release.

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

fn run_scaffold(args: &[&str], cwd: &Path, rust_impl: bool) -> Output {
    let mut cmd = Command::new(harn_bin());
    cmd.args(args).current_dir(cwd);
    if rust_impl {
        cmd.env("HARN_CLI_IMPL", "rust");
    } else {
        // Inherit env, but make sure no stale parity selector from the
        // user's shell leaks in and pins the run to the legacy impl.
        cmd.env_remove("HARN_CLI_IMPL");
    }
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

fn assert_snapshots_match(label: &str, harn_snapshot: &Snapshot, rust_snapshot: &Snapshot) {
    let harn_keys: Vec<&String> = harn_snapshot.keys().collect();
    let rust_keys: Vec<&String> = rust_snapshot.keys().collect();
    assert_eq!(
        harn_keys, rust_keys,
        "{label}: file list diverges between .harn and rust impls"
    );
    for (path, harn_bytes) in harn_snapshot {
        let rust_bytes = rust_snapshot
            .get(path)
            .unwrap_or_else(|| panic!("{label}: missing {path} in rust snapshot"));
        if harn_bytes != rust_bytes {
            let harn_text = String::from_utf8_lossy(harn_bytes);
            let rust_text = String::from_utf8_lossy(rust_bytes);
            panic!(
                "{label}: byte mismatch in {path}\n--- harn impl ---\n{harn_text}\n--- rust impl ---\n{rust_text}\n",
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
    let rust_tmp = tempfile::tempdir().expect("tempdir rust");

    let harn_args = args_with(harn_tmp.path());
    let rust_args = args_with(rust_tmp.path());
    let harn_argv: Vec<&str> = harn_args.iter().map(String::as_str).collect();
    let rust_argv: Vec<&str> = rust_args.iter().map(String::as_str).collect();

    let harn_out = run_scaffold(&harn_argv, harn_tmp.path(), false);
    assert!(
        harn_out.status.success(),
        "{label}: .harn dispatch failed: stdout={} stderr={}",
        String::from_utf8_lossy(&harn_out.stdout),
        String::from_utf8_lossy(&harn_out.stderr),
    );
    let rust_out = run_scaffold(&rust_argv, rust_tmp.path(), true);
    assert!(
        rust_out.status.success(),
        "{label}: rust impl failed: stdout={} stderr={}",
        String::from_utf8_lossy(&rust_out.stdout),
        String::from_utf8_lossy(&rust_out.stderr),
    );

    let harn_root: PathBuf = project_subdir
        .map(|s| harn_tmp.path().join(s))
        .unwrap_or_else(|| harn_tmp.path().to_path_buf());
    let rust_root: PathBuf = project_subdir
        .map(|s| rust_tmp.path().join(s))
        .unwrap_or_else(|| rust_tmp.path().to_path_buf());
    let harn_snap = snapshot_tree(&harn_root);
    let rust_snap = snapshot_tree(&rust_root);
    assert!(
        !harn_snap.is_empty(),
        "{label}: .harn impl produced no files under {}",
        harn_root.display()
    );
    (harn_snap, rust_snap)
}

#[test]
fn tool_new_dispatch_matches_rust_impl() {
    let (harn_snap, rust_snap) = pair_run(
        "tool new",
        |tmp| {
            vec![
                "tool".into(),
                "new".into(),
                "acme-tool".into(),
                "--dir".into(),
                tmp.join("acme-tool").display().to_string(),
                "--description".into(),
                "Acme tool for testing parity.".into(),
            ]
        },
        Some("acme-tool"),
    );
    assert_snapshots_match("tool new", &harn_snap, &rust_snap);
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
        false,
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
fn init_basic_dispatch_matches_rust_impl() {
    // `harn init` writes into the current working directory, so each
    // impl runs inside its own tempdir.
    let (harn_snap, rust_snap) = pair_run("init basic", |_tmp| vec!["init".into()], None);
    assert_snapshots_match("init basic", &harn_snap, &rust_snap);
    assert!(harn_snap.contains_key("harn.toml"));
    assert!(harn_snap.contains_key("main.harn"));
    assert!(harn_snap.contains_key("lib/helpers.harn"));
    assert!(harn_snap.contains_key("tests/test_main.harn"));
}

#[test]
fn new_package_dispatch_matches_rust_impl() {
    let (harn_snap, rust_snap) = pair_run(
        "new package",
        |_tmp| vec!["new".into(), "package".into(), "sample-pkg".into()],
        Some("sample-pkg"),
    );
    assert_snapshots_match("new package", &harn_snap, &rust_snap);
    assert!(harn_snap.contains_key("harn.toml"));
    assert!(harn_snap.contains_key("lib/main.harn"));
    assert!(harn_snap.contains_key("docs/api.md"));
}

#[test]
fn new_connector_dispatch_matches_rust_impl() {
    let (harn_snap, rust_snap) = pair_run(
        "new connector",
        |_tmp| vec!["new".into(), "connector".into(), "sample-conn".into()],
        Some("sample-conn"),
    );
    assert_snapshots_match("new connector", &harn_snap, &rust_snap);
    assert!(harn_snap.contains_key("connectors/echo.harn"));
}

#[test]
fn init_agent_dispatch_matches_rust_impl() {
    let (harn_snap, rust_snap) = pair_run(
        "init agent",
        |_tmp| vec!["init".into(), "--template".into(), "agent".into()],
        None,
    );
    assert_snapshots_match("init agent", &harn_snap, &rust_snap);
    assert!(harn_snap.contains_key("tests/test_agent.harn"));
}

#[test]
fn init_chat_dispatch_matches_rust_impl() {
    let (harn_snap, rust_snap) = pair_run(
        "init chat",
        |_tmp| vec!["init".into(), "--template".into(), "chat".into()],
        None,
    );
    assert_snapshots_match("init chat", &harn_snap, &rust_snap);
}

#[test]
fn init_mcp_server_dispatch_matches_rust_impl() {
    let (harn_snap, rust_snap) = pair_run(
        "init mcp-server",
        |_tmp| vec!["init".into(), "--template".into(), "mcp-server".into()],
        None,
    );
    assert_snapshots_match("init mcp-server", &harn_snap, &rust_snap);
}

#[test]
fn init_eval_dispatch_matches_rust_impl() {
    let (harn_snap, rust_snap) = pair_run(
        "init eval",
        |_tmp| vec!["init".into(), "--template".into(), "eval".into()],
        None,
    );
    assert_snapshots_match("init eval", &harn_snap, &rust_snap);
}

#[test]
fn init_pipeline_lab_dispatch_matches_rust_impl() {
    let (harn_snap, rust_snap) = pair_run(
        "init pipeline-lab",
        |_tmp| vec!["init".into(), "--template".into(), "pipeline-lab".into()],
        None,
    );
    assert_snapshots_match("init pipeline-lab", &harn_snap, &rust_snap);
}
