use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use harn_lexer::Lexer;
use harn_modules::resolve_import_path;
use harn_parser::{Parser, SNode};

use crate::package::CheckConfig;

use super::bundle::build_bundle_manifest;
use super::check_cmd::check_file_inner;
use super::config::collect_harn_targets;
use super::config::{build_module_graph, collect_cross_file_imports};
use super::host_capabilities::parse_host_capability_value;
use super::preflight::{collect_preflight_diagnostics, is_preflight_allowed};

fn parse_program(source: &str) -> Vec<SNode> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().expect("tokenize");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("parse")
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{nanos}"))
}

#[test]
fn preflight_reports_template_syntax_error() {
    let dir = unique_temp_dir("harn-check-tpl");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    // Unterminated `{{ for }}` block.
    std::fs::write(dir.join("broken.prompt"), "{{ for x in xs }}oops\n").unwrap();
    let source = r#"
pipeline main() {
  let text = render("broken.prompt")
  println(text)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics.iter().any(|d| d
            .message
            .contains("template 'broken.prompt' has a syntax error")),
        "expected template-syntax diagnostic, got {} messages",
        diagnostics.len()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_reports_missing_literal_render_target() {
    let dir = unique_temp_dir("harn-check");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let text = render("missing.txt")
  println(text)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("render target"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_resolves_imports_with_implicit_harn_extension() {
    let dir = unique_temp_dir("harn-check");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib").join("helpers.harn"), "pub fn x() { 1 }\n").unwrap();
    let file = dir.join("main.harn");
    let resolved = resolve_import_path(&file, "lib/helpers");
    assert_eq!(resolved, Some(dir.join("lib").join("helpers.harn")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_reports_missing_worker_execution_repo() {
    let dir = unique_temp_dir("harn-check-worker");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  spawn_agent({
    task: "do it",
    node: {kind: "stage"},
    execution: {worktree: {repo: "./missing-repo"}}
  })
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("worktree repo"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_detects_import_collision() {
    let dir = unique_temp_dir("harn-check-collision");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib").join("a.harn"), "pub fn helper() { 1 }\n").unwrap();
    std::fs::write(dir.join("lib").join("b.harn"), "pub fn helper() { 2 }\n").unwrap();
    let file = dir.join("main.harn");
    let source = r#"
import "lib/a.harn"
import "lib/b.harn"

pipeline main() {
  log(helper())
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("import collision")),
        "expected import collision diagnostic, got: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_no_collision_with_selective_imports() {
    let dir = unique_temp_dir("harn-check-selective");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(
        dir.join("lib").join("a.harn"),
        "pub fn foo() { 1 }\npub fn shared() { 2 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib").join("b.harn"),
        "pub fn bar() { 3 }\npub fn shared() { 4 }\n",
    )
    .unwrap();
    let file = dir.join("main.harn");
    let source = r#"
import { foo } from "lib/a.harn"
import { bar } from "lib/b.harn"

pipeline main() {
  log(foo())
  log(bar())
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("import collision")),
        "unexpected collision: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_reports_unknown_host_capability() {
    let dir = unique_temp_dir("harn-check-host");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  host_call("unknown_cap.do_stuff", {})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("unknown host capability")),
        "expected unknown host capability diagnostic, got: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_reports_tool_define_unknown_host_capability() {
    // harn#743: a host_bridge tool's host_capability binding is
    // validated against the same capability map host_call uses, so
    // typos surface during `harn check` rather than at first model
    // call.
    let dir = unique_temp_dir("harn-check-tool-define-cap");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let r = tool_registry()
  tool_define(
    r,
    "ask_user",
    "Ask the user",
    {
      parameters: {prompt: "string"},
      executor: "host_bridge",
      host_capability: "interaction.unknown_op",
    },
  )
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("interaction.unknown_op")
                && d.message.contains("not declared by the host")),
        "expected tool_define host_capability diagnostic, got: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_accepts_tool_define_known_host_capability() {
    let dir = unique_temp_dir("harn-check-tool-define-cap-ok");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let r = tool_registry()
  tool_define(
    r,
    "ask_user",
    "Ask the user",
    {
      parameters: {prompt: "string"},
      executor: "host_bridge",
      host_capability: "interaction.ask",
    },
  )
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("not declared by the host")),
        "unexpected diagnostic: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_reports_tool_define_host_bridge_missing_capability() {
    let dir = unique_temp_dir("harn-check-tool-define-missing");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let r = tool_registry()
  tool_define(
    r,
    "ask_user",
    "Ask the user",
    {parameters: {prompt: "string"}, executor: "host_bridge"},
  )
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("no `host_capability` binding")),
        "expected missing-capability diagnostic, got: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_reports_tool_define_unknown_executor_value() {
    let dir = unique_temp_dir("harn-check-tool-define-executor");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let r = tool_registry()
  tool_define(
    r,
    "fly",
    "Fly",
    {parameters: {distance: "int"}, executor: "rocketship"},
  )
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("unknown executor \"rocketship\"")),
        "expected unknown-executor diagnostic, got: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_accepts_known_host_capabilities() {
    let dir = unique_temp_dir("harn-check-host-ok");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  host_call("project.metadata_get", {dir: ".", namespace: "facts"})
  host_call("process.exec", {command: "ls"})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("unknown host capability")),
        "unexpected host cap diagnostic: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_file_inner_enforces_invariants_when_requested() {
    let dir = unique_temp_dir("harn-check-invariants");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        &file,
        r#"
@invariant("fs.writes", "src/**")
fn handler() {
  write_file("/tmp/out.txt", "unsafe")
}
"#,
    )
    .unwrap();

    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let outcome = check_file_inner(
        &file,
        &CheckConfig::default(),
        &cross_file_imports,
        &module_graph,
        true,
    );

    assert!(
        outcome.has_error,
        "expected invariant violation to fail check"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_file_inner_skips_invariants_when_disabled() {
    let dir = unique_temp_dir("harn-check-invariants-off");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        &file,
        r#"
@invariant("fs.writes", "src/**")
fn handler() {
  write_file("/tmp/out.txt", "unsafe")
}
"#,
    )
    .unwrap();

    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let outcome = check_file_inner(
        &file,
        &CheckConfig::default(),
        &cross_file_imports,
        &module_graph,
        false,
    );

    assert!(
        !outcome.has_error,
        "invariants should only run behind --invariants"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_accepts_extended_host_capabilities_from_config() {
    let dir = unique_temp_dir("harn-check-host-extended");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  host_call("project.scan", {})
  host_call("runtime.set_result", {})
}
"#;
    let program = parse_program(source);
    let diagnostics = collect_preflight_diagnostics(
        &file,
        source,
        &program,
        &CheckConfig {
            host_capabilities: HashMap::from([
                ("project".to_string(), vec!["scan".to_string()]),
                ("runtime".to_string(), vec!["set_result".to_string()]),
            ]),
            ..CheckConfig::default()
        },
    );
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("unknown host capability")),
        "unexpected host cap diagnostic: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_accepts_runtime_task_and_session_ops() {
    let dir = unique_temp_dir("harn-check-host-runtime");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  host_call("runtime.task", {})
  host_call("session.changed_paths", {})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("unknown host capability")),
        "unexpected host cap diagnostic: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_accepts_host_operations_registered_via_host_mock() {
    let dir = unique_temp_dir("harn-check-host-mock");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  host_mock("project", "metadata_get", {result: {value: "facts"}})
  host_call("project.metadata_get", {dir: "pkg"})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("unknown host capability")),
        "unexpected host cap diagnostic: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn collect_harn_targets_recurses_directories_and_deduplicates() {
    let dir = unique_temp_dir("harn-check-targets");
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(dir.join("a.harn"), "pipeline a() {}\n").unwrap();
    std::fs::write(dir.join("nested").join("b.harn"), "pipeline b() {}\n").unwrap();
    std::fs::write(dir.join("nested").join("ignore.txt"), "x\n").unwrap();

    let target_dir = dir.display().to_string();
    let target_file = dir.join("a.harn").display().to_string();
    let files = collect_harn_targets(&[target_dir.as_str(), target_file.as_str()]);

    assert_eq!(files.len(), 2);
    assert!(files.contains(&dir.join("a.harn")));
    assert!(files.contains(&dir.join("nested").join("b.harn")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parse_host_capability_value_accepts_top_level_object_schema() {
    let value = serde_json::json!({
        "workspace": ["project_root", "file_exists"],
        "runtime": {
            "operations": ["task", "pipeline_input"]
        }
    });
    let parsed = parse_host_capability_value(&value);
    assert!(parsed["workspace"].contains("project_root"));
    assert!(parsed["workspace"].contains("file_exists"));
    assert!(parsed["runtime"].contains("task"));
    assert!(parsed["runtime"].contains("pipeline_input"));
}

#[test]
fn preflight_accepts_render_target_from_bundle_root() {
    let dir = unique_temp_dir("harn-check-bundle-root");
    std::fs::create_dir_all(dir.join("bundle")).unwrap();
    std::fs::write(dir.join("bundle").join("shared.prompt"), "hello").unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let text = render("shared.prompt")
  println(text)
}
"#;
    let program = parse_program(source);
    let diagnostics = collect_preflight_diagnostics(
        &file,
        source,
        &program,
        &CheckConfig {
            bundle_root: Some(dir.join("bundle").display().to_string()),
            ..CheckConfig::default()
        },
    );
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("render target")),
        "unexpected render diagnostic: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_validates_render_in_imported_module() {
    let dir = unique_temp_dir("harn-check-import-render");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    // Module references a template that doesn't exist
    std::fs::write(
        dir.join("lib").join("tmpl.harn"),
        "pub fn load() { render(\"missing_template.txt\") }\n",
    )
    .unwrap();
    let file = dir.join("main.harn");
    let source = r#"
import "lib/tmpl.harn"

pipeline main() {
  log(load())
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("render target")),
        "expected render target diagnostic for imported module, got: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

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

pipeline main() {
  let review = render_prompt("prompts/review.harn.prompt")
  let snippet = render("shared/snippet.prompt")
  host_call("project.scan", {})
  exec_at("shared", "pwd")
  spawn_agent({
    task: "scan",
    node: {kind: "stage"},
    execution: {worktree: {repo: "./repo"}}
  })
  println(review + snippet)
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
            && asset["via"] == "render"
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
    assert_eq!(manifest["summary"]["prompt_asset_count"].as_u64(), Some(2));
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
fn unknown_host_capability_diagnostic_carries_tag() {
    let dir = unique_temp_dir("harn-check-host-tag");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  host_call("custom_cap.do_thing", {})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.tags.as_deref() == Some("custom_cap.do_thing")),
        "expected tagged diagnostic, got: {:?}",
        diagnostics
            .iter()
            .map(|d| (d.message.clone(), d.tags.clone()))
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_allow_matches_exact_wildcard_and_capability_scope() {
    let exact = Some("project.scan".to_string());
    let other_op = Some("project.refresh".to_string());
    let other_cap = Some("editor.get_selection".to_string());

    // Exact match
    assert!(is_preflight_allowed(&exact, &["project.scan".to_string()]));
    // `project.*` wildcard matches any op in the project capability
    assert!(is_preflight_allowed(&other_op, &["project.*".to_string()]));
    // Bare capability name also matches any op in that capability
    assert!(is_preflight_allowed(&other_op, &["project".to_string()]));
    // `*` blanket match
    assert!(is_preflight_allowed(&exact, &["*".to_string()]));
    // No match when capability differs
    assert!(!is_preflight_allowed(
        &other_cap,
        &["project.*".to_string()]
    ));
    // Untagged diagnostics never match
    assert!(!is_preflight_allowed(&None, &["*".to_string()]));
}

#[test]
fn check_lint_reports_missing_harndoc_for_public_functions() {
    let source = r#"
pub fn exposed() -> string {
  return "x"
}
"#;
    let program = parse_program(source);
    let diagnostics = harn_lint::lint_with_config_and_source(
        &program,
        &CheckConfig::default().disable_rules,
        Some(source),
    );
    assert!(
        diagnostics.iter().any(|d| d.rule == "missing-harndoc"),
        "expected missing-harndoc warning, got: {:?}",
        diagnostics.iter().map(|d| &d.rule).collect::<Vec<_>>()
    );
}
