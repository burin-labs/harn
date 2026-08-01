use super::*;
use std::fs;

fn apply_single(source: &str) -> (ApplyResult, String) {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(&script, source).unwrap();
    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    let updated = fs::read_to_string(&script).unwrap();
    (result, updated)
}

#[test]
fn capability_apply_converges_transitive_repairs_in_one_invocation() {
    let (result, updated) = apply_single(
        "fn needs_harness(harness: Harness, value: string) {\n  value\n}\n\nfn wrapper(value: string) {\n  needs_harness(value)\n}\n\npipeline run() {\n  wrapper(\"session\")\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert!(
        updated.contains("fn wrapper(harness: Harness, value: string)"),
        "{updated}"
    );
    assert!(
        updated.contains("needs_harness(harness, value)"),
        "{updated}"
    );
    assert!(
        updated.contains("wrapper(harness, \"session\")"),
        "{updated}"
    );
    assert!(
        updated.contains("pipeline run(harness: Harness)"),
        "{updated}"
    );
}

#[test]
fn capability_apply_widens_an_existing_handle_for_an_imported_bundle() {
    let temp = tempfile::TempDir::new().unwrap();
    let mode = temp.path().join("mode.harn");
    let entry = temp.path().join("main.harn");
    fs::write(
        &mode,
        "pub fn run_auto_mode(harness: {env: HarnessEnv, obs: HarnessObs}, setting: string = \"\") -> string {\n  harness.obs.llm_usage()\n  return harness.env.get_or(\"MODE\", setting)\n}\n",
    )
    .unwrap();
    fs::write(
        &entry,
        "import { run_auto_mode } from \"./mode\"\n\nfn invoke(setting: string, harness: HarnessObs) -> string {\n  return run_auto_mode(harness, setting)\n}\n\nfn main(harness: Harness) {\n  invoke(\"\", harness.obs)\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    let updated = fs::read_to_string(&entry).unwrap();
    assert!(
        updated.contains("fn invoke(setting: string, harness: {env: HarnessEnv, obs: HarnessObs}"),
        "{updated}"
    );
    assert!(!updated.contains("fn invoke(_harness:"), "{updated}");
    assert!(
        updated.contains("invoke(\"\", {env: harness.env, obs: harness.obs})"),
        "{updated}"
    );
    assert!(
        updated.contains("run_auto_mode(harness, setting)"),
        "{updated}"
    );
}

#[test]
fn capability_apply_inserts_an_argument_for_a_widened_existing_carrier() {
    let (result, updated) = apply_single(
        "pub fn read_mode(prefix: string, harness: HarnessEnv) -> string {\n  llm_usage()\n  return prefix + harness.get_or(\"MODE\", \"\")\n}\n\nfn invoke() -> string {\n  return read_mode(\"mode=\")\n}\n\nfn main(harness: Harness) {\n  invoke()\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert!(
        updated.contains("fn invoke(harness: {env: HarnessEnv, obs: HarnessObs})"),
        "{updated}"
    );
    assert!(
        updated.contains("read_mode(\"mode=\", harness)"),
        "{updated}"
    );
}

#[test]
fn capability_apply_absorbs_an_implicit_root_receiver_in_the_first_program_plan() {
    let (result, updated) = apply_single(
        "pub fn write_result(text: string) -> nil {\n  const input = pipeline_input() ?? {}\n  if input?.emit ?? false {\n    harness.stdio.print(text)\n  }\n}\n\nfn main(harness: Harness) {\n  write_result(\"hello\")\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert_eq!(result.applied.len(), 1, "{result:#?}");
    assert!(
        updated.contains(
            "pub fn write_result(harness: {stdio: HarnessStdio, runtime: HarnessRuntime}, text: string)"
        ),
        "{updated}"
    );
    assert!(
        updated.contains("harness.runtime.pipeline_input()"),
        "{updated}"
    );
    assert!(updated.contains("harness.stdio.print(text)"), "{updated}");
    assert!(
        updated
            .contains("write_result({stdio: harness.stdio, runtime: harness.runtime}, \"hello\")"),
        "{updated}"
    );
}

#[test]
fn capability_apply_preserves_root_values_that_escape() {
    let (result, updated) = apply_single(
        "fn consume(harness: Harness) {}\n\nfn keep_root(harness: Harness) {\n  consume(harness)\n}\n\nfn narrow(harness: Harness) -> string {\n  return harness.fs.cwd()\n}\n\nfn main(harness: Harness) {\n  keep_root(harness)\n  narrow(harness)\n}\n",
    );
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert!(
        updated.contains("fn keep_root(harness: Harness)"),
        "{updated}"
    );
    assert!(updated.contains("consume(harness)"), "{updated}");
    assert!(
        updated.contains("fn narrow(harness: HarnessFs)"),
        "{updated}"
    );
    assert!(updated.contains("narrow(harness.fs)"), "{updated}");
}

#[test]
fn capability_apply_follows_selective_re_exports_to_the_definition() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(
        temp.path().join("core.harn"),
        "pub fn evidence_candidate_dirs(items: list) -> list {\n  const _ = now_ms()\n  return items\n}\n",
    )
    .unwrap();
    fs::write(
        temp.path().join("mod.harn"),
        "pub import { evidence_candidate_dirs } from \"./core\"\n",
    )
    .unwrap();
    let entry = temp.path().join("main.harn");
    fs::write(
        &entry,
        "import { evidence_candidate_dirs } from \"./mod\"\n\nfn main(harness: Harness) {\n  evidence_candidate_dirs([])\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert_eq!(result.post_apply_diagnostics_count, 0, "{result:#?}");
    assert!(
        fs::read_to_string(temp.path().join("core.harn"))
            .unwrap()
            .contains("fn evidence_candidate_dirs(harness: HarnessClock, items: list)"),
        "the definition behind the facade must gain the carrier"
    );
    let updated = fs::read_to_string(entry).unwrap();
    assert!(
        updated.contains("evidence_candidate_dirs(harness.clock, [])"),
        "the caller through the re-export must update atomically: {updated}"
    );
}
