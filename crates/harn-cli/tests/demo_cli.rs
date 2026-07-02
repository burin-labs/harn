#![recursion_limit = "256"]

//! In-process coverage of `harn demo` (#1650).
//!
//! Each bundled scenario must run end-to-end against its checked-in
//! offline tape with no API keys, no network, no provider config.
//! These tests are the drift gate: if a scenario script changes shape
//! but the tape doesn't (or vice versa), this suite goes red.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use harn_cli::commands::demo::scenario_ids;
use harn_cli::commands::run::{
    execute_run_with_sandbox_options, CliLlmMockMode, RunOutcome, RunProfileOptions,
    RunSandboxOptions,
};
use harn_cli::env_guard::ScopedEnvVar;
use harn_cli::tests::common::{cwd_lock, env_lock};

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

struct ScopedCwd {
    previous: PathBuf,
}

impl ScopedCwd {
    fn enter(dir: &Path) -> Self {
        let previous = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(dir).expect("set isolated demo cwd");
        Self { previous }
    }
}

impl Drop for ScopedCwd {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}

fn copy_demo_assets(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create isolated demo asset dir");
    for entry in fs::read_dir(src).expect("read demo asset dir") {
        let entry = entry.expect("read demo asset entry");
        if entry.file_name() == std::ffi::OsStr::new(".harn") {
            continue;
        }
        let source = entry.path();
        let target = dst.join(entry.file_name());
        if source.is_dir() {
            copy_demo_assets(&source, &target);
        } else {
            fs::copy(&source, &target).expect("copy demo asset file");
        }
    }
}

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-demo-test".to_string())
        .stack_size(harn_cli::CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            runtime.block_on(future_factory())
        })
        .expect("spawn runtime thread");
    handle.join().expect("runtime thread completed")
}

#[test]
fn demo_asset_copy_ignores_harn_state_dirs() {
    let source = tempfile::TempDir::new().expect("create source dir");
    let target = tempfile::TempDir::new().expect("create target dir");
    fs::write(source.path().join("scenario.harn"), "").expect("write scenario");
    fs::create_dir_all(source.path().join(".harn/metadata/classification"))
        .expect("write leaked state dir");
    fs::write(
        source
            .path()
            .join(".harn/metadata/classification/entries.json"),
        r#"{"entries":[]}"#,
    )
    .expect("write leaked state");

    copy_demo_assets(source.path(), target.path());

    assert!(target.path().join("scenario.harn").is_file());
    assert!(
        !target.path().join(".harn").exists(),
        "demo test fixture copy must not import runtime state"
    );
}

fn run_demo_scenario(id: &str) -> RunOutcome {
    let assets = PathBuf::from(MANIFEST_DIR).join("assets/demo").join(id);
    assert!(
        assets.join("scenario.harn").is_file(),
        "missing scenario.harn for {id}"
    );
    assert!(
        assets.join("tape.jsonl").is_file(),
        "missing tape.jsonl for {id}"
    );
    run_in_harn_runtime(move || async move {
        let _env_guard = env_lock::lock_env().lock().await;
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        let isolated_assets = tempfile::TempDir::new().expect("create isolated demo assets dir");
        copy_demo_assets(&assets, isolated_assets.path());
        let script = isolated_assets.path().join("scenario.harn");
        let tape = isolated_assets.path().join("tape.jsonl");
        let _demo_cwd = ScopedCwd::enter(isolated_assets.path());
        // Hermetic bytecode cache: point `harn run` at a fresh per-test
        // directory so the demo always compiles the scenario from source
        // instead of replaying whatever the ambient `$HOME/.cache/harn`
        // happens to hold. Without this, a stale artifact written by an
        // earlier (or buggy) compiler masks both regressions and fixes —
        // and on a persistent runner the result is order-dependent and
        // flaky. The guards are held across `execute_run` and restore the
        // previous env / delete the temp dir on drop.
        let cache_dir = tempfile::TempDir::new().expect("create temp bytecode cache dir");
        let _cache_guard = ScopedEnvVar::set(
            harn_vm::bytecode_cache::CACHE_DIR_ENV,
            cache_dir.path().to_str().expect("temp cache path is utf-8"),
        );
        harn_vm::reset_thread_local_state();
        execute_run_with_sandbox_options(
            script.to_string_lossy().as_ref(),
            false,
            HashSet::new(),
            Vec::new(),
            Vec::new(),
            CliLlmMockMode::Replay {
                fixture_path: tape.clone(),
            },
            None,
            RunProfileOptions::default(),
            RunSandboxOptions::default().with_workspace_root(isolated_assets.path()),
        )
        .await
    })
}

#[test]
fn merge_captain_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("merge-captain");
    assert_eq!(
        outcome.exit_code, 0,
        "merge-captain demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("merge_supervision_receipt"),
        "merge-captain stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("[#421]")
            && outcome.stdout.contains("[#422]")
            && outcome.stdout.contains("[#423]"),
        "merge-captain demo should triage all three PRs:\n{}",
        outcome.stdout
    );
}

#[test]
fn review_captain_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("review-captain");
    assert_eq!(
        outcome.exit_code, 0,
        "review-captain demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("review_receipt"),
        "review-captain stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("clarifying_question_asked"),
        "review-captain demo should record HITL question:\n{}",
        outcome.stdout
    );
}

#[test]
fn provider_race_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("provider-race");
    assert_eq!(
        outcome.exit_code, 0,
        "provider-race demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("race_attribution_receipt"),
        "provider-race stdout missing attribution receipt:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("[anthropic]")
            && outcome.stdout.contains("[openai]")
            && outcome.stdout.contains("[ollama]"),
        "provider-race demo should report all three providers:\n{}",
        outcome.stdout
    );
}

#[test]
fn routing_policy_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("routing-policy");
    assert_eq!(
        outcome.exit_code, 0,
        "routing-policy demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("routing_supervision_receipt"),
        "routing-policy stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("=== task smoke ===")
            && outcome.stdout.contains("=== task rate-lim ===")
            && outcome.stdout.contains("=== task lint-fail ==="),
        "routing-policy demo should exercise all three tasks:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"escalations\":2"),
        "routing-policy demo should record two escalations (rate-lim + lint-fail):\n{}",
        outcome.stdout
    );
}

#[test]
fn stdlib_toolkit_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("stdlib-toolkit");
    assert_eq!(
        outcome.exit_code, 0,
        "stdlib-toolkit demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("stdlib_toolkit_receipt"),
        "stdlib-toolkit demo should emit the toolkit receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"merged_retries\":5"),
        "deep_merge should land the per-task override:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"unique_chats\":[\"x.jsonl\",\"y.jsonl\",\"z.jsonl\"]"),
        "unique should collapse the duplicate chat reference in first-seen order:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"lossless_address_count\":2"),
        "preserve_repeated_tag should keep both <a> entries under the inner tag:\n{}",
        outcome.stdout
    );
}

#[test]
fn embed_similarity_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("embed-similarity");
    assert_eq!(
        outcome.exit_code, 0,
        "embed-similarity demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("embed_similarity_receipt"),
        "embed-similarity demo should emit the receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"backend\":\"lexical-hash\""),
        "asset-free demo path should use the lexical backend:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"dim\":256") && outcome.stdout.contains("\"vector_dim\":256"),
        "info and vector dimensions should agree on the default backend:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"related_beats_unrelated\":true"),
        "related text must outrank unrelated text:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"top_k_count\":3")
            && outcome
                .stdout
                .contains("validate the auth token on each request")
            && outcome
                .stdout
                .contains("auth token refresh and validation flow"),
        "top-k should return the two auth-token corpus entries:\n{}",
        outcome.stdout
    );
}

#[test]
fn project_metadata_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("project-metadata");
    assert_eq!(
        outcome.exit_code, 0,
        "project-metadata demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("project_metadata_receipt"),
        "project-metadata demo should emit the receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"origin_dir\":\"demo\""),
        "metadata_inspect should report the parent origin directory:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"inherited_import_rule\":\"prefer relative imports for sibling modules\""),
        "metadata_get should inherit the parent namespace from a child directory:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"hash_refreshed\":true"),
        "metadata_refresh_hashes should clear the missing structure hash flag:\n{}",
        outcome.stdout
    );
}

#[test]
fn compaction_policy_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("compaction-policy");
    assert_eq!(
        outcome.exit_code, 0,
        "compaction-policy demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("compaction_policy_demo"),
        "compaction-policy demo should emit the receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"pre_decision_action\":\"compact_now\""),
        "compaction.check should flag the seeded session for compaction:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"post_decision_action\":\"defer\""),
        "compaction.check should defer once the transcript is compacted:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"engine_strategy\":\"custom\""),
        "compaction.run should record the engine strategy in the receipt:\n{}",
        outcome.stdout
    );
}

#[test]
fn edit_rename_symbol_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("edit-rename-symbol");
    assert_eq!(
        outcome.exit_code, 0,
        "edit-rename-symbol demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("edit_rename_symbol_receipt"),
        "edit-rename-symbol demo should emit the rename receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"planned_result\":\"applied\""),
        "dry_run planning phase must succeed against the seed workspace:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"planned_touched_files\":[\"src/lib.rs\",\"src/main.rs\"]"),
        "dry_run plan must enumerate both seed files:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"conflict_result\":\"conflict\""),
        "the shadow-site rename must short-circuit with `conflict`:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"conflict_first_shadow\":\"Gadget\""),
        "the conflict response must name `Gadget` as the shadow identifier:\n{}",
        outcome.stdout
    );
}

#[test]
fn edit_refactor_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("edit-refactor");
    assert_eq!(
        outcome.exit_code, 0,
        "edit-refactor demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("edit_refactor_receipt"),
        "edit-refactor demo should emit the refactor receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"extract_result\":\"applied\"")
            && outcome.stdout.contains("\"extract_signature\":true"),
        "extract_function dry-run must synthesize a captured-parameter signature:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"add_param_result\":\"applied\"")
            && outcome.stdout.contains("\"add_param_calls_filled\":true"),
        "add_parameter dry-run must fill the new argument at every call site:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"return_type_result\":\"applied\""),
        "change_return_type dry-run must rewrite the return type:\n{}",
        outcome.stdout
    );
}

#[test]
fn http_transport_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("http-transport");
    assert_eq!(
        outcome.exit_code, 0,
        "http-transport demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("http_transport_receipt"),
        "http-transport stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"chose_content_type\":\"application/json\""),
        "content negotiation must pick application/json from the simulated Accept:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"not_modified_status\":304"),
        "http_not_modified must produce a 304 envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"upgrade_status\":101"),
        "http_upgrade_ws must produce a 101 envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"upgrade_subprotocol\":\"v1.harn\""),
        "WS subprotocol negotiation must pick v1.harn from the offered list:\n{}",
        outcome.stdout
    );
}

#[test]
fn harn_site_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("harn-site");
    assert_eq!(
        outcome.exit_code, 0,
        "harn-site demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("harn_site_receipt"),
        "harn-site stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"conditional_fresh_status\":200")
            && outcome.stdout.contains("\"conditional_cached_status\":304"),
        "conditional handler must return 200 fresh then 304 via http_not_modified:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"echo_name\":\"ada\""),
        "the echo handler must round-trip the posted JSON body:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"ws_reply\":\"echo:hi\""),
        "the on_message WebSocket callback must echo the inbound frame:\n{}",
        outcome.stdout
    );
}

#[test]
fn obs_primitive_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("obs-primitive");
    assert_eq!(
        outcome.exit_code, 0,
        "obs-primitive demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("obs_primitive_receipt={"),
        "obs-primitive demo should emit the receipt envelope:\n{}",
        outcome.stdout
    );
    // The session_put helper opens one span, records three instruments,
    // and emits one structured log under that span.
    assert!(
        outcome.stdout.contains("span_end_count: 1"),
        "obs-primitive must close exactly one span:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("metric_count: 3"),
        "obs-primitive must record counter/histogram/gauge (3 metrics):\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("log_count: 1"),
        "obs-primitive must emit one structured log:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("request_id: <unbound>"),
        "obs-primitive runs standalone — request_id should be unbound until \
         `harn serve --obs stdout` pushes one:\n{}",
        outcome.stdout
    );
}

#[test]
fn edit_language_coverage_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("edit-language-coverage");
    assert_eq!(
        outcome.exit_code, 0,
        "edit-language-coverage demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("edit_language_coverage_receipt"),
        "demo should emit the coverage receipt envelope:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"json_result\":\"applied\""),
        "apply_node must round-trip on the JSON seed:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"css_result\":\"applied\""),
        "apply_node must round-trip on the CSS seed:\n{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("\"dockerfile_result\":\"unsupported_language\""),
        "a language with no grammar must degrade to unsupported_language:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"dockerfile_has_fallback\":true"),
        "the unsupported result must carry a fallback_suggestion:\n{}",
        outcome.stdout
    );
}

#[test]
fn prompt_guidance_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("prompt-guidance");
    assert_eq!(
        outcome.exit_code, 0,
        "prompt-guidance demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("prompt_guidance_receipt"),
        "prompt-guidance stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    // The whole point: the tool's guidance is present iff the tool is, so the
    // assembled prompt differs by exactly the one capability-gated fragment.
    assert!(
        outcome.stdout.contains("\"drift_proof\":true"),
        "guidance must appear with the tool and vanish without it:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"guidance_with_tool\":true")
            && outcome.stdout.contains("\"guidance_without_tool\":false"),
        "guidance presence must track tool presence exactly:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("tool:todo.guidance"),
        "provenance must name the capability-gated fragment:\n{}",
        outcome.stdout
    );
}

#[test]
fn pub_type_exports_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("pub-type-exports");
    assert_eq!(
        outcome.exit_code, 0,
        "pub-type-exports demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("pub_type_exports_receipt"),
        "pub-type-exports stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    // The whole point: the alias exported from verdicts.harn drives both the
    // hand-written annotation and the schema-validated LLM result.
    assert!(
        outcome.stdout.contains("\"annotation_verdict\":\"pass\"")
            && outcome.stdout.contains("\"llm_verdict\":\"pass\""),
        "both the annotated and schema-validated reports must bind the imported alias:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("verdicts.harn#GradeReport"),
        "receipt must name the exporting module as the schema source:\n{}",
        outcome.stdout
    );
}

#[test]
fn catalog_patch_models_demo_runs_end_to_end_against_bundled_tape() {
    let outcome = run_demo_scenario("catalog-patch-models");
    assert_eq!(
        outcome.exit_code, 0,
        "catalog-patch-models demo failed (exit {}):\nstderr:\n{}\nstdout:\n{}",
        outcome.exit_code, outcome.stderr, outcome.stdout
    );
    assert!(
        outcome.stdout.contains("catalog_patch_models_receipt"),
        "catalog-patch-models stdout missing receipt envelope:\n{}",
        outcome.stdout
    );
    // The whole point: the two patched fields carry the patch values while
    // sibling fields of the same row (and same nested pricing table) keep
    // their baseline values.
    assert!(
        outcome.stdout.contains("\"patched_stream_timeout\":1200.0")
            && outcome.stdout.contains("\"patched_output_per_mtok\":7.5"),
        "patched fields must reflect the [patch.models] overlay:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("\"preserved_input_per_mtok\":3.0")
            && outcome
                .stdout
                .contains("\"preserved_name\":\"Patch Target (demo)\""),
        "unpatched sibling fields must keep their baseline values:\n{}",
        outcome.stdout
    );
}

#[test]
fn every_scenario_listed_has_a_passing_smoke_run() {
    // Belt-and-suspenders: if a future scenario lands in SCENARIOS but
    // someone forgets to add a per-scenario test above, this catch-all
    // exercises it through the same offline-tape path.
    let known: HashSet<&str> = [
        "merge-captain",
        "review-captain",
        "provider-race",
        "routing-policy",
        "stdlib-toolkit",
        "embed-similarity",
        "project-metadata",
        "compaction-policy",
        "edit-rename-symbol",
        "edit-language-coverage",
        "edit-refactor",
        "http-transport",
        "harn-site",
        "obs-primitive",
        "prompt-guidance",
    ]
    .into_iter()
    .collect();
    for id in scenario_ids() {
        if known.contains(id) {
            continue;
        }
        let outcome = run_demo_scenario(id);
        assert_eq!(
            outcome.exit_code, 0,
            "demo scenario `{id}` failed (exit {}):\nstderr:\n{}",
            outcome.exit_code, outcome.stderr
        );
    }
}
