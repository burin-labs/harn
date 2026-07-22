//! Regression gate for the workflow-authoring skill pack.
//!
//! Loads every recipe and case under
//! `examples/skill-packs/workflow-authoring/`, asserts the goldens still
//! validate / preview / run with a stable graph digest, and checks each
//! case's structural assertions against its golden bundle. This is the
//! "validator catches drift in generated Harn" gate from issue #1412 —
//! adding a new case automatically extends CI coverage.

use std::path::{Path, PathBuf};
use std::process::Command;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn skill_pack_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/skill-packs/workflow-authoring")
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

fn read_json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn list_files_with_suffix(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(suffix))
        })
        .collect();
    out.sort();
    out
}

fn list_recipe_bundles(root: &Path) -> Vec<PathBuf> {
    let recipes = root.join("recipes");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&recipes)
        .unwrap_or_else(|error| panic!("read {}: {error}", recipes.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("bundle.json"))
        .filter(|path| path.exists())
        .collect();
    out.sort();
    out
}

#[test]
fn skill_pack_recipes_validate_preview_and_run() {
    let root = skill_pack_root();
    let bundles = list_recipe_bundles(&root);
    assert!(
        !bundles.is_empty(),
        "no recipe bundles found under {}",
        root.display()
    );

    for bundle in &bundles {
        let bundle_arg = bundle.to_str().expect("bundle path is UTF-8");

        let validate = run_harn(&["workflow", "validate", "--bundle", bundle_arg, "--json"]);
        assert!(
            validate.status.success(),
            "validate failed for {}\nstderr: {}",
            bundle.display(),
            String::from_utf8_lossy(&validate.stderr)
        );
        let validation = stdout_json(&validate);
        assert_eq!(
            validation["valid"],
            serde_json::json!(true),
            "{} did not validate: {}",
            bundle.display(),
            validation
        );
        let digest = validation["graph_digest"].as_str().unwrap_or_default();
        assert!(
            digest.starts_with("sha256:"),
            "{} graph_digest missing: {validation}",
            bundle.display()
        );

        let preview = run_harn(&["workflow", "preview", "--bundle", bundle_arg, "--json"]);
        assert!(
            preview.status.success(),
            "preview failed for {}\nstderr: {}",
            bundle.display(),
            String::from_utf8_lossy(&preview.stderr)
        );
        let preview_json = stdout_json(&preview);
        assert_eq!(preview_json["validation"]["graph_digest"], digest);

        let run = run_harn(&["workflow", "run", "--bundle", bundle_arg, "--json"]);
        assert!(
            run.status.success(),
            "run failed for {}\nstderr: {}",
            bundle.display(),
            String::from_utf8_lossy(&run.stderr)
        );
        let receipt = stdout_json(&run);
        assert_eq!(receipt["receipt_type"], "harn.workflow_bundle.run");
        assert_eq!(receipt["status"], "completed");
    }
}

#[test]
fn skill_pack_cases_match_their_goldens() {
    let root = skill_pack_root();
    let cases_dir = root.join("cases");
    let cases = list_files_with_suffix(&cases_dir, ".case.json");
    assert!(
        !cases.is_empty(),
        "no eval cases found under {}",
        cases_dir.display()
    );

    for case_path in &cases {
        let case = read_json(case_path);
        let case_id = case["id"]
            .as_str()
            .unwrap_or_else(|| panic!("{} missing id", case_path.display()));
        let golden_rel = case["golden_bundle_path"]
            .as_str()
            .unwrap_or_else(|| panic!("{} missing golden_bundle_path", case_path.display()));

        let golden = case_path
            .parent()
            .expect("case has parent dir")
            .join(golden_rel);
        assert!(
            golden.exists(),
            "case {case_id}: golden {} does not exist",
            golden.display()
        );

        let bundle = read_json(&golden);
        let want = case["structural_assertions"]
            .as_object()
            .unwrap_or_else(|| panic!("case {case_id}: structural_assertions must be object"));

        for (key, expected) in want {
            let failure = match key.as_str() {
                "bundle_id" => mismatch(case_id, "id", &bundle["id"], expected),
                "workflow_id" => {
                    mismatch(case_id, "workflow.id", &bundle["workflow"]["id"], expected)
                }
                "entry" => mismatch(
                    case_id,
                    "workflow.entry",
                    &bundle["workflow"]["entry"],
                    expected,
                ),
                "autonomy_tier" => mismatch(
                    case_id,
                    "policy.autonomy_tier",
                    &bundle["policy"]["autonomy_tier"],
                    expected,
                ),
                "retry_backoff" => mismatch(
                    case_id,
                    "policy.retry.backoff",
                    &bundle["policy"]["retry"]["backoff"],
                    expected,
                ),
                "catchup_mode" => mismatch(
                    case_id,
                    "policy.catchup.mode",
                    &bundle["policy"]["catchup"]["mode"],
                    expected,
                ),
                "required_worktree_policy" => mismatch(
                    case_id,
                    "environment.worktree_policy",
                    &bundle["environment"]["worktree_policy"],
                    expected,
                ),
                "required_node_ids" => members_present(
                    case_id,
                    "workflow.nodes",
                    expected,
                    bundle["workflow"]["nodes"]
                        .as_object()
                        .map(|map| map.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default(),
                ),
                "required_trigger_kinds" => members_present(
                    case_id,
                    "triggers[].kind",
                    expected,
                    project_strings(&bundle["triggers"], "kind"),
                ),
                "required_connector_ids" => members_present(
                    case_id,
                    "connectors[].id",
                    expected,
                    project_strings(&bundle["connectors"], "id"),
                ),
                "required_approval_nodes" => members_present(
                    case_id,
                    "policy.approval_required",
                    expected,
                    bundle["policy"]["approval_required"]
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
                other => panic!("case {case_id}: unknown structural assertion `{other}`"),
            };
            if let Some(message) = failure {
                panic!("{message}");
            }
        }
    }
}

fn mismatch(
    case_id: &str,
    field: &str,
    actual: &serde_json::Value,
    expected: &serde_json::Value,
) -> Option<String> {
    if actual == expected {
        None
    } else {
        Some(format!(
            "case {case_id}: {field} mismatch — expected {expected}, got {actual}"
        ))
    }
}

fn members_present(
    case_id: &str,
    field: &str,
    expected: &serde_json::Value,
    actual: Vec<String>,
) -> Option<String> {
    let required = expected
        .as_array()
        .unwrap_or_else(|| panic!("case {case_id}: {field} expected list"));
    let missing: Vec<String> = required
        .iter()
        .filter_map(|value| value.as_str().map(String::from))
        .filter(|item| !actual.contains(item))
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "case {case_id}: {field} missing required members {missing:?} (have {actual:?})"
        ))
    }
}

fn project_strings(items: &serde_json::Value, field: &str) -> Vec<String> {
    items
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value[field].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
