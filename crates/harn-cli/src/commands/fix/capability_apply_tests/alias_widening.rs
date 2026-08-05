//! Widening the function-type alias that fixes a value-referenced arity.
//!
//! #6146 froze these callables; #6153 asks the tool to perform the mechanical
//! remedy its own message describes. Every test here is either the provable
//! case or one of the shapes that must keep the refusal — a half-sound
//! rewriter inside an auto-applied migration is worse than declining.

use super::*;

/// The shape from the issue. `resolve_thing` owns the ambient capability use,
/// its only value read is `run`'s parameter default, and the parameter's type
/// is a local `fn(...)` alias — so the alias, the definition, and the dispatch
/// can all move together.
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

#[test]
fn the_alias_the_definition_and_the_dispatch_move_together() {
    let migrated = migrate(PROVABLE);
    assert!(
        migrated.contains("type ResolverFn = fn(Harness, string) -> string"),
        "the alias must widen: {migrated}"
    );
    assert!(
        migrated.contains("fn resolve_thing(harness: Harness, request: string)"),
        "the definition must widen: {migrated}"
    );
    // The one the sketch omitted. A value call's arity is not checked
    // statically, so leaving this behind produces a program that passes
    // `harn check` and then fails at run time with `Arity mismatch`.
    assert!(
        migrated.contains("return resolver(harness, \"q\")"),
        "the dispatch must gain the capability argument: {migrated}"
    );
}

/// An exported alias can be named by a file this pass never saw.
#[test]
fn an_exported_alias_keeps_the_refusal() {
    let source = PROVABLE.replace("type ResolverFn", "pub type ResolverFn");
    assert_eq!(migrate(&source), source, "an exported alias must not move");
}

/// A second parameter typed by the alias whose default is not a migrating
/// callable would be retyped without the migration ever reasoning about it.
#[test]
fn a_second_use_of_the_alias_keeps_the_refusal() {
    let source = PROVABLE.replace(
        "pub fn run(",
        "fn other(pick: ResolverFn) -> string {\n  return pick(\"z\")\n}\n\npub fn run(",
    );
    assert_eq!(
        migrate(&source),
        source,
        "an unaccounted alias use must not move"
    );
}

/// A value read outside a parameter default is a dispatch this pass cannot
/// follow — exactly the case #6146 froze.
#[test]
fn a_value_read_outside_a_parameter_default_keeps_the_refusal() {
    let source = PROVABLE.replace(
        "pub fn run(",
        "fn registry() -> list {\n  return [resolve_thing]\n}\n\npub fn run(",
    );
    assert_eq!(migrate(&source), source, "an escaping value must not move");
}

/// Nothing in scope can supply the capability the widened dispatch needs.
#[test]
fn a_dispatch_with_no_harness_in_scope_keeps_the_refusal() {
    let source = PROVABLE.replace(
        "pub fn run(harness: Harness, resolver:",
        "pub fn run(resolver:",
    );
    assert_eq!(
        migrate(&source),
        source,
        "a dispatch with no capability to pass must not move"
    );
}

/// The refusal is still reported, not silent — #6219's frozen list must name
/// the callable in every shape above.
#[test]
fn a_refused_widening_is_still_reported_as_frozen() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("app.harn"),
        PROVABLE.replace("type ResolverFn", "pub type ResolverFn"),
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
        "a refused widening must still name the frozen callable: {:?}",
        plan.frozen_callables
    );
}

/// The provable case must NOT be reported as frozen — the report is a
/// blocked-here signal, and a widened callable is not blocked.
#[test]
fn a_widened_callable_is_not_reported_as_frozen() {
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
        "a widened callable must not be reported as frozen: {:?}",
        plan.frozen_callables
    );
}
