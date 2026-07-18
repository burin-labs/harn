use super::*;
use crate::package::test_support::*;

fn installed_persona_project(root: &Path, root_manifest: &str, with_trigger: bool) -> PathBuf {
    installed_persona_project_fixture(root, root_manifest, &["reviewer"], with_trigger, "")
}

fn installed_persona_project_fixture(
    root: &Path,
    root_manifest: &str,
    persona_names: &[&str],
    with_persona_trigger: bool,
    root_triggers: &str,
) -> PathBuf {
    let root_manifest =
        format!("{root_manifest}\n[dependencies]\nagents = {{ path = \"vendor/agents\" }}\n");
    let anchor = write_trigger_project(root, &root_manifest, None);
    let mut lock = install_test_persona_package(
        root,
        "agents",
        persona_names
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        persona_names,
    );
    let dependency_source = root.join("vendor/agents");
    fs::create_dir_all(&dependency_source).unwrap();
    lock.packages[0].source = path_source_uri(&dependency_source.canonicalize().unwrap()).unwrap();
    if with_persona_trigger || !root_triggers.is_empty() {
        let package_manifest = current_packages_dir(root).join("agents").join(MANIFEST);
        let mut body = fs::read_to_string(&package_manifest).unwrap();
        if with_persona_trigger {
            body.push_str("triggers = [\"github.pr_opened\"]\n");
        }
        body.push_str(root_triggers);
        fs::write(&package_manifest, body).unwrap();
        lock.packages[0].content_hash = Some(
            compute_content_hash(package_manifest.parent().unwrap()).expect("package content hash"),
        );
    }
    let installed_package = current_packages_dir(root).join("agents");
    fs::copy(
        installed_package.join(MANIFEST),
        dependency_source.join(MANIFEST),
    )
    .unwrap();
    fs::copy(
        installed_package.join("workflow.harn"),
        dependency_source.join("workflow.harn"),
    )
    .unwrap();
    write_runtime_test_lock(root, &lock);
    try_load_runtime_extensions(&anchor).expect("persona fixture dependencies should materialize");
    anchor
}

fn write_runtime_test_lock(root: &Path, lock: &LockFile) {
    let body = toml::to_string_pretty(lock).unwrap();
    fs::write(root.join(LOCK_FILE), &body).unwrap();
    write_test_generation_lock(root, &body);
}

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

#[test]
fn installed_persona_is_inert_until_activation_and_deactivation_is_reversible() {
    let tmp = tempfile::tempdir().unwrap();
    let anchor = installed_persona_project(tmp.path(), "[package]\nname = \"consumer\"\n", true);

    let installed = try_load_runtime_extensions(&anchor).unwrap();
    assert!(installed.runtime_personas.is_empty());
    assert!(collect_persona_trigger_binding_specs(&installed)
        .unwrap()
        .is_empty());

    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation {
            autonomy_tier: Some(PersonaAutonomyTier::Suggest),
            tools: Some(vec!["shell".to_string()]),
            capabilities: Some(Vec::new()),
        },
        100,
    )
    .unwrap();
    let activated = try_load_runtime_extensions(&anchor).unwrap();
    assert_eq!(activated.runtime_personas.len(), 1);
    assert_eq!(activated.runtime_personas[0].id, "agents/reviewer");
    let bindings = collect_persona_trigger_binding_specs(&activated).unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].id, "persona.agents/reviewer.github.pr_opened");
    assert_eq!(bindings[0].autonomy_tier, harn_vm::AutonomyTier::Suggest);
    let harn_vm::TriggerHandlerSpec::Persona { binding, callable } = &bindings[0].handler else {
        panic!("expected persona handler");
    };
    assert_eq!(binding.name, "agents/reviewer");
    let harn_vm::VmCallable::Pipeline(callable) = callable else {
        panic!("expected persona pipeline callable");
    };
    let execution_policy = callable
        .execution_policy()
        .expect("activated persona callable has an execution policy");
    assert_eq!(execution_policy.tools, vec!["shell"]);
    assert!(execution_policy.tools_are_restricted());
    assert!(execution_policy.capabilities_deny_all());

    deactivate_persona(Some(&tmp.path().join(MANIFEST)), "agents/reviewer", 200).unwrap();
    let deactivated = try_load_runtime_extensions(&anchor).unwrap();
    assert!(deactivated.runtime_personas.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn activated_installed_persona_projects_its_canonical_root_trigger() {
    let tmp = tempfile::tempdir().unwrap();
    let anchor = installed_persona_project_fixture(
        tmp.path(),
        "[package]\nname = \"consumer\"\n",
        &["reviewer", "observer"],
        false,
        r#"
[[triggers]]
id = "review-digest"
kind = "cron"
provider = "cron"
match = { events = ["tick"] }
handler = "persona://reviewer"
schedule = "0 */4 * * *"
timezone = "UTC"
secrets = { signing_secret = "cron/signing-secret" }

[[triggers]]
id = "observer-events"
kind = "webhook"
provider = "github"
match = { events = ["issue_opened"] }
handler = "persona://observer"

[[triggers]]
id = "unrelated-worker"
kind = "webhook"
provider = "github"
match = { events = ["pr_opened"] }
handler = "worker://review-queue"
"#,
    );

    let installed = try_load_runtime_extensions(&anchor).unwrap();
    assert!(installed.triggers.is_empty());

    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    let activated = try_load_runtime_extensions(&anchor).unwrap();
    assert_eq!(activated.triggers.len(), 1);
    let trigger = &activated.triggers[0];
    assert_eq!(trigger.id, "agents/review-digest");
    assert_eq!(trigger.handler, "persona://agents/reviewer");
    assert_eq!(trigger.kind, TriggerKind::Cron);
    assert_eq!(trigger.provider.as_str(), "cron");
    assert_eq!(trigger.match_.events, vec!["tick"]);
    assert_eq!(
        trigger.kind_specific.get("schedule"),
        Some(&toml::Value::String("0 */4 * * *".to_string()))
    );
    assert_eq!(
        trigger.kind_specific.get("timezone"),
        Some(&toml::Value::String("UTC".to_string()))
    );
    assert_eq!(
        trigger.secrets.get("signing_secret").map(String::as_str),
        Some("cron/signing-secret")
    );
    let collected = collect_manifest_triggers(&mut test_vm(), &activated)
        .await
        .expect("qualified activated package trigger should collect");
    assert_eq!(collected.len(), 1);
    assert!(matches!(
        &collected[0].handler,
        CollectedTriggerHandler::Persona { binding, .. }
            if binding.name == "agents/reviewer"
    ));

    deactivate_persona(Some(&tmp.path().join(MANIFEST)), "agents/reviewer", 200).unwrap();
    let deactivated = try_load_runtime_extensions(&anchor).unwrap();
    assert!(deactivated.triggers.is_empty());
}

#[test]
fn activated_installed_trigger_ids_are_namespaced_by_dependency_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let anchor = write_trigger_project(
        root,
        r#"
[package]
name = "consumer"

[dependencies]
agents = { path = "vendor/agents" }
auditors = { path = "vendor/auditors" }
"#,
        None,
    );
    let mut lock =
        install_test_persona_package(root, "agents", vec!["reviewer".to_string()], &["reviewer"]);
    add_test_persona_package(
        root,
        &mut lock,
        "auditors",
        vec!["reviewer".to_string()],
        &["reviewer"],
    );

    for alias in ["agents", "auditors"] {
        let package_dir = current_packages_dir(root).join(alias);
        let mut manifest = fs::read_to_string(package_dir.join(MANIFEST)).unwrap();
        manifest.push_str(
            r#"
[[triggers]]
id = "shared-event"
kind = "webhook"
provider = "github"
match = { events = ["pr_opened"] }
handler = "persona://reviewer"
"#,
        );
        fs::write(package_dir.join(MANIFEST), manifest).unwrap();
        let source = root.join("vendor").join(alias);
        fs::create_dir_all(&source).unwrap();
        fs::copy(package_dir.join(MANIFEST), source.join(MANIFEST)).unwrap();
        fs::copy(
            package_dir.join("workflow.harn"),
            source.join("workflow.harn"),
        )
        .unwrap();
        let entry = lock
            .packages
            .iter_mut()
            .find(|entry| entry.name == alias)
            .unwrap();
        entry.source = path_source_uri(&source.canonicalize().unwrap()).unwrap();
        entry.content_hash = Some(compute_content_hash(&package_dir).unwrap());
    }
    write_runtime_test_lock(root, &lock);
    try_load_runtime_extensions(&anchor).expect("fixture dependencies should materialize");

    for id in ["agents/reviewer", "auditors/reviewer"] {
        activate_persona(
            Some(&root.join(MANIFEST)),
            id,
            &PersonaAttenuation::default(),
            100,
        )
        .unwrap();
    }
    let extensions = try_load_runtime_extensions(&anchor).unwrap();
    let mut ids = extensions
        .triggers
        .iter()
        .map(|trigger| trigger.id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, vec!["agents/shared-event", "auditors/shared-event"]);
}

#[test]
fn activated_persona_fails_closed_when_installed_content_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let anchor = installed_persona_project(tmp.path(), "[package]\nname = \"consumer\"\n", false);
    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();

    let package_dir = current_packages_dir(tmp.path()).join("agents");
    let updated_workflow = "pub pipeline run(task) -> dict { return {ok: true, revision: 2} }\n";
    fs::write(package_dir.join("workflow.harn"), updated_workflow).unwrap();
    fs::write(
        tmp.path().join("vendor/agents/workflow.harn"),
        updated_workflow,
    )
    .unwrap();
    let mut lock = LockFile::load(&tmp.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    let updated_hash = compute_content_hash(&tmp.path().join("vendor/agents")).unwrap();
    write_cached_content_hash(&package_dir, &updated_hash).unwrap();
    lock.packages[0].content_hash = Some(updated_hash);
    write_runtime_test_lock(tmp.path(), &lock);

    let error = try_load_runtime_extensions(&anchor).expect_err("stale activation must fail");
    assert!(error.to_string().contains("pinned content hash"), "{error}");
    assert!(error.to_string().contains("reactivate it before use"));
}

#[tokio::test(flavor = "current_thread")]
async fn activated_persona_guard_rechecks_content_at_invocation() {
    let tmp = tempfile::tempdir().unwrap();
    let anchor = installed_persona_project(tmp.path(), "[package]\nname = \"consumer\"\n", true);
    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    let extensions = try_load_runtime_extensions(&anchor).unwrap();
    let bindings = collect_persona_trigger_binding_specs(&extensions).unwrap();
    let harn_vm::TriggerHandlerSpec::Persona { callable, .. } = &bindings[0].handler else {
        panic!("expected persona handler");
    };

    fs::write(
        current_packages_dir(tmp.path()).join("agents/workflow.harn"),
        "pub pipeline run(task) -> dict { return {ok: false} }\n",
    )
    .unwrap();
    let error = test_vm()
        .execute_callable(callable, &[harn_vm::VmValue::Nil])
        .await
        .expect_err("guard must reject mutation after trigger registration");
    assert!(
        error
            .to_string()
            .contains("installed package execution rejected"),
        "{error}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn activated_persona_rejects_import_outside_pinned_package() {
    let tmp = tempfile::tempdir().unwrap();
    let anchor = installed_persona_project(tmp.path(), "[package]\nname = \"consumer\"\n", true);
    let packages = current_packages_dir(tmp.path());
    fs::write(
        packages.join("outside.harn"),
        "pub fn outside() -> bool { return true }\n",
    )
    .unwrap();
    fs::write(
        packages.join("agents/workflow.harn"),
        "import { outside } from \"../outside.harn\"\n\npub pipeline run(_task) -> dict { return {ok: outside()} }\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("vendor/agents/workflow.harn"),
        "import { outside } from \"../outside.harn\"\n\npub pipeline run(_task) -> dict { return {ok: outside()} }\n",
    )
    .unwrap();
    let mut lock = LockFile::load(&tmp.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    lock.packages[0].content_hash = Some(compute_content_hash(&packages.join("agents")).unwrap());
    write_runtime_test_lock(tmp.path(), &lock);
    try_load_runtime_extensions(&anchor).expect("updated package should materialize");

    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    let extensions = try_load_runtime_extensions(&anchor).unwrap();
    let bindings = collect_persona_trigger_binding_specs(&extensions).unwrap();
    let harn_vm::TriggerHandlerSpec::Persona { callable, .. } = &bindings[0].handler else {
        panic!("expected persona handler");
    };

    let error = test_vm()
        .execute_callable(callable, &[harn_vm::VmValue::Nil])
        .await
        .expect_err("guard must reject imports outside the activated package");
    assert!(
        error
            .to_string()
            .contains("installed package import rejected"),
        "{error}"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn activated_path_dependency_persona_executes_verified_self_import() {
    let tmp = tempfile::tempdir().unwrap();
    let anchor = installed_persona_project(tmp.path(), "[package]\nname = \"consumer\"\n", true);
    let packages = current_packages_dir(tmp.path());
    let source = tmp.path().join("vendor/agents");
    fs::write(
        source.join("helper.harn"),
        "pub fn helper() -> bool { return true }\n",
    )
    .unwrap();
    fs::write(
        source.join("workflow.harn"),
        "import { helper } from \"agents/helper\"\n\npub pipeline run(_task) -> dict { return {ok: helper()} }\n",
    )
    .unwrap();
    fs::remove_dir_all(packages.join("agents")).unwrap();
    std::os::unix::fs::symlink(&source, packages.join("agents")).unwrap();
    let mut lock = LockFile::load(&tmp.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    // Real local path dependencies intentionally carry no lockfile hash. The
    // activation observes and pins their current package content instead.
    lock.packages[0].content_hash = None;
    write_runtime_test_lock(tmp.path(), &lock);
    try_load_runtime_extensions(&anchor).expect("path package should materialize");

    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    let extensions = try_load_runtime_extensions(&anchor).unwrap();
    let bindings = collect_persona_trigger_binding_specs(&extensions).unwrap();
    let harn_vm::TriggerHandlerSpec::Persona { callable, .. } = &bindings[0].handler else {
        panic!("expected persona handler");
    };

    test_vm()
        .execute_callable(callable, &[harn_vm::VmValue::Nil])
        .await
        .expect("verified path dependency import should execute");
}

#[tokio::test(flavor = "current_thread")]
async fn activated_persona_executes_import_from_pinned_dependency() {
    let tmp = tempfile::tempdir().unwrap();
    let anchor = installed_persona_project(tmp.path(), "[package]\nname = \"consumer\"\n", true);
    let packages = current_packages_dir(tmp.path());

    let mut root_manifest = fs::read_to_string(tmp.path().join(MANIFEST)).unwrap();
    root_manifest.push_str("shared = { path = \"vendor/shared\" }\n");
    fs::write(tmp.path().join(MANIFEST), root_manifest).unwrap();

    let mut lock = LockFile::load(&tmp.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    add_test_persona_package(tmp.path(), &mut lock, "shared", Vec::new(), &[]);
    let shared_source = tmp.path().join("vendor/shared");
    fs::create_dir_all(&shared_source).unwrap();
    let shared_workflow = "pub fn shared_helper() -> bool { return true }\n";
    for root in [packages.join("shared"), shared_source.clone()] {
        fs::write(root.join("workflow.harn"), shared_workflow).unwrap();
    }
    fs::copy(
        packages.join("shared").join(MANIFEST),
        shared_source.join(MANIFEST),
    )
    .unwrap();
    let shared_entry = lock
        .packages
        .iter_mut()
        .find(|entry| entry.name == "shared")
        .unwrap();
    shared_entry.source = path_source_uri(&shared_source.canonicalize().unwrap()).unwrap();
    shared_entry.content_hash = Some(compute_content_hash(&packages.join("shared")).unwrap());

    let persona_workflow = "import { shared_helper } from \"shared/workflow\"\n\npub pipeline run(_task) -> dict { return {ok: shared_helper()} }\n";
    for root in [packages.join("agents"), tmp.path().join("vendor/agents")] {
        fs::write(root.join("workflow.harn"), persona_workflow).unwrap();
    }
    lock.packages[0].content_hash = Some(compute_content_hash(&packages.join("agents")).unwrap());
    write_runtime_test_lock(tmp.path(), &lock);
    try_load_runtime_extensions(&anchor).expect("updated dependency graph should materialize");

    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    let extensions = try_load_runtime_extensions(&anchor).unwrap();
    let bindings = collect_persona_trigger_binding_specs(&extensions).unwrap();
    let harn_vm::TriggerHandlerSpec::Persona { callable, .. } = &bindings[0].handler else {
        panic!("expected persona handler");
    };

    test_vm()
        .execute_callable(callable, &[harn_vm::VmValue::Nil])
        .await
        .expect("activated persona should execute its pinned package dependency");
}

#[tokio::test(flavor = "current_thread")]
async fn activated_persona_rejects_module_cached_from_unverified_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let anchor = installed_persona_project(tmp.path(), "[package]\nname = \"consumer\"\n", true);
    let packages = current_packages_dir(tmp.path());
    let expected_helper = "pub fn helper() -> bool { return true }\n";
    let workflow = "import { helper } from \"./helper\"\n\npub pipeline run(_task) -> dict { return {ok: helper()} }\n";
    for root in [packages.join("agents"), tmp.path().join("vendor/agents")] {
        fs::write(root.join("helper.harn"), expected_helper).unwrap();
        fs::write(root.join("workflow.harn"), workflow).unwrap();
    }
    let mut lock = LockFile::load(&tmp.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    lock.packages[0].content_hash = Some(compute_content_hash(&packages.join("agents")).unwrap());
    write_runtime_test_lock(tmp.path(), &lock);
    try_load_runtime_extensions(&anchor).expect("updated package should materialize");
    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    let extensions = try_load_runtime_extensions(&anchor).unwrap();
    let bindings = collect_persona_trigger_binding_specs(&extensions).unwrap();
    let harn_vm::TriggerHandlerSpec::Persona { callable, .. } = &bindings[0].handler else {
        panic!("expected persona handler");
    };

    let packages = current_packages_dir(tmp.path());
    let helper = packages.join("agents/helper.harn");
    fs::write(&helper, "pub fn helper() -> bool { return false }\n").unwrap();
    let preload = harn_vm::VmCallable::Pipeline(harn_vm::LazyPipelineCallable::new(
        packages.join("agents/workflow.harn"),
        "run",
    ));
    let mut vm = test_vm();
    vm.execute_callable(&preload, &[harn_vm::VmValue::Nil])
        .await
        .expect("unguarded preload should populate the module cache");
    fs::write(&helper, expected_helper).unwrap();

    let error = vm
        .execute_callable(callable, &[harn_vm::VmValue::Nil])
        .await
        .expect_err("guard must reject a module cached from different bytes");
    assert!(error.to_string().contains("cached module"), "{error}");
}

#[tokio::test(flavor = "current_thread")]
async fn lifecycle_resolves_qualified_activated_persona() {
    let tmp = tempfile::tempdir().unwrap();
    installed_persona_project(tmp.path(), "[package]\nname = \"consumer\"\n", false);
    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();

    let status = crate::commands::persona::status_payload(
        Some(&tmp.path().join(MANIFEST)),
        &tmp.path().join("state"),
        "agents/reviewer",
        Some("2026-01-01T00:00:00Z"),
    )
    .await
    .unwrap();
    assert_eq!(status.name, "agents/reviewer");
}

#[tokio::test(flavor = "current_thread")]
async fn installed_persona_handler_requires_activation_and_uses_qualified_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let anchor = installed_persona_project(
        tmp.path(),
        r#"
[package]
name = "consumer"

[[triggers]]
id = "installed-reviewer-pr-opened"
kind = "webhook"
provider = "github"
match = { events = ["pr_opened"] }
handler = "persona://agents/reviewer"
secrets = { signing_secret = "github/webhook-secret" }
"#,
        false,
    );
    let mut vm = test_vm();
    let error = collect_manifest_triggers(&mut vm, &try_load_runtime_extensions(&anchor).unwrap())
        .await
        .expect_err("installed handler must be inert before activation");
    assert!(error
        .to_string()
        .contains("does not match an active persona"));

    activate_persona(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
        &PersonaAttenuation::default(),
        100,
    )
    .unwrap();
    let collected =
        collect_manifest_triggers(&mut vm, &try_load_runtime_extensions(&anchor).unwrap())
            .await
            .expect("activated persona handler collects");
    assert!(matches!(
        &collected[0].handler,
        CollectedTriggerHandler::Persona { binding, .. }
            if binding.name == "agents/reviewer"
    ));
}
