//! Callables whose value escapes as a first-class reference are wrapped.

use super::*;

/// A registry dispatches `handler(args)` through a stored reference. The fixer
/// wraps the hand-over so the registry still sees arity 1 while the body gains
/// its capability parameter.
#[test]
fn capability_apply_wraps_a_handler_referenced_by_value() {
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

    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let updated = fs::read_to_string(entry).unwrap();
    let params = callable_params(&updated, "web_search_handler");
    assert_eq!(
        params.last(),
        Some(&param("args", "dict")),
        "original params must survive: {updated}"
    );
    assert!(
        params.len() >= 2,
        "the handler must gain a capability parameter: {updated}"
    );
    assert!(
        updated.contains("{ args -> web_search_handler("),
        "the registry hand-over must be wrapped: {updated}"
    );
}

/// The registration usually lives in a different file from the handler —
/// burin-code defines `web_search_handler` in `lib/tools/web.harn` and
/// registers it in `lib/tools/surface.harn`. The wrap must land at the
/// cross-file hand-over, not only when definition and registry share a file.
#[test]
fn capability_apply_wraps_a_handler_registered_in_another_file() {
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

    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let web = fs::read_to_string(temp.path().join("web.harn")).unwrap();
    let surface = fs::read_to_string(temp.path().join("surface.harn")).unwrap();
    let params = callable_params(&web, "web_search_handler");
    assert!(
        params.len() >= 2 && params.last() == Some(&param("args", "dict")),
        "handler must gain a carrier and keep args: {web}"
    );
    assert!(
        surface.contains("{ args -> web_search_handler("),
        "cross-file hand-over must be wrapped: {surface}"
    );
}

/// Refusing one value-referenced callable must not cost its neighbours their
/// repair. Each ambient diagnostic is its own synthesis; a frozen owner aborts
/// only that repair, not the file.
#[test]
fn ambient_migration_still_repairs_a_sibling_of_a_frozen_value_reference() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("main.harn"),
        concat!(
            "@host_entry\n",
            "pub fn registry() -> list {\n",
            "  return [frozen_handler]\n",
            "}\n",
            "\n",
            "fn frozen_handler(args: dict) -> string {\n",
            "  return read_text(args?.path ?? \"\")\n",
            "}\n",
            "\n",
            "pub fn summarize(path: string) -> string {\n",
            "  return read_text(path)\n",
            "}\n",
        ),
    )
    .unwrap();

    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let updated = fs::read_to_string(temp.path().join("main.harn")).unwrap();
    assert!(
        updated.contains("fn frozen_handler(args: dict)"),
        "the frozen handler must keep its arity: {updated}"
    );
    assert!(
        updated.contains("pub fn summarize(fs: HarnessFs, path: string)")
            || updated.contains("pub fn summarize(harness: Harness, path: string)"),
        "the sibling must still be migrated: {updated}"
    );
}

/// Dry-run must converge the same pass loop as a real apply and report the
/// post-apply diagnostic count of the would-be tree — not the pre-repair count.
#[test]
fn capability_dry_run_reports_converged_post_apply_diagnostics() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(
        &script,
        concat!(
            "fn helper(path: string) -> string {\n",
            "  return read_text(path)\n",
            "}\n",
            "\n",
            "fn main() {\n",
            "  helper(\"x\")\n",
            "}\n",
        ),
    )
    .unwrap();
    let before = fs::read_to_string(&script).unwrap();

    let dry = apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        true,
        FixOptions::capability_migrations(),
    )
    .unwrap();
    assert!(dry.dry_run);
    assert_eq!(
        fs::read_to_string(&script).unwrap(),
        before,
        "dry-run must restore the tree"
    );

    let applied = apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();
    assert_eq!(
        dry.post_apply_diagnostics_count, applied.post_apply_diagnostics_count,
        "dry-run post-apply count must match a real apply: dry={dry:#?} applied={applied:#?}"
    );
    assert!(
        dry.post_apply_diagnostics_count < 3,
        "migration should clear the ambient diagnostics: {dry:#?}"
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

    apply_repairs_with_options_at(
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

/// A value-referenced callable whose hand-over sites can see `harness` is
/// wrapped, not frozen: the closure keeps the pre-migration arity for the
/// invisible dispatcher while the body receives the capability.
#[test]
fn capability_apply_wraps_a_value_referenced_callable_instead_of_freezing() {
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
            "fn registry(harness: Harness) -> list {\n",
            "  return [resolve_thing]\n",
            "}\n",
            "\n",
            "pub fn run(harness: Harness, resolver: ResolverFn = resolve_thing) -> string {\n",
            "  return resolver(\"q\")\n",
            "}\n",
        ),
    )
    .unwrap();

    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();

    let updated = fs::read_to_string(entry).unwrap();
    assert!(
        updated.contains("resolve_thing(fs: HarnessFs, request: string)")
            || updated.contains("resolve_thing(harness: Harness, request: string)"),
        "the callable must gain its carrier: {updated}"
    );
    assert!(
        updated.contains("{ request -> resolve_thing("),
        "each escaping reference must be wrapped: {updated}"
    );
    assert!(
        !updated.contains("return [resolve_thing]"),
        "the bare list hand-over must not survive: {updated}"
    );
}

/// Freezing silently is not correct (#6153). When a wrap cannot be synthesized,
/// the plan must name the callable and the escaping reference's file:line.
#[test]
fn capability_plan_names_the_frozen_callable_and_its_escape_site() {
    let temp = tempfile::TempDir::new().unwrap();
    // `@host_entry` on the only container that holds the reference: the wrap
    // would need harness in that container, which the host contract forbids.
    fs::write(
        temp.path().join("main.harn"),
        concat!(
            "@host_entry\n",
            "pub fn registry() -> list {\n",
            "  return [resolve_thing]\n",
            "}\n",
            "\n",
            "fn resolve_thing(request: string) -> string {\n",
            "  return read_text(request)\n",
            "}\n",
        ),
    )
    .unwrap();

    let plan = build_plan_with_options_at(temp.path(), None, &FixOptions::capability_migrations())
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
        frozen.reason.contains("main.harn:"),
        "the reason must name the escape site file:line: {}",
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

    let plan = build_plan_with_options_at(temp.path(), None, &FixOptions::capability_migrations())
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
