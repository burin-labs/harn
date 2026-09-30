use std::path::Path;

use crate::test_util;

const HELPER: &str = r#"
import "std/predicate"

pub fn assess(llm: HarnessLlm, input: {text: string}) -> PredicateOutcome {
  const policy: EvaluationPolicy = {
    backend: "structured_llm", provider: "mock", model: "fixture",
    effort: "low", temperature: 0.0, threshold: 0.8,
    evaluation_cost_limit: 0.0, run_cost_limit: 0.0,
  }
  return llm.evaluate_predicate("finding.v1", "Is this supported?", input, policy)
}
"#;

const MAIN: &str = r#"
import "std/predicate"
import { assess } from "./helper"

fn main(harness: Harness) {
  const result = assess(harness.llm, {text: "observation"})
  match result.kind {
    "verdict" -> { if result.value.verdict { harness.stdio.println("accepted") } }
    _ -> { harness.stdio.println("${result.kind} ${result.receipt}") }
  }
}
"#;

fn check(root: &Path, cache: &Path) -> (bool, serde_json::Value) {
    let overlay = root.join("providers.toml");
    if !overlay.exists() {
        write_operations(root, "text_generation");
    }
    let output = test_util::process::harn_e2e_command()
        .args(["check", "--json", "main.harn"])
        .current_dir(root)
        .env("HARN_CACHE_DIR", cache)
        .env("HARN_CHECK_RESULT_CACHE", "1")
        .env("HARN_BYTECODE_CACHE", "1")
        .env("HARN_HOST_PROVIDERS_CONFIG", overlay)
        .output()
        .expect("run checker");
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    });
    (output.status.success(), report)
}

fn write_operations(root: &Path, operation: &str) {
    std::fs::write(
        root.join("providers.toml"),
        format!(
            r#"
[models.fixture]
name = "Declared fixture"
provider = "mock"
context_window = 8192
operations = ["{operation}"]
"#
        ),
    )
    .unwrap();
}

#[test]
pub(super) fn predicate_helper_manifest_survives_warm_cache_and_tracks_changed_question() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.harn"), MAIN).unwrap();
    std::fs::write(root.path().join("helper.harn"), HELPER).unwrap();
    let (passed, cold) = check(root.path(), cache.path());
    assert!(passed, "{cold}");
    let files = cold["data"]["files"].as_array().expect("file reports");
    let helper = files
        .iter()
        .find(|file| file["path"].as_str().unwrap().ends_with("main.harn"))
        .unwrap();
    let manifest = &helper["predicate_manifest"];
    assert_eq!(manifest["schema"], "harn.predicate_sites.v2");
    assert_eq!(manifest["sites"].as_array().unwrap().len(), 1);
    assert_eq!(manifest["sites"][0]["id"], "finding.v1");
    assert!(manifest["sites"][0]["source"]
        .as_str()
        .unwrap()
        .ends_with("helper.harn"));
    assert_eq!(
        manifest["sites"][0]["questions"][0]["instructions_sha256"],
        harn_kernel::pure::sha256_hex(b"Is this supported?")
    );
    let (passed, warm) = check(root.path(), cache.path());
    assert!(passed);
    assert_eq!(cold, warm, "a cached check must retain its measured sites");
    // Inject a marker into the cached projection to prove the next read uses
    // that artifact rather than silently re-running source analysis.
    let mut marked = 0;
    for entry in std::fs::read_dir(cache.path().join("check")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_none_or(|extension| extension != "harncheck")
        {
            continue;
        }
        let mut artifact: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if artifact["predicate_manifest"]["sites"]
            .as_array()
            .is_some_and(|sites| sites.len() == 1)
        {
            artifact["predicate_manifest"]["sites"][0]["id"] = "cache-read-probe".into();
            std::fs::write(path, serde_json::to_vec(&artifact).unwrap()).unwrap();
            marked += 1;
        }
    }
    assert_eq!(
        marked, 1,
        "exactly one persisted helper site must be measured"
    );
    let (passed, probe) = check(root.path(), cache.path());
    assert!(passed);
    assert!(
        probe["data"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["predicate_manifest"]["sites"][0]["id"] == "cache-read-probe"),
        "{probe}"
    );
    std::fs::write(
        root.path().join("helper.harn"),
        HELPER.replace("Is this supported?", "Is this contradicted?"),
    )
    .unwrap();
    let (passed, changed) = check(root.path(), cache.path());
    assert!(passed, "{changed}");
    let changed_helper = changed["data"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"].as_str().unwrap().ends_with("main.harn"))
        .unwrap();
    assert_ne!(
        manifest["sites"][0]["questions"][0]["instructions_sha256"],
        changed_helper["predicate_manifest"]["sites"][0]["questions"][0]["instructions_sha256"]
    );
    assert_eq!(
        changed_helper["predicate_manifest"]["sites"][0]["id"],
        "finding.v1"
    );
    std::fs::write(
        root.path().join("leaf.harn"),
        HELPER.replace("pub fn assess(", "pub fn evaluate("),
    )
    .unwrap();
    std::fs::write(
        root.path().join("helper.harn"),
        r#"
import "std/predicate"
import { evaluate } from "./leaf"
pub fn assess(llm: HarnessLlm, input: {text: string}) -> PredicateOutcome {
  return evaluate(llm, input)
}
"#,
    )
    .unwrap();
    let (passed, transitive) = check(root.path(), cache.path());
    assert!(passed, "{transitive}");
    let sites = transitive["data"]["files"][0]["predicate_manifest"]["sites"]
        .as_array()
        .unwrap();
    assert_eq!(sites.len(), 1, "{transitive}");
    assert!(sites[0]["source"].as_str().unwrap().ends_with("leaf.harn"));
}

#[test]
pub(super) fn predicate_checker_refuses_boolean_use_after_imported_helper() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("helper.harn"), HELPER).unwrap();
    std::fs::write(
        root.path().join("main.harn"),
        MAIN.replace("match result.kind {", "if result {}\nmatch result.kind {"),
    )
    .unwrap();
    let (passed, report) = check(root.path(), cache.path());
    assert!(!passed, "{report}");
    let main = report["data"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"].as_str().unwrap().ends_with("main.harn"))
        .unwrap();
    assert!(
        main["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "HARN-TYP-031"),
        "{report}"
    );
    assert!(
        main["predicate_manifest"].is_null(),
        "an invalid file must not advertise a complete census"
    );
}

#[test]
pub(super) fn predicate_census_refuses_an_invalid_imported_site() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.harn"), MAIN).unwrap();
    std::fs::write(
        root.path().join("helper.harn"),
        HELPER.replace("input: {text: string}", "input: any"),
    )
    .unwrap();
    let (passed, report) = check(root.path(), cache.path());
    assert!(!passed, "{report}");
    let file = &report["data"]["files"][0];
    assert!(file["predicate_manifest"].is_null(), "{report}");
    assert!(
        file["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| {
                diagnostic["source"] == "predicate"
                    && diagnostic["message"]
                        .as_str()
                        .unwrap()
                        .contains("helper.harn")
            }),
        "{report}"
    );
}

#[test]
pub(super) fn predicate_operation_admission_invalidates_cached_success() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.harn"), MAIN).unwrap();
    std::fs::write(root.path().join("helper.harn"), HELPER).unwrap();
    let (passed, accepted) = check(root.path(), cache.path());
    assert!(passed, "{accepted}");
    assert_eq!(
        accepted["data"]["files"][0]["predicate_manifest"]["sites"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Text routes derive the structured decision operation. An embedding-only
    // route is a real loss of evaluator capability, not a missing raw label.
    write_operations(root.path(), "embedding");
    let (passed, refused) = check(root.path(), cache.path());
    assert!(
        !passed,
        "a catalog change must invalidate the green cache: {refused}"
    );
    let diagnostics = refused["data"]["files"][0]["diagnostics"]
        .as_array()
        .unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "HARN-TYP-035"
                && diagnostic["message"]
                    .as_str()
                    .unwrap()
                    .contains("mock/fixture")
                && diagnostic["message"].as_str().unwrap().contains("decision")),
        "{refused}"
    );
    assert!(refused["data"]["files"][0]["predicate_manifest"].is_null());
    write_operations(root.path(), "text_generation");
    assert!(
        check(root.path(), cache.path()).0,
        "restoring the declaration restores admission"
    );
    // Preserve the unavailable-runtime contract through a checked source and
    // an explicit fixture catalog. The old conformance fixture passed policy
    // dynamically, so it no longer satisfies static operation admission.
    let execution = test_util::process::harn_e2e_command()
        .args(["run", "main.harn"])
        .current_dir(root.path())
        .env(
            "HARN_HOST_PROVIDERS_CONFIG",
            root.path().join("providers.toml"),
        )
        .env("HARN_CACHE_DIR", cache.path())
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .output()
        .expect("execute the admitted predicate source");
    // The unavailable-runtime contract is still here; it stopped being fatal.
    // A predicate with no budgeted evaluator used to abort the run with a
    // `VmError`, which a program could not branch on. It now closes with the
    // typed `unavailable` outcome and its receipt, so the run completes and
    // the caller decides what to do. Asserting the old error text would pin
    // exactly the behaviour this change exists to replace.
    let stdout = String::from_utf8_lossy(&execution.stdout);
    assert!(
        execution.status.success(),
        "an unavailable evaluator must close the predicate, not fail the run: {:?} {}",
        execution.status.code(),
        String::from_utf8_lossy(&execution.stderr)
    );
    let receipt = stdout
        .trim()
        .strip_prefix("unavailable ")
        .expect("typed unavailable outcome and receipt");
    assert!(
        !receipt.is_empty() && !receipt.chars().any(char::is_whitespace),
        "expected one receipt identity after the typed unavailable outcome, got: {stdout}"
    );
    assert!(
        !stdout.contains("accepted"),
        "no evaluator ran, so no verdict may be reported: {stdout}"
    );
}

#[test]
pub(super) fn predicate_embedding_model_is_refused_at_check_time() {
    let root = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("main.harn"), MAIN).unwrap();
    std::fs::write(
        root.path().join("helper.harn"),
        HELPER.replace(
            "provider: \"mock\", model: \"fixture\"",
            "provider: \"openai\", model: \"text-embedding-3-small\"",
        ),
    )
    .unwrap();
    let (passed, report) = check(root.path(), cache.path());
    assert!(!passed, "{report}");
    assert!(
        report["data"]["files"][0]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "HARN-TYP-035"
                && diagnostic["message"]
                    .as_str()
                    .unwrap()
                    .contains("text-embedding-3-small")
                && diagnostic["message"].as_str().unwrap().contains("decision")),
        "{report}"
    );
}
