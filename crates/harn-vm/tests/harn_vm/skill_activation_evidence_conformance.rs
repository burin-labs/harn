//! Conformance for the Harn-owned skill activation evidence contract.
//!
//! Discovers the committed fixtures under `tests/fixtures/skill-activation/`
//! plus a host-backed skill, builds the registry through the real layered
//! discovery, then drives the `skills_activation_evidence` builtin through the
//! VM. This pins the contract end to end across the source kinds a host cares
//! about: filesystem/bundled, path-scoped project, disabled/manual-only,
//! package, host-backed SKILL.md-style, and duplicate names (precedence).

use std::path::PathBuf;
use std::sync::Arc;

use harn_vm::skills::{
    skill_manifest_ref_to_vm, FsSkillSource, HostSkillSource, Layer, LayeredDiscovery, Skill,
    SkillManifest, SkillManifestRef,
};
use harn_vm::value::{DictMap, VmValue};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/skill-activation")
}

/// Build the merged registry VmValue through real layered discovery, adding a
/// host-backed skill on top so every source kind participates.
fn discovered_registry() -> VmValue {
    let root = fixtures();
    let host = HostSkillSource::new(
        || {
            vec![SkillManifestRef {
                id: "runbook".into(),
                manifest: SkillManifest {
                    name: "runbook".into(),
                    short: "Host-provided operational runbook".into(),
                    description: "Host store card, no filesystem body".into(),
                    when_to_use: Some("when the host offers an incident runbook".into()),
                    ..SkillManifest::default()
                },
                layer: Layer::Host,
                namespace: None,
                origin: "host".into(),
                unknown_fields: Vec::new(),
            }]
        },
        |id| {
            Ok(Skill {
                manifest: SkillManifest {
                    name: id.to_string(),
                    short: "Host-provided operational runbook".into(),
                    ..SkillManifest::default()
                },
                body: "host body".into(),
                skill_dir: None,
                layer: Layer::Host,
                namespace: None,
                unknown_fields: Vec::new(),
            })
        },
    );

    let discovery = LayeredDiscovery::new()
        .push(FsSkillSource::new(root.join("project"), Layer::Project))
        .push(FsSkillSource::new(root.join("user"), Layer::User))
        .push(FsSkillSource::new(root.join("package"), Layer::Package))
        .push(host);

    let report = discovery.build_report();
    // Duplicate `deploy` (project + user) must collapse to one winner.
    let deploy_winners = report.winners.iter().filter(|w| w.id == "deploy").count();
    assert_eq!(
        deploy_winners, 1,
        "duplicate name must resolve to one winner"
    );
    let deploy = report.winners.iter().find(|w| w.id == "deploy").unwrap();
    assert_eq!(
        deploy.layer,
        Layer::Project,
        "project layer must shadow the user duplicate"
    );

    let entries: Vec<VmValue> = report
        .winners
        .iter()
        .map(skill_manifest_ref_to_vm)
        .collect();
    VmValue::dict([
        (
            "_type",
            VmValue::String(arcstr::ArcStr::from("skill_registry")),
        ),
        ("skills", VmValue::List(Arc::new(entries))),
    ])
}

/// Run `skills_activation_evidence(skills, options)` against a pre-built
/// registry global and return the payload dict.
fn evidence(registry: VmValue, options_src: &str) -> DictMap {
    harn_vm::reset_thread_local_state();
    let source =
        format!("pipeline t(task) {{ return skills_activation_evidence(skills, {options_src}) }}");
    let chunk = harn_vm::compile_source(&source).expect("compile");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let value = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.set_global("skills", registry);
                vm.execute(&chunk).await.expect("execute")
            })
            .await
    });
    value.as_dict().expect("evidence is a dict").clone()
}

fn cards(payload: &DictMap) -> Vec<DictMap> {
    match payload.get("cards") {
        Some(VmValue::List(list)) => list.iter().filter_map(|v| v.as_dict().cloned()).collect(),
        _ => Vec::new(),
    }
}

fn card<'a>(cards: &'a [DictMap], id: &str) -> &'a DictMap {
    cards
        .iter()
        .find(|c| c.get("id").map(|v| v.display()).as_deref() == Some(id))
        .unwrap_or_else(|| panic!("card '{id}' missing from evidence"))
}

fn field(dict: &DictMap, key: &str) -> String {
    dict.get(key).map(|v| v.display()).unwrap_or_default()
}

#[test]
fn evidence_covers_every_source_kind_and_lifecycle() {
    let registry = discovered_registry();
    let payload = evidence(registry, "{budget: 10000}");

    assert_eq!(field(&payload, "_type"), "skill_activation_evidence");
    assert_eq!(field(&payload, "schema_version"), "1");

    let cards = cards(&payload);

    // Filesystem/bundled + path-scoped project skills are shown with source.
    let deploy = card(&cards, "deploy");
    assert_eq!(field(deploy, "lifecycle"), "shown");
    assert_eq!(field(deploy, "selected"), "true");
    assert_eq!(field(deploy, "source"), "project");
    let scoped = card(&cards, "scoped");
    assert_eq!(field(scoped, "lifecycle"), "shown");

    // Package + host-backed skills participate too.
    assert_eq!(field(card(&cards, "lint"), "source"), "package");
    assert_eq!(field(card(&cards, "runbook"), "source"), "host");
    assert_eq!(field(card(&cards, "runbook"), "lifecycle"), "shown");

    // Manual-only skill is omitted with the disable-model-invocation reason.
    let manual = card(&cards, "manual");
    assert_eq!(field(manual, "selected"), "false");
    assert_eq!(field(manual, "disable_model_invocation"), "true");
    assert_eq!(field(manual, "omitted_reason"), "disable_model_invocation");
    assert_eq!(field(manual, "lifecycle"), "omitted");

    // Token + char estimates are present on every card.
    for entry in &cards {
        let tokens: i64 = field(entry, "token_estimate").parse().unwrap_or(-1);
        assert!(tokens >= 0, "token estimate missing on {entry:?}");
    }
}

#[test]
fn budget_pressure_moves_cards_from_shown_to_omitted() {
    let registry = discovered_registry();
    // A tight budget shows a card or two and forces the rest into budget
    // omission (five model-invocable cards cannot all fit).
    let payload = evidence(registry, "{budget: 320}");
    let cards = cards(&payload);
    let shown = match payload.get("shown") {
        Some(VmValue::List(list)) => list.len(),
        _ => 0,
    };
    let budget_omitted = cards
        .iter()
        .filter(|c| field(c, "omitted_reason") == "budget")
        .count();
    assert!(shown >= 1, "at least one card should still be shown");
    assert!(
        budget_omitted >= 1,
        "a tight budget must omit at least one card for budget: {cards:?}"
    );
}

#[test]
fn loaded_and_used_ids_override_registry_lifecycle() {
    let registry = discovered_registry();
    let payload = evidence(
        registry,
        "{budget: 10000, loaded: [\"deploy\"], used: [\"review\"]}",
    );
    let cards = cards(&payload);
    assert_eq!(field(card(&cards, "deploy"), "lifecycle"), "loaded");
    assert_eq!(field(card(&cards, "review"), "lifecycle"), "used");
}
