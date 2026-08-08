//! One human-readable output channel for `harn check` (harn#6168).
//!
//! The clean-file line used to be written to stdout while diagnostics went to
//! stderr, so `harn check DIR 2>/dev/null` printed `ok` for the clean files and
//! dropped every finding — and a genuinely clean corpus produces byte-identical
//! output, so the wrong answer never looks wrong. `CheckTextOutput` now carries
//! one buffer; these tests assert both kinds of line reach it.

use crate::commands::check::check_cmd::{check_file_report_inner, CheckTextOutput};
use crate::commands::check::config::{build_module_graph, collect_cross_file_imports};
use crate::commands::check::host_capabilities::resolve_host_capabilities;
use crate::package::CheckConfig;

/// Check `source` written to a temp file and return what the text report buffered.
fn rendered_report(name: &str, source: &str) -> String {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join(name);
    std::fs::write(&file, source).expect("write source");

    let files = vec![file.clone()];
    let module_graph = build_module_graph(&files);
    let cross_file_imports = collect_cross_file_imports(&module_graph);
    let mut analysis = harn_parser::analysis::AnalysisDatabase::new();
    let config = CheckConfig::default();
    let host_capabilities = resolve_host_capabilities(&config);

    let mut text = CheckTextOutput::default();
    check_file_report_inner(
        &mut analysis,
        &file,
        &config,
        &cross_file_imports,
        &module_graph,
        &host_capabilities.capabilities,
        false,
        Some(&mut text),
    );
    text.rendered
}

#[test]
fn a_clean_file_reports_on_the_same_channel_as_a_diagnostic() {
    let clean = rendered_report(
        "clean.harn",
        "fn main(harness: Harness) {\n  harness.stdio.println(\"ok\")\n}\n",
    );
    assert!(
        clean.contains(": ok"),
        "the clean-file line must land in the rendered buffer, got: {clean:?}"
    );

    // Same buffer, so suppressing one stream cannot leave the `ok` lines
    // behind while the findings disappear.
    let failing = rendered_report(
        "failing.harn",
        "fn main(harness: Harness) {\n  const value: int = \"not an int\"\n}\n",
    );
    assert!(
        !failing.is_empty(),
        "a type error must land in the same rendered buffer"
    );
}
