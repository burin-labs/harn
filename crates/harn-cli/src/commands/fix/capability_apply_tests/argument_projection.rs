use super::*;

#[test]
fn capability_apply_preserves_imported_bundle_arguments_with_extra_capabilities() {
    for argument in ["harness", "supplied"] {
        let temp = tempfile::TempDir::new().unwrap();
        fs::write(
            temp.path().join("library.harn"),
            "pub fn inspect(harness: {env: HarnessEnv, tools: HarnessTools}, name: string, fallback: string = \"\") -> string {\n  return harness.env.get(name) ?? fallback\n}\n",
        )
        .unwrap();
        let entrypoint = temp.path().join("main.harn");
        let binding = if argument == "supplied" {
            "  const supplied = harness\n"
        } else {
            ""
        };
        fs::write(
            &entrypoint,
            format!(
                "import {{ inspect }} from \"./library\"\n\npub type ReadHarness = {{env: HarnessEnv, tools: HarnessTools, fs: HarnessFs, net: HarnessNet}}\n\npub fn read(harness: ReadHarness) -> string {{\n{binding}  return inspect({argument}, \"PATH\")\n}}\n"
            ),
        )
        .unwrap();

        let result = apply_repairs_with_options_at(
            temp.path(),
            RepairSafety::SurfaceChanging,
            false,
            FixOptions::capability_migrations(),
        )
        .unwrap();
        let updated = fs::read_to_string(&entrypoint).unwrap();
        assert_eq!(
            result.post_apply_diagnostics_count, 0,
            "{result:#?}\n{updated}"
        );
        assert_clean_after_apply_at(&result, temp.path());
        assert_eq!(call_arities(&updated, "inspect"), vec![2], "{updated}");
        assert!(result.skipped.is_empty(), "{result:#?}\n{updated}");
    }
}

/// Field paths of a capability bundle argument: a record literal, or the
/// `pick(binding, ["field", ...])` form a migration emits for one binding.
pub(super) fn dict_field_paths(node: &SNode) -> Option<BTreeMap<String, Option<String>>> {
    if let Node::FunctionCall { name, args, .. } = &node.node {
        let [binding, fields] = args.as_slice() else {
            return None;
        };
        let (true, Some(binding), Node::ListLiteral(fields)) =
            (name == "pick", expression_path(binding), &fields.node)
        else {
            return None;
        };
        return fields
            .iter()
            .map(|field| match &field.node {
                Node::StringLiteral(key) => Some((key.clone(), Some(format!("{binding}.{key}")))),
                _ => None,
            })
            .collect();
    }
    let Node::DictLiteral(entries) = &node.node else {
        return None;
    };
    entries
        .iter()
        .map(|entry| {
            let key = match &entry.key.node {
                Node::Identifier(key) | Node::StringLiteral(key) => key.clone(),
                _ => return None,
            };
            Some((key, expression_path(&entry.value)))
        })
        .collect()
}

pub(super) fn expression_path(node: &SNode) -> Option<String> {
    match &node.node {
        Node::Identifier(name) => Some(name.clone()),
        Node::PropertyAccess { object, property } => {
            Some(format!("{}.{}", expression_path(object)?, property))
        }
        _ => None,
    }
}

pub(super) fn assert_clean_after_apply(result: &ApplyResult, source: &str) {
    let temp = tempfile::NamedTempFile::with_suffix(".harn").unwrap();
    fs::write(temp.path(), source).unwrap();
    assert_clean_after_apply_at(result, temp.path());
}

pub(super) fn assert_clean_after_apply_at(result: &ApplyResult, path: &Path) {
    let plan = build_plan(path, None).unwrap();
    // A migrated program must already be in the form `harn check --strict`
    // accepts; no follow-up `prefer-pick` pass runs during a runtime bump.
    let codes: Vec<_> = plan.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert!(codes.is_empty(), "{plan:#?}");
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
