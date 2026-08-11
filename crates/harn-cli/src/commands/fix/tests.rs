use super::*;
use std::fs;
use std::path::PathBuf;

fn named_type(name: &str) -> TypeExpr {
    TypeExpr::Named(name.to_string())
}

fn capability_shape(fields: &[(&str, &str)]) -> TypeExpr {
    TypeExpr::Shape(
        fields
            .iter()
            .map(|(field, type_name)| {
                harn_parser::ShapeField::synthetic(*field, named_type(type_name), false)
            })
            .collect(),
    )
}

fn candidate(file: &str, start: usize, end: usize) -> RepairCandidate {
    RepairCandidate {
        file: file.to_string(),
        source: "typecheck",
        severity: "warning",
        code: Code::FormatterWouldReformat,
        message: "test".to_string(),
        unresolved_name: None,
        expected_type: None,
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
fn file_edits_compose_projection_before_same_offset_insertion() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    fs::write(temp.path(), "call(harness, value)\n").unwrap();
    let start = "call(".len();
    apply_file_edits(
        temp.path(),
        &[
            FixEditWire {
                span: SpanWire::from(Span::with_offsets(start, start, 1, start + 1)),
                replacement: "harness, ".to_string(),
            },
            FixEditWire {
                span: SpanWire::from(Span::with_offsets(
                    start,
                    start + "harness".len(),
                    1,
                    start + 1,
                )),
                replacement: "harness.fs".to_string(),
            },
        ],
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(temp.path()).unwrap(),
        "call(harness, harness.fs, value)\n"
    );
}

#[test]
fn file_edits_refuse_ambiguous_overlap_without_writing() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let source = "resolve_trial_path(harness, params)\n";
    fs::write(temp.path(), source).unwrap();
    let call_start = source.find("harness").unwrap();
    let error = apply_file_edits(
        temp.path(),
        &[
            FixEditWire {
                span: SpanWire::from(Span::with_offsets(
                    call_start,
                    call_start + "harness".len(),
                    1,
                    call_start + 1,
                )),
                replacement: "{fs: harness.fs, env: harness.env}".to_string(),
            },
            FixEditWire {
                span: SpanWire::from(Span::with_offsets(
                    call_start + 2,
                    call_start + 2,
                    1,
                    call_start + 3,
                )),
                replacement: "harness, ".to_string(),
            },
        ],
    )
    .expect_err("an insertion inside a replacement is ambiguous");
    assert!(
        error.contains("refusing to write an ambiguous candidate"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(temp.path()).unwrap(), source);
}

#[test]
fn capability_edits_validate_the_complete_candidate_before_writing() {
    let temp = tempfile::NamedTempFile::new().unwrap();
    let source = "fn main(harness: Harness) {\n  harness.fs.read(\"ok\")\n}\n";
    fs::write(temp.path(), source).unwrap();
    let start = source.find("harness.fs.read").unwrap();
    let error = apply_capability_file_edits(
        temp.path(),
        &[FixEditWire {
            span: SpanWire::from(Span::with_offsets(
                start,
                start + "harness.fs.read".len(),
                2,
                3,
            )),
            replacement: "harness.fs.{read".to_string(),
        }],
    )
    .expect_err("malformed migration output must be rejected");
    assert!(
        error.contains("failed to format capability migration output")
            || error.contains("capability migration produced invalid syntax"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(temp.path()).unwrap(), source);
}

#[test]
fn rollback_restores_every_snapshotted_file() {
    let temp = tempfile::TempDir::new().unwrap();
    let first = temp.path().join("first.harn");
    let second = temp.path().join("second.harn");
    fs::write(&first, "first\n").unwrap();
    fs::write(&second, "second\n").unwrap();
    let originals = BTreeMap::from([
        (first.to_string_lossy().into_owned(), "first\n".to_string()),
        (
            second.to_string_lossy().into_owned(),
            "second\n".to_string(),
        ),
    ]);
    fs::write(&first, "changed first\n").unwrap();
    fs::write(&second, "changed second\n").unwrap();

    let error = finish_with_rollback::<()>(Err("later pass failed".to_string()), false, &originals)
        .expect_err("the original failure remains visible after rollback");

    assert_eq!(error, "later pass failed");
    assert_eq!(fs::read_to_string(first).unwrap(), "first\n");
    assert_eq!(fs::read_to_string(second).unwrap(), "second\n");
}

#[test]
fn plan_reports_repairable_diagnostics_without_writing() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("repair_demo.harn");
    let source =
        "pipeline main(harness: Harness) { const count = 1; const greeting = \"hello \" + count; greeting }\n";
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
fn behavior_preserving_apply_keeps_unused_parameter_arity() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("generated_capability.harn");
    fs::write(
        &script,
        "fn probe(tools: HarnessTools, git) { return git([\"status\"]) }\n",
    )
    .unwrap();

    let result = apply_repairs(&script, RepairSafety::BehaviorPreserving, false).unwrap();
    assert!(
        result
            .applied
            .iter()
            .any(|repair| repair.repair_id == "bindings/rename-unused"),
        "the safe repair pass must consume migration fallout: {result:#?}"
    );
    assert_eq!(
        fs::read_to_string(script).unwrap(),
        "fn probe(_tools: HarnessTools, git) { return git([\"status\"]) }\n",
        "underscore prefixing must preserve the parameter's positional slot",
    );
}

#[test]
fn plan_skips_invalid_files_and_keeps_repairing_valid_files() {
    let temp = tempfile::TempDir::new().unwrap();
    let valid = temp.path().join("valid.harn");
    let invalid = temp.path().join("invalid.harn");
    fs::write(
        &valid,
        "pipeline main(harness: Harness) { const count = 1; const greeting = \"hello \" + count; greeting }\n",
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
        "pipeline main(harness: Harness) { const count = 1; const greeting = \"hello \" + count; greeting }\n",
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
        "pipeline main(harness: Harness) { const count = 1; const greeting = \"hello \" + count; greeting }\n",
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
        "pipeline main(harness: Harness) { const count = 1; const greeting = \"hello \" + count; greeting }\n";
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
        capability_migrations_only: false,
        codes: Vec::new(),
        json: false,
        workspace: false,
        paths: vec![temp.path().to_path_buf()],
    };

    let error = run(&args).unwrap_err();

    assert!(error.is_partial_failure(), "unexpected error: {error}");
    assert!(
        error.message().contains("skipped 1 file")
            && error.message().contains("read, lex, or parse errors"),
        "unexpected error: {error}"
    );
}

/// Two sibling trees migrate in one pass without their common ancestor.
///
/// A capability migration propagates requirements across resolved module
/// imports, so a declaration and its cross-module callers have to be planned
/// together or the callers are left stale. When this accepted one path, the
/// only way to reach two sibling trees was to name the directory above them --
/// which also sweeps in whatever else lives there. A repo that checks in
/// deliberately-invalid parse fixtures (Harn's own `conformance/errors/` is
/// exactly this) could therefore never run the migration at all: naming the
/// ancestor failed on the fixtures, and naming each tree separately could not
/// converge.
#[test]
fn plan_accepts_sibling_targets_without_their_common_ancestor() {
    let temp = tempfile::TempDir::new().unwrap();
    let lib = temp.path().join("lib");
    let app = temp.path().join("app");
    fs::create_dir(&lib).unwrap();
    fs::create_dir(&app).unwrap();
    fs::write(
        lib.join("greet.harn"),
        "pub fn greet() -> nil { __io_println(\"hi\") }\n",
    )
    .unwrap();
    fs::write(
        app.join("main.harn"),
        "import { greet } from \"../lib/greet\"\npipeline main(harness: Harness) { greet() }\n",
    )
    .unwrap();
    // The negative fixture that made the ancestor unusable. It sits beside both
    // targets and must not be reached.
    fs::write(temp.path().join("unparseable.harn"), "fn bad() {\n").unwrap();

    let plan = build_plan_with_options(&[lib, app], None, &FixOptions::default()).unwrap();

    assert!(
        plan.skipped_files.is_empty(),
        "a sibling fixture outside the named targets must not be collected: {:?}",
        plan.skipped_files
    );
    assert!(
        plan.path.contains("lib") && plan.path.contains("app"),
        "the plan must name both targets, got {:?}",
        plan.path
    );

    // Falsifier: the ancestor still fails, so the test above is measuring the
    // target list and not some unrelated change in how fixtures are read.
    let ancestor = build_plan_with_options(
        std::slice::from_ref(&temp.path().to_path_buf()),
        None,
        &FixOptions::default(),
    )
    .unwrap();
    assert_eq!(
        ancestor.skipped_files.len(),
        1,
        "naming the common ancestor must still collect the unparseable fixture: {:?}",
        ancestor.skipped_files
    );
}

#[test]
fn apply_skips_repairs_above_safety_ceiling() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("repair_demo.harn");
    let source =
        "pipeline main(harness: Harness) { const count = 1; const greeting = \"hello \" + count; greeting }\n";
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
        capability_migrations_only: false,
        codes: Vec::new(),
        json: false,
        workspace: false,
        paths: vec![PathBuf::from("repair_demo.harn")],
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
fn plan_threads_explicit_harness_for_stdio_repairs() {
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

    assert_eq!(repair.repair.id, "bindings/thread-harness-whole-program");
    assert_eq!(repair.repair.safety, "scope-local");
    assert_eq!(repair.impact.classification, "local-signature-threading");
    assert!(!repair.impact.signature_changes.is_empty());
    let replacements = repair
        .edits
        .iter()
        .map(|edit| edit.replacement.as_str())
        .collect::<Vec<_>>();
    assert!(
        replacements.contains(&"harness.println"),
        "expected direct call rewrite in edits: {replacements:?}"
    );
    assert!(
        replacements.contains(&"harness: HarnessStdio"),
        "{replacements:?}"
    );
}

#[test]
fn plan_marks_stdio_repairs_surface_changing_when_harness_is_unreachable() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("stdio_needs_param.harn");
    fs::write(&script, "pub fn helper() {\n  println(\"hi\")\n}\n").unwrap();

    let plan = build_plan_with_options_at(&script, None, &FixOptions::default()).unwrap();
    let repair = plan
        .repairs
        .iter()
        .find(|repair| repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string())
        .expect("ambient stdio repair should be present");

    assert_eq!(repair.repair.id, "bindings/thread-harness-whole-program");
    assert_eq!(repair.repair.safety, "surface-changing");
    assert_eq!(repair.impact.classification, "public-signature-change");
}

#[test]
fn source_coordinates_map_unicode_columns_to_byte_offsets() {
    let source = "\u{3b1}\u{3b2}\n  call(\"value\")\n";
    let expected = source.find("\"value\"").expect("first argument");
    assert_eq!(
        harn_lexer::byte_offset_for_position(source, 2, 8),
        Some(expected)
    );
}

#[test]
fn capability_only_plan_excludes_unrelated_repairs() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("capability_only.harn");
    fs::write(
        &script,
        "pub const EXPORTED = 1\n\npub fn helper() {\n  println(\"hi\")\n}\n\nfn load(harness: Harness) {\n  return harness.fs.cwd()\n}\n",
    )
    .unwrap();
    let plan =
        build_plan_with_options_at(&script, None, &FixOptions::capability_migrations()).unwrap();
    assert!(!plan.repairs.is_empty());
    assert!(plan
        .repairs
        .iter()
        .all(|repair| super::repair_classes::is_capability_migration_repair_id(&repair.repair.id)));
    assert!(
        plan.repairs.iter().all(|repair| !repair.edits.is_empty()),
        "capability-only plans must contain only executable repairs: {plan:#?}"
    );
}

#[test]
fn capability_pass_rejects_stale_offsets_before_writing_any_file() {
    let temp = tempfile::TempDir::new().unwrap();
    let valid = temp.path().join("valid.harn");
    let stale = temp.path().join("stale.harn");
    fs::write(&valid, "fn valid() -> nil { return nil }\n").unwrap();
    fs::write(&stale, "fn pin(variant: string?) -> nil { return nil }\n").unwrap();
    let original_valid = fs::read_to_string(&valid).unwrap();
    let original_stale = fs::read_to_string(&stale).unwrap();

    let edits = BTreeMap::from([
        (
            valid.display().to_string(),
            vec![FixEditWire {
                span: SpanWire {
                    start: 3,
                    end: 8,
                    line: 1,
                    column: 4,
                    end_line: 1,
                },
                replacement: "still_valid".to_string(),
            }],
        ),
        (
            stale.display().to_string(),
            vec![FixEditWire {
                // Twelve-byte drift from a removed `, with_mocks` import:
                // the intended parameter insertion now lands inside `string`.
                span: SpanWire {
                    start: 20,
                    end: 20,
                    line: 1,
                    column: 21,
                    end_line: 1,
                },
                replacement: "harness: Harness, ".to_string(),
            }],
        ),
    ]);

    let error = render_capability_migration_pass(&edits)
        .expect_err("a stale byte offset must reject the complete pass");
    assert!(
        error.contains("no files from this pass were written"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(valid).unwrap(), original_valid);
    assert_eq!(fs::read_to_string(stale).unwrap(), original_stale);
}

/// A call missing several capability carriers draws one repair per missing
/// argument and one whole-program repair supplying all of them, so the same
/// offset receives both `harness.env, ` and `harness.env, harness.fs, `. Those
/// are the same carriers in the same parameter order, and rejecting them as
/// ambiguous aborts the whole pass — one multi-carrier callee then blocks the
/// migration of every other file in the tree.
#[test]
fn a_multi_carrier_call_keeps_the_complete_prepend_over_its_prefix() {
    let insertion = |replacement: &str| FixEditWire {
        span: SpanWire {
            start: 46,
            end: 46,
            line: 2,
            column: 10,
            end_line: 2,
        },
        replacement: replacement.to_string(),
    };
    let collapsed = dedupe_wire_edits(&[
        insertion("harness.env, "),
        insertion("harness.env, harness.fs, "),
    ]);

    assert_eq!(
        collapsed
            .iter()
            .map(|edit| edit.replacement.as_str())
            .collect::<Vec<_>>(),
        vec!["harness.env, harness.fs, "],
        "the prefix insertion is subsumed, not a competing candidate"
    );
}

/// Two insertions at one offset that are not in a prefix relation really are
/// competing fixes, and must still reject rather than silently pick one.
#[test]
fn a_genuinely_ambiguous_insertion_pair_still_rejects() {
    let insertion = |replacement: &str| FixEditWire {
        span: SpanWire {
            start: 12,
            end: 12,
            line: 1,
            column: 13,
            end_line: 1,
        },
        replacement: replacement.to_string(),
    };
    let collapsed = dedupe_wire_edits(&[insertion("harness.fs, "), insertion("harness.env, ")]);

    assert_eq!(collapsed.len(), 2, "neither replacement subsumes the other");
    apply::validate_edit_composition(Path::new("call.harn"), &collapsed)
        .expect_err("competing carriers at one offset stay ambiguous");
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

    let plan = build_plan_with_options_at(temp.path(), None, &FixOptions::default()).unwrap();
    let repair_index = plan
        .repairs
        .iter()
        .position(|repair| {
            repair
                .edits
                .iter()
                .any(|edit| edit.replacement == "harness: HarnessFs, ")
        })
        .expect("public fs repair should be present");
    let repair = &plan.repairs[repair_index];
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

    let result = apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::default(),
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-whole-program"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("fn helper(harness: HarnessStdio)"),
        "expected helper to gain a harness parameter: {updated}"
    );
    assert!(
        updated.contains("helper(harness.stdio)"),
        "expected main to thread harness into helper: {updated}"
    );
    assert!(
        updated.contains("harness.println(\"hi\")"),
        "expected ambient stdio call to migrate: {updated}"
    );
}

mod repair_synthesis;
mod split_capabilities;

#[test]
fn apply_thread_params_threads_harness_for_non_stdio_capabilities() {
    let cases = [
        (
            "clock_apply.harn",
            Code::LintAmbientClockBuiltin,
            "const value = now_ms()",
            "HarnessClock",
            "harness.clock",
            "harness.now_ms()",
        ),
        (
            "fs_apply.harn",
            Code::LintAmbientFsBuiltin,
            "const value = read_file(\"notes.txt\")",
            "HarnessFs",
            "harness.fs",
            "harness.read_text(\"notes.txt\")",
        ),
        (
            "env_apply.harn",
            Code::LintAmbientEnvBuiltin,
            "const value = env_or(\"MODE\", \"dev\")",
            "HarnessEnv",
            "harness.env",
            "harness.get_or(\"MODE\", \"dev\")",
        ),
        (
            "random_apply.harn",
            Code::LintAmbientRandomBuiltin,
            "const value = random_int(0, 10)",
            "HarnessRandom",
            "harness.random",
            "harness.range(0, 10)",
        ),
        (
            "net_apply.harn",
            Code::LintAmbientNetBuiltin,
            "const value = http_get(\"https://example.test\")",
            "HarnessNet",
            "harness.net",
            "harness.get(\"https://example.test\")",
        ),
    ];

    for (filename, code, ambient_line, capability_type, projected_argument, migrated_call) in cases
    {
        let temp = tempfile::TempDir::new().unwrap();
        let script = temp.path().join(filename);
        fs::write(
                &script,
                format!(
                    "fn helper() {{\n  {ambient_line}\n  value\n}}\n\nfn main(harness: Harness) {{\n  helper()\n}}\n"
                ),
            )
            .unwrap();

        let result = apply_repairs_with_options_at(
            &script,
            RepairSafety::SurfaceChanging,
            false,
            FixOptions::default(),
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
            updated.contains(&format!("fn helper(harness: {capability_type})")),
            "{filename}: expected helper to gain a harness parameter: {updated}"
        );
        assert!(
            updated.contains(&format!("helper({projected_argument})")),
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
        "pipeline default(harness: Harness) {\n  println(\"hi\")\n  const home = env_or(\"HOME\", \"\")\n}\n",
    )
    .unwrap();

    let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
    assert!(
        result
            .applied
            .iter()
            .any(|repair| { repair.repair_id == "bindings/thread-harness-whole-program" }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pipeline default(harness: Harness)"),
        "pipeline signature should remain stable: {updated}"
    );
    assert!(
        updated.contains("harness.stdio.println(\"hi\")"),
        "expected stdio call to use the pipeline harness argument: {updated}"
    );
    assert!(
        updated.contains("harness.env.get_or(\"HOME\", \"\")"),
        "expected env call to use the pipeline harness argument: {updated}"
    );
}

#[test]
fn apply_threads_missing_harness_into_pipeline_boundary() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("pipeline_missing_harness.harn");
    fs::write(&script, "pipeline default(task) {\n  println(task)\n}\n").unwrap();

    let result = apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-whole-program"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pipeline default(harness: Harness, task)"),
        "pipeline entrypoints must receive the root Harness first: {updated}"
    );
    assert!(
        updated.contains("harness.stdio.println(task)"),
        "pipeline ambient call should use the inserted Harness: {updated}"
    );
}

#[test]
fn apply_threads_registry_owned_harness_method_through_helper() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("provider_caps.harn");
    fs::write(
        &script,
        "fn caps() {\n  return provider_capabilities(\"anthropic\", \"claude-opus-4-7\")\n}\n\nfn main(harness: Harness) {\n  caps()\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientHarnessMethod.to_string()
                && repair.repair_id.starts_with("bindings/thread-harness")
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(updated.contains("fn caps(llm: HarnessLlm)"), "{updated}");
    assert!(updated.contains("caps(harness.llm)"), "{updated}");
    assert!(
        updated.contains("llm.provider_capabilities(\"anthropic\", \"claude-opus-4-7\")"),
        "{updated}"
    );
}

#[test]
fn apply_thread_params_threads_harness_from_pipeline_to_helper() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("pipeline_helper.harn");
    fs::write(
        &script,
        "pub fn helper() {\n  println(\"hi\")\n}\n\npipeline default(harness: Harness) {\n  helper()\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::default(),
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-whole-program"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("fn helper(harness: HarnessStdio)"),
        "expected helper to gain a harness parameter: {updated}"
    );
    assert!(
        updated.contains("helper(harness.stdio)"),
        "expected pipeline to pass its harness argument into helper: {updated}"
    );
    assert!(
        updated.contains("harness.println(\"hi\")"),
        "expected ambient stdio call to migrate: {updated}"
    );
}

#[test]
fn apply_scope_local_does_not_hide_stdlib_effects_behind_ambient_harness() {
    let temp = tempfile::TempDir::new().unwrap();
    let stdlib_dir = temp.path().join("crates/harn-stdlib/src/stdlib");
    fs::create_dir_all(&stdlib_dir).unwrap();
    let script = stdlib_dir.join("public_helper.harn");
    fs::write(
            &script,
            "/**\n * Public API.\n *\n * @effects: []\n * @errors: []\n */\npub fn helper(path: string) {\n  return read_file(path)\n}\n\npipeline default(harness: Harness) {\n  helper(\"notes.txt\")\n}\n",
        )
        .unwrap();

    let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
    assert!(
        result
            .applied
            .iter()
            .all(|repair| { repair.diagnostic_code != Code::LintAmbientFsBuiltin.to_string() }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pub fn helper(path: string)"),
        "surface-changing migration should remain unapplied: {updated}"
    );
    assert!(updated.contains("return read_file(path)"), "{updated}");
}

#[test]
fn apply_scope_local_does_not_hide_private_effects_behind_ambient_harness() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("public_calls_private.harn");
    fs::write(
            &script,
            "/** Public API. */\npub fn load(path: string) {\n  return load_inner(path)\n}\n\nfn load_inner(path: string) {\n  return read_file(path)\n}\n\npipeline default(harness: Harness) {\n  load(\"notes.txt\")\n}\n",
        )
        .unwrap();

    let result = apply_repairs(&script, RepairSafety::ScopeLocal, false).unwrap();
    assert!(
        result
            .applied
            .iter()
            .all(|repair| { repair.diagnostic_code != Code::LintAmbientFsBuiltin.to_string() }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pub fn load(path: string)"),
        "public signature should remain stable: {updated}"
    );
    assert!(
        updated.contains("fn load_inner(path: string)"),
        "surface-changing migration should remain unapplied: {updated}"
    );
    assert!(updated.contains("return read_file(path)"), "{updated}");
}

#[test]
fn apply_surface_changing_threads_non_stdlib_public_api() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("public_calls_private.harn");
    fs::write(
            &script,
            "/** Public API. */\npub fn load(path: string) {\n  return load_inner(path)\n}\n\nfn load_inner(path: string) {\n  return read_file(path)\n}\n\npipeline default(harness: Harness) {\n  load(\"notes.txt\")\n}\n",
        )
        .unwrap();

    let result = apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::default(),
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-whole-program"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pub fn load(harness: HarnessFs, path: string)"),
        "non-stdlib public API should gain an explicit harness parameter: {updated}"
    );
    assert!(
        updated.contains("return load_inner(harness, path)"),
        "public caller should thread its explicit harness parameter: {updated}"
    );
    assert!(
        updated.contains("fn load_inner(harness: HarnessFs, path: string)"),
        "private helper should receive an explicit harness: {updated}"
    );
    assert!(
        updated.contains("return harness.read_text(path)"),
        "private helper should migrate ambient fs call: {updated}"
    );
    assert!(
        updated.contains("load(harness.fs, \"notes.txt\")"),
        "pipeline caller should pass the runtime harness into the public API: {updated}"
    );
}

#[test]
fn apply_threads_ambient_capability_from_default_parameter() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("cwd_default.harn");
    fs::write(
        &script,
        "pub fn resolve(path: string, base: string = cwd()) -> string {\n  return base + path\n}\n\nfn main(harness: Harness) {\n  resolve(\"notes.txt\")\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientFsBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-whole-program"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("pub fn resolve(fs: HarnessFs, path: string, base: string = fs.cwd())"),
        "{updated}"
    );
    assert!(
        updated.contains("resolve(harness.fs, \"notes.txt\")"),
        "{updated}"
    );
}

#[test]
fn apply_rewrites_positional_metadata_builtin_to_typed_request() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("metadata_request.harn");
    fs::write(
        &script,
        "fn read_fact(dir: string) {\n  return metadata_get(dir, \"classification\")\n}\n\nfn main(harness: Harness) {\n  read_fact(\"src\")\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientHarnessMethod.to_string()
                && repair.repair_id.starts_with("bindings/thread-harness")
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("project.metadata_get({dir: dir, namespace: \"classification\"})"),
        "{updated}"
    );
    assert!(
        updated.contains("read_fact(harness.project, \"src\")"),
        "{updated}"
    );
}

#[test]
fn apply_rewrites_zero_and_optional_metadata_arguments_to_named_requests() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("metadata_optional_requests.harn");
    fs::write(
        &script,
        "fn main(harness: Harness) {\n  metadata_save()\n  metadata_entries()\n  metadata_status(\"classification\")\n}\n",
    )
    .unwrap();

    apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("harness.project.metadata_save({})"),
        "{updated}"
    );
    assert!(
        updated.contains("harness.project.metadata_entries({})"),
        "{updated}"
    );
    assert!(
        updated.contains("harness.project.metadata_status({namespace: \"classification\"})"),
        "{updated}"
    );
}

#[test]
fn apply_rewrites_legacy_host_projections_to_typed_snapshots() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("host_projections.harn");
    fs::write(
        &script,
        "fn describe() {\n  return platform() + \"-\" + arch() + home_dir()\n}\n\nfn main(harness: Harness) {\n  describe()\n}\n",
    )
    .unwrap();

    apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("harness.system.platform().os"),
        "{updated}"
    );
    assert!(
        updated.contains("harness.system.platform().arch"),
        "{updated}"
    );
    assert!(updated.contains("harness.fs.home_dir()"), "{updated}");
    assert!(
        updated.contains("describe({fs: harness.fs, system: harness.system})"),
        "{updated}"
    );
}

#[test]
fn apply_rewrites_ambient_calls_inside_interpolation() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("interpolation.harn");
    fs::write(
        &script,
        r#"fn main(harness: Harness) {
  const label = "host ${platform()} ${read_file("name.txt")}"
}
"#,
    )
    .unwrap();

    apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("${harness.system.platform().os}"),
        "{updated}"
    );
    assert!(
        updated.contains("${harness.fs.read_text(\"name.txt\")}"),
        "{updated}"
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

    let result = apply_repairs_with_options_at(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::default(),
    )
    .unwrap();
    assert!(
        result.applied.iter().any(|repair| {
            repair.diagnostic_code == Code::LintAmbientStdioBuiltin.to_string()
                && repair.repair_id == "bindings/thread-harness-whole-program"
        }),
        "{result:#?}"
    );

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("fn middle(harness: HarnessStdio)"),
        "expected middle to receive exactly one harness parameter: {updated}"
    );
    assert!(
        !updated.contains("fn middle(harness: HarnessStdio, harness: HarnessStdio"),
        "shared threading edits should not duplicate params: {updated}"
    );
    assert!(
        updated.contains("leaf_a(harness)") && updated.contains("leaf_b(harness)"),
        "expected both leaf calls to receive harness: {updated}"
    );
    assert!(updated.contains("middle(harness.stdio)"), "{updated}");
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
        replacements.contains(&"_harness: HarnessStdio, "),
        "expected inserted capability parameter to avoid duplicate `harness`: {replacements:?}"
    );
    assert!(
        replacements.contains(&"_harness.println"),
        "expected call rewrite to use the inserted capability parameter: {replacements:?}"
    );
}
