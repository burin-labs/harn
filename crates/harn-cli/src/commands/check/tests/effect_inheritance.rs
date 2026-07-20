use super::*;

#[test]
fn preflight_detects_struct_and_enum_import_collisions() {
    let dir = unique_temp_dir("harn-check-type-collision");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(
        dir.join("lib").join("a.harn"),
        "pub struct Shared { value: int }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib").join("b.harn"),
        "pub enum Shared { Ready }\n",
    )
    .unwrap();
    let file = dir.join("main.harn");
    let source = r#"
import "lib/a.harn"
import "lib/b.harn"

pipeline main() {}
"#;
    let program = parse_program(source);
    let diagnostics =
        collect_preflight_diagnostics(&file, source, &program, &CheckConfig::default());
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("import collision")),
        "expected type import collision diagnostic, got: {:?}",
        diagnostics
            .iter()
            .map(|diagnostic| &diagnostic.message)
            .collect::<Vec<_>>()
    );
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
  const body = harness.fs.read_file("/workspace/in.txt")
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
  const body = harness.net.get("https://allowed.test/api")
  const workspace = harness.fs.read_file("/workspace/notes")
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
