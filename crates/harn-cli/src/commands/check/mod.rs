mod analysis;
mod bundle;
mod check_cmd;
mod config;
pub(crate) mod connector_matrix;
mod driver;
mod fmt;
mod host_capabilities;
mod imports;
mod lint;
mod lint_report;
mod mock_host;
mod outcome;
mod preflight;
pub(crate) mod provider_matrix;
mod result_cache;
mod script_rules;
mod source;
mod template_lint;

#[cfg(test)]
mod tests;

pub(crate) use analysis::{
    analyze_file, span_from_lexer_error, span_from_parser_error, FileAnalysisError,
};
pub(crate) use bundle::build_bundle_manifest;
pub(crate) use check_cmd::{check_file_inner, CheckReport, CHECK_SCHEMA_VERSION};
pub(crate) use config::{
    apply_harn_lint_config, apply_loaded_harn_lint_config, build_module_graph,
    build_module_graph_and_seed_analysis, build_module_graph_with_parsed_sources,
    collect_cross_file_imports, collect_harn_targets, load_harn_lint_config, HarnLintConfig,
};
pub(crate) use driver::{check_files, check_files_independently, CheckCliOverrides};
pub(crate) use fmt::{fmt_targets, fmt_targets_json, FmtMode, FMT_SCHEMA_VERSION};
pub(crate) use harn_lint::path_is_stdlib_source;
pub(crate) use host_capabilities::load_host_capabilities;
pub(crate) use lint::{
    lint_file_inner, lint_fix_file, project_engine_rule_sources, project_native_rule_paths,
};
pub(crate) use lint_report::{run_lint_json, LintJsonOptions, LintReport, LINT_SCHEMA_VERSION};
pub(crate) use preflight::is_preflight_allowed;
pub(crate) use script_rules::run_project_script_rules;
pub(crate) use template_lint::{collect_lint_targets, lint_prompt_file_inner};

/// Collect preflight diagnostics against a caller-owned module graph while
/// resolving the standalone call's host-capability configuration once.
pub(crate) fn collect_preflight_diagnostics_with_module_graph(
    path: &std::path::Path,
    source: &str,
    program: &[harn_parser::SNode],
    config: &crate::package::CheckConfig,
    module_graph: &harn_modules::ModuleGraph,
) -> Vec<preflight::PreflightDiagnostic> {
    let host_capabilities = host_capabilities::resolve_host_capabilities(config);
    preflight::collect_preflight_diagnostics_with_host_capabilities(
        path,
        source,
        program,
        config,
        module_graph,
        &host_capabilities.capabilities,
    )
}
