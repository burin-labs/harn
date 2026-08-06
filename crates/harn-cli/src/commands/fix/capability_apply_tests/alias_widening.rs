//! Value-referenced callables migrate by wrapping the hand-over site.
//!
//! #6146 froze these callables; #6153 asked the tool to widen a governing
//! `type X = fn(...)` alias when that was the only escape. Wrapping the
//! reference as `{ args -> f(harness, args) }` is the more general repair: it
//! keeps the alias's arity intact (including exported aliases) and covers
//! registry list/dict hand-overs the alias pass cannot prove. Alias widening
//! remains as a narrower path when it still fires; these tests lock the
//! wrap-first behavior the fleet needs.

use super::*;

/// The shape from #6153. `resolve_thing` owns the ambient capability use and
/// escapes through a parameter default typed by a local `fn(...)` alias.
const PROVABLE: &str = concat!(
    "type ResolverFn = fn(string) -> string\n",
    "\n",
    "fn resolve_thing(request: string) -> string {\n",
    "  return read_text(request)\n",
    "}\n",
    "\n",
    "pub fn run(harness: Harness, resolver: ResolverFn = resolve_thing) -> string {\n",
    "  return resolver(\"q\")\n",
    "}\n",
);

fn migrate(source: &str) -> String {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("app.harn");
    fs::write(&script, source).unwrap();
    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::CapabilityChanging,
        false,
        super::FixOptions::capability_migrations(),
    )
    .unwrap();
    fs::read_to_string(&script).unwrap()
}

fn assert_resolve_thing_gained_carrier(migrated: &str) {
    let params = callable_params(migrated, "resolve_thing");
    assert!(
        params.len() >= 2 && params.last() == Some(&param("request", "string")),
        "resolve_thing must gain a carrier and keep request: {migrated}"
    );
}

#[test]
fn the_hand_over_is_wrapped_and_the_body_gains_its_carrier() {
    let migrated = migrate(PROVABLE);
    assert_resolve_thing_gained_carrier(&migrated);
    assert!(
        migrated.contains("{ request -> resolve_thing("),
        "the parameter-default hand-over must be wrapped: {migrated}"
    );
    // The alias keeps the pre-migration arity — the closure is what matches it.
    assert!(
        migrated.contains("type ResolverFn = fn(string) -> string"),
        "wrapping must not force the alias to move: {migrated}"
    );
}

/// An exported alias can be named by a file this pass never saw. Wrapping
/// keeps its arity, so the migration can still proceed.
#[test]
fn an_exported_alias_is_preserved_by_the_wrap() {
    let source = PROVABLE.replace("type ResolverFn", "pub type ResolverFn");
    let migrated = migrate(&source);
    assert_resolve_thing_gained_carrier(&migrated);
    assert!(
        migrated.contains("pub type ResolverFn = fn(string) -> string"),
        "exported alias arity must be preserved: {migrated}"
    );
    assert!(
        migrated.contains("{ request -> resolve_thing("),
        "hand-over must be wrapped: {migrated}"
    );
}

/// A second parameter typed by the alias is safe under wrap: the alias arity
/// never moves, so `other(pick: ResolverFn)` keeps type-checking.
#[test]
fn a_second_use_of_the_alias_still_typechecks_after_wrap() {
    let source = PROVABLE.replace(
        "pub fn run(",
        "fn other(pick: ResolverFn) -> string {\n  return pick(\"z\")\n}\n\npub fn run(",
    );
    let migrated = migrate(&source);
    assert_resolve_thing_gained_carrier(&migrated);
    assert!(
        migrated.contains("fn other(pick: ResolverFn)"),
        "sibling alias use must be untouched: {migrated}"
    );
}

/// A registry list hand-over is the burin-code shape — wrap it.
#[test]
fn a_value_read_outside_a_parameter_default_is_wrapped() {
    let source = PROVABLE.replace(
        "pub fn run(",
        "fn registry(harness: Harness) -> list {\n  return [resolve_thing]\n}\n\npub fn run(",
    );
    let migrated = migrate(&source);
    assert_resolve_thing_gained_carrier(&migrated);
    assert!(
        migrated.contains("{ request -> resolve_thing("),
        "registry hand-over must be wrapped: {migrated}"
    );
    assert!(
        !migrated.contains("return [resolve_thing]"),
        "bare list hand-over must not survive: {migrated}"
    );
}

/// When the only container has no way to receive a harness (host entry), the
/// wrap is refused and the freeze names the escape site.
#[test]
fn a_host_entry_container_keeps_the_refusal() {
    let source = concat!(
        "@host_entry\n",
        "pub fn run(resolver: ResolverFn = resolve_thing) -> string {\n",
        "  return resolver(\"q\")\n",
        "}\n",
        "\n",
        "type ResolverFn = fn(string) -> string\n",
        "\n",
        "fn resolve_thing(request: string) -> string {\n",
        "  return read_text(request)\n",
        "}\n",
    );
    let migrated = migrate(source);
    assert!(
        migrated.contains("fn resolve_thing(request: string)"),
        "host-entry container must leave the callee frozen: {migrated}"
    );
}

/// The refusal is still reported, not silent — the frozen list must name the
/// callable when the wrap cannot proceed.
#[test]
fn a_refused_wrap_is_still_reported_as_frozen() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("app.harn"),
        concat!(
            "@host_entry\n",
            "pub fn run(resolver: ResolverFn = resolve_thing) -> string {\n",
            "  return resolver(\"q\")\n",
            "}\n",
            "\n",
            "type ResolverFn = fn(string) -> string\n",
            "\n",
            "fn resolve_thing(request: string) -> string {\n",
            "  return read_text(request)\n",
            "}\n",
        ),
    )
    .unwrap();

    let plan = super::build_plan_with_options_at(
        temp.path(),
        None,
        &super::FixOptions::capability_migrations(),
    )
    .expect("plan");

    assert!(
        plan.frozen_callables
            .iter()
            .any(|frozen| frozen.name == "resolve_thing"),
        "a refused wrap must still name the frozen callable: {:?}",
        plan.frozen_callables
    );
}

/// A successfully wrapped callable must NOT be reported as frozen.
#[test]
fn a_wrapped_callable_is_not_reported_as_frozen() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(temp.path().join("app.harn"), PROVABLE).unwrap();

    let plan = super::build_plan_with_options_at(
        temp.path(),
        None,
        &super::FixOptions::capability_migrations(),
    )
    .expect("plan");

    assert!(
        plan.frozen_callables.is_empty(),
        "a wrapped callable must not be reported as frozen: {:?}",
        plan.frozen_callables
    );
}
