//! Integration coverage for `harn pack` (#1781) and `harn pack verify` (#1779).
//!
//! Exercises the exit criteria spelled out on the epic:
//!  - producing a valid `.harnpack` archive from a single entrypoint,
//!  - bit-for-bit determinism across repeated invocations,
//!  - the `--json` `JsonEnvelope` shape and schema version,
//!  - the `--upgrade` path for older bundle schemas,
//!  - `harn pack verify` happy/sad paths (signed, unsigned, tampered).

use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::pkcs8::{spki::der::pem::LineEnding, DecodePrivateKey, EncodePublicKey};
use ed25519_dalek::SigningKey;
use harn_cli::cli::{PackArgs, PackVerifyArgs};
use harn_cli::commands::pack;
use harn_cli::commands::pack::BuildArgs;
use harn_cli::tests::common::cwd_lock;
use harn_vm::orchestration::{
    load_workflow_bundle, read_harnpack, verify_workflow_bundle_signature, SBOMDoc,
    WORKFLOW_BUNDLE_SCHEMA_VERSION,
};
use tempfile::TempDir;
use tokio::runtime::Builder;

const TEST_ED25519_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIDmsNDO8iZqiMmA/b7I7lwGXNKe68o+gDno6R5riUcDC\n-----END PRIVATE KEY-----\n";

fn test_public_key_pem() -> String {
    let signing_key = SigningKey::from_pkcs8_pem(TEST_ED25519_PRIVATE_KEY).unwrap();
    signing_key
        .verifying_key()
        .to_public_key_pem(LineEnding::LF)
        .unwrap()
}

fn build_args_from_pack(args: &PackArgs) -> BuildArgs {
    BuildArgs {
        entrypoint: args
            .entrypoint
            .clone()
            .expect("PackArgs.entrypoint set in tests"),
        out: args.out.clone(),
        upgrade: args.upgrade.clone(),
        sign: args.sign,
        key: args.key.clone(),
        unsigned: args.unsigned,
        exclude_secrets: args.exclude_secrets,
        json: args.json,
    }
}

fn run_pack(args: &PackArgs) {
    let _ = build_pack(args);
}

fn build_pack(args: &PackArgs) -> pack::PackOutcome {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio current-thread runtime")
        .block_on(async {
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            pack::build(&build_args_from_pack(args)).expect("pack succeeds")
        })
}

fn pack_args(entrypoint: PathBuf, out: PathBuf) -> PackArgs {
    PackArgs {
        command: None,
        entrypoint: Some(entrypoint),
        out: Some(out),
        upgrade: None,
        sign: false,
        key: None,
        unsigned: true,
        exclude_secrets: false,
        include_secrets: false,
        json: false,
    }
}

#[test]
fn pack_writes_valid_harnpack_archive() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"hi\")\n").unwrap();
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
    let sbom_entry = archive
        .contents
        .iter()
        .find(|entry| entry.path == Path::new(pack::PACK_SBOM_ARCHIVE_PATH))
        .expect("harnpack contains archived SBOM document");
    let sbom_doc: SBOMDoc = serde_json::from_slice(&sbom_entry.bytes).unwrap();
    assert_eq!(sbom_doc.format, "spdx-lite");
    assert_eq!(sbom_doc.version, "2.3");
    let sbom_value: serde_json::Value = serde_json::from_slice(&sbom_entry.bytes).unwrap();
    assert_eq!(
        sbom_value,
        serde_json::to_value(&archive.manifest.sbom).unwrap()
    );
    assert!(
        archive
            .manifest
            .sbom
            .packages
            .iter()
            .any(|package| package.name == "harn-provider-catalog"),
        "SBOM must carry provider catalog component"
    );
    assert!(
        archive
            .manifest
            .sbom
            .packages
            .iter()
            .any(|package| package.name.starts_with("provider:")),
        "SBOM must enumerate provider catalog entries"
    );
    let report = harn_vm::orchestration::validate_workflow_bundle(&archive.manifest);
    assert!(report.valid, "{report:#?}");
}

#[test]
fn pack_is_deterministic_across_runs() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"hi\")\n").unwrap();
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
        "import { greet } from \"./lib\"\n__io_println(greet())\n",
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
fn pack_bundles_non_harn_assets_from_imports() {
    let workdir = TempDir::new().unwrap();
    let prompts = workdir.path().join("prompts");
    fs::create_dir_all(&prompts).unwrap();
    fs::write(prompts.join("template.txt"), "Hello, {{name}}\n").unwrap();
    fs::write(
        workdir.path().join("lib.harn"),
        "pub fn greet() -> string { return \"howdy\" }\n",
    )
    .unwrap();
    let entry = workdir.path().join("entry.harn");
    fs::write(
        &entry,
        "import { greet } from \"./lib\"\nimport \"./prompts/template.txt\"\n__io_println(greet())\n",
    )
    .unwrap();
    let out = workdir.path().join("entry.harnpack");

    run_pack(&pack_args(entry.clone(), out.clone()));

    let archive = read_harnpack(&fs::read(&out).unwrap()).unwrap();
    assert!(archive
        .contents
        .iter()
        .any(|entry| entry.path == Path::new("sources/prompts/template.txt")));
    assert!(archive.manifest.sbom.packages.iter().any(|package| {
        package.name == "asset:prompts/template.txt"
            && package
                .package_hash_blake3
                .as_deref()
                .is_some_and(|hash| hash.starts_with("blake3:"))
    }));
    assert!(archive
        .manifest
        .sbom
        .relationships
        .iter()
        .any(|relationship| {
            relationship.from == "entrypoint:entry.harn"
                && relationship.to == "asset:prompts/template.txt"
                && relationship.relationship_type == "depends_on"
        }));
    assert!(archive
        .manifest
        .transitive_modules
        .iter()
        .all(|module| module.path != Path::new("prompts/template.txt")));
}

#[test]
fn pack_json_envelope_carries_pack_schema() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"hi\")\n").unwrap();
    let out = workdir.path().join("hello.harnpack");

    let envelope = Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            pack::run_to_envelope(&PackArgs {
                command: None,
                entrypoint: Some(entry.clone()),
                out: Some(out.clone()),
                upgrade: None,
                sign: false,
                key: None,
                unsigned: true,
                exclude_secrets: false,
                include_secrets: false,
                json: true,
            })
        });

    let value = serde_json::to_value(&envelope).unwrap();
    jsonschema::draft202012::meta::validate(&pack::json_schema()).unwrap();
    let validator = jsonschema::draft202012::new(&pack::json_schema()).unwrap();
    validator.validate(&value).unwrap();
    assert_eq!(value["schemaVersion"], pack::PACK_SCHEMA_VERSION);
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["output_path"], out.display().to_string());
    assert!(value["data"]["bundle_hash"]
        .as_str()
        .is_some_and(|s| s.starts_with("blake3:")));
    assert!(value["data"]["size_bytes"].as_u64().unwrap() > 0);
    assert_eq!(value["data"]["signature"]["algorithm"], "ed25519");
    assert_eq!(value["data"]["signature"]["present"], false);
    assert!(
        value["data"]["sbom_summary"]["components"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(value["data"]["sbom_summary"]["providers"].as_u64().unwrap() > 0);
    assert_eq!(
        value["data"]["sbom_summary"]["components"]
            .as_u64()
            .unwrap(),
        value["data"]["manifest"]["sbom"]["packages"]
            .as_array()
            .unwrap()
            .len() as u64
    );
    assert_eq!(value["data"]["debug_symbol_metadata"]["harnbc_count"], 1);
    assert!(
        value["data"]["debug_symbol_metadata"]["total_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
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
    fs::write(&entry, "__io_println(\"upgrade\")\n").unwrap();
    let out = workdir.path().join("legacy.harnpack");

    run_pack(&PackArgs {
        command: None,
        entrypoint: Some(entry.clone()),
        out: Some(out.clone()),
        upgrade: Some(v1.clone()),
        sign: false,
        key: None,
        unsigned: true,
        exclude_secrets: false,
        include_secrets: false,
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

#[test]
fn pack_signs_manifest_and_emits_release_trust_record() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"signed\")\n").unwrap();
    let key = workdir.path().join("release-key.pem");
    fs::write(&key, TEST_ED25519_PRIVATE_KEY).unwrap();
    let out = workdir.path().join("hello.harnpack");

    let outcome = build_pack(&PackArgs {
        command: None,
        entrypoint: Some(entry.clone()),
        out: Some(out.clone()),
        upgrade: None,
        sign: true,
        key: Some(key),
        unsigned: false,
        exclude_secrets: false,
        include_secrets: false,
        json: false,
    });

    let archive = read_harnpack(&fs::read(&out).unwrap()).unwrap();
    let signature = archive
        .manifest
        .signature
        .as_ref()
        .expect("signed pack embeds signature");
    assert_eq!(signature.algorithm, "ed25519");
    assert_eq!(signature.manifest_hash_blake3, outcome.bundle_hash);
    assert!(signature.key_id.is_some());
    verify_workflow_bundle_signature(&archive.manifest, &archive.contents)
        .expect("signature verifies");

    let log = harn_vm::event_log::install_default_for_base_dir(workdir.path()).unwrap();
    assert!(
        futures::executor::block_on(harn_vm::verify_trust_chain(&log))
            .unwrap()
            .verified
    );
    let records = release_records(&log);
    let record = records.last().expect("release trust record emitted");
    assert_eq!(record.autonomy_tier, harn_vm::AutonomyTier::ActAuto);
    assert_eq!(record.metadata["bundle_hash"], outcome.bundle_hash);
    assert_eq!(
        record.metadata["harn_version"],
        archive.manifest.harn_version
    );
    assert_eq!(record.metadata["signed"], true);
    assert_eq!(record.metadata["action_kind"]["kind"], "release");
}

#[test]
fn pack_unsigned_emits_suggest_release_trust_record() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"unsigned\")\n").unwrap();
    let out = workdir.path().join("hello.harnpack");

    let outcome = build_pack(&pack_args(entry.clone(), out.clone()));

    let archive = read_harnpack(&fs::read(&out).unwrap()).unwrap();
    assert!(archive.manifest.signature.is_none());
    let log = harn_vm::event_log::install_default_for_base_dir(workdir.path()).unwrap();
    assert!(
        futures::executor::block_on(harn_vm::verify_trust_chain(&log))
            .unwrap()
            .verified
    );
    let records = release_records(&log);
    let record = records.last().expect("release trust record emitted");
    assert_eq!(record.autonomy_tier, harn_vm::AutonomyTier::Suggest);
    assert_eq!(record.metadata["bundle_hash"], outcome.bundle_hash);
    assert_eq!(record.metadata["signed"], false);
}

#[test]
fn workflow_bundle_signature_verifier_rejects_content_mismatch() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"signed\")\n").unwrap();
    let key = workdir.path().join("release-key.pem");
    fs::write(&key, TEST_ED25519_PRIVATE_KEY).unwrap();
    let out = workdir.path().join("hello.harnpack");

    run_pack(&PackArgs {
        command: None,
        entrypoint: Some(entry.clone()),
        out: Some(out.clone()),
        upgrade: None,
        sign: true,
        key: Some(key),
        unsigned: false,
        exclude_secrets: false,
        include_secrets: false,
        json: false,
    });

    let mut archive = read_harnpack(&fs::read(&out).unwrap()).unwrap();
    let source = archive
        .contents
        .iter_mut()
        .find(|entry| entry.path == Path::new("sources/hello.harn"))
        .expect("source entry present");
    source.bytes = b"__io_println(\"tampered\")\n".to_vec();
    let error = verify_workflow_bundle_signature(&archive.manifest, &archive.contents)
        .expect_err("tampered content must fail verification");
    assert!(error.message.contains("signature hash mismatch"));
}

// --- `harn pack verify` integration coverage --------------------------------

#[test]
fn pack_verify_signed_bundle_passes_and_reports_signature_key() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"signed\")\n").unwrap();
    let key = workdir.path().join("release-key.pem");
    fs::write(&key, TEST_ED25519_PRIVATE_KEY).unwrap();
    let out = workdir.path().join("hello.harnpack");

    let outcome = build_pack(&PackArgs {
        command: None,
        entrypoint: Some(entry.clone()),
        out: Some(out.clone()),
        upgrade: None,
        sign: true,
        key: Some(key),
        unsigned: false,
        exclude_secrets: false,
        include_secrets: false,
        json: false,
    });

    let report = pack::verify(&PackVerifyArgs {
        bundle: out.clone(),
        allow_unsigned: false,
        trust_policy: None,
        require_trusted_signer: false,
        strict: false,
        json: false,
    })
    .expect("verify ok on signed bundle");
    assert_eq!(report.bundle_hash, outcome.bundle_hash);
    assert!(report.signature_present);
    assert!(report.signature_verified);
    assert!(report.key_id.is_some());
    assert_eq!(
        report.recorded_bundle_hash.as_deref(),
        Some(outcome.bundle_hash.as_str())
    );
    assert_eq!(report.module_count, 1);
    assert!(report.content_entry_count >= 3, "{:?}", report);
}

#[test]
fn pack_verify_unsigned_bundle_refused_without_flag_but_ok_with_flag() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"hi\")\n").unwrap();
    let out = workdir.path().join("hello.harnpack");
    build_pack(&pack_args(entry.clone(), out.clone()));

    let strict = pack::verify(&PackVerifyArgs {
        bundle: out.clone(),
        allow_unsigned: false,
        trust_policy: None,
        require_trusted_signer: false,
        strict: false,
        json: false,
    })
    .expect_err("unsigned bundle must refuse without --allow-unsigned");
    assert_eq!(strict.code, "verify.unsigned");

    let lenient = pack::verify(&PackVerifyArgs {
        bundle: out.clone(),
        allow_unsigned: true,
        trust_policy: None,
        require_trusted_signer: false,
        strict: false,
        json: false,
    })
    .expect("verify ok on unsigned bundle with --allow-unsigned");
    assert!(!lenient.signature_present);
    assert!(!lenient.signature_verified);
}

#[test]
fn pack_verify_tampered_signed_bundle_fails() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"signed\")\n").unwrap();
    let key = workdir.path().join("release-key.pem");
    fs::write(&key, TEST_ED25519_PRIVATE_KEY).unwrap();
    let out = workdir.path().join("hello.harnpack");

    build_pack(&PackArgs {
        command: None,
        entrypoint: Some(entry.clone()),
        out: Some(out.clone()),
        upgrade: None,
        sign: true,
        key: Some(key),
        unsigned: false,
        exclude_secrets: false,
        include_secrets: false,
        json: false,
    });

    // Tamper the source bytes inside the archive but keep the original
    // manifest (with its recorded source_hash_blake3) intact. Repack
    // with `build_harnpack` so the on-disk archive is structurally
    // valid but its content no longer matches the manifest's hashes.
    let mut archive = read_harnpack(&fs::read(&out).unwrap()).unwrap();
    let source = archive
        .contents
        .iter_mut()
        .find(|entry| entry.path == Path::new("sources/hello.harn"))
        .expect("source entry present");
    source.bytes = b"__io_println(\"tampered\")\n".to_vec();
    let tampered =
        harn_vm::orchestration::build_harnpack(&archive.manifest, &archive.contents).unwrap();
    fs::write(&out, &tampered).unwrap();

    let err = pack::verify(&PackVerifyArgs {
        bundle: out.clone(),
        allow_unsigned: true,
        trust_policy: None,
        require_trusted_signer: false,
        strict: false,
        json: false,
    })
    .expect_err("tampered bundle must fail verification");
    assert!(
        err.code.starts_with("verify."),
        "expected verify.* error, got {}",
        err.code
    );
}

#[test]
fn pack_verify_json_envelope_round_trips_schema() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"hi\")\n").unwrap();
    let out = workdir.path().join("hello.harnpack");
    build_pack(&pack_args(entry.clone(), out.clone()));

    let envelope = pack::verify_to_envelope(&PackVerifyArgs {
        bundle: out.clone(),
        allow_unsigned: true,
        trust_policy: None,
        require_trusted_signer: false,
        strict: false,
        json: true,
    });
    let value = serde_json::to_value(&envelope).unwrap();
    jsonschema::draft202012::meta::validate(&pack::verify_json_schema()).unwrap();
    let validator = jsonschema::draft202012::new(&pack::verify_json_schema()).unwrap();
    validator.validate(&value).unwrap();
    assert_eq!(value["schemaVersion"], pack::PACK_VERIFY_SCHEMA_VERSION);
    assert_eq!(value["ok"], true);
    assert!(value["data"]["bundle_hash"]
        .as_str()
        .is_some_and(|s| s.starts_with("blake3:")));
    assert_eq!(value["data"]["signature_present"], false);
    assert_eq!(value["data"]["signature_verified"], false);
    assert!(value["data"]["module_count"].as_u64().unwrap() >= 1);
}

#[test]
fn pack_verify_require_trusted_signer_rejects_signer_outside_policy_allowlist() {
    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();

        let workdir = TempDir::new().unwrap();
        let signers_dir = workdir.path().join("signers");
        fs::create_dir_all(&signers_dir).unwrap();
        let entry = workdir.path().join("hello.harn");
        fs::write(&entry, "__io_println(\"signed\")\n").unwrap();
        let key = workdir.path().join("release-key.pem");
        fs::write(&key, TEST_ED25519_PRIVATE_KEY).unwrap();
        let out = workdir.path().join("hello.harnpack");

        let outcome = pack::build(&BuildArgs {
            entrypoint: entry.clone(),
            out: Some(out.clone()),
            upgrade: None,
            sign: true,
            key: Some(key),
            unsigned: false,
            exclude_secrets: false,
            json: false,
        })
        .expect("pack succeeds");
        let signer_fingerprint = outcome.json.signature.key_id.clone().unwrap();
        fs::write(
            signers_dir.join(format!("{signer_fingerprint}.pub")),
            test_public_key_pem(),
        )
        .unwrap();
        let policy = workdir.path().join("trust-policy.json");
        fs::write(
            &policy,
            format!(
                r#"{{"signer_registry_url":"{}","trusted_signers":["not-the-real-signer"]}}"#,
                signers_dir.display()
            ),
        )
        .unwrap();

        let err = pack::verify(&PackVerifyArgs {
            bundle: out,
            allow_unsigned: false,
            trust_policy: Some(policy),
            require_trusted_signer: true,
            strict: false,
            json: false,
        })
        .expect_err("unexpected signer must fail the trust policy");

        assert_eq!(err.code, "verify.untrusted_signer");
        assert!(err.message.contains("trusted_signers allowlist"));
    });
}

#[test]
fn pack_verify_strict_rejects_tampered_sbom_module_hash() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(&entry, "__io_println(\"hi\")\n").unwrap();
    let out = workdir.path().join("hello.harnpack");
    build_pack(&pack_args(entry.clone(), out.clone()));

    let mut archive = read_harnpack(&fs::read(&out).unwrap()).unwrap();
    let module_package = archive
        .manifest
        .sbom
        .packages
        .iter_mut()
        .find(|package| package.name == "module:hello.harn")
        .expect("module package present");
    module_package.package_hash_blake3 =
        Some("blake3:0000000000000000000000000000000000000000000000000000000000000000".to_string());
    let sbom_entry = archive
        .contents
        .iter_mut()
        .find(|entry| entry.path == Path::new(pack::PACK_SBOM_ARCHIVE_PATH))
        .expect("sbom entry present");
    sbom_entry.bytes = serde_json::to_vec(&archive.manifest.sbom).unwrap();
    let tampered =
        harn_vm::orchestration::build_harnpack(&archive.manifest, &archive.contents).unwrap();
    fs::write(&out, &tampered).unwrap();

    pack::verify(&PackVerifyArgs {
        bundle: out.clone(),
        allow_unsigned: true,
        trust_policy: None,
        require_trusted_signer: false,
        strict: false,
        json: false,
    })
    .expect("non-strict verify ignores SBOM-only drift");

    let err = pack::verify(&PackVerifyArgs {
        bundle: out,
        allow_unsigned: true,
        trust_policy: None,
        require_trusted_signer: false,
        strict: true,
        json: false,
    })
    .expect_err("strict verify must reject SBOM package hash drift");
    assert_eq!(err.code, "verify.sbom_mismatch");
    assert!(err.message.contains("module:hello.harn"));
}

#[test]
fn pack_verify_strict_rejects_tampered_sbom_asset_hash() {
    let workdir = TempDir::new().unwrap();
    let prompts = workdir.path().join("prompts");
    fs::create_dir_all(&prompts).unwrap();
    fs::write(prompts.join("template.txt"), "asset body\n").unwrap();
    let entry = workdir.path().join("hello.harn");
    fs::write(
        &entry,
        "import \"./prompts/template.txt\"\n__io_println(\"hi\")\n",
    )
    .unwrap();
    let out = workdir.path().join("hello.harnpack");
    build_pack(&pack_args(entry.clone(), out.clone()));

    let mut archive = read_harnpack(&fs::read(&out).unwrap()).unwrap();
    let asset_package = archive
        .manifest
        .sbom
        .packages
        .iter_mut()
        .find(|package| package.name == "asset:prompts/template.txt")
        .expect("asset package present");
    asset_package.package_hash_blake3 =
        Some("blake3:0000000000000000000000000000000000000000000000000000000000000000".to_string());
    let sbom_entry = archive
        .contents
        .iter_mut()
        .find(|entry| entry.path == Path::new(pack::PACK_SBOM_ARCHIVE_PATH))
        .expect("sbom entry present");
    sbom_entry.bytes = serde_json::to_vec(&archive.manifest.sbom).unwrap();
    let tampered =
        harn_vm::orchestration::build_harnpack(&archive.manifest, &archive.contents).unwrap();
    fs::write(&out, &tampered).unwrap();

    let err = pack::verify(&PackVerifyArgs {
        bundle: out,
        allow_unsigned: true,
        trust_policy: None,
        require_trusted_signer: false,
        strict: true,
        json: false,
    })
    .expect_err("strict verify must reject SBOM asset hash drift");
    assert_eq!(err.code, "verify.sbom_mismatch");
    assert!(err.message.contains("asset:prompts/template.txt"));
}

#[test]
fn pack_exclude_secrets_blocks_entrypoint_with_dotenv_name() {
    let workdir = TempDir::new().unwrap();
    let entry = workdir.path().join(".env.harn");
    fs::write(&entry, "__io_println(\"secret\")\n").unwrap();
    let out = workdir.path().join(".env.harnpack");

    let runtime = Builder::new_current_thread().enable_all().build().unwrap();
    let err = runtime.block_on(async {
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        pack::build(&BuildArgs {
            entrypoint: entry.clone(),
            out: Some(out.clone()),
            upgrade: None,
            sign: false,
            key: None,
            unsigned: true,
            exclude_secrets: true,
            json: false,
        })
        .expect_err("--exclude-secrets must refuse secret-looking entrypoints")
    });
    assert_eq!(err.code, "pack.secret_blocked");
    assert!(!out.exists(), "blocked bundle must not be written");
}

#[test]
fn pack_exclude_secrets_skips_imported_assets_with_warning() {
    let workdir = TempDir::new().unwrap();
    let secrets = workdir.path().join("secrets");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(secrets.join("prompt.txt"), "token={{secret}}\n").unwrap();
    let entry = workdir.path().join("entry.harn");
    fs::write(
        &entry,
        "import \"./secrets/prompt.txt\"\n__io_println(\"ok\")\n",
    )
    .unwrap();
    let out = workdir.path().join("entry.harnpack");

    let envelope = Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            pack::run_to_envelope(&PackArgs {
                command: None,
                entrypoint: Some(entry.clone()),
                out: Some(out.clone()),
                upgrade: None,
                sign: false,
                key: None,
                unsigned: true,
                exclude_secrets: true,
                include_secrets: false,
                json: true,
            })
        });

    let value = serde_json::to_value(&envelope).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["warnings"][0]["code"], "pack.asset_skipped_secret");
    assert_eq!(
        value["data"]["manifest"]["metadata"]["skipped_assets"][0]["path"],
        "secrets/prompt.txt"
    );
    assert_eq!(
        value["data"]["manifest"]["metadata"]["skipped_assets"][0]["reason"],
        "secret_path"
    );

    let archive = read_harnpack(&fs::read(&out).unwrap()).unwrap();
    assert!(!archive
        .contents
        .iter()
        .any(|entry| entry.path == Path::new("sources/secrets/prompt.txt")));
    assert!(!archive
        .manifest
        .sbom
        .packages
        .iter()
        .any(|package| package.name == "asset:secrets/prompt.txt"));
}

fn release_records(
    log: &std::sync::Arc<harn_vm::event_log::AnyEventLog>,
) -> Vec<harn_vm::TrustRecord> {
    futures::executor::block_on(harn_vm::query_trust_records(
        log,
        &harn_vm::TrustQueryFilters {
            action: Some(harn_vm::TRUST_ACTION_RELEASE.to_string()),
            ..harn_vm::TrustQueryFilters::default()
        },
    ))
    .unwrap()
}
