//! Tests for the edit-application layer: how planned edits are grouped per
//! file, and what `--apply` refuses to write.
//!
//! These sit apart from `tests.rs` because they exercise `apply.rs` directly
//! rather than the planner, and because a repair that writes unparseable
//! source is the one failure the rest of the suite cannot observe (#6148).

use super::*;
use std::fs;

/// `--code` narrows the plan to one diagnostic, so a targeted migration does
/// not drag in every other repair at the same safety class. Without it,
/// applying one rename to a file also rewrote unrelated bindings.
#[test]
fn code_selector_narrows_the_plan_to_the_named_diagnostic() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("mixed.harn");
    fs::write(
        &script,
        "fn helper() {\n  let unchanged = 1\n  println(\"hi\")\n  return unchanged\n}\n",
    )
    .unwrap();

    let everything = build_plan_with_options_at(&script, None, &FixOptions::default()).unwrap();
    let codes = everything
        .repairs
        .iter()
        .map(|repair| repair.diagnostic_code.as_str())
        .collect::<BTreeSet<_>>();
    assert!(
        codes.len() > 1,
        "fixture must offer more than one code to narrow: {codes:?}"
    );
    let wanted = Code::LintMutableNeverReassigned;
    assert!(codes.contains(wanted.as_str()), "{codes:?}");

    let narrowed = build_plan_with_options_at(
        &script,
        None,
        &FixOptions {
            codes: BTreeSet::from([wanted]),
            ..FixOptions::default()
        },
    )
    .unwrap();
    assert!(!narrowed.repairs.is_empty());
    assert!(
        narrowed
            .repairs
            .iter()
            .all(|repair| repair.diagnostic_code == wanted.as_str()),
        "{:?}",
        narrowed.repairs
    );
    // Repairs index into `diagnostics`, so both must be narrowed together or
    // every reported repair points at the wrong diagnostic.
    for repair in &narrowed.repairs {
        assert_eq!(
            narrowed.diagnostics[repair.diagnostic_index].code,
            repair.diagnostic_code
        );
    }
}

/// A repair that defers to the whole-program capability pass must not defer to
/// a pass `--code` has excluded — that postpones it forever. Selecting only the
/// rename in a file that also has attenuation work planned nothing at all, so a
/// targeted migration was silently a no-op.
#[test]
fn code_selector_does_not_defer_to_an_unselected_whole_program_pass() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("lib.harn");
    fs::write(
        &script,
        concat!(
            "fn main(harness: Harness) {\n",
            "  read_config(harness.fs)\n",
            "  publish(harness)\n",
            "}\n",
            "\n",
            // Already narrow, misnamed: the rename this test selects.
            "fn read_config(harness: HarnessFs) {\n",
            "  return harness.read_text(\"harn.toml\")\n",
            "}\n",
            "\n",
            // Broad: the attenuation the whole-program pass owns, in the same
            // file, which is what the rename was deferring to.
            "fn publish(harness: Harness) {\n",
            "  harness.fs.write_text(\"out.txt\", \"done\")\n",
            "}\n",
        ),
    )
    .unwrap();

    let rename = Code::LintCapabilityParameterName;
    let unselected = build_plan_with_options_at(
        &script,
        Some(RepairSafety::SurfaceChanging),
        &FixOptions::default(),
    )
    .unwrap();
    assert!(
        unselected
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == Code::LintBroadHarnessParameter.as_str()),
        "fixture must give the whole-program pass work to defer to: {:?}",
        unselected.diagnostics
    );

    let narrowed = build_plan_with_options_at(
        &script,
        Some(RepairSafety::SurfaceChanging),
        &FixOptions {
            codes: BTreeSet::from([rename]),
            ..FixOptions::default()
        },
    )
    .unwrap();
    assert!(
        narrowed
            .repairs
            .iter()
            .any(|repair| repair.diagnostic_code == rename.as_str() && !repair.edits.is_empty()),
        "the selected rename must be planned, not deferred to an excluded pass: {:?}",
        narrowed.repairs
    );
}

/// A file `harn fmt` would rewrite must come back exactly as its author keeps
/// it, apart from the repair. Formatting every edited file would turn a
/// three-line repair into a whole-file diff for any project that does not run
/// `harn fmt`.
#[test]
fn apply_leaves_an_unformatted_file_unformatted() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("loose.harn");
    let original = "fn main(harness: Harness) {\n      const value    = 1\n}\n";
    fs::write(&script, original).unwrap();
    let start = original.find('1').unwrap();

    apply_file_edits(
        &script,
        &[FixEditWire {
            span: SpanWire::from(Span::with_offsets(start, start + 1, 2, 1)),
            replacement: "2".to_string(),
        }],
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(&script).unwrap(),
        "fn main(harness: Harness) {\n      const value    = 2\n}\n",
        "only the repaired span may differ"
    );
}

/// The converse: a canonically formatted file must stay that way. A repair
/// changes line lengths, so a shortening rename could otherwise leave a
/// package failing the `harn fmt --check` its own CI runs.
#[test]
fn apply_keeps_a_formatted_file_formatted() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("canonical.harn");
    let original = "fn main(harness: Harness) {\n  const value = 1\n}\n";
    fs::write(&script, original).unwrap();
    let start = original.find("const").unwrap();

    // Replacing the statement with a longer one the formatter would re-wrap.
    apply_file_edits(
        &script,
        &[FixEditWire {
            span: SpanWire::from(Span::with_offsets(
                start,
                start + "const value = 1".len(),
                2,
                3,
            )),
            replacement: "const value    =    1".to_string(),
        }],
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(&script).unwrap(),
        original,
        "a canonical file must come back canonical"
    );
}

#[test]
fn edit_group_key_collapses_relative_and_absolute_spellings() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("workflow.harn");
    fs::write(&script, "const value = 1\n").unwrap();

    // The per-file lint pass reports a relative path and the whole-program
    // capability pass reports an absolute one. Keyed on the raw string these
    // are two groups over one file, and the second group then applies spans
    // computed against source the first group already rewrote (#6148).
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp.path()).unwrap();
    let relative = edit_group_key("./workflow.harn");
    std::env::set_current_dir(previous).unwrap();

    let absolute = edit_group_key(&script.to_string_lossy());
    assert_eq!(
        relative, absolute,
        "both spellings of one file must group together"
    );
}

#[test]
fn edit_group_key_keeps_unresolvable_paths_verbatim() {
    let missing = "./does/not/exist.harn";
    assert_eq!(
        edit_group_key(missing),
        missing,
        "an unresolvable path keeps its spelling so the later read reports it"
    );
}

/// The v0.10.80 -> v0.10.81 Harn Cloud bump normalized the leading comment
/// and renamed the later unused Harness parameter in one pass. The route
/// wildcard in the comment contains `/*`; copying it verbatim into HarnDoc
/// opens a nested block comment and made the applicator reject its own output.
#[test]
fn harn_cloud_doc_migration_composes_with_a_later_binding_edit() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("admin_trial_telemetry.harn");
    fs::write(
        &script,
        include_str!(
            "../../../tests/fixtures/capability_migration/admin_trial_telemetry_before.harn"
        ),
    )
    .unwrap();

    let result = apply_repairs(&script, RepairSafety::BehaviorPreserving, false).unwrap();
    let updated = fs::read_to_string(&script).unwrap();

    harn_parser::parse_source(&updated).expect("the complete candidate must parse");
    assert!(
        updated.contains("public `/try/&#42;`")
            && updated.contains("get_admin_trial_telemetry(_harness: Harness"),
        "both planned edits must reach the valid candidate:\n{updated}"
    );
    assert_eq!(
        result.applied.len(),
        2,
        "the doc and binding repairs must both fire: {result:#?}"
    );
}

#[test]
fn apply_file_edits_refuses_to_write_unparseable_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("broken.harn");
    let original = "fn main(harness: Harness) {\n  const value = 1\n}\n";
    fs::write(&script, original).unwrap();

    // An edit landing mid-token is what a stale span produces. Writing that
    // out is never right, and the caller cannot detect it afterwards: an
    // unparseable file contributes no diagnostics, so the run reports clean.
    let edits = vec![FixEditWire {
        span: SpanWire::from(Span::with_offsets(10, 10, 1, 11)),
        replacement: "!!(".to_string(),
    }];
    let error = apply_file_edits(&script, &edits)
        .expect_err("a candidate that does not parse must be rejected");

    assert!(
        error.contains("invalid Harn syntax"),
        "error should name the syntax failure: {error}"
    );
    assert!(
        error.contains("applied edits:"),
        "error should list the edits so the bad span is visible: {error}"
    );
    assert_eq!(
        fs::read_to_string(&script).unwrap(),
        original,
        "the file must be left exactly as it was"
    );
}
