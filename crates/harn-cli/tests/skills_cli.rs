//! End-to-end coverage for `harn skill list / get / dump` against
//! the canonical skill corpus shipped with the binary or loaded from disk.
//!
//! These spawn the real `harn` binary so we exercise the clap parser
//! and `JsonEnvelope` serialization paths that agents and CI consume.

mod test_util;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use harn_cli::json_envelope::CATALOG_SCHEMA_VERSION;
use harn_cli::tests::common::json_envelope::assert_envelope;
use harn_skills::list_embedded_skills;
use serde_json::Value;
use tempfile::TempDir;

use test_util::process::harn_e2e_command;

const SKILLS_LIST_SCHEMA_VERSION: u32 = 1;
const SKILLS_GET_SCHEMA_VERSION: u32 = 1;

fn parse_json_stdout(out: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim_start().starts_with('{'),
        "stdout is not JSON:\n{stdout}"
    );
    serde_json::from_str(stdout.trim()).expect("stdout parses as JSON")
}

#[test]
fn list_json_matches_embedded_corpus_count() {
    let output = harn_command_without_skills_dir()
        .args(["skill", "list", "--json"])
        .output()
        .expect("spawn harn skill list --json");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_json_stdout(&output);
    let data = assert_envelope(&parsed, SKILLS_LIST_SCHEMA_VERSION);

    let skills = data
        .get("skills")
        .and_then(Value::as_array)
        .expect("data.skills is an array");
    let embedded_count = list_embedded_skills().len();
    assert_eq!(
        skills.len(),
        embedded_count,
        "expected {} skills, got {}: {parsed}",
        embedded_count,
        skills.len()
    );

    // Every entry advertises a stable name + short.
    for entry in skills {
        assert!(
            entry.get("name").and_then(Value::as_str).is_some(),
            "missing name: {entry}"
        );
        assert!(
            entry.get("short").and_then(Value::as_str).is_some(),
            "missing short: {entry}"
        );
    }

    // Cross-check against the in-process corpus: names match exactly.
    let advertised: BTreeSet<&str> = skills
        .iter()
        .filter_map(|e| e.get("name").and_then(Value::as_str))
        .collect();
    let embedded: BTreeSet<&str> = list_embedded_skills().iter().map(|s| s.name).collect();
    assert_eq!(advertised, embedded);
}

#[test]
fn list_human_output_mentions_embedded_skills() {
    let output = harn_command_without_skills_dir()
        .args(["skill", "list"])
        .output()
        .expect("spawn harn skill list");
    assert!(output.status.success(), "exit={:?}", output.status.code());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Embedded canonical skills"),
        "human output should call out the embedded corpus:\n{stdout}"
    );
    assert!(
        stdout.contains("harn-language"),
        "human output should list the harn-language skill:\n{stdout}"
    );
}

#[test]
fn list_json_uses_harn_skills_dir_when_it_contains_skills() {
    let temp = TempDir::new().expect("temp dir");
    write_disk_skill(&temp.path().join("custom").join("SKILL.md"), "custom-harn");

    let output = harn_command_without_skills_dir()
        .env("HARN_SKILLS_DIR", temp.path())
        .args(["skill", "list", "--json"])
        .output()
        .expect("spawn harn skill list --json");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_json_stdout(&output);
    let data = assert_envelope(&parsed, SKILLS_LIST_SCHEMA_VERSION);
    let skills = data["skills"].as_array().expect("data.skills is an array");
    assert_eq!(skills.len(), 1, "expected only disk skill: {parsed}");
    assert_eq!(skills[0]["name"], "custom-harn");
}

#[test]
fn list_json_falls_back_to_embedded_when_harn_skills_dir_is_empty() {
    let temp = TempDir::new().expect("temp dir");

    let output = harn_command_without_skills_dir()
        .env("HARN_SKILLS_DIR", temp.path())
        .args(["skill", "list", "--json"])
        .output()
        .expect("spawn harn skill list --json");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_json_stdout(&output);
    let data = assert_envelope(&parsed, SKILLS_LIST_SCHEMA_VERSION);
    let skills = data["skills"].as_array().expect("data.skills is an array");
    assert_eq!(skills.len(), list_embedded_skills().len());
}

#[test]
fn list_json_falls_back_to_embedded_when_harn_skills_dir_is_missing() {
    let temp = TempDir::new().expect("temp dir");

    let output = harn_command_without_skills_dir()
        .env("HARN_SKILLS_DIR", temp.path().join("missing"))
        .args(["skill", "list", "--json"])
        .output()
        .expect("spawn harn skill list --json");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_json_stdout(&output);
    let data = assert_envelope(&parsed, SKILLS_LIST_SCHEMA_VERSION);
    let skills = data["skills"].as_array().expect("data.skills is an array");
    assert_eq!(skills.len(), list_embedded_skills().len());
}

#[test]
fn get_returns_frontmatter_without_body_by_default() {
    let output = harn_command_without_skills_dir()
        .args(["skill", "get", "harn-language", "--json"])
        .output()
        .expect("spawn harn skill get --json");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_json_stdout(&output);
    let data = assert_envelope(&parsed, SKILLS_GET_SCHEMA_VERSION);
    assert_eq!(data["name"], "harn-language");
    assert!(
        data.get("short").and_then(Value::as_str).is_some(),
        "short missing: {data}"
    );
    // Without --full, body is omitted.
    assert!(
        data.get("body").is_none() || data["body"].is_null(),
        "body should be absent without --full: {data}"
    );
}

#[test]
fn get_full_includes_body() {
    let output = harn_command_without_skills_dir()
        .args(["skill", "get", "harn-language", "--full", "--json"])
        .output()
        .expect("spawn harn skill get --full --json");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_json_stdout(&output);
    let data = assert_envelope(&parsed, SKILLS_GET_SCHEMA_VERSION);
    let body = data
        .get("body")
        .and_then(Value::as_str)
        .expect("body present with --full");
    assert!(
        body.contains("Harn language"),
        "body should contain skill content: {body}"
    );
}

#[test]
fn get_full_uses_harn_skills_dir_body() {
    let temp = TempDir::new().expect("temp dir");
    write_disk_skill(
        &temp.path().join("nested").join("custom").join("SKILL.md"),
        "custom-harn",
    );

    let output = harn_command_without_skills_dir()
        .env("HARN_SKILLS_DIR", temp.path())
        .args(["skill", "get", "custom-harn", "--full", "--json"])
        .output()
        .expect("spawn harn skill get --full --json");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_json_stdout(&output);
    let data = assert_envelope(&parsed, SKILLS_GET_SCHEMA_VERSION);
    assert_eq!(data["name"], "custom-harn");
    assert_eq!(data["short"], "custom short");
    let body = data["body"].as_str().expect("body is present");
    assert!(
        body.contains("Custom disk body"),
        "body should come from HARN_SKILLS_DIR: {body}"
    );
}

#[test]
fn get_unknown_skill_emits_error_envelope_and_nonzero_exit() {
    let output = harn_command_without_skills_dir()
        .args(["skill", "get", "not-a-real-skill", "--json"])
        .output()
        .expect("spawn harn skill get not-a-real-skill --json");
    assert!(!output.status.success(), "expected nonzero exit");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["schemaVersion"], SKILLS_GET_SCHEMA_VERSION);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "skill_not_found");
    // Details enumerate available skills so callers can recover.
    let available = parsed["error"]["details"]["available"]
        .as_array()
        .expect("error.details.available is an array");
    assert!(!available.is_empty());
}

#[test]
fn get_human_prints_frontmatter() {
    let output = harn_command_without_skills_dir()
        .args(["skill", "get", "harn-language"])
        .output()
        .expect("spawn harn skill get harn-language");
    assert!(output.status.success(), "exit={:?}", output.status.code());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("name:"),
        "frontmatter missing name: {stdout}"
    );
    assert!(
        stdout.contains("harn-language"),
        "frontmatter should print the requested name: {stdout}"
    );
    assert!(
        !stdout.contains("---- SKILL.md body ----"),
        "body block leaked without --full: {stdout}"
    );
}

#[test]
fn dump_requires_all_flag() {
    let output = harn_command_without_skills_dir()
        .args(["skill", "dump"])
        .output()
        .expect("spawn harn skill dump (no --all)");
    assert!(!output.status.success(), "dump without --all should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--all"),
        "stderr should hint at --all: {stderr}"
    );
}

#[test]
fn dump_all_writes_every_skill_to_out_dir() {
    let temp = TempDir::new().expect("temp dir");
    let out = temp.path().join("dump-target");

    let output = harn_command_without_skills_dir()
        .args(["skill", "dump", "--all", "--out"])
        .arg(&out)
        .output()
        .expect("spawn harn skill dump --all --out <dir>");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    for skill in list_embedded_skills() {
        let path = out.join(skill.name).join("SKILL.md");
        assert!(
            path.exists(),
            "missing dumped SKILL.md for {} at {}",
            skill.name,
            path.display()
        );
        let on_disk = std::fs::read_to_string(&path).expect("read dumped SKILL.md");
        assert_eq!(
            on_disk, skill.source,
            "dumped SKILL.md for `{}` must mirror the embedded source byte-for-byte",
            skill.name
        );
    }
}

#[test]
fn dump_all_uses_harn_skills_dir_when_it_contains_skills() {
    let temp = TempDir::new().expect("temp dir");
    let source = temp
        .path()
        .join("disk-skills")
        .join("custom")
        .join("SKILL.md");
    let out = temp.path().join("dump-target");
    write_disk_skill(&source, "custom-harn");

    let output = harn_command_without_skills_dir()
        .env("HARN_SKILLS_DIR", temp.path().join("disk-skills"))
        .args(["skill", "dump", "--all", "--out"])
        .arg(&out)
        .output()
        .expect("spawn harn skill dump --all --out <dir>");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let dumped = out.join("custom-harn").join("SKILL.md");
    let dumped_source = fs::read_to_string(&dumped).expect("read dumped disk skill");
    let original_source = fs::read_to_string(&source).expect("read source disk skill");
    assert_eq!(dumped_source, original_source);
    assert!(
        !out.join("harn-language").exists(),
        "disk override should dump only disk skills"
    );
}

#[test]
fn dump_refuses_to_overwrite_without_force() {
    let temp = TempDir::new().expect("temp dir");
    let out = temp.path().join("dump-target");

    // First dump succeeds.
    let first = harn_command_without_skills_dir()
        .args(["skill", "dump", "--all", "--out"])
        .arg(&out)
        .output()
        .expect("spawn first dump");
    assert!(first.status.success(), "first dump should succeed");

    // Second dump without --force should refuse.
    let second = harn_command_without_skills_dir()
        .args(["skill", "dump", "--all", "--out"])
        .arg(&out)
        .output()
        .expect("spawn second dump");
    assert!(
        !second.status.success(),
        "second dump without --force should fail"
    );

    // Third dump with --force should succeed.
    let third = harn_command_without_skills_dir()
        .args(["skill", "dump", "--all", "--force", "--out"])
        .arg(&out)
        .output()
        .expect("spawn forced dump");
    assert!(third.status.success(), "forced dump should succeed");
}

#[test]
fn json_schemas_catalog_lists_skills_list_and_get() {
    let output = harn_command_without_skills_dir()
        .args(["--json-schemas"])
        .output()
        .expect("spawn harn --json-schemas");
    assert!(output.status.success(), "exit={:?}", output.status.code());

    let parsed = parse_json_stdout(&output);
    let data = assert_envelope(&parsed, CATALOG_SCHEMA_VERSION);
    let commands: BTreeSet<&str> = data
        .as_array()
        .expect("data is array")
        .iter()
        .filter_map(|e| e["command"].as_str())
        .collect();
    assert!(commands.contains("skills list"), "missing skills list");
    assert!(commands.contains("skills get"), "missing skills get");
}

fn write_disk_skill(path: &Path, name: &str) {
    fs::create_dir_all(path.parent().expect("skill parent")).expect("create skill parent");
    fs::write(
        path,
        format!(
            "---\nname: {name}\nshort: custom short\ndescription: custom description\n---\n# Custom disk skill\n\nCustom disk body\n"
        ),
    )
    .expect("write disk skill");
}

fn harn_command_without_skills_dir() -> Command {
    let mut command = harn_e2e_command();
    command.env_remove("HARN_SKILLS_DIR");
    command
}
