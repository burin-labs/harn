mod analysis;
mod bundle;
mod check_cmd;
mod config;
pub(crate) mod connector_matrix;
mod fmt;
mod host_capabilities;
mod imports;
mod lint;
mod lint_report;
mod mock_host;
mod outcome;
mod preflight;
pub(crate) mod provider_matrix;
mod source;
mod template_lint;

#[cfg(test)]
mod tests;

pub(crate) use analysis::{
    analyze_file, span_from_lexer_error, span_from_parser_error, FileAnalysisError,
};
pub(crate) use bundle::build_bundle_manifest;
pub(crate) use check_cmd::{
    check_file_inner, check_file_report, CheckReport, CHECK_SCHEMA_VERSION,
};
pub(crate) use config::{
    apply_harn_lint_config, apply_loaded_harn_lint_config, build_module_graph,
    build_module_graph_and_seed_analysis, collect_cross_file_imports, collect_harn_targets,
    harn_lint_complexity_threshold, harn_lint_persona_step_allowlist,
    harn_lint_require_file_header, load_harn_lint_config,
};
pub(crate) use fmt::{fmt_targets, fmt_targets_json, FmtMode, FMT_SCHEMA_VERSION};
pub(crate) use host_capabilities::load_host_capabilities;
pub(crate) use lint::{
    lint_file_inner, lint_fix_file, path_is_stdlib_source, project_engine_rule_sources,
};
pub(crate) use lint_report::{lint_file_report, LintFileReport, LintReport, LINT_SCHEMA_VERSION};
pub(crate) use preflight::{collect_preflight_diagnostics, is_preflight_allowed};
pub(crate) use template_lint::{collect_lint_targets, lint_prompt_file_inner};
