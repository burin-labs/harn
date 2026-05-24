#![recursion_limit = "256"]

//! Partial-port verification for `harn routes` + `harn graph` (W11 —
//! harn#2311).
//!
//! Each subcommand's render layer now lives in
//! `crates/harn-stdlib/src/stdlib/cli/{routes,graph}.harn`. The Rust
//! dispatch shims keep doing the host-only work (manifest cache +
//! IR analyser for routes; collect_harn_targets + build_module_graph +
//! per-module IR walk for graph) and hand a JSON `RoutesReport` /
//! `GraphReport` across the dispatch wedge to the script for
//! formatting.
//!
//! The `HARN_CLI_IMPL=rust` escape hatch keeps the legacy direct path
//! so this test can compare both impls at runtime until the C1
//! ratchet (#2314) deletes it.
//!
//! Parity bar:
//!   * Human text: byte-for-byte identity.
//!   * JSON envelopes: structural identity (Harn's
//!     `json_stringify_pretty` sorts dict keys alphabetically; serde
//!     emits struct fields in declaration order, so wire byte order
//!     differs but the parsed shape must match).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn harn_binary() -> &'static str {
    env!("CARGO_BIN_EXE_harn")
}

struct SubprocessOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn run(argv: &[&str], extra_env: &[(&str, &str)]) -> SubprocessOutcome {
    let mut cmd = Command::new(harn_binary());
    for arg in argv {
        cmd.arg(arg);
    }
    for key in ["HARN_CLI_IMPL", "NO_COLOR", "HARN_COLOR"] {
        cmd.env_remove(key);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let Output {
        status,
        stdout,
        stderr,
    } = cmd.output().expect("spawn harn");
    SubprocessOutcome {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code: status.code().unwrap_or(-1),
    }
}

fn parse_json(s: &str, label: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|err| {
        panic!("{label} stdout is not valid JSON: {err}\n--- payload ---\n{s}")
    })
}

// ─── routes fixtures ─────────────────────────────────────────────────────

fn write_routes_fixture(root: &Path) {
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

fn write_routes_empty_fixture(root: &Path) {
    // Minimal manifest with no triggers — exercises the empty-table
    // branch on the human render path and the empty-list JSON shape.
    fs::write(
        root.join("harn.toml"),
        r#"
[package]
name = "empty-fixture"
"#,
    )
    .unwrap();
}

fn write_routes_single_cron_fixture(root: &Path) {
    // Single cron trigger that resolves through `worker://` (no local
    // handler module). Exercises the no-path + non-local-handler path
    // through both impls.
    fs::write(
        root.join("harn.toml"),
        r#"
[package]
name = "single-cron-fixture"

[[triggers]]
id = "hourly-poll"
kind = "cron"
provider = "cron"
match = { events = ["cron.tick"] }
schedule = "0 * * * *"
handler = "worker://poll"
"#,
    )
    .unwrap();
}

fn fixture_root(temp: &TempDir) -> &str {
    temp.path().to_str().expect("temp path utf8")
}

// ─── routes parity tests ─────────────────────────────────────────────────

#[test]
fn routes_text_is_byte_identical_between_impls() {
    let temp = TempDir::new().unwrap();
    write_routes_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["routes", root], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(&["routes", root], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(harn.stdout, rust.stdout, "routes text stdout diverged");
}

#[test]
fn routes_json_is_structurally_identical_between_impls() {
    let temp = TempDir::new().unwrap();
    write_routes_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["routes", root, "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(&["routes", root, "--json"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let rust_value = parse_json(&rust.stdout, "rust");
    assert_eq!(
        rust_value, harn_value,
        "routes JSON envelope diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust.stdout, harn.stdout
    );
    // Sanity-check the wrapping envelope shape — the script must
    // re-emit `schemaVersion: 1` / `ok: true` so consumers can
    // dispatch on the same canonical contract.
    assert_eq!(harn_value["schemaVersion"], 1);
    assert_eq!(harn_value["ok"], true);
    assert!(harn_value["data"]["triggers"].is_array());
}

#[test]
fn routes_empty_manifest_is_byte_identical_between_impls() {
    let temp = TempDir::new().unwrap();
    write_routes_empty_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["routes", root], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(&["routes", root], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(
        harn.stdout, rust.stdout,
        "routes empty text stdout diverged"
    );
    // Header row must still be emitted on the empty path.
    assert!(
        harn.stdout.contains("capabilities"),
        "expected header row in stdout, got: {}",
        harn.stdout
    );
}

#[test]
fn routes_single_cron_no_path_byte_identical_between_impls() {
    // Cron triggers omit `path` entirely on the Rust side — verify the
    // script renders `-` in the path column and drops `path` from the
    // JSON envelope to match the legacy `skip_serializing_if = "Option::is_none"`.
    let temp = TempDir::new().unwrap();
    write_routes_single_cron_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn_text = run(&["routes", root], &[]);
    let rust_text = run(&["routes", root], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn_text.exit_code, 0, "harn stderr={}", harn_text.stderr);
    assert_eq!(rust_text.exit_code, 0, "rust stderr={}", rust_text.stderr);
    assert_eq!(harn_text.stdout, rust_text.stdout);

    let harn_json = run(&["routes", root, "--json"], &[]);
    let rust_json = run(&["routes", root, "--json"], &[("HARN_CLI_IMPL", "rust")]);
    let harn_value = parse_json(&harn_json.stdout, "harn");
    let rust_value = parse_json(&rust_json.stdout, "rust");
    assert_eq!(rust_value, harn_value);
    assert!(
        harn_value["data"]["triggers"][0].get("path").is_none(),
        "cron trigger should omit `path` field, got: {harn_value}"
    );
}

#[test]
fn routes_missing_manifest_errors_byte_identical_between_impls() {
    // Pointing `harn routes` at a directory without `harn.toml` must
    // surface the same `no harn.toml found from <path>` stderr line on
    // both impls. The shim renders the error envelope on the Rust side
    // before dispatch, so this verifies the error path doesn't drift.
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let harn = run(&["routes", root], &[]);
    let rust = run(&["routes", root], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.exit_code, 1, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 1, "rust stderr={}", rust.stderr);
    assert_eq!(harn.stderr, rust.stderr, "routes error stderr diverged");
    assert!(
        harn.stderr.contains("no harn.toml"),
        "expected `no harn.toml` in stderr, got: {}",
        harn.stderr
    );
}

#[test]
fn routes_missing_manifest_error_envelope_structurally_identical() {
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let harn = run(&["routes", root, "--json"], &[]);
    let rust = run(&["routes", root, "--json"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.exit_code, 1, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 1, "rust stderr={}", rust.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let rust_value = parse_json(&rust.stdout, "rust");
    assert_eq!(rust_value, harn_value);
    assert_eq!(harn_value["ok"], false);
    assert_eq!(harn_value["error"]["code"], "routes_error");
}

// ─── graph fixtures ──────────────────────────────────────────────────────

fn write_graph_fixture(root: &Path) {
    fs::write(
        root.join("main.harn"),
        r#"
import { format_title } from "util"

pub fn main() -> string {
  let title = format_title("Graph")
  let body = read_file("README.md")
  return title + body
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("util.harn"),
        r#"
import { Thing } from "types"

pub fn format_title(value: string) -> string {
  let prompt = "Title: " + value
  let _response = llm_call(prompt, {provider: "auto"})
  return value
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("types.harn"),
        r#"
type Thing = {name: string}
"#,
    )
    .unwrap();
}

fn write_graph_metadata_fixture(root: &Path) {
    fs::write(
        root.join("annotated.harn"),
        r#"
/**
 * Read a file and return its contents.
 *
 * @effects: [fs.read]
 * @allocation: heap
 * @errors: [FileNotFound]
 * @api_stability: stable
 * @example: read_file("README.md")
 */
pub fn read_file(path: string) -> string {
  return path
}
"#,
    )
    .unwrap();
}

fn write_graph_harness_fixture(root: &Path) {
    fs::write(
        root.join("main.harn"),
        r#"
fn main(harness: Harness) {
  let body = harness.fs.read_text("README.md")
  harness.net.get("https://example.test/data")
  harness.stdio.println(body)
}
"#,
    )
    .unwrap();
}

// ─── graph parity tests ──────────────────────────────────────────────────

#[test]
fn graph_text_is_byte_identical_between_impls() {
    let temp = TempDir::new().unwrap();
    write_graph_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(&["graph", root], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(harn.stdout, rust.stdout, "graph text stdout diverged");
}

#[test]
fn graph_json_is_structurally_identical_between_impls() {
    let temp = TempDir::new().unwrap();
    write_graph_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root, "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(&["graph", root, "--json"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let rust_value = parse_json(&rust.stdout, "rust");
    assert_eq!(
        rust_value, harn_value,
        "graph JSON envelope diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust.stdout, harn.stdout
    );
    assert_eq!(harn_value["schemaVersion"], 1);
    assert_eq!(harn_value["ok"], true);
}

#[test]
fn graph_metadata_round_trips_byte_identical_between_impls() {
    // Public fns with declared stdlib metadata frontmatter must round-
    // trip the parsed dict through serde -> JSON env var -> json_parse
    // unchanged. The dispatch shim hands the metadata as a serialised
    // `StdlibMetadata` (snake_case-keyed) and the script must echo it
    // verbatim through the envelope.
    let temp = TempDir::new().unwrap();
    write_graph_metadata_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root, "--json"], &[]);
    let rust = run(&["graph", root, "--json"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let rust_value = parse_json(&rust.stdout, "rust");
    assert_eq!(rust_value, harn_value);
    let read_file = &harn_value["data"]["modules"][0]["public_symbols"][0];
    assert_eq!(read_file["metadata"]["allocation"], "heap");
    assert_eq!(read_file["metadata"]["api_stability"], "stable");
}

#[test]
fn graph_harness_sub_calls_byte_identical_between_impls() {
    let temp = TempDir::new().unwrap();
    write_graph_harness_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root], &[]);
    let rust = run(&["graph", root], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(harn.stdout, rust.stdout);
    // The harness.fs / harness.net classifier surfaces both
    // workspace.read_text and network.http; the text path must list
    // them on a `requires` line.
    assert!(
        harn.stdout.contains("requires "),
        "expected `requires ` line, got: {}",
        harn.stdout
    );
}

#[test]
fn graph_module_filter_byte_identical_between_impls() {
    let temp = TempDir::new().unwrap();
    write_graph_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root, "--module", "util"], &[]);
    let rust = run(
        &["graph", root, "--module", "util"],
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(harn.stdout, rust.stdout);
}

#[test]
fn graph_missing_root_errors_byte_identical_between_impls() {
    // Pointing `harn graph` at a non-existent path must surface the
    // same stderr line on both impls.
    let bogus = PathBuf::from("/tmp/harn-graph-port-does-not-exist-9f3d2");
    let root = bogus.to_str().unwrap();
    let harn = run(&["graph", root], &[]);
    let rust = run(&["graph", root], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.exit_code, 1, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 1, "rust stderr={}", rust.stderr);
    assert_eq!(harn.stderr, rust.stderr, "graph error stderr diverged");
}
