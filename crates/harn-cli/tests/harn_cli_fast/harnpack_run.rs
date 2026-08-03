//! `harn run <bundle.harnpack>` end-to-end coverage (#1784).
//!
//! Exercises the four exit-criteria items from the issue:
//!  - signed `.harnpack` runs end-to-end,
//!  - unsigned bundles are rejected without `--allow-unsigned`,
//!  - tampered bundles fail verification, and
//!  - the content-addressed cache prevents redundant unpack on the
//!    second run.

// The async stack inside `execute_run_with_harnpack_options` is deeply
// nested (tokio LocalSet, VM execute, persona/hook fan-out). Layout
// monomorphization hits the default rustc query depth, so bump it for
// this test crate.

use std::fs;
use std::path::{Path, PathBuf};

use harn_cli::cli::{PackArgs, PackVerifyArgs};
use harn_cli::commands::pack;
use harn_cli::commands::run::harnpack::HarnpackRunOptions;
use harn_cli::commands::run::{
    execute_run_with_harnpack_options, CliLlmMockMode, RunOutcome, RunProfileOptions,
};
use harn_cli::tests::common::{cwd_lock, harn_state_lock};
use harn_vm::orchestration::{build_harnpack, read_harnpack};
use tempfile::TempDir;
use tokio::runtime::Builder;

const TEST_ED25519_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIDmsNDO8iZqiMmA/b7I7lwGXNKe68o+gDno6R5riUcDC\n-----END PRIVATE KEY-----\n";

struct HarnpackFixture {
    workdir: TempDir,
    cache_dir: PathBuf,
    pack_path: PathBuf,
    key_path: PathBuf,
}

impl HarnpackFixture {
    fn new(script: &str) -> Self {
        let workdir = TempDir::new().expect("workdir");
        let cache_dir = workdir.path().join("cache");
        let entry = workdir.path().join("hello.harn");
        fs::write(&entry, script).expect("write entry");
        let key_path = workdir.path().join("release-key.pem");
        fs::write(&key_path, TEST_ED25519_PRIVATE_KEY).expect("write key");
        let pack_path = workdir.path().join("hello.harnpack");
        Self {
            workdir,
            cache_dir,
            pack_path,
            key_path,
        }
    }

    fn pack_args(&self, sign: bool) -> PackArgs {
        PackArgs {
            command: None,
            entrypoint: Some(self.workdir.path().join("hello.harn")),
            out: Some(self.pack_path.clone()),
            upgrade: None,
            sign,
            key: if sign {
                Some(self.key_path.clone())
            } else {
                None
            },
            unsigned: !sign,
            exclude_secrets: false,
            include_secrets: false,
            json: false,
        }
    }
}

fn execute(pack_path: &Path, cache_dir: &Path, options: HarnpackRunOptions) -> RunOutcome {
    let pack_path = pack_path.to_path_buf();
    let cache_dir = cache_dir.to_path_buf();
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio current-thread runtime")
        .block_on(async move {
            let _env = harn_state_lock::lock_harn_state_async().await;
            let _cwd = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            let prev_cache = std::env::var("HARN_CACHE_DIR").ok();
            std::env::set_var("HARN_CACHE_DIR", &cache_dir);
            let outcome = execute_run_with_harnpack_options(
                &pack_path.to_string_lossy(),
                false,
                std::collections::HashSet::new(),
                Vec::new(),
                Vec::new(),
                CliLlmMockMode::Off,
                None,
                RunProfileOptions::default(),
                options,
            )
            .await;
            match prev_cache {
                Some(value) => std::env::set_var("HARN_CACHE_DIR", value),
                None => std::env::remove_var("HARN_CACHE_DIR"),
            }
            outcome
        })
}

fn build_pack(args: &PackArgs) -> pack::PackOutcome {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio current-thread runtime")
        .block_on(async {
            let _cwd_guard = cwd_lock::lock_cwd_async().await;
            harn_vm::reset_thread_local_state();
            let build_args = pack::BuildArgs {
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
            };
            pack::build(&build_args).expect("pack succeeds")
        })
}

#[test]
fn signed_harnpack_runs_end_to_end_and_reuses_cache() {
    let fixture = HarnpackFixture::new(
        "fn main(harness: Harness) { harness.stdio.println(\"signed-pack-greeting\") }\n",
    );
    let outcome = build_pack(&fixture.pack_args(true));
    let sanitized_hash = outcome.bundle_hash.replace(':', "_");

    let first = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions::default(),
    );
    assert_eq!(
        first.exit_code, 0,
        "first run failed; stderr=\n{}",
        first.stderr
    );
    assert!(
        first.stdout.contains("signed-pack-greeting"),
        "stdout should carry pipeline output; stdout=\n{}\nstderr=\n{}",
        first.stdout,
        first.stderr
    );

    let cache_slot = fixture
        .cache_dir
        .join("packs")
        .join(&sanitized_hash)
        .join("harnpack.json");
    assert!(
        cache_slot.exists(),
        "cache slot {} should be populated after first run",
        cache_slot.display()
    );
    let cached_mtime = fs::metadata(&cache_slot)
        .and_then(|meta| meta.modified())
        .expect("read cache mtime");

    let second = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions::default(),
    );
    assert_eq!(second.exit_code, 0, "second run failed: {}", second.stderr);
    assert!(second.stdout.contains("signed-pack-greeting"));
    let reused_mtime = fs::metadata(&cache_slot)
        .and_then(|meta| meta.modified())
        .expect("read cache mtime again");
    assert_eq!(
        cached_mtime, reused_mtime,
        "second run must reuse the unpacked cache instead of overwriting it"
    );
}

#[test]
fn first_run_reaches_packaged_entry_and_nested_module_artifacts() {
    let fixture = HarnpackFixture::new(
        "import { greeting } from \"./message\"\n\
         fn main(harness: Harness) { harness.stdio.println(greeting()) }\n",
    );
    fs::create_dir(fixture.workdir.path().join("nested")).unwrap();
    fs::write(
        fixture.workdir.path().join("message.harn"),
        "import { suffix } from \"./nested/suffix\"\n\
         pub fn greeting() -> string { return \"packaged-\" + suffix() }\n",
    )
    .unwrap();
    fs::write(
        fixture.workdir.path().join("nested/suffix.harn"),
        "pub fn suffix() -> string { return \"artifact\" }\n",
    )
    .unwrap();
    let packed = build_pack(&fixture.pack_args(true));
    let archive = read_harnpack(&fs::read(&fixture.pack_path).unwrap()).unwrap();
    assert_eq!(
        archive
            .contents
            .iter()
            .filter(|entry| entry.path.extension().and_then(|ext| ext.to_str()) == Some("harnbc"))
            .count(),
        3
    );
    let expected_suffix_module = archive
        .contents
        .iter()
        .find(|entry| entry.path == Path::new("bytecode/nested/suffix.harnmod"))
        .expect("suffix module artifact")
        .bytes
        .clone();
    assert_eq!(
        archive
            .contents
            .iter()
            .filter(|entry| entry.path.extension().and_then(|ext| ext.to_str()) == Some("harnmod"))
            .count(),
        3
    );

    let first = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions::default(),
    );
    assert_eq!(first.exit_code, 0, "first run failed: {}", first.stderr);
    assert!(first.stdout.contains("packaged-artifact"));

    let replay_dir = fixture
        .cache_dir
        .join("packs")
        .join(packed.bundle_hash.replace(':', "_"));
    for relative in [
        "hello.harnbc",
        "hello.harnmod",
        "message.harnbc",
        "message.harnmod",
        "nested/suffix.harnbc",
        "nested/suffix.harnmod",
    ] {
        assert!(
            replay_dir.join("sources").join(relative).is_file(),
            "canonical adjacent projection is missing {relative}"
        );
    }
    assert!(
        !replay_dir.join("bytecode").exists(),
        "fresh replay must not duplicate archive bytecode"
    );

    let shared_artifacts = fs::read_dir(&fixture.cache_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            matches!(
                entry.path().extension().and_then(|ext| ext.to_str()),
                Some("harnbc" | "harnmod")
            )
        })
        .collect::<Vec<_>>();
    assert!(
        shared_artifacts.is_empty(),
        "a first-run miss writes to the shared cache; found {shared_artifacts:?}"
    );

    // A cache hit is not an integrity decision. The verified archive repairs a
    // damaged adjacent module before the canonical loader can observe it.
    fs::write(
        replay_dir.join("sources/nested/suffix.harnmod"),
        b"not a harn module artifact",
    )
    .unwrap();
    let fallback = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions::default(),
    );
    assert_eq!(fallback.exit_code, 0, "repair failed: {}", fallback.stderr);
    assert!(fallback.stdout.contains("packaged-artifact"));
    assert_eq!(
        fs::read(replay_dir.join("sources/nested/suffix.harnmod")).unwrap(),
        expected_suffix_module,
        "verified archive bytes must repair the tampered cache target"
    );
    assert!(
        fs::read_dir(&fixture.cache_dir).unwrap().all(|entry| {
            entry.ok().is_some_and(|entry| {
                !matches!(
                    entry.path().extension().and_then(|ext| ext.to_str()),
                    Some("harnbc" | "harnmod")
                )
            })
        }),
        "authoritative repair should preserve entry and module cache hits"
    );
}

#[test]
fn unsigned_harnpack_is_rejected_by_default() {
    let fixture =
        HarnpackFixture::new("fn main(harness: Harness) { harness.stdio.println(\"unsigned\") }\n");
    build_pack(&fixture.pack_args(false));

    let outcome = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions::default(),
    );
    assert_eq!(outcome.exit_code, 1, "unsigned should be refused");
    assert!(
        outcome.stderr.contains("refusing to run unsigned bundle"),
        "stderr should explain refusal: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("--allow-unsigned"),
        "stderr should hint at the override: {}",
        outcome.stderr
    );
    assert!(
        outcome.stdout.is_empty(),
        "no pipeline output should be produced on refusal: {}",
        outcome.stdout
    );
}

#[test]
fn unsigned_harnpack_runs_with_allow_unsigned() {
    let fixture = HarnpackFixture::new(
        "fn main(harness: Harness) { harness.stdio.println(\"local-dev\") }\n",
    );
    build_pack(&fixture.pack_args(false));

    let outcome = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions {
            allow_unsigned: true,
            dry_run_verify: false,
        },
    );
    assert_eq!(
        outcome.exit_code, 0,
        "--allow-unsigned should permit running; stderr={}",
        outcome.stderr
    );
    assert!(outcome.stdout.contains("local-dev"));
}

#[test]
fn tampered_harnpack_fails_verification() {
    let fixture =
        HarnpackFixture::new("fn main(harness: Harness) { harness.stdio.println(\"original\") }\n");
    build_pack(&fixture.pack_args(true));

    // Decompose + tamper + re-emit: flip a source byte without touching
    // the embedded signature. The bundle hash will diverge, so the
    // signature check has to fail before the pipeline ever runs.
    let bytes = fs::read(&fixture.pack_path).unwrap();
    let mut archive = read_harnpack(&bytes).expect("archive parses");
    let target = archive
        .contents
        .iter_mut()
        .find(|entry| entry.path == Path::new("sources/hello.harn"))
        .expect("source entry");
    target.bytes = b"__io_println(\"tampered\")\n".to_vec();
    let tampered_bytes =
        build_harnpack(&archive.manifest, &archive.contents).expect("rebuild archive");
    fs::write(&fixture.pack_path, &tampered_bytes).unwrap();

    let outcome = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions::default(),
    );
    assert_eq!(outcome.exit_code, 1, "tampered pack must refuse");
    assert!(
        outcome.stderr.contains("signature hash mismatch"),
        "stderr should mention hash mismatch: {}",
        outcome.stderr
    );
    assert!(
        outcome.stdout.is_empty(),
        "tampered pack must not run the pipeline: {}",
        outcome.stdout
    );
}

#[test]
fn signed_asset_path_swap_fails_legacy_path_binding() {
    let fixture = HarnpackFixture::new(
        "import \"./assets/a.txt\"\n\
         import \"./assets/b.txt\"\n\
         fn main(harness: Harness) { harness.stdio.println(\"assets-bound\") }\n",
    );
    fs::create_dir(fixture.workdir.path().join("assets")).unwrap();
    fs::write(fixture.workdir.path().join("assets/a.txt"), b"asset A\n").unwrap();
    fs::write(fixture.workdir.path().join("assets/b.txt"), b"asset B\n").unwrap();
    build_pack(&fixture.pack_args(true));

    // v2 signatures cover the multiset of payload hashes. Swapping two paths
    // therefore preserves the signature but must fail the SBOM path binding
    // that `harn run` applies before replay.
    let mut archive = read_harnpack(&fs::read(&fixture.pack_path).unwrap()).unwrap();
    let a = archive
        .contents
        .iter()
        .position(|entry| entry.path == Path::new("sources/assets/a.txt"))
        .unwrap();
    let b = archive
        .contents
        .iter()
        .position(|entry| entry.path == Path::new("sources/assets/b.txt"))
        .unwrap();
    let a_bytes = archive.contents[a].bytes.clone();
    archive.contents[a].bytes = archive.contents[b].bytes.clone();
    archive.contents[b].bytes = a_bytes;
    fs::write(
        &fixture.pack_path,
        build_harnpack(&archive.manifest, &archive.contents).unwrap(),
    )
    .unwrap();

    let verify_error = pack::verify(&PackVerifyArgs {
        bundle: fixture.pack_path.clone(),
        allow_unsigned: false,
        trust_policy: None,
        require_trusted_signer: false,
        strict: false,
        json: false,
    })
    .expect_err("standalone verification must enforce the same path binding");
    assert!(verify_error.message.contains("SBOM package asset:assets/"));

    let outcome = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions::default(),
    );
    assert_eq!(outcome.exit_code, 1);
    assert!(
        outcome.stderr.contains("SBOM package asset:assets/"),
        "path-bound asset verification should decide the failure: {}",
        outcome.stderr
    );
    assert!(outcome.stdout.is_empty());
}

#[test]
fn dry_run_verify_returns_without_executing() {
    let fixture = HarnpackFixture::new(
        "fn main(harness: Harness) {\n  harness.fs.write_text(\"side-effect.txt\", \"oops\")\n  harness.stdio.println(\"ran\")\n}\n",
    );
    let outcome_pack = build_pack(&fixture.pack_args(true));
    let sanitized_hash = outcome_pack.bundle_hash.replace(':', "_");

    let outcome = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions {
            allow_unsigned: false,
            dry_run_verify: true,
        },
    );
    assert_eq!(
        outcome.exit_code, 0,
        "dry-run-verify on a valid pack should succeed; stderr={}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("harnpack verify ok"),
        "stderr should report the verify summary: {}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains(&outcome_pack.bundle_hash),
        "stderr should include the bundle hash: {}",
        outcome.stderr
    );
    assert!(
        outcome.stdout.is_empty(),
        "dry-run-verify must not run the pipeline: {}",
        outcome.stdout
    );
    assert!(
        !fixture.workdir.path().join("side-effect.txt").exists(),
        "dry-run-verify must not execute side effects"
    );

    // Replay still populates the cache slot so a later real run is a hit.
    let cache_slot = fixture
        .cache_dir
        .join("packs")
        .join(&sanitized_hash)
        .join("harnpack.json");
    assert!(
        cache_slot.exists(),
        "dry-run-verify still replays into the content-addressed cache"
    );
}

#[test]
fn missing_signature_with_dry_run_still_refuses() {
    let fixture =
        HarnpackFixture::new("fn main(harness: Harness) { harness.stdio.println(\"x\") }\n");
    build_pack(&fixture.pack_args(false));

    let outcome = execute(
        &fixture.pack_path,
        &fixture.cache_dir,
        HarnpackRunOptions {
            allow_unsigned: false,
            dry_run_verify: true,
        },
    );
    assert_eq!(
        outcome.exit_code, 1,
        "unsigned pack must still refuse even under --dry-run-verify"
    );
    assert!(
        outcome.stderr.contains("refusing to run unsigned bundle"),
        "stderr: {}",
        outcome.stderr
    );
}
