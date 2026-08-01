//! HARN-LNT-054..057 — ambient fs/env/random/net builtins now route
//! through `harness.{fs,env,random,net}.*`. Direct lint fixes run only
//! when a Harness binding is already in scope; `harn fix` owns broader
//! migration planning.

use super::*;

#[test]
fn ambient_fs_call_inside_main_rewrites_to_harness_fs() {
    let source =
        "fn main(harness: Harness) {\n  let body = read_file(\"path.txt\")\n  harness.stdio.println(body)\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-fs-builtin"),
        1,
        "expected one ambient-fs lint, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("harness.fs.read_text(\"path.txt\")"),
        "expected rewrite to harness.fs.read_text, got: {fixed}"
    );
}

#[test]
fn ambient_fs_mkdtemp_inside_main_rewrites_to_harness_fs() {
    let source = "fn main(harness: Harness) {\n  let dir = mkdtemp(\"harn-\")\n}\n";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "ambient-fs-builtin"), 1);
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("harness.fs.mkdtemp(\"harn-\")"),
        "expected rewrite to harness.fs.mkdtemp, got: {fixed}"
    );
}

#[test]
fn ambient_fs_path_status_inside_main_rewrites_to_harness_fs_status() {
    let source = "fn main(harness: Harness) {\n  let status = path_status(\"path.txt\")\n}\n";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "ambient-fs-builtin"), 1);
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("harness.fs.status(\"path.txt\")"),
        "expected rewrite to harness.fs.status, got: {fixed}"
    );
}

#[test]
fn ambient_fs_lints_full_surface_inside_main() {
    let source = r#"fn main(harness: Harness) {
  read_file("a")
  write_file("b", "x")
  file_exists("c")
  path_status("c")
  delete_file("d")
  append_file("e", "y")
  list_dir("f")
  mkdir("g")
  copy_file("h", "i")
  temp_dir()
  mkdtemp("tmp-")
  stat("j")
  move_file("k", "l")
  read_lines("m")
  walk_dir("n")
  glob("o")
  cwd()
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-fs-builtin"),
        17,
        "expected one lint per ambient fs call, got: {diags:?}"
    );
}

#[test]
fn ambient_cwd_default_rewrites_to_harness_fs() {
    let source =
        "fn resolve(harness: Harness, path: string, base: string = cwd()) -> string {\n  return base + path\n}\n";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "ambient-fs-builtin"), 1);
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("base: string = harness.fs.cwd()"),
        "expected default-value rewrite to harness.fs.cwd, got: {fixed}"
    );
}

#[test]
fn ambient_metadata_builtin_recommends_typed_project_request() {
    let source = "fn main(harness: Harness) {\n  metadata_get(\"src\", \"classification\")\n}\n";
    let diags = lint_source(source);
    let diagnostic = diags
        .iter()
        .find(|diag| diag.rule == "ambient-harness-method")
        .expect("ambient metadata diagnostic");
    assert!(
        diagnostic.message.contains("harness.project.metadata_get"),
        "{diagnostic:?}"
    );
    assert!(
        diagnostic
            .suggestion
            .as_deref()
            .is_some_and(|suggestion| suggestion
                .contains("harness.project.metadata_get({dir: ..., namespace: ...})")),
        "request-record migration needs an actionable shape: {diagnostic:?}"
    );
    assert!(
        diagnostic.fix.is_none(),
        "request-record migration belongs to harn fix"
    );
}

#[test]
fn ambient_env_call_rewrites_to_harness_env() {
    let source =
        "fn main(harness: Harness) {\n  let v = env(\"HOME\")\n  harness.stdio.println(v)\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-env-builtin"),
        1,
        "expected one ambient-env lint, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("harness.env.get(\"HOME\")"),
        "expected rewrite to harness.env.get, got: {fixed}"
    );
}

#[test]
fn ambient_env_or_rewrites_to_harness_env_get_or() {
    let source = "fn main(harness: Harness) {\n  let v = env_or(\"X\", \"default\")\n}\n";
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "ambient-env-builtin"), 1);
    let fixed = apply_fixes(source, &diags);
    assert!(fixed.contains("harness.env.get_or(\"X\", \"default\")"));
}

#[test]
fn ambient_random_call_rewrites_to_harness_random() {
    let source =
        "fn main(harness: Harness) {\n  let n = random_int(0, 10)\n  harness.stdio.println(n)\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-random-builtin"),
        1,
        "expected one ambient-random lint, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("harness.random.range(0, 10)"),
        "expected rewrite to harness.random.range, got: {fixed}"
    );
}

#[test]
fn explicit_seeded_random_calls_are_not_ambient_host_random() {
    let source = r#"fn main(harness: Harness) {
  let rng = rng_seed(42)
  random(rng)
  random_int(rng, 0, 10)
  random_choice(rng, ["a", "b"])
  random_shuffle(rng, [1, 2])
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-random-builtin"),
        0,
        "seeded Rng calls should stay on the deterministic Rng surface: {diags:?}"
    );
}

#[test]
fn ambient_net_call_rewrites_to_harness_net() {
    let source =
        "fn main(harness: Harness) {\n  let r = http_get(\"https://example.test\")\n  harness.stdio.println(r)\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-net-builtin"),
        1,
        "expected one ambient-net lint, got: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("harness.net.get(\"https://example.test\")"),
        "expected rewrite to harness.net.get, got: {fixed}"
    );
}

#[test]
fn ambient_net_lint_rewrites_lifecycle_surfaces_to_harness_net() {
    let source = r#"fn main(harness: Harness) {
  let server = http_server({})
  http_server_route(server, "GET", "/", { request -> http_response_text("ok") })
  let session = http_session({})
  http_session_request(session, "GET", "https://example.test")
  let stream = http_stream_open("https://example.test")
  http_stream_read(stream)
  let sse = sse_connect("GET", "https://example.test")
  sse_receive(sse)
  let websocket = websocket_connect("wss://example.test")
  websocket_receive(websocket)
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-net-builtin"),
        10,
        "expected every effectful lifecycle call to migrate: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    for expected in [
        "harness.net.server({})",
        "harness.net.server_route(",
        "harness.net.session({})",
        "harness.net.session_request(",
        "harness.net.stream_open(",
        "harness.net.stream_read(",
        "harness.net.sse_connect(",
        "harness.net.sse_receive(",
        "harness.net.websocket_connect(",
        "harness.net.websocket_receive(",
    ] {
        assert!(
            fixed.contains(expected),
            "expected `{expected}` in migrated source: {fixed}"
        );
    }
    assert!(
        fixed.contains("http_response_text(\"ok\")"),
        "pure response constructors must remain global: {fixed}"
    );
}

#[test]
fn ambient_capability_lint_without_harness_param_keeps_no_fix() {
    let source = "fn helper() {\n  let _ = read_file(\"x\")\n}\n";
    let diags = lint_source(source);
    let entry = diags
        .iter()
        .find(|d| d.rule == "ambient-fs-builtin")
        .expect("ambient-fs lint should fire even without harness in scope");
    assert!(
        entry.fix.is_none(),
        "should not auto-fix without harness in scope, got: {:?}",
        entry.fix
    );
    let suggestion = entry
        .suggestion
        .as_deref()
        .expect("lint must carry a suggestion");
    assert!(
        suggestion.contains("--safety surface-changing")
            && suggestion.contains("explicit capability"),
        "suggestion should describe explicit capability threading, got: {suggestion}"
    );
}

#[test]
fn manifest_owned_ambient_llm_method_rewrites_without_a_second_table() {
    let source = r#"fn main(harness: Harness) {
  const caps = provider_capabilities("anthropic", "claude-opus-4-7")
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-harness-method"),
        1,
        "manifest HarnessMethod exposure should drive the lint: {diags:?}"
    );
    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("harness.llm.provider_capabilities(\"anthropic\", \"claude-opus-4-7\")"),
        "expected registry-derived Harness rewrite: {fixed}"
    );
}

#[test]
fn legacy_host_projection_recommends_the_structured_typed_snapshot() {
    let source = "fn main(harness: Harness) {\n  const os = platform()\n}\n";
    let diags = lint_source(source);
    let entry = diags
        .iter()
        .find(|diag| diag.rule == "ambient-harness-method")
        .expect("legacy host projection should receive migration guidance");
    assert!(
        entry.fix.is_none(),
        "whole-call projection needs the CLI fixer"
    );
    assert!(
        entry
            .suggestion
            .as_deref()
            .is_some_and(|text| text.contains("harness.system.platform().os")),
        "unexpected projection guidance: {entry:?}"
    );
}

#[test]
fn ambient_calls_inside_interpolation_are_linted_with_absolute_spans() {
    let source = r#"fn main(harness: Harness) {
  const label = "host ${platform()} ${read_file("name.txt")}"
}
"#;
    let diags = lint_source(source);
    assert_eq!(count_rule(&diags, "ambient-harness-method"), 1, "{diags:?}");
    assert_eq!(count_rule(&diags, "ambient-fs-builtin"), 1, "{diags:?}");

    let fixed = apply_fixes(source, &diags);
    assert!(
        fixed.contains("${harness.fs.read_text(\"name.txt\")}"),
        "interpolation fix must target the containing source: {fixed}"
    );
}

#[test]
fn pure_global_with_similar_domain_is_not_an_ambient_harness_method() {
    let source = "fn main(harness: Harness) {\n  const value = json_parse(\"{}\")\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-harness-method"),
        0,
        "only HarnessMethod manifest entries should migrate: {diags:?}"
    );
}

#[test]
fn language_intrinsic_with_capability_name_is_not_an_ambient_harness_method() {
    let source = "fn main(harness: Harness) {\n  const task = spawn { harness.stdio.log(\"x\") }\n  cancel(task)\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-harness-method"),
        0,
        "language intrinsics remain source-callable even when a capability method shares the name: {diags:?}"
    );
}

#[test]
fn forward_declared_callable_is_not_an_ambient_harness_method() {
    let source = "fn main(harness: Harness) {\n  harness.stdio.println(counter())\n}\nfn counter() {\n  return 1\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-harness-method"),
        0,
        "hoisted source callables must win over same-named capability methods: {diags:?}"
    );
}

#[test]
fn wildcard_imported_callable_is_not_an_ambient_harness_method() {
    let source = "import \"std/runtime\"\nfn main(harness: Harness) {\n  runtime_prompt_content(harness.runtime)\n}\n";
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-harness-method"),
        0,
        "wildcard imports may supply the apparent ambient name: {diags:?}"
    );
}
