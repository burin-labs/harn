//! Callables whose value escapes as a first-class reference keep their arity.

use super::*;

/// A registry dispatches `handler(args)` through a stored reference, so the
/// fixer sees no call site it could widen. Threading a capability into the
/// handler would move `args` into the capability slot at runtime, and
/// `harn check` reports nothing because the call goes through a value.
#[test]
fn capability_apply_keeps_the_arity_of_a_handler_referenced_by_value() {
    let temp = tempfile::TempDir::new().unwrap();
    let entry = temp.path().join("main.harn");
    fs::write(
        &entry,
        concat!(
            "fn web_search_handler(args: dict) -> string {\n",
            "  return read_text(args?.path ?? \"\")\n",
            "}\n",
            "\n",
            "fn bindings() -> list {\n",
            "  return [{name: \"web_search\", handler: web_search_handler}]\n",
            "}\n",
            "\n",
            "pipeline default(input: dict) {\n",
            "  for binding in bindings() {\n",
            "    if binding?.name == input?.name {\n",
            "      const handler = binding?.handler\n",
            "      return handler(input ?? {})\n",
            "    }\n",
            "  }\n",
            "  return \"\"\n",
            "}\n",
        ),
    )
    .unwrap();

    apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let updated = fs::read_to_string(entry).unwrap();
    assert_eq!(
        callable_params(&updated, "web_search_handler"),
        vec![param("args", "dict")],
        "a handler reached through a stored reference must keep its arity: {updated}"
    );
}

/// The registration usually lives in a different file from the handler —
/// burin-code defines `web_search_handler` in `lib/tools/web.harn` and
/// registers it in `lib/tools/surface.harn`. A per-file scan sees a definition
/// nothing references and threads it, so the escape has to be observed across
/// the whole program.
#[test]
fn capability_apply_keeps_the_arity_of_a_handler_registered_in_another_file() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("web.harn"),
        concat!(
            "pub fn web_search_handler(args: dict) -> string {\n",
            "  return read_text(args?.path ?? \"\")\n",
            "}\n",
        ),
    )
    .unwrap();
    fs::write(
        temp.path().join("surface.harn"),
        concat!(
            "import { web_search_handler } from \"./web\"\n",
            "\n",
            "pub fn bindings() -> list {\n",
            "  return [{name: \"web_search\", handler: web_search_handler}]\n",
            "}\n",
        ),
    )
    .unwrap();
    let entry = temp.path().join("main.harn");
    fs::write(
        &entry,
        concat!(
            "import { bindings } from \"./surface\"\n",
            "\n",
            "pipeline default(input: dict) {\n",
            "  for binding in bindings() {\n",
            "    if binding?.name == input?.name {\n",
            "      const handler = binding?.handler\n",
            "      return handler(input ?? {})\n",
            "    }\n",
            "  }\n",
            "  return \"\"\n",
            "}\n",
        ),
    )
    .unwrap();

    apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let web = fs::read_to_string(temp.path().join("web.harn")).unwrap();
    assert_eq!(
        callable_params(&web, "web_search_handler"),
        vec![param("args", "dict")],
        "a handler registered in another file must keep its arity: {web}"
    );
}

/// The freeze is keyed on the escape, not on the name: an ordinary callable
/// that is only ever called directly still gains its carrier.
#[test]
fn capability_apply_still_threads_a_callable_that_never_escapes() {
    let temp = tempfile::TempDir::new().unwrap();
    let entry = temp.path().join("main.harn");
    fs::write(
        &entry,
        concat!(
            "fn web_search_handler(args: dict) -> string {\n",
            "  return read_text(args?.path ?? \"\")\n",
            "}\n",
            "\n",
            "pipeline default(input: dict) {\n",
            "  return web_search_handler(input ?? {})\n",
            "}\n",
        ),
    )
    .unwrap();

    apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let updated = fs::read_to_string(entry).unwrap();
    let params = callable_params(&updated, "web_search_handler");
    assert_eq!(
        params.len(),
        2,
        "a directly-called callable must still gain its carrier: {updated}"
    );
    assert_eq!(
        params[1],
        param("args", "dict"),
        "the original parameter must survive the threading: {updated}"
    );
}

/// Freezing is correct; freezing silently is not.
///
/// The frozen callable owns the ambient capability use, so its missing
/// parameter edit is `None` and the `?` discards the whole file's repair. The
/// run then reports `applied 0 repair(s), skipped 0` with the capability
/// diagnostics still standing, and nothing names the callable that blocked it
/// (#6153).
#[test]
fn capability_plan_names_the_frozen_callable_that_blocked_the_migration() {
    let temp = tempfile::TempDir::new().unwrap();
    let entry = temp.path().join("main.harn");
    fs::write(
        &entry,
        concat!(
            "type ResolverFn = fn(string) -> string\n",
            "\n",
            "fn resolve_thing(request: string) -> string {\n",
            "  return read_text(request)\n",
            "}\n",
            "\n",
            // The value also escapes into a list, so the alias widening in
            // #6153 cannot prove this one and the refusal stands. Without the
            // extra read this fixture now migrates, and the test would assert
            // a report that correctly no longer exists.
            "fn registry() -> list {\n",
            "  return [resolve_thing]\n",
            "}\n",
            "\n",
            "pub fn run(harness: Harness, resolver: ResolverFn = resolve_thing) -> string {\n",
            "  return resolver(\"q\")\n",
            "}\n",
        ),
    )
    .unwrap();

    let plan = build_plan_with_options(temp.path(), None, &FixOptions::capability_migrations())
        .expect("plan");

    let frozen = plan
        .frozen_callables
        .iter()
        .find(|frozen| frozen.name == "resolve_thing")
        .unwrap_or_else(|| {
            panic!(
                "the frozen callable must be named; got {:?}",
                plan.frozen_callables
            )
        });
    assert!(
        frozen.reason.contains("first-class reference"),
        "the reason must say why it was frozen: {}",
        frozen.reason
    );
    assert!(
        frozen.reason.contains("resolve_thing(harness, args)"),
        "the reason must show the wrap that unblocks it: {}",
        frozen.reason
    );
}

/// A callable the migration can re-sign must not be reported as frozen. The
/// report is a blocked-here signal, not a log of every callable considered.
#[test]
fn capability_plan_reports_no_frozen_callable_when_the_migration_proceeds() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("main.harn"),
        concat!(
            "fn resolve_thing(request: string) -> string {\n",
            "  return read_text(request)\n",
            "}\n",
            "\n",
            "pub fn run(harness: Harness) -> string {\n",
            "  return resolve_thing(\"q\")\n",
            "}\n",
        ),
    )
    .unwrap();

    let plan = build_plan_with_options(temp.path(), None, &FixOptions::capability_migrations())
        .expect("plan");

    assert!(
        plan.frozen_callables.is_empty(),
        "an unfrozen migration must report nothing: {:?}",
        plan.frozen_callables
    );
    assert!(
        !plan.repairs.is_empty(),
        "the control must still produce its repair"
    );
}
