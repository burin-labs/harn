//! A `harn.toml` handler is a declared entry point (harn#6272).
//!
//! `@host_entry` covers the boundary nothing can see. This covers the boundary
//! everything can see and nothing read: `[[hooks]]` and `[[triggers]]` name the
//! callables the runtime invokes, at the arity their declaration fixes.
//!
//! Found in burin-code, whose five hook handlers all take `(event)`. The
//! migration rewrote each to take a capability parameter first — including
//! `enforce_stage_tool_gate({agent: HarnessAgent, runtime: HarnessRuntime}, event)`,
//! a record no hook engine can construct — and would have landed all five in
//! one auto-merged bump.

use super::*;

/// Two hook handlers, one registered and one not, in the file `pkg::` resolves
/// to. Both use an ambient capability, so both are migration candidates; only
/// the registration distinguishes them.
const HOOKS: &str = concat!(
    "pub fn on_pre_tool_use(event) -> nil {\n",
    "  store_set(\"last\", event?.tool?.name ?? \"\")\n",
    "  return nil\n",
    "}\n",
    "\n",
    "pub fn unregistered_helper(event) -> nil {\n",
    "  store_set(\"other\", event?.tool?.name ?? \"\")\n",
    "  return nil\n",
    "}\n",
);

const MANIFEST_WITH_HOOK: &str = concat!(
    "[package]\n",
    "name = \"pkg\"\n",
    "version = \"0.1.0\"\n",
    "\n",
    "[[hooks]]\n",
    "event = \"PreToolUse\"\n",
    "pattern = \"*\"\n",
    "handler = \"pkg::on_pre_tool_use\"\n",
);

const MANIFEST_WITHOUT_HOOK: &str =
    concat!("[package]\n", "name = \"pkg\"\n", "version = \"0.1.0\"\n",);

/// Lay out a package and migrate it, returning each file's post-apply source.
fn migrate_package(files: &[(&str, &str)]) -> BTreeMap<String, String> {
    let temp = tempfile::TempDir::new().unwrap();
    for (name, body) in files {
        let path = temp.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
    }
    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        super::FixOptions::capability_migrations(),
    )
    .expect("apply should succeed");
    files
        .iter()
        .filter(|(name, _)| name.ends_with(".harn"))
        .map(|(name, _)| {
            (
                (*name).to_string(),
                fs::read_to_string(temp.path().join(name)).unwrap(),
            )
        })
        .collect()
}

/// The falsifier. Without the `[[hooks]]` block the migration must still add a
/// parameter — otherwise the assertion below passes for some unrelated reason
/// and proves nothing about the manifest.
#[test]
fn an_unregistered_handler_still_gains_a_capability_parameter() {
    let migrated = migrate_package(&[("harn.toml", MANIFEST_WITHOUT_HOOK), ("lib.harn", HOOKS)]);
    let lib = &migrated["lib.harn"];
    assert!(
        lib.contains("pub fn on_pre_tool_use(runtime: HarnessRuntime, event)"),
        "an unregistered handler should still be threaded: {lib}"
    );
}

#[test]
fn a_registered_hook_handler_keeps_the_arity_the_manifest_fixed() {
    let migrated = migrate_package(&[("harn.toml", MANIFEST_WITH_HOOK), ("lib.harn", HOOKS)]);
    let lib = &migrated["lib.harn"];
    assert!(
        lib.contains("pub fn on_pre_tool_use(event) -> nil"),
        "the hook engine calls this with exactly `event`: {lib}"
    );
    assert!(
        !lib.contains("on_pre_tool_use(runtime"),
        "no parameter may be introduced ahead of `event`: {lib}"
    );
}

/// Freezing one handler must not cost the rest of the file its migration.
#[test]
fn a_sibling_of_a_registered_handler_still_migrates() {
    let migrated = migrate_package(&[("harn.toml", MANIFEST_WITH_HOOK), ("lib.harn", HOOKS)]);
    let lib = &migrated["lib.harn"];
    assert!(
        lib.contains("pub fn unregistered_helper(runtime: HarnessRuntime, event)"),
        "only the registered handler is frozen: {lib}"
    );
}

/// The registration resolves to one module, so the freeze must too.
///
/// This is the difference between reading the manifest and pattern-matching a
/// name: a package whose hook is `pkg::render` must not freeze every `render`
/// in the tree, or the manifest becomes a way to silently block migrations in
/// files it never mentioned.
#[test]
fn a_same_named_callable_in_another_module_is_not_frozen() {
    let migrated = migrate_package(&[
        ("harn.toml", MANIFEST_WITH_HOOK),
        ("lib.harn", HOOKS),
        ("other.harn", HOOKS),
    ]);
    let other = &migrated["other.harn"];
    assert!(
        other.contains("pub fn on_pre_tool_use(runtime: HarnessRuntime, event)"),
        "the manifest named lib.harn's handler, not this one: {other}"
    );
}

/// `[[triggers]]` resolves its handler the same way and is registered the same
/// way, including through an `[exports]` key rather than the package name.
#[test]
fn a_trigger_handler_reached_through_an_export_is_frozen() {
    let manifest = concat!(
        "[package]\n",
        "name = \"pkg\"\n",
        "version = \"0.1.0\"\n",
        "\n",
        "[exports]\n",
        "notifier = \"scripts/notifier.harn\"\n",
        "\n",
        "[[triggers]]\n",
        "id = \"nightly\"\n",
        "kind = \"cron\"\n",
        "provider = \"cron\"\n",
        "match = { events = [\"cron.tick\"] }\n",
        "handler = \"notifier::run_nightly\"\n",
        "schedule = \"0 10 * * 1\"\n",
    );
    let notifier_source = HOOKS.replace("on_pre_tool_use", "run_nightly");
    let migrated = migrate_package(&[
        ("harn.toml", manifest),
        ("scripts/notifier.harn", notifier_source.as_str()),
    ]);
    let notifier = &migrated["scripts/notifier.harn"];
    assert!(
        notifier.contains("pub fn run_nightly(event) -> nil"),
        "a trigger handler is entered by the runtime too: {notifier}"
    );
}

/// A frozen handler with more than one ambient call must not have its body
/// rewritten either.
///
/// Only the primary ambient call's repair carries the signature edit; a
/// secondary site emits just the body rewrite and leans on that repair to bind
/// the receiver. Freezing refuses the primary and the secondary landed anyway,
/// producing `harness.runtime.store_get(...)` inside a declaration that never
/// gains a `harness`. Byte-identical is the only safe outcome: a half-applied
/// migration is worse than none, because it does not parse as the code anyone
/// wrote.
///
/// This is `enforce_stage_tool_gate` in burin-code, reduced.
#[test]
fn a_frozen_handler_with_two_ambient_calls_is_not_half_rewritten() {
    const TWO_AMBIENT_CALLS: &str = concat!(
        "pub fn on_pre_tool_use(event) {\n",
        "  const current = (agent_session_current_id() ?? \"\").trim()\n",
        "  const session = current ? current : (store_get(\"k\") ?? \"\").trim()\n",
        "  store_set(\"last\", session)\n",
        "  return nil\n",
        "}\n",
    );
    let migrated = migrate_package(&[
        ("harn.toml", MANIFEST_WITH_HOOK),
        ("lib.harn", TWO_AMBIENT_CALLS),
    ]);
    assert_eq!(
        migrated["lib.harn"], TWO_AMBIENT_CALLS,
        "a frozen handler must be left byte-identical, body included"
    );
}

/// The reason has to send the reader to `harn.toml`. Told it was a value
/// escape, an author would go looking for a first-class reference that does not
/// exist; told it was `@host_entry`, for an attribute that is not there.
#[test]
fn a_frozen_manifest_handler_is_reported_with_its_own_reason() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(temp.path().join("harn.toml"), MANIFEST_WITH_HOOK).unwrap();
    fs::write(temp.path().join("lib.harn"), HOOKS).unwrap();

    let plan = super::build_plan_with_options_at(
        temp.path(),
        None,
        &super::FixOptions::capability_migrations(),
    )
    .expect("plan");

    let frozen = plan
        .frozen_callables
        .iter()
        .find(|frozen| frozen.name == "on_pre_tool_use")
        .unwrap_or_else(|| {
            panic!(
                "the frozen handler must be named; got {:?}",
                plan.frozen_callables
            )
        });
    assert!(
        frozen.reason.contains("`harn.toml`"),
        "the reason must name the registration: {}",
        frozen.reason
    );
    assert!(
        !frozen.reason.contains("first-class reference") && !frozen.reason.contains("@host_entry"),
        "a manifest handler must not be explained as either other cause: {}",
        frozen.reason
    );
}
