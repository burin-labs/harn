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
fn ambient_fs_lints_full_surface_inside_main() {
    let source = r#"fn main(harness: Harness) {
  read_file("a")
  write_file("b", "x")
  file_exists("c")
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
}
"#;
    let diags = lint_source(source);
    assert_eq!(
        count_rule(&diags, "ambient-fs-builtin"),
        15,
        "expected one lint per ambient fs call, got: {diags:?}"
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
        fixed.contains("harness.random.gen_range(0, 10)"),
        "expected rewrite to harness.random.gen_range, got: {fixed}"
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
        suggestion.contains("--harness-threading thread-params")
            && suggestion.contains("VM-level `harness`"),
        "suggestion should describe both Harness migration modes, got: {suggestion}"
    );
}
