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

    let plan = build_plan_with_options(
        &script,
        None,
        FixOptions {
            capability_migrations_only: false,
        },
    )
    .unwrap();
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
fn callable_param_insert_handles_dict_defaults_before_body() {
    let source = "pub fn poll(check, options: dict = {}) -> any {\n  harness.clock.now_ms()\n}\n";
    let (offset, has_params) = super::signature_threading::callable_param_insert(
        source,
        harn_lexer::Span::with_offsets(0, source.len(), 1, 1),
    )
    .expect("callable header");
    assert!(has_params);
    assert_eq!(&source[offset..offset + 5], "check");
}

#[test]
fn missing_capability_argument_repair_uses_typed_root_field() {
    let source = "fn main(harness: Harness) {\n  helper(\"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let span = source.find("\"old\"").unwrap();
    let span = harn_lexer::Span::with_offsets(span, span + 5, 2, 10);
    let (_, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &named_type("HarnessFs"),
        &named_type("string"),
        source,
        &program,
    )
    .expect("capability migration repair");
    assert_eq!(edits.len(), 1);
    let insert_at = source.find("(\"old\")").unwrap() + 1;
    assert_eq!(edits[0].span.start, insert_at);
    assert_eq!(edits[0].span.end, insert_at);
    assert_eq!(edits[0].replacement, "harness.fs, ");
}

#[test]
fn missing_capability_argument_is_inserted_at_the_diagnosed_position() {
    let source = "fn main(harness: Harness) {\n  helper(harness.fs, \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("\"old\"").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + 5, 2, 22);
    let (_, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &named_type("HarnessSystem"),
        &named_type("string"),
        source,
        &program,
    )
    .expect("positioned capability repair");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].replacement, ", harness.system");
    assert_eq!(
        FixEdit::apply_all(source, &edits),
        "fn main(harness: Harness) {\n  helper(harness.fs, harness.system, \"old\")\n}\n"
    );
}

#[test]
fn attenuated_capability_argument_repair_projects_existing_root_grant() {
    let source = "fn main(harness: Harness) {\n  helper(harness, \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("harness, \"").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + "harness".len(), 2, 10);
    let (repair, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &named_type("HarnessFs"),
        &named_type("Harness"),
        source,
        &program,
    )
    .expect("attenuation repair");
    assert_eq!(repair.id.as_str(), "bindings/attenuate-capability-argument");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].span, span);
    assert_eq!(edits[0].replacement, "harness.fs");
}

#[test]
fn attenuated_capability_bundle_repair_projects_existing_root_grant() {
    let source = "fn main(harness: Harness) {\n  helper(harness, \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("harness, \"").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + "harness".len(), 2, 10);
    let (repair, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &capability_shape(&[("fs", "HarnessFs"), ("tools", "HarnessTools")]),
        &named_type("Harness"),
        source,
        &program,
    )
    .expect("capability bundle repair");
    assert_eq!(
        repair.id.as_str(),
        "bindings/attenuate-capability-bundle-argument"
    );
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].span, span);
    assert_eq!(
        edits[0].replacement,
        "{fs: harness.fs, tools: harness.tools}"
    );
}

#[test]
fn missing_capability_argument_repair_inserts_before_parenthesized_expression() {
    let source = "fn main(harness: Harness) {\n  helper((params ?? {}) + {path: \"src\"})\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let mut span = None;
    visit::walk_program(&program, &mut |node| {
        if let Node::FunctionCall { name, args, .. } = &node.node {
            if name == "helper" {
                span = args.first().map(|arg| arg.span);
            }
        }
    });
    let span = span.expect("helper first argument");

    let (_, edits, _) = synthesize_missing_capability_argument_repair(
        span,
        &named_type("HarnessFs"),
        &named_type("dict"),
        source,
        &program,
    )
    .expect("capability migration repair");
    let insert_at = source.find("((params").unwrap() + 1;
    assert_eq!(edits[0].span.start, insert_at);
    assert_eq!(edits[0].replacement, "harness.fs, ");
}

#[test]
fn missing_root_argument_threads_a_distinct_root_past_a_narrow_harness_binding() {
    let source = "fn leaf(harness: HarnessFs, path: string) {\n  needs_root(harness, path)\n}\n\nfn main(harness: Harness) {\n  leaf(harness.fs, \"old\")\n}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("harness, path").unwrap();
    let span = harn_lexer::Span::with_offsets(start, start + "harness".len(), 2, 14);
    let (_, edits, _) = synthesize_missing_root_argument_repair(
        span,
        source,
        &program,
        &BTreeSet::new(),
        &AmbientRepairContext {
            cross_module_importer_count: 0,
        },
    )
    .expect("root threading repair");
    let fixed = FixEdit::apply_all(source, &edits);
    assert_eq!(
        fixed,
        "fn leaf(_harness: Harness, harness: HarnessFs, path: string) {\n  needs_root(_harness, harness, path)\n}\n\nfn main(harness: Harness) {\n  leaf(harness, harness.fs, \"old\")\n}\n"
    );
}

#[test]
fn missing_capability_argument_repair_rejects_non_call_spans() {
    let source = "import { helper } from \"./lib\"\n\nfn main(harness: Harness) {}\n";
    let program = harn_parser::parse_source(source).unwrap();
    let start = source.find("helper").unwrap();
    let import_span = harn_lexer::Span::with_offsets(start, start + "helper".len(), 1, 10);

    assert!(
        synthesize_missing_capability_argument_repair(
            import_span,
            &named_type("HarnessAst"),
            &named_type("string"),
            source,
            &program,
        )
        .is_none(),
        "an imported-declaration diagnostic must never edit the importer"
    );
}

#[test]
fn missing_root_argument_repair_preserves_parenthesized_first_argument() {
    // An expression span excludes its grouping parentheses, so a repair that
    // inserted at the diagnosed offset would produce `((harness, params ...`.
    let source = "pipeline test(harness: Harness, params: dict) {\n  host_search_request((params ?? {}) + {path: \"root\"})\n}\n";
    let program = harn_parser::parse_source(source).expect("source parses");
    let mut span = None;
    visit::walk_program(&program, &mut |node| {
        if let Node::FunctionCall { name, args, .. } = &node.node {
            if name == "host_search_request" {
                span = args.first().map(|arg| arg.span);
            }
        }
    });
    let span = span.expect("host_search_request first argument");

    let (_, edits, _) = synthesize_missing_root_argument_repair(
        span,
        source,
        &program,
        &BTreeSet::new(),
        &AmbientRepairContext {
            cross_module_importer_count: 0,
        },
    )
    .expect("root argument repair");

    let applied = FixEdit::apply_all(source, &edits);
    assert!(
        applied.contains("host_search_request(harness, (params ?? {}) + {path: \"root\"})"),
        "capability must be inserted at the call boundary: {applied}"
    );
    harn_parser::parse_source(&applied).expect("repair must remain parse-safe");
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
    let plan = build_plan_with_options(
        &script,
        None,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(!plan.repairs.is_empty());
    assert!(plan.repairs.iter().all(|repair| repair
        .repair
        .id
        .starts_with("bindings/thread-harness")
        || repair.repair.id == "bindings/attenuate-harness"));
    assert!(plan
        .repairs
        .iter()
        .any(|repair| repair.repair.id == "bindings/attenuate-harness"));
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
            capability_migrations_only: false,
        },
    )
    .unwrap();
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

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: false,
        },
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

        let result = apply_repairs_with_options(
            &script,
            RepairSafety::SurfaceChanging,
            false,
            FixOptions {
                capability_migrations_only: false,
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

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
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

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
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
        updated.contains("fn caps(harness: HarnessLlm)"),
        "{updated}"
    );
    assert!(updated.contains("caps(harness.llm)"), "{updated}");
    assert!(
        updated.contains("harness.provider_capabilities(\"anthropic\", \"claude-opus-4-7\")"),
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

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: false,
        },
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

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: false,
        },
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

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
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
        updated.contains(
            "pub fn resolve(harness: HarnessFs, path: string, base: string = harness.cwd())"
        ),
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

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
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
        updated.contains("harness.metadata_get({dir: dir, namespace: \"classification\"})"),
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

    apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
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

    apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
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

    apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
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

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: false,
        },
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
