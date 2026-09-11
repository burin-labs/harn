use super::*;

pub(super) fn assert_only_pick_diagnostic(result: &ApplyResult, source: &str) {
    let temp = tempfile::NamedTempFile::with_suffix(".harn").unwrap();
    fs::write(temp.path(), source).unwrap();
    assert_only_pick_diagnostic_at(result, temp.path());
}

pub(super) fn assert_only_pick_diagnostic_at(result: &ApplyResult, path: &Path) {
    let plan = build_plan(path, None).unwrap();
    let codes: Vec<_> = plan.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["HARN-LNT-077"], "{plan:#?}");
    assert_eq!(
        result.post_apply_diagnostics_count,
        codes.len(),
        "{result:#?}"
    );
}

#[test]
fn capability_apply_replaces_bundle_arguments_before_their_receivers() {
    let (result, updated) = apply_single(
        r#"
fn inspect(harness: {runtime: HarnessRuntime}) -> bool {
  return harness.runtime.host_has("workspace", "project_root")
}

fn wrapper(harness: Harness) -> bool {
  return inspect({runtime: harness.runtime})
}

fn main(harness: Harness) {
  wrapper(harness)
}
"#,
    );
    assert!(!result.applied.is_empty(), "the migration must run");
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert_eq!(
        callable_params(&updated, "wrapper"),
        [param("runtime", "HarnessRuntime")]
    );
    assert_eq!(
        call_argument_paths(&updated, "inspect"),
        [vec![Some("runtime".into())]]
    );
    assert_eq!(
        call_argument_paths(&updated, "wrapper"),
        [vec![Some("harness.runtime".into())]]
    );
}

#[test]
fn capability_apply_replaces_existing_projection_for_a_root_parameter() {
    for projection in [
        "pick(harness, [\"env\", \"runtime\"])",
        "{env: harness.env, runtime: harness.runtime}",
        "harness.runtime",
    ] {
        let (result, updated) = apply_single(&format!(
            "import {{ with_scenario }} from \"std/testing\"\n\nfn main(harness: Harness) {{\n  with_scenario({projection}, {{}}, {{ _ -> \"ok\" }})\n}}\n"
        ));
        assert!(!result.applied.is_empty(), "the migration must run");
        assert_eq!(
            result.post_apply_diagnostics_count, 0,
            "{result:#?}\n{updated}"
        );
        assert_eq!(
            call_argument_paths(&updated, "with_scenario"),
            [vec![Some("harness".into()), None, None]],
            "an existing carrier must be replaced rather than shifted: {updated}"
        );
    }
}

#[test]
fn capability_apply_replaces_existing_bundle_for_a_narrow_parameter() {
    for projection in ["pick(harness, [\"fs\"])", "{fs: harness.fs}"] {
        let (result, updated) = apply_single(&format!(
            "import {{ with_temp_dir }} from \"std/testing\"\n\nfn main(harness: Harness) {{\n  with_temp_dir({projection}, {{ dir -> harness.stdio.println(dir) }})\n}}\n"
        ));
        assert!(!result.applied.is_empty(), "the migration must run");
        assert_eq!(
            result.post_apply_diagnostics_count, 0,
            "{result:#?}\n{updated}"
        );
        assert_eq!(
            call_argument_paths(&updated, "with_temp_dir"),
            [vec![Some("harness.fs".into()), None]],
            "an existing carrier must be replaced rather than shifted: {updated}"
        );
    }
}

#[test]
fn capability_apply_preserves_opaque_or_ambiguous_projection_arguments() {
    for projection in [
        "get_runtime(harness.runtime)",
        "{runtime: get_runtime(harness.runtime)}",
        "{env: harness.env, runtime: other.runtime}",
        "pick(harness, [/* retain this explanation */ \"runtime\"])",
    ] {
        let source = format!(
            "import {{ with_scenario }} from \"std/testing\"\n\nfn get_runtime(runtime: HarnessRuntime) -> HarnessRuntime {{\n  runtime.store_set(\"observed\", true)\n  return runtime\n}}\n\nfn main(harness: Harness, other: Harness) {{\n  with_scenario({projection}, {{}}, {{ _ -> \"ok\" }})\n}}\n"
        );
        let (result, updated) = apply_single(&source);
        assert!(
            updated.contains(&format!("with_scenario({projection}, {{}}")),
            "an ambiguous or observable expression must not be dropped or shifted: {updated}"
        );
        assert!(
            result.post_apply_diagnostics_count > 0,
            "the unresolved argument must remain visible: {result:#?}"
        );
    }
}
