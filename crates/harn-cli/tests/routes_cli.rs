//! End-to-end coverage for `harn routes --json`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn run_harn(args: &[&str]) -> std::process::Output {
    Command::new(binary_path())
        .args(args)
        .output()
        .expect("spawn harn")
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

fn write_fixture(root: &Path) {
    fs::create_dir_all(root.join("prompts")).unwrap();
    fs::write(
        root.join("harn.toml"),
        r#"
[package]
name = "routes-fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "webhook-review"
kind = "webhook"
provider = "webhook"
path = "/hooks/review"
match = { events = ["review.created"] }
handler = "handlers::on_review"
when = "handlers::should_handle"
when_budget = { max_cost_usd = 0.01, tokens_max = 20, timeout = "3s" }
budget = { max_cost_usd = 0.20, max_tokens = 1000, daily_cost_usd = 5.0, max_concurrent = 2 }
timeout = "4s"
secrets = { signing_secret = "webhook/signing-secret" }

[[triggers]]
id = "match-path"
kind = "webhook"
provider = "webhook"
match = { path = "/hooks/from-match", events = ["review.updated"] }
handler = "handlers::portable"
secrets = { signing_secret = "webhook/signing-secret" }

[[triggers]]
id = "daily"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
schedule = "0 9 * * *"
handler = "worker://daily"
budget = { max_cost_usd = 0.02 }
"#,
    )
    .unwrap();
    fs::write(
        root.join("lib.harn"),
        r#"
import "std/triggers"
import { post_message } from "std/connectors/slack"

pub fn on_review(event: TriggerEvent) -> dict {
  let body = read_file("README.md")
  let prompt = render_prompt("prompts/review.harn.prompt", {body: body})
  http_post("https://example.test/hook", prompt)
  return {ok: true}
}

pub fn should_handle(event: TriggerEvent) -> bool {
  return true
}

pub fn portable(event: TriggerEvent) -> dict {
  return {ok: true}
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("prompts/review.harn.prompt"),
        "Review {{ body }}\n{{ if body }}has body{{ endif }}\n",
    )
    .unwrap();
}

#[test]
fn routes_json_reports_manifest_trigger_inventory() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());

    let root = temp.path().to_str().unwrap();
    let output = run_harn(&["routes", root, "--json"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = stdout_json(&output);
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["ok"], true);

    let triggers = parsed["data"]["triggers"].as_array().unwrap();
    assert_eq!(triggers.len(), 3);

    let webhook = triggers
        .iter()
        .find(|trigger| trigger["id"] == "webhook-review")
        .unwrap();
    assert_eq!(webhook["kind"], "webhook");
    assert_eq!(webhook["provider"], "webhook");
    assert_eq!(webhook["path"], "/hooks/review");
    assert_eq!(webhook["module"], "lib.harn");
    assert_eq!(webhook["handler"], "on_review");
    assert_eq!(webhook["events"], serde_json::json!(["review.created"]));
    assert_eq!(webhook["budgets"]["max_latency_ms"], 4000);
    assert_eq!(webhook["budgets"]["max_cost_usd"], 0.20);
    assert_eq!(webhook["budgets"]["max_tokens"], 1000);
    assert_eq!(webhook["budgets"]["max_concurrent"], 2);
    assert_eq!(webhook["vendor_locked"], true);
    assert!(webhook["framework_overhead_tokens"].as_u64().unwrap() > 0);
    assert_eq!(
        webhook["requires_capabilities"],
        serde_json::json!(["network.http", "template.render", "workspace.read_text"])
    );

    let match_path = triggers
        .iter()
        .find(|trigger| trigger["id"] == "match-path")
        .unwrap();
    assert_eq!(match_path["path"], "/hooks/from-match");

    let cron = triggers
        .iter()
        .find(|trigger| trigger["id"] == "daily")
        .unwrap();
    assert!(cron.get("path").is_none());
    assert_eq!(cron["handler"], "worker://daily");
    assert_eq!(
        cron["requires_capabilities"],
        serde_json::json!(["worker.dispatch"])
    );
}

#[test]
fn routes_appears_in_json_schemas_catalog() {
    let output = run_harn(&["--json-schemas", "--command", "routes"]);
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = stdout_json(&output);
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["ok"], true);
    let entries = parsed["data"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["command"], "routes");
    assert_eq!(entries[0]["schemaVersion"], 1);
}
