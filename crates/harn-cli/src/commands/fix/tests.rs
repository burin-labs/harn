use super::*;
use std::fs;
use std::path::PathBuf;

fn candidate(file: &str, start: usize, end: usize) -> RepairCandidate {
    RepairCandidate {
        file: file.to_string(),
        source: "typecheck",
        severity: "warning",
        code: Code::FormatterWouldReformat,
        message: "test".to_string(),
        span: Some(Span::with_offsets(start, end, 1, start + 1)),
        repair: Repair::from_template(Code::FormatterWouldReformat.repair_template().unwrap()),
        impact: RepairImpactWire::generic(),
        edits: vec![FixEdit {
            span: Span::with_offsets(start, end, 1, start + 1),
            replacement: "x".to_string(),
        }],
    }
}

#[test]
fn conflict_detection_marks_overlapping_edits() {
    let conflicts = detect_conflicts(&[
        candidate("a.harn", 0, 3),
        candidate("a.harn", 2, 4),
        candidate("a.harn", 4, 5),
        candidate("b.harn", 2, 4),
    ]);
    assert_eq!(conflicts[0], vec![1]);
    assert_eq!(conflicts[1], vec![0]);
    assert!(conflicts[2].is_empty());
    assert!(conflicts[3].is_empty());
}

#[test]
fn plan_reports_repairable_diagnostics_without_writing() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("repair_demo.harn");
    let source =
        "pipeline main() { const count = 1; const greeting = \"hello \" + count; greeting }\n";
    fs::write(&script, source).unwrap();
    let before = fs::read(&script).unwrap();

    let plan = build_plan(&script, Some(RepairSafety::BehaviorPreserving)).unwrap();

    assert_eq!(plan.schema_version, FIX_PLAN_SCHEMA_VERSION);
    assert!(
        plan.repairs.iter().any(|repair| {
            repair.repair.id == "style/string-interpolation"
                && repair.repair.safety == "behavior-preserving"
                && repair.applies_cleanly
        }),
        "expected string-interpolation repair in plan: {plan:#?}"
    );
    assert!(
        plan.repairs
            .iter()
            .all(|repair| repair.repair.safety != "needs-human"),
        "behavior-preserving ceiling must exclude needs-human repairs: {plan:#?}"
    );
    assert_eq!(fs::read(&script).unwrap(), before, "--plan must not write");

    let encoded = serde_json::to_value(&plan).unwrap();
    assert_eq!(encoded["schemaVersion"], FIX_PLAN_SCHEMA_VERSION);
    assert!(encoded["repairs"].as_array().is_some());
}

#[test]
fn plan_skips_invalid_files_and_keeps_repairing_valid_files() {
    let temp = tempfile::TempDir::new().unwrap();
    let valid = temp.path().join("valid.harn");
    let invalid = temp.path().join("invalid.harn");
    fs::write(
        &valid,
        "pipeline main() { const count = 1; const greeting = \"hello \" + count; greeting }\n",
    )
    .unwrap();
    fs::write(&invalid, "fn bad() {\n").unwrap();

    let plan = build_plan(temp.path(), Some(RepairSafety::BehaviorPreserving)).unwrap();

    assert!(
        plan.repairs.iter().any(|repair| {
            repair.repair.id == "style/string-interpolation"
                && repair_path(&plan, repair).unwrap() == valid.to_string_lossy().as_ref()
        }),
        "expected valid file repair despite invalid sibling: {plan:#?}"
    );
    assert_eq!(plan.skipped_files.len(), 1, "{plan:#?}");
    let skipped = &plan.skipped_files[0];
    assert_eq!(skipped.path, invalid.to_string_lossy().as_ref());
    assert_eq!(skipped.reason, "parse_error");
    assert_eq!(skipped.diagnostics[0].source, "parser");
    assert!(skipped.diagnostics[0].code.is_some());
    assert!(skipped.diagnostics[0].span.is_some());

    let encoded = serde_json::to_value(&plan).unwrap();
    assert_eq!(encoded["skippedFiles"][0]["reason"], "parse_error");
    assert!(encoded["skippedFiles"][0]["diagnostics"][0]["span"]["line"].is_u64());
}

#[test]
fn apply_writes_clean_repairs_and_reports_post_check_count() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("repair_demo.harn");
    fs::write(
        &script,
        "pipeline main() { const count = 1; const greeting = \"hello \" + count; greeting }\n",
    )
    .unwrap();

    let result = apply_repairs(&script, RepairSafety::BehaviorPreserving, false).unwrap();

    assert_eq!(result.schema_version, FIX_APPLY_SCHEMA_VERSION);
    assert_eq!(result.applied.len(), 1, "{result:#?}");
    assert!(result.skipped.is_empty(), "{result:#?}");
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    let updated = fs::read_to_string(&script).unwrap();
    assert!(updated.contains("\"hello ${count}\""), "{updated}");
}

#[test]
fn apply_directory_skips_invalid_files_after_applying_valid_files() {
    let temp = tempfile::TempDir::new().unwrap();
    let valid = temp.path().join("valid.harn");
    let invalid = temp.path().join("invalid.harn");
    fs::write(
        &valid,
        "pipeline main() { const count = 1; const greeting = \"hello \" + count; greeting }\n",
    )
    .unwrap();
    fs::write(&invalid, "fn bad() {\n").unwrap();

    let result = apply_repairs(temp.path(), RepairSafety::BehaviorPreserving, false).unwrap();

    assert_eq!(result.applied.len(), 1, "{result:#?}");
    assert_eq!(result.skipped_files.len(), 1, "{result:#?}");
    assert_eq!(
        result.skipped_files[0].path,
        invalid.to_string_lossy().as_ref()
    );
    assert_eq!(result.skipped_files[0].reason, "parse_error");
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    let updated = fs::read_to_string(&valid).unwrap();
    assert!(updated.contains("\"hello ${count}\""), "{updated}");
}

#[test]
fn apply_dry_run_reports_without_writing() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("repair_demo.harn");
    let source =
        "pipeline main() { const count = 1; const greeting = \"hello \" + count; greeting }\n";
    fs::write(&script, source).unwrap();

    let result = apply_repairs(&script, RepairSafety::BehaviorPreserving, true).unwrap();

    assert!(result.dry_run);
    assert_eq!(result.applied.len(), 1, "{result:#?}");
    assert_eq!(fs::read_to_string(&script).unwrap(), source);
}

#[test]
fn run_returns_error_after_reporting_skipped_files() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(temp.path().join("invalid.harn"), "fn bad() {\n").unwrap();
    let args = FixArgs {
        plan: true,
        apply: false,
        dry_run: false,
        safety: None,
        harness_threading: HarnessThreadingMode::default(),
        json: false,
        path: temp.path().to_path_buf(),
    };

    let error = run(&args).unwrap_err();

    assert!(error.is_partial_failure(), "unexpected error: {error}");
    assert!(
        error.message().contains("skipped 1 file")
            && error.message().contains("read, lex, or parse errors"),
        "unexpected error: {error}"
    );
}

#[test]
fn apply_skips_repairs_above_safety_ceiling() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("repair_demo.harn");
    let source =
        "pipeline main() { const count = 1; const greeting = \"hello \" + count; greeting }\n";
    fs::write(&script, source).unwrap();

    let result = apply_repairs(&script, RepairSafety::FormatOnly, false).unwrap();

    assert!(result.applied.is_empty(), "{result:#?}");
    assert!(
        result.skipped.iter().any(|skipped| {
            skipped.repair_id == "style/string-interpolation"
                && skipped.reason == "above_safety_ceiling"
        }),
        "{result:#?}"
    );
    assert_eq!(fs::read_to_string(&script).unwrap(), source);
}

#[test]
fn apply_rejects_needs_human_safety_ceiling() {
    let args = FixArgs {
        plan: false,
        apply: true,
        dry_run: false,
        safety: Some(RepairSafety::NeedsHuman),
        harness_threading: HarnessThreadingMode::default(),
        json: false,
        path: PathBuf::from("repair_demo.harn"),
    };

    let error = run(&args).unwrap_err();
    assert!(
        error.message().contains("needs-human")
            && error.message().contains("--plan --json")
            && !error.is_partial_failure(),
        "unexpected error: {error}"
    );
}

#[test]
fn plan_uses_global_harness_for_stdio_repairs_by_default() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("stdio_threading.harn");
    fs::write(
        &script,
        "fn helper() {\n  println(\"hi\")\n}\n\nfn main(harness: Harness) {\n  helper()\n}\n",
    )
    .unwrap();

    let plan = build_plan(&script, None).unwrap();
    let repair = plan
        .repairs
        .iter()
        .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
        .expect("ambient stdio repair should be present");

    assert_eq!(repair.repair.id, "bindings/use-enclosing-harness-global");
    assert_eq!(repair.repair.safety, "scope-local");
    assert_eq!(repair.impact.classification, "local-ambient-rewrite");
    assert!(repair.impact.signature_changes.is_empty());
    let replacements = repair
        .edits
        .iter()
        .map(|edit| edit.replacement.as_str())
        .collect::<Vec<_>>();
    assert!(
        replacements.contains(&"harness.stdio.println"),
        "expected direct call rewrite in edits: {replacements:?}"
    );
    assert!(
        !replacements
            .iter()
            .any(|replacement| replacement.contains("Harness")),
        "default repair should not change helper signatures: {replacements:?}"
    );
}

#[test]
fn plan_marks_stdio_repairs_surface_changing_when_harness_is_unreachable() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("stdio_needs_param.harn");
    fs::write(&script, "pub fn helper() {\n  println(\"hi\")\n}\n").unwrap();

    let plan = build_plan_with_options(
        &script,
        None,
        FixOptions {
            harness_threading: HarnessThreadingMode::ThreadParams,
        },
    )
    .unwrap();
    let repair = plan
        .repairs
        .iter()
        .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
        .expect("ambient stdio repair should be present");

    assert_eq!(repair.repair.id, "bindings/thread-harness-needs-param");
    assert_eq!(repair.repair.safety, "surface-changing");
    assert_eq!(repair.impact.classification, "public-signature-change");
}

#[test]
fn plan_json_reports_cross_module_public_signature_impact() {
    let temp = tempfile::TempDir::new().unwrap();
    let lib = temp.path().join("lib.harn");
    let entry = temp.path().join("main.harn");
    fs::write(
        &lib,
        "pub fn host_write_file(path: string, body: string) {\n  write_file(path, body)\n}\n",
    )
    .unwrap();
    fs::write(
            &entry,
            "import \"./lib\"\n\nfn main(harness: Harness) {\n  host_write_file(\"out.txt\", \"hi\")\n}\n",
        )
        .unwrap();

    let plan = build_plan_with_options(
        temp.path(),
        None,
        FixOptions {
            harness_threading: HarnessThreadingMode::ThreadParams,
        },
    )
    .unwrap();
    let repair_index = plan
        .repairs
        .iter()
        .position(|repair| {
            repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                && repair
                    .edits
                    .iter()
                    .any(|edit| edit.replacement == "harness: Harness, ")
        })
        .expect("public fs repair should be present");
    let repair = &plan.repairs[repair_index];

    assert_eq!(plan.harness_threading, "thread-params");
    assert_eq!(repair.impact.classification, "public-signature-change");
    assert!(repair.impact.requires_cross_module_caller_updates);
    assert_eq!(
        repair.impact.signature_changes,
        vec![SignatureChangeWire {
            callable: "host_write_file".to_string(),
            is_exported: true,
            is_entrypoint: false,
        }]
    );
    assert!(
        repair
            .impact
            .notes
            .iter()
            .any(|note| note.contains("cross-module callers must be updated")),
        "{repair:#?}"
    );

    let encoded = serde_json::to_value(&plan).unwrap();
    assert_eq!(
        encoded["repairs"][repair_index]["impact"]["classification"],
        "public-signature-change"
    );
}

#[test]
fn apply_thread_params_threads_harness_for_stdio_migration() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("stdio_apply.harn");
    fs::write(
        &script,
        "pub fn helper() {\n  println(\"hi\")\n}\n\nfn main(harness: Harness) {\n  helper()\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            harness_threading: HarnessThreadingMode::ThreadParams,
        },
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-needs-param"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("fn helper(harness: Harness)"),
        "expected helper to gain a harness parameter: {updated}"
    );
    assert!(
        updated.contains("helper(harness)"),
        "expected main to thread harness into helper: {updated}"
    );
    assert!(
        updated.contains("harness.stdio.println(\"hi\")"),
        "expected ambient stdio call to migrate: {updated}"
    );
}

#[test]
fn apply_thread_params_threads_harness_for_non_stdio_capabilities() {
    let cases = [
        (
            "clock_apply.harn",
            Code::LintAmbientClockBuiltin,
            "const value = now_ms()",
            "harness.clock.now_ms()",
        ),
        (
            "fs_apply.harn",
            Code::LintAmbientFsBuiltin,
            "const value = read_file(\"notes.txt\")",
            "harness.fs.read_text(\"notes.txt\")",
        ),
        (
            "env_apply.harn",
            Code::LintAmbientEnvBuiltin,
            "const value = env_or(\"MODE\", \"dev\")",
            "harness.env.get_or(\"MODE\", \"dev\")",
        ),
        (
            "random_apply.harn",
            Code::LintAmbientRandomBuiltin,
            "const value = random_int(0, 10)",
            "harness.random.gen_range(0, 10)",
        ),
        (
            "net_apply.harn",
            Code::LintAmbientNetBuiltin,
            "const value = http_get(\"https://example.test\")",
            "harness.net.get(\"https://example.test\")",
        ),
    ];

    for (filename, code, ambient_line, migrated_call) in cases {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join(filename);
        fs::write(
                &script,
                format!(
                    "fn helper() {{\n  {ambient_line}\n  value\n}}\n\nfn main(harness: Harness) {{\n  helper()\n}}\n"
                ),
            )
            .unwrap();

        let result = apply_repairs_with_options(
            &script,
            RepairSafety::SurfaceChanging,
            false,
            FixOptions {
                harness_threading: HarnessThreadingMode::ThreadParams,
            },
        )
        .unwrap();
        assert!(
            result.applied.iter().any(|repair| {
                repair.diagnostic_code == code.to_string()
                    && repair.repair_id.starts_with("bindings/thread-harness")
            }),
            "{filename}: {result:#?}"
        );

        let updated = fs::read_to_string(&script).unwrap();
        assert!(
            updated.contains("fn helper(harness: Harness)"),
            "{filename}: expected helper to gain a harness parameter: {updated}"
        );
        assert!(
            updated.contains("helper(harness)"),
            "{filename}: expected main to thread harness into helper: {updated}"
        );
        assert!(
            updated.contains(migrated_call),
            "{filename}: expected ambient call to migrate to {migrated_call}: {updated}"
        );
    }
}

#[test]
fn apply_scope_local_rewrites_ambient_calls_inside_pipeline() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("pipeline_direct.harn");
    fs::write(
        &script,
        "pipeline default() {\n  println(\"hi\")\n  const home = env_or(\"HOME\", \"\")\n}\n",
    )
    .unwrap();

    let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness"
        }),
        "{result:#?}"
    );
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientEnvBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-env"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pipeline default()"),
        "pipeline signature should remain stable: {updated}"
    );
    assert!(
        updated.contains("harness.stdio.println(\"hi\")"),
        "expected stdio call to use the pipeline harness global: {updated}"
    );
    assert!(
        updated.contains("harness.env.get_or(\"HOME\", \"\")"),
        "expected env call to use the pipeline harness global: {updated}"
    );
}

#[test]
fn apply_thread_params_threads_harness_from_pipeline_to_helper() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("pipeline_helper.harn");
    fs::write(
        &script,
        "pub fn helper() {\n  println(\"hi\")\n}\n\npipeline default() {\n  helper()\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            harness_threading: HarnessThreadingMode::ThreadParams,
        },
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-needs-param"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("fn helper(harness: Harness)"),
        "expected helper to gain a harness parameter: {updated}"
    );
    assert!(
        updated.contains("helper(harness)"),
        "expected pipeline to pass its harness global into helper: {updated}"
    );
    assert!(
        updated.contains("harness.stdio.println(\"hi\")"),
        "expected ambient stdio call to migrate: {updated}"
    );
}

#[test]
fn apply_scope_local_preserves_stdlib_public_signature_with_global_harness() {
    let temp = tempfile::TempDir::new().unwrap();
    let stdlib_dir = temp.path().join("crates/harn-stdlib/src/stdlib");
    fs::create_dir_all(&stdlib_dir).unwrap();
    let script = stdlib_dir.join("public_helper.harn");
    fs::write(
            &script,
            "/**\n * Public API.\n *\n * @effects: []\n * @errors: []\n */\npub fn helper(path: string) {\n  return read_file(path)\n}\n\npipeline default() {\n  helper(\"notes.txt\")\n}\n",
        )
        .unwrap();

    let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                && repair.repair_id == "bindings/use-enclosing-harness-global"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pub fn helper(path: string)"),
        "public signature should remain stable: {updated}"
    );
    assert!(
        updated.contains("return harness.fs.read_text(path)"),
        "public function internals should use the VM harness global: {updated}"
    );
    assert!(
        updated.contains("helper(\"notes.txt\")"),
        "callers should not receive an inserted harness argument: {updated}"
    );
}

#[test]
fn apply_default_preserves_non_stdlib_public_signature_with_global_harness() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("public_calls_private.harn");
    fs::write(
            &script,
            "/** Public API. */\npub fn load(path: string) {\n  return load_inner(path)\n}\n\nfn load_inner(path: string) {\n  return read_file(path)\n}\n\npipeline default() {\n  load(\"notes.txt\")\n}\n",
        )
        .unwrap();

    let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                && repair.repair_id == "bindings/use-enclosing-harness-global"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pub fn load(path: string)"),
        "public signature should remain stable: {updated}"
    );
    assert!(
        updated.contains("fn load_inner(path: string)"),
        "private helper signature should remain stable in local-global mode: {updated}"
    );
    assert!(
        updated.contains("return harness.fs.read_text(path)"),
        "private helper should use the VM harness global: {updated}"
    );
    assert!(
        updated.contains("load(\"notes.txt\")"),
        "callers should not receive an inserted harness argument: {updated}"
    );
}

#[test]
fn apply_surface_changing_threads_non_stdlib_public_api() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("public_calls_private.harn");
    fs::write(
            &script,
            "/** Public API. */\npub fn load(path: string) {\n  return load_inner(path)\n}\n\nfn load_inner(path: string) {\n  return read_file(path)\n}\n\npipeline default() {\n  load(\"notes.txt\")\n}\n",
        )
        .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            harness_threading: HarnessThreadingMode::ThreadParams,
        },
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-needs-param"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pub fn load(harness: Harness, path: string)"),
        "non-stdlib public API should gain an explicit harness parameter: {updated}"
    );
    assert!(
        updated.contains("return load_inner(harness, path)"),
        "public caller should thread its explicit harness parameter: {updated}"
    );
    assert!(
        updated.contains("fn load_inner(harness: Harness, path: string)"),
        "private helper should receive an explicit harness: {updated}"
    );
    assert!(
        updated.contains("return harness.fs.read_text(path)"),
        "private helper should migrate ambient fs call: {updated}"
    );
    assert!(
        updated.contains("load(harness, \"notes.txt\")"),
        "pipeline caller should pass the runtime harness into the public API: {updated}"
    );
}

#[test]
fn apply_dedupes_shared_stdio_threading_edits() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("stdio_shared.harn");
    fs::write(
            &script,
            "pub fn leaf_a() {\n  println(\"a\")\n}\n\npub fn leaf_b() {\n  println(\"b\")\n}\n\npub fn middle() {\n  leaf_a()\n  leaf_b()\n}\n\nfn main(harness: Harness) {\n  middle()\n}\n",
        )
        .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            harness_threading: HarnessThreadingMode::ThreadParams,
        },
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-needs-param"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("fn middle(harness: Harness)"),
        "expected middle to receive exactly one harness parameter: {updated}"
    );
    assert!(
        !updated.contains("fn middle(harness: Harness, harness: Harness"),
        "shared threading edits should not duplicate params: {updated}"
    );
    assert!(
        updated.contains("leaf_a(harness)") && updated.contains("leaf_b(harness)"),
        "expected both leaf calls to receive harness: {updated}"
    );
}

#[test]
fn plan_uses_underscore_harness_when_harness_name_is_taken() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("stdio_taken_name.harn");
    fs::write(
        &script,
        "fn helper(harness: string) {\n  println(harness)\n}\n",
    )
    .unwrap();

    let plan = build_plan(&script, None).unwrap();
    let repair = plan
        .repairs
        .iter()
        .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
        .expect("ambient stdio repair should be present");

    let replacements = repair
        .edits
        .iter()
        .map(|edit| edit.replacement.as_str())
        .collect::<Vec<_>>();
    assert!(
        replacements.contains(&"_harness: Harness, "),
        "expected inserted capability parameter to avoid duplicate `harness`: {replacements:?}"
    );
    assert!(
        replacements.contains(&"_harness.stdio.println"),
        "expected call rewrite to use the inserted capability parameter: {replacements:?}"
    );
}
