//! Integration coverage for `harn pack` (#1781).
//!
//! Exercises the exit criteria spelled out on the issue:
//!  - producing a valid `.harnpack` archive from a single entrypoint,
//!  - bit-for-bit determinism across repeated invocations,
//!  - the `--json` `JsonEnvelope` shape and schema version, and
//!  - the `--upgrade` path for older bundle schemas.

use std::fs;
use std::path::{Path, PathBuf};

use harn_cli::cli::PackArgs;
use harn_cli::commands::pack;
use harn_cli::tests::common::cwd_lock;
use harn_vm::orchestration::{load_workflow_bundle, read_harnpack, WORKFLOW_BUNDLE_SCHEMA_VERSION};
use tempfile::TempDir;
use tokio::runtime::Builder;

fn run_pack(args: &PackArgs) {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio current-thread runtime")
        .block_on(async {
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            pack::build(args).expect("pack succeeds");
        });
}

fn pack_args(entrypoint: PathBuf, out: PathBuf) -> PackArgs {
    PackArgs {
        entrypoint,
        out: Some(out),
        upgrade: None,
        unsigned: true,
        json: false,
    }
}

#[test]
fn pack_writes_valid_harnpack_archive() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "println(\"hi\")\n").unwrap();
    let out = workdir.path().join("hello.harnpack");

    run_pack(&pack_args(entry.clone(), out.clone()));

    assert!(out.exists(), "expected pack to write {}", out.display());
    let bytes = fs::read(&out).unwrap();
    let archive = read_harnpack(&bytes).expect("archive parses");
    assert_eq!(
        archive.manifest.schema_version,
        WORKFLOW_BUNDLE_SCHEMA_VERSION
    );
    assert_eq!(archive.manifest.entrypoint, PathBuf::from("hello.harn"));
    assert!(
        !archive.manifest.transitive_modules.is_empty(),
        "transitive_modules must include at least the entrypoint"
    );
    assert!(archive
        .contents
        .iter()
        .any(|entry| entry.path == Path::new("sources/hello.harn")));
    assert!(archive
        .contents
        .iter()
        .any(|entry| entry.path == Path::new("bytecode/hello.harnbc")));
    let report = harn_vm::orchestration::validate_workflow_bundle(&archive.manifest);
    assert!(report.valid, "{report:#?}");
}

#[test]
fn pack_is_deterministic_across_runs() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "println(\"hi\")\n").unwrap();
    let out_a = workdir.path().join("a.harnpack");
    let out_b = workdir.path().join("b.harnpack");

    run_pack(&pack_args(entry.clone(), out_a.clone()));
    run_pack(&pack_args(entry.clone(), out_b.clone()));

    let bytes_a = fs::read(&out_a).unwrap();
    let bytes_b = fs::read(&out_b).unwrap();
    assert_eq!(
        bytes_a, bytes_b,
        "harn pack must produce byte-identical archives for the same source"
    );
}

#[test]
fn pack_resolves_transitive_imports() {
    let workdir = TempDir::new().unwrap();
    let lib = workdir.path().join("lib.harn");
    fs::write(&lib, "pub fn greet() -> string { return \"howdy\" }\n").unwrap();
    let entry = workdir.path().join("entry.harn");
    fs::write(
        &entry,
        "import { greet } from \"./lib\"\nprintln(greet())\n",
    )
    .unwrap();

    let out = workdir.path().join("entry.harnpack");
    run_pack(&pack_args(entry.clone(), out.clone()));

    let archive = read_harnpack(&fs::read(&out).unwrap()).unwrap();
    let paths: Vec<String> = archive
        .manifest
        .transitive_modules
        .iter()
        .map(|module| module.path.display().to_string())
        .collect();
    assert!(paths.iter().any(|p| p == "entry.harn"), "{paths:?}");
    assert!(paths.iter().any(|p| p == "lib.harn"), "{paths:?}");
    let archive_paths: Vec<String> = archive
        .contents
        .iter()
        .map(|entry| entry.path.display().to_string())
        .collect();
    assert!(archive_paths.iter().any(|p| p == "sources/lib.harn"));
    assert!(archive_paths.iter().any(|p| p == "bytecode/lib.harnbc"));
}

#[test]
fn pack_json_envelope_carries_pack_schema() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "println(\"hi\")\n").unwrap();
    let out = workdir.path().join("hello.harnpack");

    let envelope = Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            pack::run_to_envelope(&PackArgs {
                entrypoint: entry.clone(),
                out: Some(out.clone()),
                upgrade: None,
                unsigned: true,
                json: true,
            })
        });

    let value = serde_json::to_value(&envelope).unwrap();
    assert_eq!(value["schemaVersion"], pack::PACK_SCHEMA_VERSION);
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["output_path"], out.display().to_string());
    assert!(value["data"]["bundle_hash"]
        .as_str()
        .is_some_and(|s| s.starts_with("blake3:")));
    assert!(value["data"]["size_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        value["data"]["manifest"]["schema_version"],
        WORKFLOW_BUNDLE_SCHEMA_VERSION
    );
}

#[test]
fn pack_upgrade_replaces_schema_version_keeping_workflow() {
    let workdir = TempDir::new().unwrap();
    let v1 = workdir.path().join("legacy.json");
    fs::write(
        &v1,
        r#"{
          "schema_version": 1,
          "id": "legacy-monitor",
          "name": "Legacy monitor",
          "version": "1.0.0",
          "workflow": {
            "_type": "workflow_graph",
            "id": "legacy_workflow",
            "version": 1,
            "entry": "step",
            "nodes": { "step": { "id": "step", "kind": "action" } },
            "edges": []
          },
          "triggers": [{ "id": "manual", "kind": "manual", "node_id": "step" }]
        }"#,
    )
    .unwrap();

    let entry = workdir.path().join("entry.harn");
    fs::write(&entry, "println(\"upgrade\")\n").unwrap();
    let out = workdir.path().join("legacy.harnpack");

    run_pack(&PackArgs {
        entrypoint: entry.clone(),
        out: Some(out.clone()),
        upgrade: Some(v1.clone()),
        unsigned: true,
        json: false,
    });

    let bundle = load_workflow_bundle(&out).unwrap();
    assert_eq!(bundle.schema_version, WORKFLOW_BUNDLE_SCHEMA_VERSION);
    assert_eq!(bundle.id, "legacy-monitor");
    assert_eq!(bundle.workflow.id, "legacy_workflow");
    assert_eq!(bundle.workflow.entry, "step");
    assert_eq!(bundle.triggers.len(), 1);
    assert_eq!(bundle.triggers[0].id, "manual");
    assert!(!bundle.transitive_modules.is_empty(), "v2 fields populated");
    assert!(bundle.provider_catalog_hash.starts_with("blake3:"));
}
