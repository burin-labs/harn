//! Dispatch-audit contract tests for the provider catalog audit CLI.

mod test_util;

use test_util::process::run_harn_e2e as run;

fn parse_json(s: &str, label: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|err| {
        panic!("{label} stdout is not valid JSON: {err}\n--- payload ---\n{s}")
    })
}

#[test]
fn provider_dispatch_audit_explains_selected_route_variants_in_process() {
    let argv = [
        "provider",
        "dispatch-audit",
        "--route",
        "anthropic:claude-sonnet-4-6",
    ];
    let harn = run(&argv, &[]);
    let repeat = run(&argv, &[]);
    assert_eq!(
        harn.exit_code, 0,
        "unexpected dispatch audit failure; stderr={}\nstdout={}",
        harn.stderr, harn.stdout
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
        "dispatch audit JSON diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
    assert_eq!(
        harn_value["schema_version"],
        harn_cli::DISPATCH_AUDIT_SCHEMA_VERSION
    );
    assert!(
        harn_value["catalog"]["hash_blake3"]
            .as_str()
            .unwrap_or_default()
            .starts_with("blake3:"),
        "dispatch audit should fingerprint the audited catalog: {}",
        harn.stdout
    );
    assert!(
        harn_value["catalog"]["routing_route_count"]
            .as_u64()
            .unwrap_or_default()
            >= harn_value["route_count"].as_u64().unwrap_or_default(),
        "catalog route universe should cover selected route count: {}",
        harn.stdout
    );
    assert_eq!(harn_value["route_count"], 1);
    assert_eq!(harn_value["variant_count"], 5);
    assert_eq!(harn_value["row_count"], 5);
    assert_eq!(harn_value["pass_count"], 5);
    assert_eq!(harn_value["fail_count"], 0);
    assert!(
        harn_value["unrouted_provider_count"].as_u64().is_some(),
        "dispatch audit should count providers that cannot enter route probes: {}",
        harn.stdout
    );
    assert_eq!(
        harn_value["variants"],
        serde_json::json!(["default", "thinking", "native", "text", "json"])
    );
    assert_eq!(harn_value["rows"][0]["provider"], "anthropic");
    assert_eq!(harn_value["rows"][0]["model"], "claude-sonnet-4-6");
    assert!(
        harn_value["rows"][0]["id"]
            .as_str()
            .unwrap_or_default()
            .len()
            >= 16,
        "dispatch rows should carry stable ids: {}",
        harn.stdout
    );
    assert_eq!(harn_value["rows"][0]["wire_format"], "anthropic_native");
    assert_eq!(harn_value["rows"][0]["structured_output"], "tool_use");
    assert_eq!(
        harn_value["rows"][0]["structured_output_mode"],
        "xml_tagged"
    );
}

#[test]
fn provider_dispatch_audit_can_emit_structured_tool_probe_plan() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--route",
            "anthropic:claude-sonnet-4-6",
            "--include-tool-probe-plan",
            "--tool-probe-case",
            "tool_result_followup",
            "--tool-probe-mode",
            "non-streaming",
            "--tool-probe-repeat",
            "2",
            "--tool-probe-timeout-secs",
            "45",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, 0,
        "unexpected dispatch audit failure; stderr={}\nstdout={}",
        harn.stderr, harn.stdout
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let plan = &harn_value["tool_probe_plan"];
    assert_eq!(
        plan["schema_version"],
        harn_cli::DISPATCH_AUDIT_SCHEMA_VERSION
    );
    assert!(
        plan["plan_id"].as_str().unwrap_or_default().len() >= 16,
        "tool-probe plan should carry a stable id: {}",
        harn.stdout
    );
    assert_eq!(
        plan["catalog_hash_blake3"],
        harn_value["catalog"]["hash_blake3"]
    );
    assert_eq!(plan["route_count"], 1);
    assert_eq!(plan["readiness_command_count"], 1);
    assert_eq!(plan["command_count"], 1);
    assert_eq!(plan["matrix"]["provider_count"], 1);
    assert_eq!(plan["matrix"]["model_count"], 1);
    assert_eq!(plan["matrix"]["provider_model_count"], 1);
    assert_eq!(plan["matrix"]["route_count"], 1);
    assert_eq!(plan["matrix"]["case_count"], 1);
    assert_eq!(plan["matrix"]["mode_count"], 1);
    assert_eq!(plan["matrix"]["live_request_profile_count"], 1);
    assert_eq!(plan["matrix"]["request_audit_profile_count"], 1);
    assert_eq!(plan["matrix"]["readiness_command_count"], 1);
    assert_eq!(plan["matrix"]["command_count"], 1);
    assert_eq!(plan["matrix"]["not_applicable_count"], 0);
    assert_eq!(plan["cases"], serde_json::json!(["tool_result_followup"]));
    assert!(
        plan.get("excluded_cases").is_none(),
        "explicit tool-probe case selection should not report default exclusions: {}",
        harn.stdout
    );
    assert_eq!(plan["modes"], serde_json::json!(["non_streaming"]));
    assert_eq!(
        plan["live_request_profiles"],
        serde_json::json!(["catalog_default"])
    );
    assert_eq!(
        plan["request_audit_profiles"],
        serde_json::json!(["parameter_edges"])
    );
    assert_eq!(plan["repeat"], 2);
    assert_eq!(plan["timeout_secs"], 45);
    assert!(
        plan["output_dir"]
            .as_str()
            .unwrap_or_default()
            .starts_with(".harn-runs/provider-live-probes/"),
        "default output_dir should be catalog-derived: {}",
        harn.stdout
    );
    assert_eq!(plan["readiness_commands"][0]["provider"], "anthropic");
    assert_eq!(plan["readiness_commands"][0]["model"], "claude-sonnet-4-6");
    assert_eq!(
        plan["readiness_commands"][0]["route"],
        "anthropic:claude-sonnet-4-6"
    );
    assert_eq!(
        plan["readiness_commands"][0]["structured_output"],
        "tool_use"
    );
    assert_eq!(
        plan["readiness_commands"][0]["structured_output_mode"],
        "xml_tagged"
    );
    assert_eq!(
        plan["readiness_commands"][0]["secret_envs"],
        serde_json::json!(["ANTHROPIC_API_KEY"])
    );
    assert!(
        plan["readiness_commands"][0]["id"]
            .as_str()
            .unwrap_or_default()
            .len()
            >= 16,
        "readiness commands should carry stable ids: {}",
        harn.stdout
    );
    let readiness_id_prefix = &plan["readiness_commands"][0]["id"]
        .as_str()
        .expect("readiness id")[..12];
    assert_eq!(
        plan["readiness_commands"][0]["argv"],
        serde_json::json!([
            "harn",
            "provider",
            "ready",
            "anthropic",
            "--model",
            "claude-sonnet-4-6",
            "--json"
        ])
    );
    assert_eq!(
        plan["readiness_commands"][0]["output_path"],
        format!(
            "{}/{}-anthropic-claude-sonnet-4-6-readiness.json",
            plan["output_dir"].as_str().expect("output_dir is string"),
            readiness_id_prefix
        )
    );
    assert_eq!(plan["commands"][0]["provider"], "anthropic");
    assert_eq!(plan["commands"][0]["model"], "claude-sonnet-4-6");
    assert_eq!(plan["commands"][0]["route"], "anthropic:claude-sonnet-4-6");
    assert_eq!(plan["commands"][0]["structured_output"], "tool_use");
    assert_eq!(plan["commands"][0]["structured_output_mode"], "xml_tagged");
    assert_eq!(
        plan["commands"][0]["secret_envs"],
        serde_json::json!(["ANTHROPIC_API_KEY"])
    );
    assert_eq!(plan["commands"][0]["request_profile"], "catalog_default");
    assert!(
        plan["commands"][0]["id"].as_str().unwrap_or_default().len() >= 16,
        "tool-probe commands should carry stable ids: {}",
        harn.stdout
    );
    let command_id_prefix = &plan["commands"][0]["id"].as_str().expect("command id")[..12];
    assert_eq!(
        plan["commands"][0]["argv"],
        serde_json::json!([
            "harn",
            "provider",
            "tool-probe",
            "anthropic",
            "--model",
            "claude-sonnet-4-6",
            "--case",
            "tool_result_followup",
            "--request-profile",
            "catalog_default",
            "--mode",
            "non-streaming",
            "--repeat",
            "2",
            "--timeout-secs",
            "45",
            "--json"
        ])
    );
    assert_eq!(
        plan["commands"][0]["output_path"],
        format!(
            "{}/{}-anthropic-claude-sonnet-4-6-tool_result_followup-catalog_default-non_streaming.json",
            plan["output_dir"].as_str().expect("output_dir is string"),
            command_id_prefix
        )
    );
}

#[test]
fn provider_dispatch_audit_tool_probe_plan_honors_custom_output_dir() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--route",
            "openrouter:anthropic/claude-sonnet-4-6",
            "--include-tool-probe-plan",
            "--tool-probe-case",
            "single_tool_call",
            "--tool-probe-mode",
            "streaming",
            "--tool-probe-output-dir",
            ".harn-runs/provider-live-probes/custom-run",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, 0,
        "unexpected dispatch audit failure; stderr={}\nstdout={}",
        harn.stderr, harn.stdout
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let plan = &harn_value["tool_probe_plan"];
    assert_eq!(
        plan["output_dir"],
        ".harn-runs/provider-live-probes/custom-run"
    );
    let readiness_id_prefix = &plan["readiness_commands"][0]["id"]
        .as_str()
        .expect("readiness id")[..12];
    assert_eq!(
        plan["readiness_commands"][0]["output_path"],
        format!(
            ".harn-runs/provider-live-probes/custom-run/{readiness_id_prefix}-openrouter-anthropic_claude-sonnet-4-6-readiness.json"
        )
    );
    let command_id_prefix = &plan["commands"][0]["id"].as_str().expect("command id")[..12];
    assert_eq!(
        plan["commands"][0]["output_path"],
        format!(
            ".harn-runs/provider-live-probes/custom-run/{command_id_prefix}-openrouter-anthropic_claude-sonnet-4-6-single_tool_call-catalog_default-streaming.json"
        )
    );
}

#[test]
fn provider_dispatch_audit_tool_probe_plan_preserves_all_auth_env_names() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--route",
            "huggingface:Qwen/Qwen3-Coder-480B-A35B-Instruct",
            "--include-tool-probe-plan",
            "--tool-probe-case",
            "single_tool_call",
            "--tool-probe-mode",
            "non-streaming",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, 0,
        "unexpected dispatch audit failure; stderr={}\nstdout={}",
        harn.stderr, harn.stdout
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let plan = &harn_value["tool_probe_plan"];
    assert_eq!(
        plan["readiness_commands"][0]["secret_envs"],
        serde_json::json!(["HF_TOKEN", "HUGGINGFACE_API_KEY"])
    );
    assert_eq!(
        plan["commands"][0]["secret_envs"],
        serde_json::json!(["HF_TOKEN", "HUGGINGFACE_API_KEY"])
    );
}

#[test]
fn provider_dispatch_audit_tool_probe_plan_marks_signed_thinking_not_applicable() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--route",
            "ollama:devstral-small-2:24b",
            "--include-tool-probe-plan",
            "--tool-probe-case",
            "signed_thinking_tool_result_followup",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, 0,
        "unexpected dispatch audit failure; stderr={}\nstdout={}",
        harn.stderr, harn.stdout
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let plan = &harn_value["tool_probe_plan"];
    assert_eq!(plan["readiness_command_count"], 1);
    assert_eq!(plan["matrix"]["readiness_command_count"], 1);
    assert_eq!(plan["command_count"], 0);
    assert_eq!(plan["matrix"]["command_count"], 0);
    // One not-applicable (case, mode) pair, expanded across both request
    // profiles (live + request-audit) the plan now enumerates — bump this if the
    // profile set changes, not the schema.
    assert_eq!(plan["matrix"]["not_applicable_count"], 4);
    assert_eq!(
        plan["readiness_commands"][0]["route"],
        "ollama:devstral-small-2:24b"
    );
    // One route x one case x two modes x two request profiles, every cell
    // not-applicable for the same reason. Assert the shape and coverage as
    // sets rather than fragile per-index positions that reorder when the
    // request-profile set grows.
    let not_applicable = plan["not_applicable_commands"]
        .as_array()
        .expect("not_applicable_commands is array");
    assert_eq!(not_applicable.len(), 4);
    let modes: std::collections::BTreeSet<&str> = not_applicable
        .iter()
        .map(|c| c["mode"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        modes,
        std::collections::BTreeSet::from(["non_streaming", "streaming"])
    );
    let profiles: std::collections::BTreeSet<&str> = not_applicable
        .iter()
        .map(|c| c["request_profile"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        profiles,
        std::collections::BTreeSet::from(["catalog_default", "parameter_edges"])
    );
    assert!(not_applicable.iter().all(|c| {
        c["route"] == "ollama:devstral-small-2:24b"
            && c["case"] == "signed_thinking_tool_result_followup"
            && c["structured_output"] == "format_kw"
            && c["structured_output_mode"] == "delimited"
            && c["reason"] == "route_has_no_signed_thinking_tool_history_surface"
    }));
}

#[test]
fn provider_dispatch_audit_default_tool_probe_plan_discloses_excluded_cases() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--route",
            "anthropic:claude-sonnet-4-6",
            "--include-tool-probe-plan",
            "--tool-probe-mode",
            "non-streaming",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, 0,
        "unexpected dispatch audit failure; stderr={}\nstdout={}",
        harn.stderr, harn.stdout
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let plan = &harn_value["tool_probe_plan"];
    assert_eq!(
        plan["excluded_cases"],
        serde_json::json!(["signed_thinking_tool_result_followup"])
    );
    assert!(
        plan["cases"]
            .as_array()
            .expect("cases is array")
            .iter()
            .all(|case| case != "signed_thinking_tool_result_followup"),
        "default plan should not silently include signed-thinking case: {}",
        harn.stdout
    );
}

#[test]
fn provider_dispatch_audit_reports_unrouted_providers_without_failing_full_catalog() {
    let harn = run(&["provider", "dispatch-audit"], &[]);
    assert_eq!(
        harn.exit_code, 0,
        "full catalog dispatch audit should pass while reporting unrouted providers; stderr={}\nstdout={}",
        harn.stderr, harn.stdout
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let unrouted = harn_value["unrouted_providers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        harn_value["unrouted_provider_count"],
        serde_json::json!(unrouted.len())
    );
    for provider in unrouted {
        assert!(
            provider["provider"].as_str().is_some(),
            "unrouted provider row should name provider: {}",
            harn.stdout
        );
        assert_eq!(provider["route_count"], 0);
        assert!(
            provider["reason"]
                .as_str()
                .unwrap_or_default()
                .starts_with("catalog_provider_"),
            "unrouted provider row should carry a typed reason: {}",
            harn.stdout
        );
    }
}

#[test]
fn provider_dispatch_audit_missing_provider_filter_fails_closed() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--provider",
            "definitely-not-a-real-provider",
        ],
        &[],
    );
    assert_ne!(
        harn.exit_code, 0,
        "missing provider filter should fail; stdout={}\nstderr={}",
        harn.stdout, harn.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["route_count"], 0);
    assert_eq!(harn_value["row_count"], 0);
    assert_eq!(harn_value["pass_count"], 0);
    assert_eq!(harn_value["failures"][0]["code"], "missing_provider_filter");
    assert_eq!(
        harn_value["failures"][0]["provider"],
        "definitely-not-a-real-provider"
    );
    assert_eq!(harn_value["failures"][0]["filter_kind"], "provider");
    assert_eq!(
        harn_value["failures"][0]["filter_value"],
        "definitely-not-a-real-provider"
    );
    assert!(
        harn_value["failures"][0].get("route").is_none(),
        "provider filter failure should not overload route field: {}",
        harn.stdout
    );
}

#[test]
fn provider_dispatch_audit_mixed_provider_filter_fails_closed() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--provider",
            "anthropic",
            "--provider",
            "definitely-not-a-real-provider",
        ],
        &[],
    );
    assert_ne!(
        harn.exit_code, 0,
        "mixed provider filter should fail without dropping invalid provider; stdout={}\nstderr={}",
        harn.stdout, harn.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    assert!(
        harn_value["row_count"].as_u64().unwrap_or_default() > 0,
        "valid provider arm should still be audited: {}",
        harn.stdout
    );
    let failures = harn_value["failures"]
        .as_array()
        .expect("failures is array");
    assert!(
        failures
            .iter()
            .any(|failure| failure["code"] == "missing_provider_filter"
                && failure["provider"] == "definitely-not-a-real-provider"),
        "missing provider should be reported alongside valid rows: {}",
        harn.stdout
    );
}

#[test]
fn provider_dispatch_audit_filters_by_model_and_capability() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--provider",
            "anthropic",
            "--model",
            "claude-sonnet-4-6",
            "--capability",
            "tools",
            "--variant",
            "default",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, 0,
        "unexpected dispatch audit failure; stderr={}\nstdout={}",
        harn.stderr, harn.stdout
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["route_count"], 1);
    assert_eq!(harn_value["row_count"], 1);
    assert_eq!(harn_value["rows"][0]["provider"], "anthropic");
    assert_eq!(harn_value["rows"][0]["model"], "claude-sonnet-4-6");
}

#[test]
fn provider_dispatch_audit_missing_model_filter_fails_closed() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--provider",
            "anthropic",
            "--model",
            "definitely-not-a-catalog-model",
        ],
        &[],
    );
    assert_ne!(
        harn.exit_code, 0,
        "missing model filter should fail; stdout={}\nstderr={}",
        harn.stdout, harn.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["route_count"], 0);
    assert_eq!(harn_value["row_count"], 0);
    assert!(
        harn_value["failures"]
            .as_array()
            .expect("failures is array")
            .iter()
            .any(|failure| failure["code"] == "missing_model_filter"
                && failure["filter_kind"] == "model"
                && failure["filter_value"] == "definitely-not-a-catalog-model"
                && failure.get("route").is_none()),
        "missing model filter should be reported: {}",
        harn.stdout
    );
}

#[test]
fn provider_dispatch_audit_missing_capability_filter_fails_closed() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--provider",
            "anthropic",
            "--capability",
            "definitely-not-a-catalog-capability",
        ],
        &[],
    );
    assert_ne!(
        harn.exit_code, 0,
        "missing capability filter should fail; stdout={}\nstderr={}",
        harn.stdout, harn.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["route_count"], 0);
    assert_eq!(harn_value["row_count"], 0);
    assert!(
        harn_value["failures"]
            .as_array()
            .expect("failures is array")
            .iter()
            .any(|failure| failure["code"] == "missing_capability_filter"
                && failure["filter_kind"] == "capability"
                && failure["filter_value"] == "definitely-not-a-catalog-capability"
                && failure.get("route").is_none()),
        "missing capability filter should be reported: {}",
        harn.stdout
    );
}

#[test]
fn provider_dispatch_audit_deduplicates_repeated_variants() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--route",
            "anthropic:claude-sonnet-4-6",
            "--variant",
            "native",
            "--variant",
            "native",
            "--variant",
            "text",
        ],
        &[],
    );
    assert_eq!(
        harn.exit_code, 0,
        "unexpected dispatch audit failure; stderr={}\nstdout={}",
        harn.stderr, harn.stdout
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(
        harn_value["variants"],
        serde_json::json!(["native", "text"])
    );
    assert_eq!(harn_value["variant_count"], 2);
    assert_eq!(harn_value["row_count"], 2);
    let mut ids = std::collections::BTreeSet::new();
    for row in harn_value["rows"].as_array().expect("rows is array") {
        let id = row["id"].as_str().expect("row id").to_string();
        assert!(ids.insert(id), "row ids must be unique: {}", harn.stdout);
    }
}

#[test]
fn provider_dispatch_audit_invalid_route_filter_fails_closed() {
    let harn = run(
        &["provider", "dispatch-audit", "--route", "not-a-route"],
        &[],
    );
    assert_ne!(
        harn.exit_code, 0,
        "invalid route filter should fail; stdout={}\nstderr={}",
        harn.stdout, harn.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["route_count"], 0);
    assert_eq!(harn_value["row_count"], 0);
    assert_eq!(harn_value["pass_count"], 0);
    assert_eq!(harn_value["fail_count"], 1);
    assert_eq!(harn_value["failures"][0]["code"], "invalid_route_filter");
}

#[test]
fn provider_dispatch_audit_missing_route_filter_fails_closed() {
    let harn = run(
        &[
            "provider",
            "dispatch-audit",
            "--route",
            "anthropic:not-a-catalog-model",
        ],
        &[],
    );
    assert_ne!(
        harn.exit_code, 0,
        "missing route filter should fail; stdout={}\nstderr={}",
        harn.stdout, harn.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["route_count"], 0);
    assert_eq!(harn_value["row_count"], 0);
    assert_eq!(harn_value["pass_count"], 0);
    assert_eq!(harn_value["fail_count"], 1);
    assert_eq!(harn_value["failures"][0]["code"], "missing_route_filter");
}
