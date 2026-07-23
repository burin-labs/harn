//! Dispatch contract tests for `harn routes` + `harn graph` (W11 —
//! harn#2311).
//!
//! Each subcommand's render layer now lives in
//! `crates/harn-stdlib/src/stdlib/cli/{routes,graph}.harn`. The host
//! dispatch shims keep doing the host-only work (manifest cache +
//! IR analyser for routes; collect_harn_targets + build_module_graph +
//! per-module IR walk for graph) and hand a JSON `RoutesReport` /
//! `GraphReport` across the dispatch wedge to the script for
//! formatting.
//!
//! Contract bar:
//!   * Human text: byte-for-byte identity.
//!   * JSON envelopes: structural identity (Harn's
//!     `json_stringify_pretty` sorts dict keys alphabetically; serde
//!     emits struct fields in declaration order, so wire byte order
//!     differs but the parsed shape must match).

use std::fs;
use std::path::{Path, PathBuf};

use crate::test_util;

use test_util::process::run_harn_e2e as run;

use tempfile::TempDir;

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
  const body = read_file("README.md")
  const prompt = render_prompt("prompts/review.harn.prompt", {body: body})
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
    // through repeated runs.
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

// ─── routes contract tests ─────────────────────────────────────────────────

#[test]
fn routes_text_is_byte_identical_across_runs() {
    let temp = TempDir::new().unwrap();
    write_routes_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["routes", root], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let repeat = run(&["routes", root], &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    assert_eq!(harn.stdout, repeat.stdout, "routes text stdout diverged");
}

#[test]
fn routes_json_is_structurally_identical_across_runs() {
    let temp = TempDir::new().unwrap();
    write_routes_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["routes", root, "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let repeat = run(&["routes", root, "--json"], &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "routes JSON envelope diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
    // Sanity-check the wrapping envelope shape — the script must
    // re-emit `schemaVersion: 1` / `ok: true` so consumers can
    // dispatch on the same canonical contract.
    assert_eq!(harn_value["schemaVersion"], 1);
    assert_eq!(harn_value["ok"], true);
    assert!(harn_value["data"]["triggers"].is_array());
}

#[test]
fn routes_empty_manifest_is_byte_identical_across_runs() {
    let temp = TempDir::new().unwrap();
    write_routes_empty_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["routes", root], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let repeat = run(&["routes", root], &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    assert_eq!(
        harn.stdout, repeat.stdout,
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
fn routes_single_cron_no_path_byte_identical_across_runs() {
    // Cron triggers omit `path` entirely in the host report — verify the
    // script renders `-` in the path column and drops `path` from the
    // JSON envelope to match the current `skip_serializing_if = "Option::is_none"`.
    let temp = TempDir::new().unwrap();
    write_routes_single_cron_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn_text = run(&["routes", root], &[]);
    let repeat_text = run(&["routes", root], &[]);
    assert_eq!(harn_text.exit_code, 0, "harn stderr={}", harn_text.stderr);
    assert_eq!(
        repeat_text.exit_code, 0,
        "repeat stderr={}",
        repeat_text.stderr
    );
    assert_eq!(harn_text.stdout, repeat_text.stdout);

    let harn_json = run(&["routes", root, "--json"], &[]);
    let repeat_json = run(&["routes", root, "--json"], &[]);
    let harn_value = parse_json(&harn_json.stdout, "harn");
    let repeat_value = parse_json(&repeat_json.stdout, "repeat");
    assert_eq!(repeat_value, harn_value);
    assert!(
        harn_value["data"]["triggers"][0].get("path").is_none(),
        "cron trigger should omit `path` field, got: {harn_value}"
    );
}

#[test]
fn routes_missing_manifest_errors_byte_identical_across_runs() {
    // Pointing `harn routes` at a directory without `harn.toml` must
    // surface the same `no harn.toml found from <path>` stderr line on
    // repeated runs. The shim renders the error envelope on the host side
    // before dispatch, so this verifies the error path doesn't drift.
    let temp = TempDir::new().unwrap();
    let root = fixture_root(&temp);
    let harn = run(&["routes", root], &[]);
    let repeat = run(&["routes", root], &[]);
    assert_eq!(harn.exit_code, 1, "harn stderr={}", harn.stderr);
    assert_eq!(repeat.exit_code, 1, "repeat stderr={}", repeat.stderr);
    assert_eq!(harn.stderr, repeat.stderr, "routes error stderr diverged");
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
    let repeat = run(&["routes", root, "--json"], &[]);
    assert_eq!(harn.exit_code, 1, "harn stderr={}", harn.stderr);
    assert_eq!(repeat.exit_code, 1, "repeat stderr={}", repeat.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(repeat_value, harn_value);
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
  const title = format_title("Graph")
  const body = read_file("README.md")
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
  const prompt = "Title: " + value
  const _response = llm_call(prompt, {provider: "auto"})
  return value
}
"#,
    )
    .unwrap();
    fs::write(
        root.join("types.harn"),
        r"
type Thing = {name: string}
",
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
 * @errors: [FileNotFound]
 * @api_stability: experimental
 * @example: read_file("README.md")
 */
pub fn read_file(path: string) -> string {
  return path
}

/**
 * Count lines.
 *
 * @effects: []
 * @errors: []
 */
pub fn count_lines(text: string) -> int {
  return 1
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
  const body = harness.fs.read_text("README.md")
  harness.fs.mkdtemp("harn-graph-")
  harness.net.get("https://example.test/data")
  harness.term.width()
  harness.stdio.println(body)
}
"#,
    )
    .unwrap();
}

// ─── graph contract tests ──────────────────────────────────────────────────

#[test]
fn graph_text_is_byte_identical_across_runs() {
    let temp = TempDir::new().unwrap();
    write_graph_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let repeat = run(&["graph", root], &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    assert_eq!(harn.stdout, repeat.stdout, "graph text stdout diverged");
}

#[test]
fn graph_json_is_structurally_identical_across_runs() {
    let temp = TempDir::new().unwrap();
    write_graph_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root, "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let repeat = run(&["graph", root, "--json"], &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "graph JSON envelope diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
    assert_eq!(harn_value["schemaVersion"], 1);
    assert_eq!(harn_value["ok"], true);
}

#[test]
fn graph_metadata_round_trips_byte_identical_across_runs() {
    // Public fns with declared stdlib metadata frontmatter must round-
    // trip the parsed dict through serde -> JSON env var -> json_parse
    // unchanged. The dispatch shim hands the metadata as a serialised
    // `StdlibMetadata` (snake_case-keyed) and the script must echo it
    // verbatim through the envelope.
    let temp = TempDir::new().unwrap();
    write_graph_metadata_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root, "--json"], &[]);
    let repeat = run(&["graph", root, "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(repeat_value, harn_value);
    // Symbols sort by name: count_lines first, read_file second.
    let count_lines = &harn_value["data"]["modules"][0]["public_symbols"][0];
    assert_eq!(count_lines["name"], "count_lines");
    // No authored @example → the derived one must survive the dispatch
    // round-trip identically in repeated runs.
    assert_eq!(
        count_lines["derived_example"],
        "const out = count_lines(text)"
    );
    let read_file = &harn_value["data"]["modules"][0]["public_symbols"][1];
    assert_eq!(read_file["metadata"]["api_stability"], "experimental");
    assert_eq!(read_file["metadata"]["example"], "read_file(\"README.md\")");
    assert!(read_file.get("derived_example").is_none());
}

#[test]
fn graph_harness_sub_calls_byte_identical_across_runs() {
    let temp = TempDir::new().unwrap();
    write_graph_harness_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root], &[]);
    let repeat = run(&["graph", root], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    assert_eq!(harn.stdout, repeat.stdout);
    // The harness.fs / harness.net classifier surfaces workspace
    // read/write requirements and network.http; the text path must
    // list them on a `requires` line.
    assert!(
        harn.stdout.contains("requires "),
        "expected `requires ` line, got: {}",
        harn.stdout
    );
}

#[test]
fn graph_module_filter_byte_identical_across_runs() {
    let temp = TempDir::new().unwrap();
    write_graph_fixture(temp.path());
    let root = fixture_root(&temp);
    let harn = run(&["graph", root, "--module", "util"], &[]);
    let repeat = run(&["graph", root, "--module", "util"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    assert_eq!(harn.stdout, repeat.stdout);
}

#[test]
fn graph_missing_root_errors_byte_identical_across_runs() {
    // Pointing `harn graph` at a non-existent path must surface the
    // same stderr line on repeated runs.
    let bogus = PathBuf::from("/tmp/harn-graph-port-does-not-exist-9f3d2");
    let root = bogus.to_str().unwrap();
    let harn = run(&["graph", root], &[]);
    let repeat = run(&["graph", root], &[]);
    assert_eq!(harn.exit_code, 1, "harn stderr={}", harn.stderr);
    assert_eq!(repeat.exit_code, 1, "repeat stderr={}", repeat.stderr);
    assert_eq!(harn.stderr, repeat.stderr, "graph error stderr diverged");
}
