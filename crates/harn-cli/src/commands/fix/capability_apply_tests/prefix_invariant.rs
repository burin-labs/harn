//! The leading-prefix invariant for inserted capability arguments.

use super::*;

#[test]
fn capability_apply_does_not_shift_an_imported_call_with_an_untyped_leading_argument() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("surface.harn"),
        "pub fn render_surface(env: HarnessEnv, fs: HarnessFs, chain, surface: string) -> string {\n  const _ = env.get(\"USER\")\n  const _ = fs.read_text(surface)\n  return surface\n}\n",
    )
    .unwrap();
    let entry = temp.path().join("main.harn");
    fs::write(
        &entry,
        "import { render_surface } from \"./surface\"\n\npipeline default(task) {\n  const chain = task?.chain ?? {}\n  return render_surface(chain, \"github\")\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .unwrap();
    let updated = fs::read_to_string(entry).unwrap();
    assert_eq!(
        call_argument_paths(&updated, "render_surface")[0],
        [Some("chain".to_string()), None],
        "an unresolvable leading argument must be left intact, not shifted one slot \
         by a lone capability: {updated}"
    );
    assert!(
        result.post_apply_diagnostics_count > 0,
        "the call stays visible for a human instead of being silently shifted: {result:#?}"
    );
}

#[test]
fn capability_apply_still_extends_an_established_capability_prefix() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("surface.harn"),
        "pub fn render_surface(env: HarnessEnv, fs: HarnessFs, chain, surface: string) -> string {\n  const _ = env.get(\"USER\")\n  const _ = fs.read_text(surface)\n  return surface\n}\n",
    )
    .unwrap();
    let entry = temp.path().join("main.harn");
    fs::write(
        &entry,
        "import { render_surface } from \"./surface\"\n\npipeline default() {\n  return render_surface(harness.env, \"acme\", \"github\")\n}\n",
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
    assert_eq!(
        call_argument_paths(&updated, "render_surface")[0],
        [
            Some("harness.env".to_string()),
            Some("harness.fs".to_string()),
            None,
            None
        ],
        "a capability may still be appended to an established prefix: {updated}"
    );
}
