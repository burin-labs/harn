//! Coverage for IR construction, call classification, and each invariant.

use crate::*;
use harn_glob::match_path as glob_match;
use harn_parser::SNode;

fn parse_program(source: &str) -> Vec<SNode> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().expect("tokenize");
    let mut parser = harn_parser::Parser::new(tokens);
    parser.parse().expect("parse")
}

fn analyze(source: &str) -> AnalysisReport {
    analyze_program(&parse_program(source))
}

fn diagnostics_by_invariant<'a>(
    report: &'a AnalysisReport,
    invariant: &str,
) -> Vec<&'a InvariantDiagnostic> {
    report
        .diagnostics
        .iter()
        .filter(|diag| diag.invariant == invariant)
        .collect()
}

fn handler_call_names(report: &AnalysisReport) -> Vec<String> {
    report
        .handlers
        .iter()
        .flat_map(|h| h.nodes.iter())
        .filter_map(|node| match &node.semantics {
            NodeSemantics::Call(call) => Some(call.name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn harness_fs_method_call_keeps_capability_identity() {
    let report = analyze(
        r#"
fn main(harness: Harness) {
  const body = harness.fs.read_text("notes.txt")
  harness.fs.mkdtemp("harn-ir-")
  harness.stdio.println(body)
}
"#,
    );

    let calls = handler_call_names(&report);
    assert!(
        calls.iter().any(|name| name == "harness.fs.read_text"),
        "expected harness.fs.read_text to keep its capability identity, got: {calls:?}"
    );
    assert!(
        calls.iter().any(|name| name == "harness.fs.mkdtemp"),
        "expected harness.fs.mkdtemp to keep its capability identity, got: {calls:?}"
    );
    assert!(
        calls.iter().any(|name| name == "harness.stdio.println"),
        "expected harness.stdio.println to keep its capability identity, got: {calls:?}"
    );
}

#[test]
fn harness_net_method_call_keeps_capability_identity() {
    let report = analyze(
        r#"
fn main(harness: Harness) {
  harness.net.get("https://api.example.com")
}
"#,
    );

    let calls = handler_call_names(&report);
    assert!(
        calls.iter().any(|name| name == "harness.net.get"),
        "expected harness.net.get to keep its capability identity, got: {calls:?}"
    );
}

#[test]
fn harness_term_method_calls_keep_capability_identity() {
    let report = analyze(
        r#"
fn main(harness: Harness) {
  harness.term.width()
  harness.term.height()
  harness.term.read_password("password: ")
}
"#,
    );

    let calls = handler_call_names(&report);
    assert!(
        calls.iter().any(|name| name == "harness.term.width"),
        "expected harness.term.width to keep its capability identity, got: {calls:?}"
    );
    assert!(
        calls.iter().any(|name| name == "harness.term.height"),
        "expected harness.term.height to keep its capability identity, got: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|name| name == "harness.term.read_password"),
        "expected harness.term.read_password to keep its capability identity, got: {calls:?}"
    );
}

#[test]
fn harness_process_method_call_keeps_capability_identity() {
    let report = analyze(
        r#"
fn main(harness: Harness) {
  harness.process.run({program: "printf", args: ["hi"]})
}
"#,
    );

    let calls = handler_call_names(&report);
    assert!(
        calls.iter().any(|name| name == "harness.process.run"),
        "expected harness.process.run to keep its capability identity, got: {calls:?}"
    );
}

#[test]
fn deterministic_crypto_is_a_pure_global() {
    let report = analyze(
        r#"
fn main(harness: Harness) {
  sha256_hex("hello")
}
"#,
    );

    let calls = handler_call_names(&report);
    assert!(
        calls.iter().any(|name| name == "sha256_hex"),
        "expected the deterministic digest to remain a pure global, got: {calls:?}"
    );
}

#[test]
fn harness_llm_method_calls_keep_capability_identity() {
    let report = analyze(
        r"
fn main(harness: Harness) {
  harness.llm.catalog()
  harness.llm.providers()
}
",
    );

    let calls = handler_call_names(&report);
    assert!(
        calls.iter().any(|name| name == "harness.llm.catalog"),
        "expected harness.llm.catalog to keep its capability identity, got: {calls:?}"
    );
    assert!(
        calls.iter().any(|name| name == "harness.llm.providers"),
        "expected harness.llm.providers to keep its capability identity, got: {calls:?}"
    );
}

#[test]
fn fs_writes_within_glob_passes() {
    let report = analyze(
        r#"
@invariant("fs.writes", "src/**")
fn handler(harness: Harness) {
  harness.fs.write_text("src/main.rs", "ok")
}
"#,
    );

    assert!(
        diagnostics_by_invariant(&report, "fs.writes").is_empty(),
        "unexpected diagnostics: {:?}",
        report.diagnostics
    );
}

#[test]
fn fs_writes_outside_glob_fails() {
    let report = analyze(
        r#"
@invariant("fs.writes", "src/**")
fn handler(harness: Harness) {
  harness.fs.write_text("/tmp/main.rs", "nope")
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "fs.writes");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("outside the allowed glob"));
    assert!(diags[0]
        .path
        .iter()
        .any(|step| step.label.contains("harness.fs.write_text")));
}

#[test]
fn fs_writes_attributes_nominal_narrow_handle_calls() {
    let report = analyze(
        r#"
@invariant("fs.writes", "src/**")
fn write_output(fs: HarnessFs) {
  fs.write_text("/tmp/main.rs", "nope")
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "fs.writes");
    assert_eq!(diags.len(), 1, "{diags:#?}");
    assert!(diags[0].message.contains("/tmp/main.rs"));
    assert!(diags[0]
        .path
        .iter()
        .any(|step| step.label.contains("harness.fs.write_text")));
}

#[test]
fn approval_requires_gate_on_all_paths() {
    let report = analyze(
        r#"
@invariant("approval.reachability")
fn handler(harness: Harness) {
  if true {
    harness.interaction.request_approval("ship it")
  }
  harness.fs.write_text("src/main.rs", "unsafe")
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "approval.reachability");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("before any approval gate"));
}

#[test]
fn approval_inside_dual_control_closure_is_accepted() {
    let report = analyze(
        r#"
@invariant("approval.reachability")
fn handler(harness: Harness) {
  dual_control(2, 3, { ->
    harness.fs.write_text("src/main.rs", "safe")
  }, ["alice", "bob", "carol"])
}
"#,
    );

    assert!(
        diagnostics_by_invariant(&report, "approval.reachability").is_empty(),
        "unexpected diagnostics: {:?}",
        report.diagnostics
    );
}

#[test]
fn budget_remaining_rejects_addition() {
    let report = analyze(
        r#"
@invariant("budget.remaining", target: "remaining")
fn handler() {
  let remaining = llm_budget_remaining()
  remaining = remaining + 1
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "budget.remaining");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("may increase"));
}

#[test]
fn budget_remaining_accepts_subtraction() {
    let report = analyze(
        r#"
@invariant("budget.remaining", target: "remaining")
fn handler(cost) {
  let remaining = llm_budget_remaining()
  remaining -= cost
}
"#,
    );

    assert!(
        diagnostics_by_invariant(&report, "budget.remaining").is_empty(),
        "unexpected diagnostics: {:?}",
        report.diagnostics
    );
}

#[test]
fn capability_policy_rejects_undeclared_connector_access() {
    let report = analyze(
        r#"
@invariant("capability.policy", allow: "fs.write")
fn handler(harness: Harness, client) {
  harness.tools.mcp_call(client, "github.search", {})
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "capability.policy");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("mcp.connector"));
    assert!(diags[0].message.contains("not declared"));
    assert_eq!(diags[0].handler, "handler");
    assert!(diags[0]
        .path
        .iter()
        .any(|step| step.label.contains("harness.tools.mcp_call")));
}

#[test]
fn capability_policy_rejects_workspace_mutation_outside_allowed_glob() {
    let report = analyze(
        r#"
@invariant("capability.policy", allow: "fs.write", workspace: "src/**")
fn handler(harness: Harness) {
  harness.fs.write_text("/tmp/out.txt", "unsafe")
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "capability.policy");
    assert_eq!(diags.len(), 1);
    assert!(diags[0]
        .message
        .contains("outside the allowed workspace glob"));
}

#[test]
fn capability_policy_accepts_approved_workspace_mutation_and_budgeted_llm() {
    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "fs.write,llm.model",
  workspace: "src/**",
  require_approval: "fs.write",
  require_budget: "llm.model")
fn handler(harness: Harness) {
  harness.interaction.request_approval("edit", {capabilities_requested: ["fs.write"]})
  harness.fs.write_text("src/main.rs", "safe")
  harness.llm.call("summarize", nil, {budget: {max_output_tokens: 64}})
}
"#,
    );

    assert!(
        diagnostics_by_invariant(&report, "capability.policy").is_empty(),
        "unexpected diagnostics: {:?}",
        report.diagnostics
    );
}

#[test]
fn capability_policy_requires_command_policy_for_exec() {
    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "process.exec",
  require_command_policy: "process.exec")
fn handler(harness: Harness) {
  harness.process.shell("rm -rf /tmp/harn")
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "capability.policy");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("process.exec"));
    assert!(diags[0].message.contains("command policy"));

    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "process.exec",
  require_command_policy: "process.exec")
fn handler(harness: Harness) {
  harness.runtime.with_command_policy({deny: ["rm"]}, { ->
    harness.process.shell("echo ok")
  })
}
"#,
    );

    assert!(
        diagnostics_by_invariant(&report, "capability.policy").is_empty(),
        "unexpected diagnostics: {:?}",
        report.diagnostics
    );
}

#[test]
fn capability_policy_tracks_command_policy_push_and_pop() {
    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "process.exec",
  require_command_policy: "process.exec")
fn handler(harness: Harness) {
  harness.runtime.command_policy_push({deny: ["rm"]})
  harness.process.shell("echo ok")
  harness.runtime.command_policy_pop()
}
"#,
    );

    assert!(
        diagnostics_by_invariant(&report, "capability.policy").is_empty(),
        "unexpected diagnostics: {:?}",
        report.diagnostics
    );

    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "process.exec",
  require_command_policy: "process.exec")
fn handler(harness: Harness) {
  harness.runtime.command_policy_push({deny: ["rm"]})
  harness.runtime.command_policy_pop()
  harness.process.shell("echo unsafe")
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "capability.policy");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("command policy"));
}

#[test]
fn capability_policy_requires_egress_policy_for_network_and_connector_access() {
    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "network.access,mcp.connector",
  require_egress_policy: "network.access,mcp.connector")
fn handler(harness: Harness, client) {
  harness.net.request("GET", "https://example.com")
  harness.tools.mcp_call(client, "github.search", {})
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "capability.policy");
    assert_eq!(diags.len(), 2);
    assert!(diags
        .iter()
        .any(|diag| diag.message.contains("network.access")));
    assert!(diags
        .iter()
        .any(|diag| diag.message.contains("mcp.connector")));

    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "network.access,mcp.connector",
  require_egress_policy: "network.access,mcp.connector")
fn handler(harness: Harness, client) {
  harness.net.egress_policy({default: "deny", allow: ["example.com"]})
  harness.net.request("GET", "https://example.com")
  harness.tools.mcp_call(client, "github.search", {})
}
"#,
    );

    assert!(
        diagnostics_by_invariant(&report, "capability.policy").is_empty(),
        "unexpected diagnostics: {:?}",
        report.diagnostics
    );
}

#[test]
fn capability_policy_treats_unix_socket_json_request_as_network_access() {
    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "network.access",
  require_egress_policy: "network.access")
fn handler(harness: Harness) {
  harness.net.unix_socket_json_request("/tmp/harn.sock", {})
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "capability.policy");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("network.access"));
}

#[test]
fn capability_policy_requires_autonomy_policy_for_worker_dispatch() {
    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "worker.dispatch",
  require_autonomy: "worker.dispatch")
fn handler(harness: Harness) {
  harness.agent.worker_spawn({task: "summarize"})
}
"#,
    );

    let diags = diagnostics_by_invariant(&report, "capability.policy");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("worker.dispatch"));
    assert!(diags[0].message.contains("autonomy policy"));

    let report = analyze(
        r#"
@invariant("capability.policy",
  allow: "worker.dispatch",
  require_autonomy: "worker.dispatch")
fn handler(harness: Harness) {
  harness.runtime.with_autonomy_policy({autonomy_tier: "act_with_approval"}, { ->
    harness.agent.worker_spawn({task: "summarize"})
  })
}
"#,
    );

    assert!(
        diagnostics_by_invariant(&report, "capability.policy").is_empty(),
        "unexpected diagnostics: {:?}",
        report.diagnostics
    );
}

#[test]
fn explain_returns_violation_path() {
    let diags = explain_handler_invariant(
        &parse_program(
            r#"
@invariant("approval.reachability")
fn handler(harness: Harness) {
  harness.fs.write_text("src/main.rs", "unsafe")
}
"#,
        ),
        "handler",
        "approval.reachability",
    )
    .expect("explain succeeds");

    assert_eq!(diags.len(), 1);
    assert!(diags[0].path.len() >= 2);
}

#[test]
fn glob_match_supports_single_and_double_star() {
    assert!(glob_match("src/*.rs", "src/main.rs"));
    assert!(!glob_match("src/*.rs", "src/nested/main.rs"));
    assert!(glob_match("src/**/*.rs", "src/nested/main.rs"));
    // `**/` also matches zero directories (git/globset convention).
    assert!(glob_match("src/**/*.rs", "src/main.rs"));
}
