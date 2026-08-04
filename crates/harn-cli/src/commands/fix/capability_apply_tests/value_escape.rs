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
