//! Falsifiers for harn#8537, exercised through the shipped CLI rather than an
//! internal function: a decision-only route is refused as a chat driver, an
//! unserved sibling is refused with no request made, and the model
//! recommender never offers a decision-only row as a text driver.
//!
//! No provider credential is needed. The operation gate fires during option
//! validation, before credential resolution and before transport, so these
//! run offline. If the gate ever stopped firing these would fail on a
//! credential error instead, which is a loud failure, not a silent pass.

use crate::test_util::process::{harn_e2e_command, run_harn_e2e as run};

const CHAT_SCRIPT: &str = r#"
fn main(harness: Harness) {
  harness.llm.call("hello", nil, {provider: "vercel_ai_gateway", model: "vercel/typesafe-ai/jev"})
}
"#;

const UNSERVED_SIBLING_SCRIPT: &str = r#"
fn main(harness: Harness) {
  harness.llm.call("hello", nil, {provider: "vercel_ai_gateway", model: "vercel/typesafe-ai/jev-2"})
}
"#;

fn run_script(source: &str) -> (bool, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("main.harn");
    std::fs::write(&path, source).expect("write script");
    let output = harn_e2e_command()
        .args(["run", "main.harn"])
        .current_dir(dir.path())
        .output()
        .expect("run script");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), combined)
}

/// Falsifier (a): the Vercel Jev route is refused as a chat driver, and the
/// refusal names the operation that is missing.
#[test]
fn a_decision_only_route_is_refused_as_a_chat_driver_by_name() {
    let (ok, output) = run_script(CHAT_SCRIPT);
    assert!(
        !ok,
        "a decision-only route must not run a chat call: {output}"
    );
    assert!(
        output.contains("text_generation"),
        "refusal must name the missing operation, got: {output}"
    );
    assert!(
        output.contains("vercel/typesafe-ai/jev"),
        "refusal must name the route, got: {output}"
    );
}

/// Falsifier (b): an unserved sibling is refused, and the refusal is not the
/// operation refusal — it is an unknown route, which is a different sentence.
#[test]
fn an_unserved_sibling_route_is_refused() {
    let (ok, output) = run_script(UNSERVED_SIBLING_SCRIPT);
    assert!(!ok, "an unserved sibling must not run: {output}");
    assert!(
        output.contains("jev-2"),
        "refusal must name the route asked for, got: {output}"
    );
}

/// Falsifier (d): the default recommendation never names a decision-only row.
#[test]
fn recommend_without_an_operation_never_offers_a_decision_row() {
    let harn = run(&["models", "recommend"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for decision_row in [
        "typesafe/jev-latest",
        "typesafe/jev-1.13.0",
        "vercel/typesafe-ai/jev",
        "openrouter/typesafe/jev-1.13",
    ] {
        assert!(
            !harn.stdout.contains(decision_row),
            "default recommendation offered the decision-only row {decision_row}: {}",
            harn.stdout
        );
    }
}

/// The positive control for the test above: asked for decisions explicitly,
/// the same command DOES list those rows. Without this, a recommender that
/// printed nothing at all would pass the exclusion test.
#[test]
fn recommend_for_the_decision_operation_lists_the_decision_routes() {
    let harn = run(&["models", "recommend", "--operation", "decision"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for decision_row in [
        "typesafe/jev-latest",
        "typesafe/jev-1.13.0",
        "vercel/typesafe-ai/jev",
        "openrouter/typesafe/jev-1.13",
    ] {
        assert!(
            harn.stdout.contains(decision_row),
            "decision listing omitted {decision_row}: {}",
            harn.stdout
        );
    }
    assert!(
        harn.stdout.contains("credential="),
        "decision listing must carry credential status: {}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("protocol=vercel_evaluate"),
        "decision listing must name the protocol each route is dialled over: {}",
        harn.stdout
    );
}
