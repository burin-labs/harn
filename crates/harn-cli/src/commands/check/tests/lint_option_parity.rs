//! `harn check` and `harn lint` must answer the same question the same way
//! (harn#6171).
//!
//! `harn check` runs the lint rules too, but built their options from
//! `LintOptions::default()` — so the trust declaration never reached them and
//! `harn check --trusted-host-dispatch` cleared the `HARN-NAM-002` type error
//! on a privileged wire while still reporting `HARN-LNT-072` for the same call.

use crate::commands::check::check_cmd::{check_file_report_inner, CheckTextOutput};
use crate::commands::check::config::{build_module_graph, collect_cross_file_imports};
use crate::commands::check::host_capabilities::resolve_host_capabilities;
use crate::package::CheckConfig;

/// A privileged wire call — the construct `trusted_host_dispatch` admits.
const PRIVILEGED_WIRE: &str =
    "fn main(harness: Harness) {\n  let out = host_call(\"ast.outline\", {path: \"a.rs\"})\n  harness.stdio.println(out)\n}\n";

/// Diagnostic codes `harn check` reports for `source` under `config`.
fn check_codes(source: &str, config: &CheckConfig) -> Vec<String> {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("wire.harn");
    std::fs::write(&file, source).expect("write source");

    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let host_capabilities = resolve_host_capabilities(config);

    let mut text = CheckTextOutput::default();
    let report = check_file_report_inner(
        &mut analysis,
        &file,
        config,
        &cross_file_imports,
        &module_graph,
        &host_capabilities.capabilities,
        false,
        Some(&mut text),
    );
    report
        .diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.code.clone())
        .collect()
}

#[test]
fn trusted_host_dispatch_reaches_the_lint_pass_inside_check() {
    let untrusted = check_codes(PRIVILEGED_WIRE, &CheckConfig::default());
    assert!(
        untrusted.iter().any(|code| code == "HARN-LNT-072"),
        "an unprivileged check must report the wire: {untrusted:?}"
    );

    let trusted = CheckConfig {
        trusted_host_dispatch: true,
        ..CheckConfig::default()
    };
    let codes = check_codes(PRIVILEGED_WIRE, &trusted);
    assert!(
        !codes.iter().any(|code| code == "HARN-LNT-072"),
        "trusted dispatch must silence the lint too, not just the type error: {codes:?}"
    );
    assert!(
        !codes.iter().any(|code| code == "HARN-NAM-002"),
        "the type error was already silenced and must stay silenced: {codes:?}"
    );
}
