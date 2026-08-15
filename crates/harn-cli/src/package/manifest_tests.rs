use super::*;
use crate::package::test_support::{current_packages_dir, TestWorkspace};

#[test]
fn rules_table_parses_camel_and_kebab_dir_keys() {
    // The documented `ruleDirs` camelCase form and the kebab alias both map
    // to `rule_dirs` (#2843).
    let camel: Manifest =
        toml::from_str("[rules]\nruleDirs = [\"rules\", \"vendor/rules\"]\n").unwrap();
    assert_eq!(camel.rules.rule_dirs, vec!["rules", "vendor/rules"]);

    let kebab: Manifest = toml::from_str("[rules]\nrule-dirs = [\"r\"]\n").unwrap();
    assert_eq!(kebab.rules.rule_dirs, vec!["r"]);

    let native: Manifest =
        toml::from_str("[rules]\nnativeRuleDirs = [\"native-rules\"]\n").unwrap();
    assert_eq!(native.rules.native_rule_dirs, vec!["native-rules"]);

    let native_kebab: Manifest = toml::from_str("[rules]\nnative-rule-dirs = [\"nr\"]\n").unwrap();
    assert_eq!(native_kebab.rules.native_rule_dirs, vec!["nr"]);

    // No `[rules]` table → empty discovery, never an error.
    let none: Manifest = toml::from_str("[package]\nname = \"x\"\n").unwrap();
    assert!(none.rules.rule_dirs.is_empty());
    assert!(none.rules.native_rule_dirs.is_empty());
}

#[test]
fn llm_manifest_diagnostics_report_unknown_model_fields() {
    let diagnostics = llm_manifest_diagnostics(
        r#"
[llm.models."demo/model"]
name = "Demo"
provider = "demo"
context_window = 4096
fast_mode = true
"#,
    );
    let texts: Vec<String> = diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.to_string())
        .collect();
    assert!(
        texts.iter().any(
            |diagnostic| diagnostic.contains("llm.models.demo/model.fast_mode")
                && diagnostic.contains("serving_tiers")
        ),
        "expected manifest [llm] unknown-field diagnostic, got {texts:?}"
    );
}

#[test]
fn package_eval_pack_paths_use_package_manifest_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("evals")).unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[package]
name = "demo"
version = "0.1.0"
evals = ["evals/webhook.toml"]
"#,
    )
    .unwrap();
    fs::write(
        root.join("evals/webhook.toml"),
        "version = 1\n[[cases]]\nrun = \"run.json\"\n",
    )
    .unwrap();

    let paths = load_package_eval_pack_paths(Some(&root.join("src/main.harn"))).unwrap();

    assert_eq!(paths, vec![root.join("evals/webhook.toml")]);
    assert!(
        !root.join(".harn").exists(),
        "loading project eval packs without dependencies must remain read-only"
    );
}

#[test]
fn package_eval_pack_paths_include_installed_package_evals() {
    let dependency_tmp = tempfile::tempdir().unwrap();
    let dependency = dependency_tmp.path().join("coding-pack");
    fs::create_dir_all(dependency.join("evals")).unwrap();
    fs::write(
        dependency.join(MANIFEST),
        r#"
[package]
name = "coding-pack"
version = "0.1.0"
evals = ["evals/coding.toml"]
"#,
    )
    .unwrap();
    fs::write(
        dependency.join("evals/run.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "_type": "workflow_run",
            "id": "run_1",
            "workflow_id": "workflow_1",
            "status": "completed",
            "usage": {
                "total_duration_ms": 12,
                "total_cost": 0.01,
                "input_tokens": 3,
                "output_tokens": 4,
                "call_count": 1,
                "models": ["mock"]
            },
            "replay_fixture": {
                "_type": "replay_fixture",
                "expected_status": "completed"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        dependency.join("evals/coding.toml"),
        r#"
version = 1
id = "coding-pack"
trials = 2

[package]
name = "coding-pack"
version = "0.1.0"
source = "path:test"
templates = ["templates/rubric.harn.prompt"]

[metadata]
model = "mock-model"
commit = "commit-a"

[[cases]]
id = "case-a"
run = "run.json"
rubrics = ["status"]

[[rubrics]]
id = "status"
kind = "deterministic"

[[rubrics.assertions]]
kind = "run-status"
expected = "completed"
"#,
    )
    .unwrap();

    let helper = dependency_tmp.path().join("helper-lib");
    fs::create_dir_all(&helper).unwrap();
    fs::write(
        helper.join(MANIFEST),
        r#"
[package]
name = "helper-lib"
version = "0.1.0"
"#,
    )
    .unwrap();

    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(
        root.join(MANIFEST),
        format!(
            r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
coding-pack = {{ path = {} }}
helper-lib = {{ path = {} }}
"#,
            crate::format::toml_basic_string_literal(&dependency.display().to_string()),
            crate::format::toml_basic_string_literal(&helper.display().to_string())
        ),
    )
    .unwrap();

    install_packages_in(workspace.env(), false, None, false).unwrap();

    let paths = load_package_eval_pack_paths(Some(&root.join("src/main.harn"))).unwrap();
    assert_eq!(
        paths,
        vec![current_packages_dir(root)
            .join("coding-pack")
            .join("evals/coding.toml")]
    );

    harn_vm::event_log::reset_active_event_log();
    let manifest = harn_vm::orchestration::load_eval_pack_manifest(&paths[0]).unwrap();
    let package = manifest.package.as_ref().expect("package descriptor");
    assert_eq!(package.name.as_deref(), Some("coding-pack"));
    assert_eq!(package.templates, vec!["templates/rubric.harn.prompt"]);

    let report = harn_vm::orchestration::evaluate_eval_pack_manifest_resumable(
        &manifest,
        Some(serde_json::json!({
            "namespace": "installed-pack-evals",
            "suite": "coding-pack",
            "model": "mock-model",
            "commit": "commit-a",
            "branch": "main"
        })),
    )
    .unwrap();
    assert!(report.pass);
    assert_eq!(report.trial_count, 2);
    assert_eq!(report.run_state.ledger_rows_inserted, 2);
    assert_eq!(report.stats_rows.len(), 1);
    assert_eq!(report.stats_rows[0].trials, 2);
    assert!(!report.stats_rows[0].case_fingerprint.is_empty());
    assert_eq!(
        report.harness_config_fingerprint,
        report.stats_rows[0].harness_config_fingerprint
    );

    let ledger = harn_vm::orchestration::eval_ledger_read_report(Some(serde_json::json!({
        "namespace": "installed-pack-evals",
        "suite": "coding-pack",
        "model": "mock-model",
        "commit": "commit-a"
    })))
    .unwrap();
    assert_eq!(ledger.rows.len(), 2);
    harn_vm::event_log::reset_active_event_log();
}
#[test]
fn preflight_severity_parsing_accepts_synonyms() {
    assert_eq!(
        PreflightSeverity::from_opt(Some("warning")),
        PreflightSeverity::Warning
    );
    assert_eq!(
        PreflightSeverity::from_opt(Some("WARN")),
        PreflightSeverity::Warning
    );
    assert_eq!(
        PreflightSeverity::from_opt(Some("off")),
        PreflightSeverity::Off
    );
    assert_eq!(
        PreflightSeverity::from_opt(Some("allow")),
        PreflightSeverity::Off
    );
    assert_eq!(
        PreflightSeverity::from_opt(Some("error")),
        PreflightSeverity::Error
    );
    assert_eq!(PreflightSeverity::from_opt(None), PreflightSeverity::Error);
    // Unknown values fall back to the safe default (error).
    assert_eq!(
        PreflightSeverity::from_opt(Some("bogus")),
        PreflightSeverity::Error
    );
}

#[test]
fn load_check_config_walks_up_from_nested_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Mark root as project boundary so walk-up terminates here.
    std::fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[check]
preflight_severity = "warning"
preflight_allow = ["custom.scan", "runtime.*"]
host_capabilities_path = "./schemas/host-caps.json"

[workspace]
pipelines = ["pipelines", "scripts"]
"#,
    )
    .unwrap();
    let nested = root.join("src").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    let harn_file = nested.join("pipeline.harn");
    fs::write(&harn_file, "pipeline main() {}\n").unwrap();

    let cfg = load_check_config(Some(&harn_file));
    assert_eq!(cfg.preflight_severity.as_deref(), Some("warning"));
    assert_eq!(cfg.preflight_allow, vec!["custom.scan", "runtime.*"]);
    let caps_path = cfg.host_capabilities_path.expect("host caps path");
    assert!(
        caps_path.ends_with("schemas/host-caps.json")
            || caps_path.ends_with("schemas\\host-caps.json"),
        "unexpected absolutized path: {caps_path}"
    );

    let (workspace, manifest_dir) =
        load_workspace_config(Some(&harn_file)).expect("workspace manifest");
    assert_eq!(workspace.pipelines, vec!["pipelines", "scripts"]);
    // Walk-up lands on the directory containing the harn.toml.
    assert_eq!(manifest_dir, root);
}

#[test]
fn toml_string_literal_escapes_all_basic_control_characters() {
    let literal = toml_string_literal("a\u{08}\t\n\u{0C}\r\"\\\u{07}z").unwrap();
    let parsed: toml::Value = toml::from_str(&format!("value = {literal}\n")).unwrap();
    assert_eq!(
        parsed.get("value").and_then(toml::Value::as_str),
        Some("a\u{08}\t\n\u{0C}\r\"\\\u{07}z")
    );
}

#[test]
fn orchestrator_drain_config_parses_defaults_and_overrides() {
    let default_manifest: Manifest = toml::from_str(
        r#"
[package]
name = "fixture"
"#,
    )
    .unwrap();
    assert_eq!(default_manifest.orchestrator.drain.max_items, 1024);
    assert_eq!(default_manifest.orchestrator.drain.deadline_seconds, 30);
    assert_eq!(default_manifest.orchestrator.pumps.max_outstanding, 64);

    let configured: Manifest = toml::from_str(
        r#"
[package]
name = "fixture"

[orchestrator]
drain.max_items = 77
drain.deadline_seconds = 12
pumps.max_outstanding = 3
"#,
    )
    .unwrap();
    assert_eq!(configured.orchestrator.drain.max_items, 77);
    assert_eq!(configured.orchestrator.drain.deadline_seconds, 12);
    assert_eq!(configured.orchestrator.pumps.max_outstanding, 3);
}

#[test]
fn load_check_config_stops_at_git_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    // An ancestor harn.toml above .git must NOT be picked up.
    fs::write(
        tmp.path().join(MANIFEST),
        "[check]\npreflight_severity = \"off\"\n",
    )
    .unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    let inner = project.join("src");
    std::fs::create_dir_all(&inner).unwrap();
    let harn_file = inner.join("main.harn");
    fs::write(&harn_file, "pipeline main() {}\n").unwrap();
    let cfg = load_check_config(Some(&harn_file));
    assert!(
        cfg.preflight_severity.is_none(),
        "must not inherit harn.toml from outside the .git boundary"
    );
}
