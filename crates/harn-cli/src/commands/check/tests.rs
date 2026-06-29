use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use harn_lexer::Lexer;
use harn_modules::resolve_import_path;
use harn_parser::{Parser, SNode};

use crate::package::CheckConfig;

use super::bundle::build_bundle_manifest;
use super::check_cmd::{check_file_inner, check_file_report};
use super::config::collect_harn_targets;
use super::config::{
    build_module_graph, build_module_graph_and_seed_analysis, collect_cross_file_imports,
};
use super::host_capabilities::parse_host_capability_value;
use super::lint::lint_file_inner;
use super::lint_report::lint_file_report;
use super::preflight::{
    collect_preflight_diagnostics, collect_preflight_diagnostics_with_module_graph,
    is_preflight_allowed,
};

fn parse_program(source: &str) -> Vec<SNode> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().expect("tokenize");
    let mut parser = Parser::new(tokens);
    parser.parse().expect("parse")
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    // Process ID disambiguates across parallel test shards/processes; an
    // atomic counter disambiguates within a process. Wall-clock-based
    // uniqueness (SystemTime nanos) collides when two callers land in the
    // same nanosecond, which has been observed on loaded CI runners.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}-{pid}-{seq}"))
}

fn load_replay_oracle_fixture(name: &str) -> Option<serde_json::Value> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixtures_dir = workspace_root.join("conformance/replay-oracle/fixtures");
    let path = fixtures_dir.join(name);
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !fixtures_dir.exists() => {
            return None;
        }
        Err(err) => panic!("failed to read {}: {err}", path.display()),
    };
    Some(serde_json::from_str(&source).expect("replay oracle fixture parses"))
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
  __io_println(text)
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
  __io_println(text)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("render target"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance for issue #771: `render_prompt(...)` literal-string targets
/// must be validated alongside `render(...)`, the diagnostic must name
/// the actual builtin (`render_prompt`), and the resolved candidate path
/// must be visible so the author can see exactly where the lookup tried
/// to land.
#[test]
fn preflight_reports_missing_literal_render_prompt_target() {
    let dir = unique_temp_dir("harn-check-render-prompt");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("chat.harn");
    let source = r#"
pub fn chat() -> string {
  let trimmed = "hello"
  return render_prompt("lane-classifier.harn.prompt", {task: trimmed})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert_eq!(diagnostics.len(), 1);
    let diag = &diagnostics[0];
    assert!(
        diag.message.contains("render_prompt target"),
        "expected diagnostic to name render_prompt, got: {}",
        diag.message
    );
    assert!(
        diag.message.contains("lane-classifier.harn.prompt"),
        "expected diagnostic to include the literal path, got: {}",
        diag.message
    );
    assert!(
        diag.message.contains(
            &dir.join("lane-classifier.harn.prompt")
                .display()
                .to_string()
        ),
        "expected diagnostic to include the resolved candidate path, got: {}",
        diag.message
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_accepts_embedded_stdlib_prompt_target() {
    let dir = unique_temp_dir("harn-check-stdlib-render-prompt");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("chat.harn");
    let source = r#"
pub fn chat() -> string {
  return render_prompt("std/agent/prompts/tool_contract_text.harn.prompt", {})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("render_prompt target")),
        "embedded stdlib prompt should not be treated as a missing file: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance for issue #771: dynamic first arguments are not statically
/// checkable, so `render_prompt(some_var, ...)` must be silently
/// skipped — no false positives on legitimate dynamic dispatch.
#[test]
fn preflight_skips_non_literal_render_prompt_target() {
    let dir = unique_temp_dir("harn-check-render-dynamic");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let path = "missing.harn.prompt"
  let prompt = render_prompt(path, {})
  let key = "1"
  let interp = render_prompt("missing_${key}.prompt", {})
  __io_println(prompt + interp)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("render_prompt target")),
        "dynamic first args must not produce render-target diagnostics, got: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_reports_prompt_tool_reference_outside_literal_surface() {
    let dir = unique_temp_dir("harn-check-tool-surface-prompt");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        dir.join("agent.harn.prompt"),
        "Use run_command({command: \"cargo test\"})",
    )
    .unwrap();
    let source = r#"
pipeline main() {
  var tools = tool_registry()
  tools = tool_define(
    tools,
    "read_file",
    "Read a file",
    {parameters: {path: "string"}, executor: "host_bridge", host_capability: "workspace.read"},
  )
  let system = render_prompt("agent.harn.prompt", {})
  agent_loop("task", system, {tools: tools})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("TOOL_SURFACE_UNKNOWN_PROMPT_TOOL")),
        "expected prompt tool-surface diagnostic, got: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_honors_prompt_tool_surface_suppression() {
    let dir = unique_temp_dir("harn-check-tool-surface-suppressed");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        dir.join("agent.harn.prompt"),
        "<!-- harn-tool-surface: ignore-next-line -->\nrun_command({command: \"old\"})",
    )
    .unwrap();
    let source = r#"
pipeline main() {
  var tools = tool_registry()
  tools = tool_define(
    tools,
    "read_file",
    "Read a file",
    {parameters: {path: "string"}, executor: "host_bridge", host_capability: "workspace.read"},
  )
  let system = render_prompt("agent.harn.prompt", {})
  agent_loop("task", system, {tools: tools})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("TOOL_SURFACE_UNKNOWN_PROMPT_TOOL")),
        "suppressed prompt example should not report tool-surface diagnostics: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance for issue #771: raw string literals (`r"foo"`) are still
/// statically known and must be validated like ordinary string literals.
#[test]
fn preflight_reports_missing_render_prompt_target_for_raw_string() {
    let dir = unique_temp_dir("harn-check-render-raw");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let prompt = render_prompt(r"missing.prompt", {})
  __io_println(prompt)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("render_prompt target")),
        "raw string literal must trigger preflight check, got: {:?}",
        diagnostics.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance for issue #771: the diagnostic span must point at the
/// literal-string argument, not the whole `render_prompt(...)`
/// expression — this is what enables an editor's quick-fix to jump
/// straight to the path that needs editing.
#[test]
fn preflight_render_prompt_diagnostic_spans_literal_argument() {
    let dir = unique_temp_dir("harn-check-render-span");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let prompt = render_prompt("missing.prompt", {})
  __io_println(prompt)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    let render_diag = diagnostics
        .iter()
        .find(|d| d.message.contains("render_prompt target"))
        .expect("expected render_prompt target diagnostic");
    let span_text = &source[render_diag.span.start..render_diag.span.end];
    assert_eq!(
        span_text, "\"missing.prompt\"",
        "expected diagnostic span to cover only the literal-string argument"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance for issue #771: `@/<rel>` forms must resolve through the
/// same `harn_modules::asset_paths` logic the runtime uses, so a missing
/// project-root prompt fails the static check.
#[test]
fn preflight_reports_missing_project_root_asset_path() {
    let dir = unique_temp_dir("harn-check-asset-projroot");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("harn.toml"), "[package]\nname = \"x\"\n").unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let prompt = render_prompt("@/prompts/missing.harn.prompt", {})
  __io_println(prompt)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    let render_diag = diagnostics
        .iter()
        .find(|d| d.message.contains("render_prompt target"))
        .expect("expected render_prompt target diagnostic for missing @/ asset");
    assert!(
        render_diag.message.contains(&crate::format::slash_path(
            &dir.join("prompts/missing.harn.prompt")
        )),
        "expected diagnostic to surface the resolved project-root path, got: {}",
        render_diag.message
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance for issue #771: `@<alias>/<rel>` with an unknown alias
/// must surface the asset-resolver's structural error so the user sees
/// the missing `[asset_roots]` entry, not a generic file-existence
/// message.
#[test]
fn preflight_reports_unknown_asset_alias() {
    let dir = unique_temp_dir("harn-check-asset-alias");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("harn.toml"), "[package]\nname = \"x\"\n").unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let prompt = render_prompt("@unknown/foo.harn.prompt", {})
  __io_println(prompt)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    let alias_diag = diagnostics
        .iter()
        .find(|d| d.message.contains("[asset_roots]"))
        .expect("expected unknown-alias diagnostic citing [asset_roots]");
    assert!(
        alias_diag.message.contains("unknown"),
        "expected diagnostic to name the missing alias, got: {}",
        alias_diag.message
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance for issue #771: a defined `@<alias>/<rel>` resolves
/// through the same logic the runtime uses, so a missing file under the
/// alias is still flagged.
#[test]
fn preflight_reports_missing_aliased_asset_path() {
    let dir = unique_temp_dir("harn-check-asset-alias-missing");
    std::fs::create_dir_all(dir.join("src/prompts")).unwrap();
    std::fs::write(
        dir.join("harn.toml"),
        "[package]\nname = \"x\"\n[asset_roots]\npartials = \"src/prompts\"\n",
    )
    .unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let prompt = render_prompt("@partials/missing.harn.prompt", {})
  __io_println(prompt)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    let render_diag = diagnostics
        .iter()
        .find(|d| d.message.contains("render_prompt target"))
        .expect("expected render_prompt target diagnostic for missing aliased asset");
    assert!(
        render_diag.message.contains(&crate::format::slash_path(
            &dir.join("src/prompts/missing.harn.prompt")
        )),
        "expected diagnostic to surface the alias-resolved path, got: {}",
        render_diag.message
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance for issue #771: when the missing prompt file exists elsewhere
/// under the same project root, the diagnostic must include a
/// "did you mean ...?" suggestion pointing at the misfiled location.
#[test]
fn preflight_suggests_misfiled_render_prompt_target() {
    let dir = unique_temp_dir("harn-check-render-suggest");
    std::fs::create_dir_all(dir.join("lib/runtime")).unwrap();
    std::fs::create_dir_all(dir.join("lib/mode")).unwrap();
    std::fs::write(dir.join("harn.toml"), "[package]\nname = \"x\"\n").unwrap();
    std::fs::write(
        dir.join("lib/mode/lane-classifier.harn.prompt"),
        "task: {{task}}\n",
    )
    .unwrap();
    let file = dir.join("lib/runtime/chat.harn");
    let source = r#"
pub fn chat() -> string {
  return render_prompt("lane-classifier.harn.prompt", {task: "hi"})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    let render_diag = diagnostics
        .iter()
        .find(|d| d.message.contains("render_prompt target"))
        .expect("expected render_prompt target diagnostic");
    let help = render_diag
        .help
        .as_ref()
        .expect("expected diagnostic help text");
    assert!(
        help.contains("did you mean"),
        "expected help text to include 'did you mean' suggestion, got: {help}",
    );
    assert!(
        help.contains("lib/mode/lane-classifier.harn.prompt"),
        "expected help text to point at the misfiled location, got: {help}",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Acceptance for issue #771: when no near-miss exists, the diagnostic
/// must still emit useful generic guidance instead of the misleading
/// "did you mean ..." prefix.
#[test]
fn preflight_omits_did_you_mean_when_no_near_miss() {
    let dir = unique_temp_dir("harn-check-render-no-suggest");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("harn.toml"), "[package]\nname = \"x\"\n").unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  let prompt = render_prompt("nowhere.harn.prompt", {})
  __io_println(prompt)
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    let render_diag = diagnostics
        .iter()
        .find(|d| d.message.contains("render_prompt target"))
        .expect("expected render_prompt target diagnostic");
    let help = render_diag
        .help
        .as_ref()
        .expect("expected diagnostic help text");
    assert!(
        !help.contains("did you mean"),
        "expected no 'did you mean' when there's no near-miss, got: {help}",
    );
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
fn preflight_re_export_conflicts_use_supplied_module_graph() {
    let dir = unique_temp_dir("harn-check-re-export-conflict");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.harn"), "pub fn helper() { 1 }\n").unwrap();
    std::fs::write(dir.join("b.harn"), "pub fn helper() { 2 }\n").unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pub import { helper } from "a.harn"
pub import { helper } from "b.harn"

pipeline main() {}
"#;
    std::fs::write(&file, source).unwrap();
    let program = parse_program(source);
    let module_graph = build_module_graph(&[file.clone(), dir.join("a.harn"), dir.join("b.harn")]);
    let diagnostics = collect_preflight_diagnostics_with_module_graph(
        &file,
        source,
        &program,
        &CheckConfig::default(),
        &module_graph,
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message.contains("re-export conflict")),
        "expected re-export conflict diagnostic, got: {:?}",
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
  host_call("project.metadata_inspect", {dir: ".", namespace: "facts"})
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
fn preflight_accepts_process_spawn_lifecycle_ops() {
    // #3252: the non-blocking process lifecycle ops
    // (spawn/poll/wait/kill/release) are part of the "process" capability
    // manifest, so `host_call` targets naming them must type-check.
    let dir = unique_temp_dir("harn-check-process-spawn");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
pipeline main() {
  var h = host_call("process.spawn", {mode: "argv", argv: ["echo", "hi"]})
  host_call("process.poll", {handle_id: h.handle_id})
  host_call("process.wait", {handle_id: h.handle_id, timeout_ms: 1000})
  host_call("process.kill", {handle_id: h.handle_id})
  host_call("process.release", {handle_id: h.handle_id})
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .all(|d| !d.message.contains("unknown host capability")),
        "unexpected host cap diagnostic for process.spawn lifecycle ops: {:?}",
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
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let outcome = check_file_inner(
        &mut analysis,
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
fn check_file_inner_uses_imported_callable_signatures() {
    let dir = unique_temp_dir("harn-check-imported-callable");
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("lib.harn");
    let file = dir.join("main.harn");
    std::fs::write(
        &lib,
        r"
type PickOptions = {drop_nil?: bool}

pub fn pick(options: PickOptions = {}) -> nil {
  return nil
}
",
    )
    .unwrap();
    std::fs::write(
        &file,
        r#"
import { pick } from "lib"

pipeline main() {
  pick({dropnil: true})
}
"#,
    )
    .unwrap();

    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let outcome = check_file_inner(
        &mut analysis,
        &file,
        &CheckConfig::default(),
        &cross_file_imports,
        &module_graph,
        true,
    );

    assert!(
        outcome.has_error,
        "expected imported function option typo to fail check"
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
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let outcome = check_file_inner(
        &mut analysis,
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
fn check_file_inner_enforces_capability_policy_invariants_when_requested() {
    let dir = unique_temp_dir("harn-check-capability-policy");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        &file,
        r#"
@invariant("capability.policy", allow: "fs.write")
fn _handler(client) {
  mcp_call(client, "github.search", {})
}
"#,
    )
    .unwrap();

    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let outcome = check_file_inner(
        &mut analysis,
        &file,
        &CheckConfig::default(),
        &cross_file_imports,
        &module_graph,
        true,
    );

    assert!(
        outcome.has_error,
        "expected undeclared connector capability to fail check"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn capability_policy_approval_matches_replay_oracle_fixture() {
    let Some(fixture) = load_replay_oracle_fixture("approval_tool_call.valid.json") else {
        return;
    };
    let decisions = fixture["first_run"]["policy_decisions"]
        .as_array()
        .expect("fixture has policy decisions");
    assert!(
        decisions.iter().any(|decision| {
            decision["capability"] == "fs.write" && decision["approval_required"] == true
        }),
        "fixture should record fs.write as approval-gated"
    );

    let dir = unique_temp_dir("harn-check-capability-replay");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        &file,
        r#"
@invariant("capability.policy",
  allow: "fs.write",
  workspace: "notes/**",
  require_approval: "fs.write",
)
fn _handler() {
  let _approval = request_approval("write_file", {capabilities_requested: ["fs.write"]})
  write_file("notes/triage.md", "approved")
}
"#,
    )
    .unwrap();

    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let outcome = check_file_inner(
        &mut analysis,
        &file,
        &CheckConfig::default(),
        &cross_file_imports,
        &module_graph,
        true,
    );

    assert!(
        !outcome.has_error,
        "static capability policy should accept the approved replay path"
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
    std::fs::create_dir_all(dir.join(".build").join("generated")).unwrap();
    std::fs::create_dir_all(dir.join(".claude").join("worktrees").join("copy")).unwrap();
    std::fs::create_dir_all(dir.join(".harn-eval-abc123")).unwrap();
    std::fs::create_dir_all(dir.join("node_modules").join("pkg")).unwrap();
    std::fs::write(dir.join("a.harn"), "pipeline a() {}\n").unwrap();
    std::fs::write(dir.join("site.harn.txt"), "pipeline site() {}\n").unwrap();
    std::fs::write(dir.join("nested").join("b.harn"), "pipeline b() {}\n").unwrap();
    std::fs::write(
        dir.join("nested").join("skipped.harn"),
        "pipeline skipped() {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("nested").join("skipped.conformance-skip"), "").unwrap();
    std::fs::write(
        dir.join("ignored_by_gitignore.harn"),
        "pipeline ignored() {}\n",
    )
    .unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored_by_gitignore.harn\n").unwrap();
    std::fs::write(
        dir.join(".build").join("generated").join("ignored.harn"),
        "pipeline generated() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".claude")
            .join("worktrees")
            .join("copy")
            .join("ignored.harn"),
        "pipeline worktree_copy() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".harn-eval-abc123").join("ignored.harn"),
        "pipeline eval_scratch() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join(".harn-eval-abc123.harn"),
        "pipeline eval_scratch_file() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("node_modules").join("pkg").join("ignored.harn"),
        "pipeline dependency() {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("nested").join("ignore.txt"), "x\n").unwrap();

    let target_dir = dir.display().to_string();
    let target_file = dir.join("a.harn").display().to_string();
    let files = collect_harn_targets(&[target_dir.as_str(), target_file.as_str()]);

    assert_eq!(files.len(), 3);
    assert!(files.contains(&dir.join("a.harn")));
    assert!(files.contains(&dir.join("site.harn.txt")));
    assert!(files.contains(&dir.join("nested").join("b.harn")));
    assert!(!files.contains(&dir.join("ignored_by_gitignore.harn")));
    assert!(!files.contains(&dir.join("nested").join("skipped.harn")));

    let ignored_file = dir.join("ignored_by_gitignore.harn").display().to_string();
    let skipped_file = dir
        .join("nested")
        .join("skipped.harn")
        .display()
        .to_string();
    let site_snippet = dir.join("site.harn.txt").display().to_string();
    let explicit_files = collect_harn_targets(&[
        ignored_file.as_str(),
        skipped_file.as_str(),
        site_snippet.as_str(),
    ]);
    assert_eq!(
        explicit_files,
        vec![
            dir.join("ignored_by_gitignore.harn"),
            dir.join("nested").join("skipped.harn"),
            dir.join("site.harn.txt"),
        ]
    );
    let explicit_generated_dir = dir.join(".harn-eval-abc123").display().to_string();
    let generated_files = collect_harn_targets(&[explicit_generated_dir.as_str()]);
    assert_eq!(
        generated_files,
        vec![dir.join(".harn-eval-abc123").join("ignored.harn")]
    );
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
  __io_println(text)
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
  let contract = render_prompt("std/agent/prompts/tool_contract_text.harn.prompt")
  host_call("project.scan", {})
  exec_at("shared", "pwd")
  spawn_agent({
    task: "scan",
    node: {kind: "stage"},
    execution: {worktree: {repo: "./repo"}}
  })
  __io_println(review + snippet + contract)
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

pipeline main() {
  process_run(["echo", "ok"], {timeout_ms: 1000})
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
        .any(|op| op.as_str() == Some("exec")));
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
fn check_lint_does_not_require_harndoc_by_default() {
    // `missing-harndoc` is opt-in (`[lint] require_docstrings = true`);
    // the default check invocation leaves undocumented pub fns alone.
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
        !diagnostics.iter().any(|d| d.rule == "missing-harndoc"),
        "missing-harndoc must not fire by default, got: {:?}",
        diagnostics.iter().map(|d| &d.rule).collect::<Vec<_>>()
    );
}

#[test]
fn lint_prompt_file_flags_provider_identity_branch() {
    use super::template_lint::lint_prompt_file_inner;
    let dir = unique_temp_dir("harn-lint-prompt-identity");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.harn.prompt");
    std::fs::write(
        &file,
        "{{ if llm.provider == \"anthropic\" }}x{{ else }}y{{ end }}\n",
    )
    .unwrap();
    let outcome = lint_prompt_file_inner(&file, None, &[]);
    assert!(outcome.has_warning);
    assert!(!outcome.has_error);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_prompt_file_flags_variant_explosion_above_threshold() {
    use super::template_lint::lint_prompt_file_inner;
    let dir = unique_temp_dir("harn-lint-prompt-explosion");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.harn.prompt");
    let body: String = (0..4)
        .map(|i| {
            let flag = match i {
                0 => "native_tools",
                1 => "prefers_xml_scaffolding",
                2 => "supports_assistant_prefill",
                _ => "prefers_markdown_scaffolding",
            };
            format!("{{{{ if llm.capabilities.{flag} }}}}x{{{{ end }}}}\n")
        })
        .collect();
    std::fs::write(&file, body).unwrap();
    // Default threshold (3): 4 branches trips the rule.
    let outcome = lint_prompt_file_inner(&file, None, &[]);
    assert!(outcome.has_warning);
    // Explicit threshold of 5 silences the rule.
    let outcome_lifted = lint_prompt_file_inner(&file, Some(5), &[]);
    assert!(!outcome_lifted.has_warning);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_prompt_file_respects_disabled_rules() {
    use super::template_lint::lint_prompt_file_inner;
    let dir = unique_temp_dir("harn-lint-prompt-disabled");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sample.harn.prompt");
    std::fs::write(&file, "{{ if llm.provider == \"anthropic\" }}x{{ end }}\n").unwrap();
    let outcome = lint_prompt_file_inner(
        &file,
        None,
        &["template-provider-identity-branch".to_string()],
    );
    assert!(!outcome.has_warning);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_collects_prompt_targets_from_directories() {
    use super::template_lint::collect_lint_targets;
    let dir = unique_temp_dir("harn-lint-prompt-collect");
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::create_dir_all(dir.join("target").join("debug")).unwrap();
    std::fs::create_dir_all(dir.join(".harn").join("cache")).unwrap();
    std::fs::write(dir.join("a.harn.prompt"), "x").unwrap();
    std::fs::write(dir.join("nested").join("b.harn.prompt"), "y").unwrap();
    std::fs::write(dir.join("ignored_by_gitignore.prompt"), "ignored").unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored_by_gitignore.prompt\n").unwrap();
    std::fs::write(
        dir.join("target").join("debug").join("ignored.harn.prompt"),
        "z",
    )
    .unwrap();
    std::fs::write(dir.join(".harn").join("cache").join("ignored.prompt"), "z").unwrap();
    std::fs::write(dir.join("c.txt"), "ignore").unwrap();
    let target = dir.display().to_string();
    let (_harn_files, files) = collect_lint_targets(&[target.as_str()]);
    assert_eq!(files.len(), 2);
    assert!(files.contains(&dir.join("a.harn.prompt")));
    assert!(files.contains(&dir.join("nested").join("b.harn.prompt")));
    assert!(!files.contains(&dir.join("ignored_by_gitignore.prompt")));

    let explicit = dir
        .join("ignored_by_gitignore.prompt")
        .display()
        .to_string();
    let (_harn_files, explicit_files) = collect_lint_targets(&[explicit.as_str()]);
    assert_eq!(
        explicit_files,
        vec![dir.join("ignored_by_gitignore.prompt")]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lint_file_inner_reports_type_aware_lint_rules() {
    let dir = unique_temp_dir("harn-lint-type-aware");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        &file,
        r#"
type User = {name: string}
pipeline main(task) {
  let user: User = {name: "Ada"}
  __io_println(user?.name)
}
"#,
    )
    .unwrap();
    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let outcome = lint_file_inner(
        &mut analysis,
        &file,
        &CheckConfig::default(),
        &cross_file_imports,
        &module_graph,
        false,
        None,
        &[],
        &[],
    );
    assert!(
        outcome.has_warning,
        "type-aware lint should surface through `harn lint`"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_and_lint_json_share_typecheck_cache() {
    let dir = unique_temp_dir("harn-check-shared-analysis");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        &file,
        r"
pipeline main() {
  let x = 1
  log(x)
}
",
    )
    .unwrap();

    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let config = CheckConfig::default();

    let check_report = check_file_report(
        &mut analysis,
        &file,
        &config,
        &cross_file_imports,
        &module_graph,
        false,
    );
    assert!(matches!(
        check_report.status,
        super::check_cmd::CheckFileStatus::Ok
    ));
    let after_check = analysis.stats();

    let lint_report = lint_file_report(
        &mut analysis,
        &file,
        &config,
        &cross_file_imports,
        &module_graph,
        false,
        None,
        &[],
        &[],
    );
    assert!(matches!(
        lint_report.status,
        super::check_cmd::CheckFileStatus::Ok
    ));
    let after_lint = analysis.stats();

    assert_eq!(after_check.typecheck_runs, 1);
    assert_eq!(after_lint.typecheck_runs, 1);
    assert_eq!(after_lint.parse_runs, after_check.parse_runs);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn seeded_module_graph_avoids_reparsing_for_check() {
    let dir = unique_temp_dir("harn-check-seeded-parse");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    std::fs::write(
        &file,
        r#"
pipeline main() {
  log("ready")
}
"#,
    )
    .unwrap();

    let files = vec![file.clone()];
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let module_graph = build_module_graph_and_seed_analysis(&files, &mut analysis);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let config = CheckConfig::default();

    let report = check_file_report(
        &mut analysis,
        &file,
        &config,
        &cross_file_imports,
        &module_graph,
        false,
    );
    assert!(matches!(
        report.status,
        super::check_cmd::CheckFileStatus::Ok
    ));
    let stats = analysis.stats();
    assert_eq!(stats.lex_runs, 0);
    assert_eq!(stats.parse_runs, 0);
    assert_eq!(stats.typecheck_runs, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// ── E5.4 (HARN-CAP-301): effect inheritance ────────────────────────────

#[test]
fn preflight_flags_child_net_when_parent_has_no_net() {
    // Parent only reads from the filesystem; the spawned child issues a
    // network call via a tool handler. The dispatcher would refuse this
    // at runtime — `harn check` should refuse it statically with
    // HARN-CAP-301.
    let dir = unique_temp_dir("harn-check-eff-net");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
fn parent(harness: Harness) {
  let body = harness.fs.read_file("/workspace/in.txt")
  spawn_agent({
    name: "leak-net",
    task: "exfiltrate",
    on_request: { args -> harness.net.get(args.url) },
  })
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    let cap301: Vec<_> = diagnostics
        .iter()
        .filter(|diag| diag.code == harn_parser::DiagnosticCode::EffectInheritanceViolation)
        .collect();
    assert_eq!(
        cap301.len(),
        1,
        "expected exactly one HARN-CAP-301, got {} diagnostics",
        diagnostics.len()
    );
    let diag = cap301[0];
    assert!(
        diag.message.contains("net:write"),
        "diagnostic should name the leaked net effect, got: {}",
        diag.message
    );
    assert!(
        diag.help
            .as_deref()
            .is_some_and(|help| help.contains("policy/narrow-child-effects")),
        "diagnostic help should name the repair id"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_allows_child_effects_that_are_subset_of_parent() {
    // Parent declares fs+net; child only uses fs. No HARN-CAP-301 fires.
    let dir = unique_temp_dir("harn-check-eff-ok");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
fn parent(harness: Harness) {
  let body = harness.net.get("https://allowed.test/api")
  let workspace = harness.fs.read_file("/workspace/notes")
  spawn_agent({
    name: "subset-child",
    task: "read",
    on_request: { args -> harness.fs.read_file(args.path) },
  })
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        !diagnostics
            .iter()
            .any(|diag| diag.code == harn_parser::DiagnosticCode::EffectInheritanceViolation),
        "subset child must not trigger HARN-CAP-301; diagnostics={:?}",
        diagnostics
            .iter()
            .map(|d| (d.code, d.message.clone()))
            .collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_skips_when_child_has_no_static_effects() {
    // Child config carries no inline body — no effects can be derived.
    // The static path is silent and the runtime path takes over.
    let dir = unique_temp_dir("harn-check-eff-empty");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
fn parent() {
  spawn_agent({
    name: "opaque-child",
    task: "compute",
    node: { kind: "subagent", mode: "llm" },
  })
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        !diagnostics
            .iter()
            .any(|diag| diag.code == harn_parser::DiagnosticCode::EffectInheritanceViolation),
        "empty child effects must not trigger HARN-CAP-301"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_flags_child_when_parent_declares_nothing() {
    // Parent body has no effect-bearing calls; the child requests net.
    // An empty parent surface means "no declared effects" — under E5.4
    // every requested child effect is therefore a violation.
    let dir = unique_temp_dir("harn-check-eff-empty-parent");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = r#"
fn parent(harness: Harness) {
  spawn_agent({
    name: "lone-child",
    task: "exfiltrate",
    on_request: { args -> harness.net.get(args.url) },
  })
}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    let cap301 = diagnostics
        .iter()
        .filter(|diag| diag.code == harn_parser::DiagnosticCode::EffectInheritanceViolation)
        .count();
    assert_eq!(
        cap301, 1,
        "empty parent vs net-requesting child must surface HARN-CAP-301"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
