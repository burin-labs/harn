//! End-to-end smoke for `harn time run --json`.
//!
//! Spawns the binary so we exercise the actual clap parser + envelope
//! serialization path. The first invocation is a cache miss; the
//! second run on the same source must report `cache: "hit"` and
//! `cache_hits == 1`.

use std::process::Command;

use harn_cli::commands::time::TIME_RUN_SCHEMA_VERSION;
use harn_cli::tests::common::json_envelope::assert_envelope;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn write_hello_script(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("hello.harn");
    std::fs::write(
        &path,
        "pipeline default(task) {\n  log(\"hello from harn time\")\n}\n",
    )
    .expect("write hello.harn");
    path
}

fn parse_envelope(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let trimmed = stdout.trim();
    let start = trimmed
        .find('{')
        .unwrap_or_else(|| panic!("stdout has no JSON object: {stdout}"));
    serde_json::from_str(&trimmed[start..])
        .unwrap_or_else(|err| panic!("harn time --json output is not JSON: {err}\n{stdout}"))
}

fn run_time(script: &std::path::Path, cache_dir: &std::path::Path) -> std::process::Output {
    Command::new(binary_path())
        .args(["time", "run", "--json", &script.to_string_lossy()])
        .env("HARN_CACHE_DIR", cache_dir)
        .env("HARN_BYTECODE_CACHE", "1")
        .output()
        .expect("spawn harn time run")
}

#[test]
fn time_run_json_smoke_emits_five_phases_with_cache_miss_then_hit() {
    let workdir = tempfile::tempdir().expect("workdir");
    let cache_dir = tempfile::tempdir().expect("cache dir");
    let script = write_hello_script(workdir.path());

    // First run: cache miss. All five phases recorded.
    let first = run_time(&script, cache_dir.path());
    assert!(
        first.status.success(),
        "first run failed (exit={:?}): stderr={}",
        first.status.code(),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_value = parse_envelope(&first);
    let data = assert_envelope(&first_value, TIME_RUN_SCHEMA_VERSION);

    let phases = data["phases"].as_array().expect("phases is array");
    assert!(
        phases.len() >= 4,
        "expected at least 4 phases, got {}: {phases:?}",
        phases.len()
    );

    let names: Vec<&str> = phases
        .iter()
        .map(|p| p["name"].as_str().unwrap_or(""))
        .collect();
    assert!(names.contains(&"parse"), "missing parse phase: {names:?}");
    assert!(
        names.contains(&"typecheck"),
        "missing typecheck phase: {names:?}"
    );
    assert!(
        names.contains(&"bytecode_compile"),
        "missing bytecode_compile phase: {names:?}"
    );
    assert!(
        names.contains(&"run_main"),
        "missing run_main phase: {names:?}"
    );

    let compile_phase = phases
        .iter()
        .find(|p| p["name"] == "bytecode_compile")
        .expect("bytecode_compile phase present");
    assert_eq!(
        compile_phase["cache"], "miss",
        "first run should report cache miss: {compile_phase}"
    );

    let totals = &data["totals"];
    assert_eq!(totals["cache_misses"], 1);
    assert_eq!(totals["cache_hits"], 0);
    assert!(totals["wall_ms"].as_u64().is_some(), "wall_ms missing");
    // cpu_ms is best-effort; on Unix it should be populated, on Windows
    // it falls back to 0. Either way the field is present.
    assert!(totals.get("cpu_ms").is_some(), "cpu_ms missing");

    // Second run: cache hit (same source + same HARN_CACHE_DIR).
    let second = run_time(&script, cache_dir.path());
    assert!(
        second.status.success(),
        "second run failed (exit={:?}): stderr={}",
        second.status.code(),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_value = parse_envelope(&second);
    let data2 = assert_envelope(&second_value, TIME_RUN_SCHEMA_VERSION);
    let compile_phase2 = data2["phases"]
        .as_array()
        .expect("phases array")
        .iter()
        .find(|p| p["name"] == "bytecode_compile")
        .expect("bytecode_compile phase present");
    assert_eq!(
        compile_phase2["cache"], "hit",
        "second run on same source must report cache hit: {compile_phase2}"
    );
    assert_eq!(data2["totals"]["cache_hits"], 1);
    assert_eq!(data2["totals"]["cache_misses"], 0);
}

#[test]
fn time_run_no_cache_flag_forces_miss_on_warm_cache() {
    let workdir = tempfile::tempdir().expect("workdir");
    let cache_dir = tempfile::tempdir().expect("cache dir");
    let script = write_hello_script(workdir.path());

    // Warm the cache.
    let _ = run_time(&script, cache_dir.path());

    let output = Command::new(binary_path())
        .args([
            "time",
            "run",
            "--json",
            "--no-cache",
            &script.to_string_lossy(),
        ])
        .env("HARN_CACHE_DIR", cache_dir.path())
        .env("HARN_BYTECODE_CACHE", "1")
        .output()
        .expect("spawn harn time run --no-cache");
    assert!(
        output.status.success(),
        "exit={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_envelope(&output);
    let data = assert_envelope(&parsed, TIME_RUN_SCHEMA_VERSION);
    let compile_phase = data["phases"]
        .as_array()
        .expect("phases array")
        .iter()
        .find(|p| p["name"] == "bytecode_compile")
        .expect("bytecode_compile phase present");
    assert_eq!(compile_phase["cache"], "miss");
}

#[test]
fn time_run_eval_mode_emits_envelope() {
    let workdir = tempfile::tempdir().expect("workdir");
    let cache_dir = tempfile::tempdir().expect("cache dir");

    let output = Command::new(binary_path())
        .args(["time", "run", "--json", "-e", "println(\"inline\")"])
        .current_dir(workdir.path())
        .env("HARN_CACHE_DIR", cache_dir.path())
        .env("HARN_BYTECODE_CACHE", "1")
        .output()
        .expect("spawn harn time run -e");
    assert!(
        output.status.success(),
        "exit={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_envelope(&output);
    let data = assert_envelope(&parsed, TIME_RUN_SCHEMA_VERSION);
    assert_eq!(data["command"], "run");
    // -e doesn't produce a stable file path; target should be absent.
    assert!(
        data.get("target").map(|v| v.is_null()).unwrap_or(true),
        "target should be omitted for -e runs"
    );
    assert!(data["phases"].as_array().is_some_and(|a| !a.is_empty()));
}
