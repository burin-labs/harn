#![recursion_limit = "256"]

//! Dispatch contract tests for the provider cluster: `provider catalog show`,
//! `provider probe`, `provider tool-probe`, and `provider catalog recommend`
//! (harn#2310 / W10).
//!
//! Each subcommand's render pipeline now lives in
//! `crates/harn-stdlib/src/stdlib/cli/providers/*.harn`. The host
//! dispatch shims keep the host-only work (HTTP probes against
//! `/v1/models` and Ollama `/api/ps`, the tool-conformance fixture
//! classifier, the readiness-report disk reader) and hand a JSON
//! payload across the dispatch wedge to the script for formatting.
//!
//! Contract bar:
//!   * Human text: byte-for-byte identity.
//!   * JSON envelopes: structural identity (Harn's
//!     `json_stringify_pretty` sorts dict keys alphabetically; serde
//!     emits struct fields in declaration order, so wire byte order
//!     differs but the parsed shape must match).

use std::path::Path;

mod test_util;

use test_util::process::run_harn_e2e as run;

fn parse_json(s: &str, label: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|err| {
        panic!("{label} stdout is not valid JSON: {err}\n--- payload ---\n{s}")
    })
}

// ─── provider catalog show ────────────────────────────────────────────────────

/// Catalog dump is JSON-only on repeated runs. Compare structurally
/// because `json_stringify` sorts dict keys alphabetically while serde
/// emits in declaration order — byte order differs but the parsed
/// shape must match.
#[test]
fn provider_catalog_full_is_structurally_identical_across_runs() {
    let harn = run(&["provider", "catalog", "show"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let repeat = run(&["provider", "catalog", "show"], &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "provider catalog show shape diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

#[test]
fn provider_catalog_available_only_is_structurally_identical_across_runs() {
    let harn = run(&["provider", "catalog", "show", "--available-only"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let repeat = run(&["provider", "catalog", "show", "--available-only"], &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "provider catalog show --available-only shape diverged"
    );
    // Sanity: --available-only must be a subset (by name) of full.
    let full = run(&["provider", "catalog", "show"], &[]);
    let full_value = parse_json(&full.stdout, "full");
    let full_names: std::collections::BTreeSet<String> = full_value["providers"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|p| p["name"].as_str().map(str::to_string))
        .collect();
    for name in harn_value["providers"].as_array().unwrap_or(&Vec::new()) {
        let name_str = name["name"].as_str().expect("name field");
        assert!(
            full_names.contains(name_str),
            "available-only provider {name_str} missing from full catalog"
        );
    }
}

/// Catalog must always carry the six top-level keys the downstream
/// consumers (downstream hosts, the JSON schema, the eval JSON aggregator)
/// depend on. Pin the keyset directly on repeated runs so a drift in
/// either path shows up here rather than in a downstream regression.
#[test]
fn provider_catalog_carries_canonical_top_level_keyset() {
    let harn = run(&["provider", "catalog", "show"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let repeat = run(&["provider", "catalog", "show"], &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    for label in [("harn", &harn), ("repeat", &repeat)] {
        let (name, outcome) = label;
        let value = parse_json(&outcome.stdout, name);
        for key in [
            "providers",
            "known_model_names",
            "available_providers",
            "aliases",
            "models",
            "qc_defaults",
        ] {
            assert!(
                value.get(key).is_some(),
                "{name} catalog missing top-level key {key}: {}",
                outcome.stdout
            );
        }
    }
}

/// `--available-only` is the only switch on the catalog command; pin
/// that it returns a non-empty list under realistic conditions (Harn
/// always has the local/mlx/ollama family available without API keys).
#[test]
fn provider_catalog_available_only_includes_local_provider_family() {
    let harn = run(&["provider", "catalog", "show", "--available-only"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let repeat = run(&["provider", "catalog", "show", "--available-only"], &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    for label in [("harn", &harn), ("repeat", &repeat)] {
        let (name, outcome) = label;
        let value = parse_json(&outcome.stdout, name);
        let provider_names: std::collections::BTreeSet<String> = value["available_providers"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        // At least one of the always-on local providers must be in
        // the available set. Don't pin the exact name in case the
        // canonical set shifts; just require non-empty intersection.
        let always_on = ["ollama", "mlx", "llamacpp", "local"];
        assert!(
            always_on.iter().any(|p| provider_names.contains(*p)),
            "{name} --available-only missing every local provider: {provider_names:?}"
        );
    }
}

// ─── provider probe ──────────────────────────────────────────────────────

/// Mock provider always reports ready, so this exit code is
/// deterministic.
#[test]
fn provider_probe_mock_json_is_structurally_identical_across_runs() {
    let harn = run(&["provider", "probe", "mock"], &[]);
    let repeat = run(&["provider", "probe", "mock"], &[]);
    assert_eq!(harn.exit_code, repeat.exit_code, "exit code diverged");
    // Default for provider probe is JSON (clap default_value_t = true).
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    // Compare a stable subset — `readiness.message` and `available_models`
    // can vary tiny amounts (status code text differs between back-to-back
    // probes); pin the structural keys instead.
    for key in ["provider", "readiness"] {
        assert!(
            harn_value.get(key).is_some(),
            "harn missing key {key}: {}",
            harn.stdout
        );
        assert!(
            repeat_value.get(key).is_some(),
            "repeat missing key {key}: {}",
            repeat.stdout
        );
    }
    assert_eq!(
        repeat_value["provider"], harn_value["provider"],
        "provider field diverged"
    );
    assert_eq!(
        repeat_value["readiness"]["ok"], harn_value["readiness"]["ok"],
        "readiness.ok diverged"
    );
}

/// Human form for the human render path (`--json=false`). For mock
/// the readiness message is stable.
#[test]
fn provider_probe_mock_human_is_byte_identical_across_runs() {
    let harn = run(&["provider", "probe", "mock", "--json=false"], &[]);
    let repeat = run(&["provider", "probe", "mock", "--json=false"], &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    assert_eq!(
        harn.stdout, repeat.stdout,
        "provider probe mock human stdout diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

// ─── provider tool-probe (fixture mode) ──────────────────────────────────

/// Fixture-mode tool probe runs offline and is deterministic. Create a
/// minimal valid response on the fly so the test doesn't depend on a
/// committed fixture path.
#[test]
fn provider_tool_probe_fixture_json_is_structurally_identical_across_runs() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("response.json");
    write_minimal_tool_fixture(&fixture);
    let path = fixture.to_string_lossy().into_owned();
    let argv = [
        "provider",
        "tool-probe",
        "mock",
        "--model",
        "mock",
        "--response-fixture",
        path.as_str(),
    ];
    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "tool-probe fixture JSON diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

/// Human form for fixture-mode: classification debug name, fallback
/// mode, per-case reason. Must be byte-identical because the fixture
/// is deterministic.
#[test]
fn provider_tool_probe_fixture_human_is_byte_identical_across_runs() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("response.json");
    write_minimal_tool_fixture(&fixture);
    let path = fixture.to_string_lossy().into_owned();
    let argv = [
        "provider",
        "tool-probe",
        "mock",
        "--model",
        "mock",
        "--response-fixture",
        path.as_str(),
        "--json=false",
    ];
    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    assert_eq!(
        harn.stdout, repeat.stdout,
        "tool-probe fixture human stdout diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

#[test]
fn provider_tool_probe_fixture_rejects_repeat() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("response.json");
    write_minimal_tool_fixture(&fixture);
    let path = fixture.to_string_lossy().into_owned();
    let output = run(
        &[
            "provider",
            "tool-probe",
            "mock",
            "--model",
            "mock",
            "--response-fixture",
            path.as_str(),
            "--repeat",
            "5",
        ],
        &[],
    );
    assert_eq!(output.exit_code, 1, "stderr={}", output.stderr);
    assert!(
        output
            .stderr
            .contains("--repeat is only supported for live"),
        "stderr={}",
        output.stderr
    );
}

fn write_minimal_tool_fixture(path: &Path) {
    // An OpenAI-style response body the fixture classifier accepts.
    // The marker matches the default `DEFAULT_TOOL_PROBE_MARKER`. Even
    // a "prose-only" classification still produces a stable
    // ToolConformanceReport, which is what the contract test cares
    // about.
    let body = r#"{"choices":[{"message":{"role":"assistant","content":"hello world","tool_calls":null}}]}"#;
    std::fs::write(path, body).expect("write fixture");
}

// ─── provider catalog recommend ────────────────────────────────────────────────

/// `provider catalog recommend` reads from disk or the default summary cache
/// and renders the filtered report. The default-load path is
/// deterministic on a clean system.
#[test]
fn providers_recommend_human_is_byte_identical_across_runs() {
    // The default `load_default_report` returns the bundled seed report
    // when no on-disk readiness file is present.
    let harn = run(&["provider", "catalog", "recommend"], &[]);
    let repeat = run(&["provider", "catalog", "recommend"], &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    assert_eq!(
        harn.stdout, repeat.stdout,
        "provider catalog recommend human stdout diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

#[test]
fn providers_recommend_json_is_structurally_identical_across_runs() {
    let harn = run(&["provider", "catalog", "recommend", "--json"], &[]);
    let repeat = run(&["provider", "catalog", "recommend", "--json"], &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "provider catalog recommend JSON shape diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

#[test]
fn providers_recommend_provider_filter_human_is_byte_identical_across_runs() {
    // Filtering by a non-existent provider exercises the empty-list
    // branch ("(no local model outcomes found)") which is its own
    // render path.
    let harn = run(
        &[
            "provider",
            "catalog",
            "recommend",
            "--provider",
            "definitely-not-a-real-provider",
        ],
        &[],
    );
    let repeat = run(
        &[
            "provider",
            "catalog",
            "recommend",
            "--provider",
            "definitely-not-a-real-provider",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    assert_eq!(
        harn.stdout, repeat.stdout,
        "provider catalog recommend --provider stdout diverged"
    );
}

#[test]
fn providers_recommend_provider_filter_json_is_structurally_identical_across_runs() {
    let harn = run(
        &[
            "provider",
            "catalog",
            "recommend",
            "--provider",
            "ollama",
            "--json",
        ],
        &[],
    );
    let repeat = run(
        &[
            "provider",
            "catalog",
            "recommend",
            "--provider",
            "ollama",
            "--json",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "provider catalog recommend --provider --json shape diverged"
    );
}

// ─── extra coverage for provider command branches ──────────────────────

/// Pin that --json=false (human readiness summary) on mock also
/// produces a structurally valid empty `loaded:` section when the
/// provider has no loaded models.
#[test]
fn provider_probe_mock_human_with_model_is_byte_identical_across_runs() {
    let harn = run(
        &[
            "provider",
            "probe",
            "mock",
            "--model",
            "mock",
            "--json=false",
        ],
        &[],
    );
    let repeat = run(
        &[
            "provider",
            "probe",
            "mock",
            "--model",
            "mock",
            "--json=false",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    assert_eq!(
        harn.stdout, repeat.stdout,
        "provider probe mock --model human stdout diverged"
    );
}

/// JSON-mode coverage with `--model` set — exercises the `runtime_profile`
/// branch even though mock isn't in `LOCAL_PROVIDERS`.
#[test]
fn provider_probe_mock_json_with_model_is_structurally_identical_across_runs() {
    let harn = run(&["provider", "probe", "mock", "--model", "mock"], &[]);
    let repeat = run(&["provider", "probe", "mock", "--model", "mock"], &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value["provider"], harn_value["provider"],
        "provider field diverged"
    );
    assert_eq!(
        repeat_value["readiness"]["ok"], harn_value["readiness"]["ok"],
        "readiness.ok diverged"
    );
}

/// Tool-probe fixture with `--mode streaming` — exercises a different
/// branch of the classifier (single-mode rather than both).
#[test]
fn provider_tool_probe_fixture_streaming_only_json_is_structurally_identical_across_runs() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("response.json");
    write_minimal_tool_fixture(&fixture);
    let path = fixture.to_string_lossy().into_owned();
    let argv = [
        "provider",
        "tool-probe",
        "mock",
        "--model",
        "mock",
        "--response-fixture",
        path.as_str(),
        "--mode",
        "streaming",
    ];
    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "tool-probe streaming JSON shape diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

/// Tool-probe fixture human render with `--mode non-streaming` — the
/// other side of the single-mode branch.
#[test]
fn provider_tool_probe_fixture_nonstreaming_human_is_byte_identical_across_runs() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("response.json");
    write_minimal_tool_fixture(&fixture);
    let path = fixture.to_string_lossy().into_owned();
    let argv = [
        "provider",
        "tool-probe",
        "mock",
        "--model",
        "mock",
        "--response-fixture",
        path.as_str(),
        "--mode",
        "non-streaming",
        "--json=false",
    ];
    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    assert_eq!(
        harn.stdout, repeat.stdout,
        "tool-probe non-streaming human stdout diverged"
    );
}

// ─── provider tool-scorecard (fixture report mode) ───────────────────────

#[test]
fn provider_tool_scorecard_json_is_structurally_identical_across_runs() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let passing = dir.path().join("passing.json");
    let failing = dir.path().join("failing.json");
    write_tool_probe_report_fixture(
        &passing,
        1,
        "anthropic",
        "claude",
        "structured_native_tool_call",
        true,
    );
    write_tool_probe_report_fixture(&failing, 1, "fireworks", "gpt-oss", "empty_silent", false);
    let passing_path = passing.to_string_lossy().into_owned();
    let failing_path = failing.to_string_lossy().into_owned();
    let argv = [
        "provider",
        "tool-scorecard",
        "--tool-probe-report",
        passing_path.as_str(),
        "--tool-probe-report",
        failing_path.as_str(),
    ];

    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "tool-scorecard JSON shape diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
    assert_eq!(harn_value["route_count"], 2);
    assert_eq!(harn_value["summary"]["pass"], 1);
    assert_eq!(harn_value["summary"]["fail"], 1);
    assert_eq!(harn_value["routes"][0]["provider"], "anthropic");
    assert_eq!(harn_value["routes"][1]["status"], "fail");
}

#[test]
fn provider_tool_scorecard_plan_json_includes_fixed_micro_case_matrix() {
    let argv = [
        "provider",
        "tool-scorecard",
        "--plan-from-catalog",
        "--route",
        "anthropic:claude-sonnet-5",
        "--include-batch-manifest",
    ];

    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let repeat_value = parse_json(&repeat.stdout, "repeat");
    assert_eq!(
        repeat_value, harn_value,
        "tool-scorecard plan JSON shape diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
    assert_eq!(harn_value["kind"], "plan");
    assert_eq!(harn_value["route_count"], 1);
    assert_eq!(harn_value["routes"][0]["provider"], "anthropic");
    assert_eq!(harn_value["routes"][0]["model"], "claude-sonnet-5");
    let cases = harn_value["routes"][0]["cases"]
        .as_array()
        .expect("cases should be an array");
    assert!(
        cases.iter().any(|case| case["id"] == "single_tool_call"),
        "single tool case missing: {}",
        harn.stdout
    );
    let single_tool_case = cases
        .iter()
        .find(|case| case["id"] == "single_tool_call")
        .expect("single tool case");
    assert_eq!(single_tool_case["execution"]["status"], "executable");
    assert_eq!(
        single_tool_case["execution"]["runner"],
        "provider_tool_probe"
    );
    assert_eq!(
        single_tool_case["execution"]["command"],
        serde_json::json!([
            "harn",
            "provider",
            "tool-probe",
            "anthropic",
            "--model",
            "claude-sonnet-5",
            "--mode",
            "both",
            "--repeat",
            "1",
            "--timeout-secs",
            "120",
            "--json"
        ])
    );
    assert!(
        cases
            .iter()
            .any(|case| case["id"] == "tool_result_followup"),
        "multi-turn follow-up case missing: {}",
        harn.stdout
    );
    let followup_case = cases
        .iter()
        .find(|case| case["id"] == "tool_result_followup")
        .expect("follow-up case");
    assert_eq!(followup_case["execution"]["status"], "missing_runner");
    assert!(followup_case["execution"].get("command").is_none());
}

#[test]
fn provider_tool_scorecard_plan_human_is_byte_identical_across_runs() {
    let argv = [
        "provider",
        "tool-scorecard",
        "--plan-from-catalog",
        "--route",
        "anthropic:claude-sonnet-5",
        "--json=false",
    ];

    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    assert_eq!(
        harn.stdout, repeat.stdout,
        "tool-scorecard plan human stdout diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
    assert!(
        harn.stdout
            .contains("provider tool-call scorecard plan: routes=1"),
        "unexpected human output: {}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("single_tool_call"),
        "plan cases missing from human output: {}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("executable=1 missing_runner=7"),
        "execution summary missing from human output: {}",
        harn.stdout
    );
}

#[test]
fn provider_tool_scorecard_plan_markdown_is_byte_identical_across_runs() {
    let argv = [
        "provider",
        "tool-scorecard",
        "--plan-from-catalog",
        "--route",
        "anthropic:claude-sonnet-5",
        "--markdown",
    ];

    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    assert_eq!(
        harn.stdout, repeat.stdout,
        "tool-scorecard plan markdown stdout diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
    assert!(
        harn.stdout
            .starts_with("# Provider Tool-Call Scorecard Plan"),
        "unexpected markdown output: {}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("| Provider | Model | Preferred |"),
        "markdown table missing: {}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("| Executable | Missing runner |"),
        "execution columns missing from markdown output: {}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("single_tool_call"),
        "plan cases missing from markdown output: {}",
        harn.stdout
    );
}

#[test]
fn provider_tool_scorecard_json_reports_catalog_mismatches() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("text-observed.json");
    let (provider, model) = native_preferred_text_supported_scorecard_route();
    write_tool_probe_report_fixture(
        &fixture,
        1,
        &provider,
        &model,
        "parseable_harn_text_tool_call",
        true,
    );
    let path = fixture.to_string_lossy().into_owned();
    let harn = run(
        &[
            "provider",
            "tool-scorecard",
            "--tool-probe-report",
            path.as_str(),
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "stderr: {}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["schema_version"], 2);
    let route = &harn_value["routes"][0];
    assert_eq!(route["provider"], provider);
    assert_eq!(route["model"], model);
    assert_eq!(route["recommended_tool_mode"], "text");
    assert_eq!(
        route["catalog_mismatches"][0]["code"],
        "preferred_tool_format_disagrees"
    );
    assert_eq!(
        route["suggested_catalog_updates"][0]["field"],
        "tool_support.preferred_format"
    );
    assert_eq!(route["suggested_catalog_updates"][0]["operation"], "set");
    assert_eq!(route["suggested_catalog_updates"][0]["value"], "json");
}

#[test]
fn provider_tool_scorecard_human_reports_catalog_mismatch_codes() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("text-observed.json");
    let (provider, model) = native_preferred_text_supported_scorecard_route();
    write_tool_probe_report_fixture(
        &fixture,
        1,
        &provider,
        &model,
        "parseable_harn_text_tool_call",
        true,
    );
    let path = fixture.to_string_lossy().into_owned();
    let harn = run(
        &[
            "provider",
            "tool-scorecard",
            "--tool-probe-report",
            path.as_str(),
            "--json=false",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "stderr: {}", harn.stderr);
    assert!(
        harn.stdout
            .contains("catalog_mismatches=preferred_tool_format_disagrees"),
        "unexpected human output: {}",
        harn.stdout
    );
}

#[test]
fn provider_tool_scorecard_markdown_reports_catalog_mismatch_codes() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("text-observed.json");
    let (provider, model) = native_preferred_text_supported_scorecard_route();
    write_tool_probe_report_fixture(
        &fixture,
        1,
        &provider,
        &model,
        "parseable_harn_text_tool_call",
        true,
    );
    let path = fixture.to_string_lossy().into_owned();
    let harn = run(
        &[
            "provider",
            "tool-scorecard",
            "--tool-probe-report",
            path.as_str(),
            "--markdown",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "stderr: {}", harn.stderr);
    assert!(
        harn.stdout.starts_with("# Provider Tool-Call Scorecard"),
        "unexpected markdown output: {}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("preferred_tool_format_disagrees"),
        "catalog mismatch code missing from markdown output: {}",
        harn.stdout
    );
}

#[test]
fn provider_tool_scorecard_json_reports_unknown_catalog_route() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("unknown-route.json");
    write_tool_probe_report_fixture(
        &fixture,
        1,
        "unknown-provider",
        "unknown-model",
        "structured_native_tool_call",
        true,
    );
    let path = fixture.to_string_lossy().into_owned();
    let harn = run(
        &[
            "provider",
            "tool-scorecard",
            "--tool-probe-report",
            path.as_str(),
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "stderr: {}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["schema_version"], 2);
    let route = &harn_value["routes"][0];
    assert!(route["catalog_claim"].is_null());
    assert_eq!(
        route["catalog_mismatches"][0]["code"],
        "route_missing_from_catalog"
    );
    assert!(route["suggested_catalog_updates"]
        .as_array()
        .expect("updates should be an array")
        .is_empty());
}

#[test]
fn provider_tool_scorecard_human_is_byte_identical_across_runs() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("passing.json");
    write_tool_probe_report_fixture(
        &fixture,
        1,
        "anthropic",
        "claude",
        "structured_native_tool_call",
        true,
    );
    let path = fixture.to_string_lossy().into_owned();
    let argv = [
        "provider",
        "tool-scorecard",
        "--tool-probe-report",
        path.as_str(),
        "--json=false",
    ];

    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, repeat.exit_code,
        "exit code diverged; harn stderr={} repeat stderr={}",
        harn.stderr, repeat.stderr
    );
    assert_eq!(
        harn.stdout, repeat.stdout,
        "tool-scorecard human stdout diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
    assert!(
        harn.stdout
            .contains("provider tool-call scorecard: routes=1 pass=1 warn=0 fail=0"),
        "unexpected human output: {}",
        harn.stdout
    );
}

#[test]
fn provider_tool_scorecard_rejects_unsupported_tool_probe_schema() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = dir.path().join("future.json");
    write_tool_probe_report_fixture(
        &fixture,
        999,
        "anthropic",
        "claude",
        "structured_native_tool_call",
        true,
    );
    let path = fixture.to_string_lossy().into_owned();
    let harn = run(
        &[
            "provider",
            "tool-scorecard",
            "--tool-probe-report",
            path.as_str(),
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "unexpected stdout: {}", harn.stdout);
    assert!(
        harn.stderr
            .contains("unsupported tool-probe report schema_version 999"),
        "unexpected stderr: {}",
        harn.stderr
    );
}

fn write_tool_probe_report_fixture(
    path: &Path,
    schema_version: u32,
    provider: &str,
    model: &str,
    classification: &str,
    ok: bool,
) {
    let native_tool_call_count = usize::from(ok && classification == "structured_native_tool_call");
    let text_tool_call_count = usize::from(ok && classification == "parseable_harn_text_tool_call");
    let fallback = if native_tool_call_count > 0 {
        "native"
    } else if text_tool_call_count > 0 {
        "text"
    } else {
        "disabled"
    };
    let body = serde_json::json!({
        "schema_version": schema_version,
        "provider": provider,
        "model": model,
        "tool_name": "echo_marker",
        "marker": "marker",
        "cases": [
            {
                "mode": "non_streaming",
                "ok": ok,
                "classification": classification,
                "fallback_mode": fallback,
                "native_tool_call_count": native_tool_call_count,
                "text_tool_call_count": text_tool_call_count,
                "parser_errors": [],
                "protocol_violations": []
            }
        ],
        "tool_calling": {
            "native": if ok { "pass" } else { "fail" },
            "text": "unknown",
            "streaming_native": "unknown",
            "fallback_mode": fallback
        }
    });
    std::fs::write(
        path,
        serde_json::to_string_pretty(&body).expect("serialize fixture"),
    )
    .expect("write scorecard fixture");
}

fn native_preferred_text_supported_scorecard_route() -> (String, String) {
    let harn = run(&["provider", "tool-scorecard", "--plan-from-catalog"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr: {}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let routes = harn_value["routes"]
        .as_array()
        .expect("tool-scorecard plan routes should be an array");
    for route in routes {
        let claim = &route["catalog_claim"];
        if claim["preferred_tool_format"].as_str() == Some("native")
            && claim["text_tools"].as_bool() == Some(true)
        {
            let provider = route["provider"]
                .as_str()
                .expect("route provider should be a string")
                .to_string();
            let model = route["model"]
                .as_str()
                .expect("route model should be a string")
                .to_string();
            return (provider, model);
        }
    }
    panic!("expected at least one native-preferred catalog route with text-channel support");
}
