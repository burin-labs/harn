//! CLI coverage for the local workflow supervisor host surface.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn run_harn(args: &[&str]) -> std::process::Output {
    Command::new(binary_path())
        .args(args)
        .env("HARN_SECRET_PROVIDERS", "env")
        .output()
        .expect("spawn harn")
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

#[test]
fn supervisor_pause_resume_fire_and_dlq_json_surfaces() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("fired.txt");
    write_file(
        temp.path(),
        "harn.toml",
        r#"
[package]
name = "supervisor-fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "cron-ok"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "handlers::on_ok"

[[triggers]]
id = "cron-fail"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "handlers::on_fail"
retry = { max = 1, backoff = "immediate", retention_days = 7 }
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        &format!(
            r#"
import "std/triggers"

let marker = "{}"

pub fn on_ok(event: TriggerEvent) -> dict {{
  write_file(marker, event.kind)
  return {{event_id: event.id, kind: event.kind}}
}}

pub fn on_fail(event: TriggerEvent) {{
  throw "intentional:" + event.kind
}}
"#,
            marker.display()
        ),
    );

    let config = temp.path().join("harn.toml");
    let state_dir = temp.path().join("state");
    let config = config.to_str().unwrap();
    let state_dir = state_dir.to_str().unwrap();

    let list = run_harn(&[
        "supervisor",
        "list",
        "--config",
        config,
        "--state-dir",
        state_dir,
        "--json",
    ]);
    assert!(
        list.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list_json = stdout_json(&list);
    assert_eq!(list_json["schema_version"], 1);
    assert_eq!(list_json["workflows"].as_array().unwrap().len(), 2);
    assert!(list_json["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|workflow| workflow["workflow_id"] == "cron-ok" && workflow["status"] == "active"));

    let pause = run_harn(&[
        "supervisor",
        "pause",
        "cron-ok",
        "--config",
        config,
        "--state-dir",
        state_dir,
        "--reason",
        "test pause",
        "--json",
    ]);
    assert!(
        pause.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&pause.stderr)
    );
    let pause_json = stdout_json(&pause);
    assert_eq!(pause_json["status"], "paused");
    assert_eq!(
        pause_json["result"]["workflow"]["notification_hint"]["kind"],
        "burin.resume_workflow"
    );

    let paused_list = run_harn(&[
        "supervisor",
        "list",
        "--config",
        config,
        "--state-dir",
        state_dir,
        "--json",
    ]);
    assert!(paused_list.status.success());
    let paused_json = stdout_json(&paused_list);
    assert!(paused_json["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|workflow| workflow["workflow_id"] == "cron-ok" && workflow["status"] == "paused"));

    let paused_fire = run_harn(&[
        "supervisor",
        "fire",
        "cron-ok",
        "--config",
        config,
        "--state-dir",
        state_dir,
        "--json",
    ]);
    assert!(!paused_fire.status.success());
    assert!(
        String::from_utf8_lossy(&paused_fire.stderr).contains("paused"),
        "stderr={}",
        String::from_utf8_lossy(&paused_fire.stderr)
    );

    let resume = run_harn(&[
        "supervisor",
        "resume",
        "cron-ok",
        "--config",
        config,
        "--state-dir",
        state_dir,
        "--json",
    ]);
    assert!(
        resume.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&resume.stderr)
    );
    assert_eq!(stdout_json(&resume)["status"], "running");

    let fire = run_harn(&[
        "supervisor",
        "fire",
        "cron-ok",
        "--config",
        config,
        "--state-dir",
        state_dir,
        "--json",
    ]);
    assert!(
        fire.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&fire.stderr)
    );
    let fire_json = stdout_json(&fire);
    assert_eq!(fire_json["status"], "dispatched");
    assert_eq!(fire_json["binding_id"], "cron-ok");
    assert_eq!(fs::read_to_string(marker).unwrap(), "cron.tick");

    let dlq_fire = run_harn(&[
        "supervisor",
        "fire",
        "cron-fail",
        "--config",
        config,
        "--state-dir",
        state_dir,
        "--json",
    ]);
    assert!(dlq_fire.status.success());
    assert_eq!(stdout_json(&dlq_fire)["status"], "dlq");

    let dlq = run_harn(&[
        "supervisor",
        "dlq",
        "list",
        "--config",
        config,
        "--state-dir",
        state_dir,
        "--json",
    ]);
    assert!(
        dlq.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&dlq.stderr)
    );
    let dlq_json = stdout_json(&dlq);
    assert_eq!(dlq_json["pending_entries"], 1);
    assert_eq!(dlq_json["entries"][0]["binding_id"], "cron-fail");
}
