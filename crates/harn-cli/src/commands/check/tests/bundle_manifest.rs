//! `harn check` bundle-manifest coverage.
//!
//! A bundle manifest records everything a pipeline reaches — prompt assets,
//! host capabilities, worktree repos, and the stdlib modules its imports pull
//! in — so a host can ship a pipeline without re-deriving its closure.

use crate::commands::check::bundle::build_bundle_manifest;
use crate::package::CheckConfig;

use super::unique_temp_dir;

#[test]
fn bundle_manifest_tracks_prompt_assets_host_caps_and_worktree_repos() {
    let dir = unique_temp_dir("harn-check-bundle-manifest");
    std::fs::create_dir_all(dir.join("prompts")).unwrap();
    std::fs::create_dir_all(dir.join("shared")).unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("prompts").join("review.harn.prompt"), "review").unwrap();
    std::fs::write(dir.join("shared").join("snippet.prompt"), "snippet").unwrap();
    std::fs::write(
        dir.join("lib").join("helper.harn"),
        r#"
pub fn helper() -> string {
  return "ok"
}
"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("main.harn"),
        r#"
import "lib/helper.harn"

pipeline main(harness: Harness) {
  const review = harness.fs.render_prompt("prompts/review.harn.prompt")
  const snippet = harness.fs.render_prompt("shared/snippet.prompt")
  const contract = harness.fs.render_prompt("std/agent/prompts/tool_contract_text.harn.prompt")
  harness.project.scan(".", {})
  harness.process.exec_at("shared", "pwd")
  spawn_agent(harness.agent, {
    task: "scan",
    node: {kind: "stage"},
    execution: {worktree: {repo: "./repo"}}
  })
  harness.stdio.println(review + snippet + contract)
}
"#,
    )
    .unwrap();
    let manifest = build_bundle_manifest(&[dir.join("main.harn")], &CheckConfig::default());
    assert_eq!(
        manifest["entry_modules"].as_array().map(|v| v.len()),
        Some(1)
    );
    assert_eq!(
        manifest["import_modules"].as_array().map(|v| v.len()),
        Some(1)
    );
    assert!(manifest["module_dependencies"]
        .as_array()
        .expect("module dependencies")
        .iter()
        .any(|edge| edge["from"]
            .as_str()
            .is_some_and(|value| value.ends_with("/main.harn"))
            && edge["to"]
                .as_str()
                .is_some_and(|value| value.ends_with("/lib/helper.harn"))));
    let assets = manifest["assets"].as_array().expect("assets array");
    assert!(assets.iter().any(|asset| {
        asset["kind"] == "prompt_asset"
            && asset["via"] == "render_prompt"
            && asset["target"] == "prompts/review.harn.prompt"
    }));
    assert!(assets.iter().any(|asset| {
        asset["kind"] == "prompt_asset"
            && asset["via"] == "render_prompt"
            && asset["target"] == "shared/snippet.prompt"
    }));
    assert!(manifest["prompt_assets"]
        .as_array()
        .expect("prompt assets")
        .iter()
        .any(|entry| entry
            .as_str()
            .is_some_and(|value| value.ends_with("/prompts/review.harn.prompt"))));
    assert!(manifest["prompt_assets"]
        .as_array()
        .expect("prompt assets")
        .iter()
        .any(|entry| entry
            .as_str()
            .is_some_and(|value| value.ends_with("/shared/snippet.prompt"))));
    assert!(manifest["prompt_assets"]
        .as_array()
        .expect("prompt assets")
        .iter()
        .any(|entry| entry
            .as_str()
            .is_some_and(|value| value == "std://agent/prompts/tool_contract_text.harn.prompt")));
    assert_eq!(manifest["summary"]["prompt_asset_count"].as_u64(), Some(3));
    assert_eq!(
        manifest["summary"]["module_dependency_count"].as_u64(),
        Some(1)
    );
    assert_eq!(manifest["required_host_capabilities"]["project"][0], "scan");
    assert!(manifest["execution_dirs"]
        .as_array()
        .expect("execution dirs")
        .iter()
        .any(|entry| entry
            .as_str()
            .is_some_and(|value| value.ends_with("/shared"))));
    assert!(manifest["worktree_repos"]
        .as_array()
        .expect("worktree repos")
        .iter()
        .any(|entry| entry.as_str().is_some_and(|value| value.ends_with("/repo"))));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundle_manifest_tracks_reachable_stdlib_imports() {
    let dir = unique_temp_dir("harn-check-bundle-stdlib");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.harn"),
        r#"
import { process_run } from "std/runtime"

pipeline main(harness: Harness) {
  process_run(harness.process, ["echo", "ok"], {timeout_ms: 1000})
}
"#,
    )
    .unwrap();

    let manifest = build_bundle_manifest(&[dir.join("main.harn")], &CheckConfig::default());
    let import_modules = manifest["import_modules"]
        .as_array()
        .expect("import modules");
    assert!(import_modules
        .iter()
        .any(|module| module.as_str() == Some("<std>/runtime")));
    assert!(import_modules
        .iter()
        .any(|module| module.as_str() == Some("<std>/collections")));

    let dependencies = manifest["module_dependencies"]
        .as_array()
        .expect("module dependencies");
    assert!(dependencies.iter().any(|edge| {
        edge["from"]
            .as_str()
            .is_some_and(|value| value.ends_with("/main.harn"))
            && edge["to"].as_str() == Some("<std>/runtime")
    }));
    assert!(dependencies.iter().any(|edge| {
        edge["from"].as_str() == Some("<std>/runtime")
            && edge["to"].as_str() == Some("<std>/collections")
    }));
    assert!(manifest["required_host_capabilities"]["process"]
        .as_array()
        .expect("process capabilities")
        .iter()
        .any(|op| op.as_str() == Some("run")));
    let _ = std::fs::remove_dir_all(&dir);
}
