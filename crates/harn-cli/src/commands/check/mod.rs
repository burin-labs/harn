mod bundle;
mod check_cmd;
mod config;
pub(crate) mod connector_matrix;
mod fmt;
mod host_capabilities;
mod imports;
mod lint;
mod mock_host;
mod outcome;
mod preflight;
pub(crate) mod provider_matrix;
mod source;
mod template_lint;

#[cfg(test)]
mod tests;

pub(crate) use bundle::build_bundle_manifest;
pub(crate) use check_cmd::{
    check_file_inner, check_file_report, CheckReport, CHECK_SCHEMA_VERSION,
};
pub(crate) use config::{
    apply_harn_lint_config, build_module_graph, collect_cross_file_imports, collect_harn_targets,
    harn_lint_complexity_threshold, harn_lint_disabled_rules, harn_lint_persona_step_allowlist,
    harn_lint_require_file_header, harn_lint_template_variant_branch_threshold,
};
pub(crate) use fmt::{fmt_targets, fmt_targets_json, FmtMode, FMT_SCHEMA_VERSION};
pub(crate) use host_capabilities::load_host_capabilities;
pub(crate) use lint::{lint_file_inner, lint_fix_file};
pub(crate) use preflight::{collect_preflight_diagnostics, is_preflight_allowed};
pub(crate) use template_lint::{collect_prompt_targets, lint_prompt_file_inner};
