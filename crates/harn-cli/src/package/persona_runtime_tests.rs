use super::*;
use crate::package::test_support::*;

#[test]
fn persona_triggers_install_as_manifest_bindings() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[personas]]
name = "merge_captain"
description = "Owns PR readiness."
entry_workflow = "workflows/merge_captain.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"
triggers = ["github.pr_opened"]
budget = { daily_usd = 2.0 }
"#,
        None,
    );
    fs::create_dir_all(tmp.path().join("workflows")).unwrap();
    fs::write(
        tmp.path().join("workflows/merge_captain.harn"),
        "pub pipeline run(event) { return event }\n",
    )
    .unwrap();
    let extensions = load_runtime_extensions(&harn_file);
    let bindings =
        collect_persona_trigger_binding_specs(&extensions).expect("persona bindings collect");

    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].id, "persona.merge_captain.github.pr_opened");
    assert_eq!(bindings[0].provider.as_str(), "github");
    assert_eq!(bindings[0].kind, "pr_opened");
    assert_eq!(bindings[0].handler.kind(), "persona");
    assert!(matches!(
        &bindings[0].handler,
        harn_vm::TriggerHandlerSpec::Persona {
            callable: harn_vm::VmCallable::Pipeline(_),
            ..
        }
    ));
    assert_eq!(bindings[0].daily_cost_usd, Some(2.0));
}

#[test]
fn persona_triggers_reject_malformed_entry_workflow_coordinates() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[personas]]
name = "merge_captain"
description = "Owns PR readiness."
entry_workflow = "workflows/merge_captain.harn"
tools = ["github"]
autonomy = "suggest"
receipts = "required"
triggers = ["github.pr_opened"]
"#,
        None,
    );
    let extensions = load_runtime_extensions(&harn_file);
    let error = collect_persona_trigger_binding_specs(&extensions)
        .expect_err("invalid entry workflow should fail collection");

    assert!(
        error
            .to_string()
            .contains("entry_workflow must be <module.harn>#<function>"),
        "{error}"
    );
}

#[test]
fn persona_triggers_reject_entry_workflow_path_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[personas]]
name = "merge_captain"
description = "Owns PR readiness."
entry_workflow = "../outside.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"
triggers = ["github.pr_opened"]
"#,
        None,
    );
    let extensions = load_runtime_extensions(&harn_file);
    let error = collect_persona_trigger_binding_specs(&extensions)
        .expect_err("entry workflow path escape should fail collection");

    assert!(
        error.to_string().contains("escapes package root"),
        "{error}"
    );
}

#[test]
fn persona_triggers_reject_private_entry_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[personas]]
name = "merge_captain"
description = "Owns PR readiness."
entry_workflow = "workflows/merge_captain.harn#run"
tools = ["github"]
autonomy = "suggest"
receipts = "required"
triggers = ["github.pr_opened"]
"#,
        None,
    );
    fs::create_dir_all(tmp.path().join("workflows")).unwrap();
    fs::write(
        tmp.path().join("workflows/merge_captain.harn"),
        "pipeline run(event) { return event }\n",
    )
    .unwrap();
    let extensions = load_runtime_extensions(&harn_file);
    let error = collect_persona_trigger_binding_specs(&extensions)
        .expect_err("private entry workflow should fail collection");

    assert!(
        error
            .to_string()
            .contains("entry_workflow 'workflows/merge_captain.harn#run' is not exported"),
        "{error}"
    );
}
