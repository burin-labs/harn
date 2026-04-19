use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_manifest(dir: &Path) {
    write_file(
        dir,
        "harn.toml",
        r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_issue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
    );
}

fn write_seed_script(dir: &Path, dedupe_key: &str) {
    write_file(
        dir,
        "seed.harn",
        &format!(
            r#"
import "std/triggers"

pipeline default() {{
  println(json_stringify(trigger_fire("github-new-issue", {{
    provider: "github",
    kind: "issues.opened",
    dedupe_key: "{dedupe_key}",
    provider_payload: {{
      provider: "github",
      event: "issues",
      action: "opened",
      delivery_id: "{dedupe_key}",
      installation_id: 42,
      raw: {{ action: "opened" }},
    }},
    signature_status: {{ state: "verified" }},
  }})))
}}
"#
        ),
    );
}

fn run_harn(temp: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(temp.path())
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn seed_event(temp: &TempDir, dedupe_key: &str) -> serde_json::Value {
    write_seed_script(temp.path(), dedupe_key);
    let output = run_harn(temp, &["run", "seed.harn"]);
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        stdout(&output),
        stderr(&output)
    );
    serde_json::from_str(stdout(&output).trim()).unwrap()
}

#[test]
fn trigger_replay_diff_reports_structured_drift() {
    let temp = TempDir::new().unwrap();
    write_manifest(temp.path());
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) -> dict {
  return {
    event_id: event.id,
    replay_env: env("HARN_REPLAY"),
    child_replay_env: shell("printf '%s' \"$HARN_REPLAY\"").stdout,
  }
}
"#,
    );

    let seeded = seed_event(&temp, "delivery-diff");
    let event_id = seeded["event_id"].as_str().unwrap().to_string();

    let output = run_harn(&temp, &["trigger", "replay", &event_id, "--diff"]);
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        stdout(&output),
        stderr(&output)
    );

    let report: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["event_id"].as_str(), Some(event_id.as_str()));
    assert_eq!(report["replay"]["status"].as_str(), Some("succeeded"));
    assert_eq!(report["original"]["status"].as_str(), Some("succeeded"));
    assert_eq!(report["drift"]["changed"].as_bool(), Some(true));
    assert_eq!(
        report["drift"]["fields"]["result"]["original"]["replay_env"],
        serde_json::Value::Null
    );
    assert_eq!(
        report["drift"]["fields"]["result"]["replayed"]["replay_env"],
        serde_json::json!("1")
    );
    assert_eq!(
        report["drift"]["fields"]["result"]["original"]["child_replay_env"],
        serde_json::Value::String(String::new())
    );
    assert_eq!(
        report["drift"]["fields"]["result"]["replayed"]["child_replay_env"],
        serde_json::json!("1")
    );
}

#[test]
fn trigger_replay_as_of_uses_historical_binding_version() {
    let temp = TempDir::new().unwrap();
    write_manifest(temp.path());
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) -> dict {
  return { version: "v1" }
}
"#,
    );

    let seeded = seed_event(&temp, "delivery-as-of-v1");
    let event_id = seeded["event_id"].as_str().unwrap().to_string();
    let as_of = OffsetDateTime::now_utc();
    std::thread::sleep(std::time::Duration::from_millis(10));

    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) -> dict {
  return { version: "v2" }
}
"#,
    );
    let _ = seed_event(&temp, "delivery-as-of-v2");

    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) -> dict {
  return { version: "v1" }
}
"#,
    );

    let as_of = as_of.format(&Rfc3339).unwrap();
    let output = run_harn(&temp, &["trigger", "replay", &event_id, "--as-of", &as_of]);
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        stdout(&output),
        stderr(&output)
    );

    let report: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["binding_version"].as_u64(), Some(1));
    assert_eq!(report["replay"]["result"]["version"].as_str(), Some("v1"));
    assert_eq!(report["as_of"].as_str(), Some(as_of.as_str()));
}
