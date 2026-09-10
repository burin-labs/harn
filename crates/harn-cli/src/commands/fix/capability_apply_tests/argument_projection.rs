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
